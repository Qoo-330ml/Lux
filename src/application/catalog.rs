use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use tokio::sync::{Mutex, Notify, mpsc};

use crate::{
    application::access::{AccessError, AccessPrincipal, MediaAccessService},
    application::recommendations::{
        RECOMMENDATION_CANDIDATE_POOL, current_day_bucket, daily_recommendation_items,
    },
    storage::{
        CatalogFilterQuery, CatalogSort as StorageCatalogSort, Database, ResumeItemsQuery,
        StorageError, StoredCatalogDetail, StoredCatalogRow, StoredMediaChapter,
    },
};

#[derive(Clone, Debug, Default, Eq, Hash, PartialEq)]
pub struct CatalogFilter {
    pub item_types: Vec<String>,
    pub excluded_item_types: Vec<String>,
    pub item_ids: Option<Vec<String>>,
    pub person_id: Option<String>,
    pub media_source_ids: Option<Vec<String>>,
    pub years: Vec<i64>,
    pub is_played: Option<bool>,
    pub is_favorite: Option<bool>,
    pub metadata_pending: bool,
    pub sort_by: CatalogSort,
    pub descending: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum CatalogSort {
    #[default]
    Name,
    DateCreated,
    PremiereDate,
    Rating,
}

#[derive(Clone)]
pub struct CatalogService {
    database: Database,
    access: MediaAccessService,
    library_page_cache: Arc<LibraryPageCache>,
    search_flights: Arc<SearchFlightRegistry>,
}

const LIBRARY_PAGE_CACHE_TTL: Duration = Duration::from_secs(15);
const LIBRARY_PAGE_REFRESH_DEBOUNCE: Duration = Duration::from_secs(2);
const MAX_LIBRARY_PAGE_CACHE_ENTRIES: usize = 256;
// Cold pages are refreshed on demand; the background worker keeps only hot pages warm.
const MAX_LIBRARY_PAGE_REFRESH_ENTRIES: usize = 64;
const MAX_SEARCH_FLIGHTS: usize = 256;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct SearchFlightKey {
    user_id: String,
    is_admin: bool,
    library_ids: Option<Vec<String>>,
    query: String,
    like_query: String,
    offset: i64,
    limit: i64,
}

struct SearchFlight {
    result: Mutex<Option<Arc<CatalogPage>>>,
    completed: AtomicBool,
    notify: Notify,
}

struct SearchFlightRegistry {
    flights: Mutex<HashMap<SearchFlightKey, Arc<SearchFlight>>>,
}

enum SearchFlightHandle {
    Leader(Arc<SearchFlight>),
    Waiter(Arc<SearchFlight>),
    Bypass,
}

impl SearchFlightRegistry {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            flights: Mutex::new(HashMap::new()),
        })
    }

    async fn begin(&self, key: SearchFlightKey) -> (SearchFlightKey, SearchFlightHandle) {
        let mut flights = self.flights.lock().await;
        if let Some(flight) = flights.get(&key) {
            return (key, SearchFlightHandle::Waiter(flight.clone()));
        }
        if flights.len() >= MAX_SEARCH_FLIGHTS {
            return (key, SearchFlightHandle::Bypass);
        }
        let flight = Arc::new(SearchFlight {
            result: Mutex::new(None),
            completed: AtomicBool::new(false),
            notify: Notify::new(),
        });
        flights.insert(key.clone(), flight.clone());
        (key, SearchFlightHandle::Leader(flight))
    }

    async fn finish(
        &self,
        key: &SearchFlightKey,
        flight: &Arc<SearchFlight>,
        page: Option<Arc<CatalogPage>>,
    ) {
        *flight.result.lock().await = page;
        flight.completed.store(true, Ordering::Release);
        flight.notify.notify_waiters();
        let mut flights = self.flights.lock().await;
        if flights
            .get(key)
            .is_some_and(|current| Arc::ptr_eq(current, flight))
        {
            flights.remove(key);
        }
    }
}

impl SearchFlight {
    async fn wait(&self) -> Option<Arc<CatalogPage>> {
        if self.completed.load(Ordering::Acquire) {
            return self.result.lock().await.clone();
        }
        {
            let result = self.result.lock().await;
            if self.completed.load(Ordering::Acquire) {
                return result.clone();
            }
        }
        let notified = self.notify.notified();
        {
            let result = self.result.lock().await;
            if self.completed.load(Ordering::Acquire) {
                return result.clone();
            }
        }
        notified.await;
        self.result.lock().await.clone()
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct LibraryPageCacheKey {
    user_id: String,
    is_admin: bool,
    library_id: String,
    filter: CatalogFilter,
    offset: i64,
    limit: i64,
}

#[derive(Clone)]
struct LibraryPageRequest {
    principal: AccessPrincipal,
    library_id: String,
    filter: CatalogFilter,
    offset: i64,
    limit: i64,
}

struct CachedLibraryPage {
    generation: u64,
    refreshed_at: Instant,
    page: Arc<CatalogPage>,
}

struct LibraryPageCacheEntry {
    request: LibraryPageRequest,
    value: Mutex<Option<CachedLibraryPage>>,
    compute_lock: Mutex<()>,
    last_accessed_at: Mutex<Instant>,
}

struct LibraryPageCache {
    generation: AtomicU64,
    entries: Mutex<HashMap<LibraryPageCacheKey, Arc<LibraryPageCacheEntry>>>,
    refresh_tx: mpsc::Sender<()>,
    refresh_pending: AtomicBool,
}

impl LibraryPageCache {
    fn new(
        database: Database,
        access: MediaAccessService,
        search_flights: Arc<SearchFlightRegistry>,
    ) -> Arc<Self> {
        let (refresh_tx, mut refresh_rx) = mpsc::channel(1);
        let cache = Arc::new(Self {
            generation: AtomicU64::new(0),
            entries: Mutex::new(HashMap::new()),
            refresh_tx,
            refresh_pending: AtomicBool::new(false),
        });
        let worker_cache = Arc::downgrade(&cache);
        tokio::spawn(async move {
            while refresh_rx.recv().await.is_some() {
                while let Ok(Some(())) =
                    tokio::time::timeout(LIBRARY_PAGE_REFRESH_DEBOUNCE, refresh_rx.recv()).await
                {
                }
                let Some(cache) = worker_cache.upgrade() else {
                    break;
                };
                cache.refresh_pending.store(false, Ordering::Release);
                let service = CatalogService {
                    database: database.clone(),
                    access: access.clone(),
                    library_page_cache: cache.clone(),
                    search_flights: search_flights.clone(),
                };
                cache.refresh_entries(&service).await;
            }
        });
        cache
    }

    async fn entry(
        &self,
        key: LibraryPageCacheKey,
        request: LibraryPageRequest,
    ) -> Arc<LibraryPageCacheEntry> {
        let mut entries = self.entries.lock().await;
        if !entries.contains_key(&key) && entries.len() >= MAX_LIBRARY_PAGE_CACHE_ENTRIES {
            entries.clear();
        }
        entries
            .entry(key)
            .or_insert_with(|| {
                Arc::new(LibraryPageCacheEntry {
                    request,
                    value: Mutex::new(None),
                    compute_lock: Mutex::new(()),
                    last_accessed_at: Mutex::new(Instant::now()),
                })
            })
            .clone()
    }

    fn schedule_refresh(&self) {
        if self.refresh_pending.swap(true, Ordering::AcqRel) {
            return;
        }
        if self.refresh_tx.try_send(()).is_err() {
            self.refresh_pending.store(false, Ordering::Release);
        }
    }

    fn invalidate(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.schedule_refresh();
    }

    async fn get_or_load(
        &self,
        service: &CatalogService,
        request: LibraryPageRequest,
    ) -> Result<CatalogPage, CatalogError> {
        let key = LibraryPageCacheKey {
            user_id: request.principal.user_id.to_string(),
            is_admin: request.principal.is_admin,
            library_id: request.library_id.clone(),
            filter: request.filter.clone(),
            offset: request.offset,
            limit: request.limit,
        };
        let entry = self.entry(key, request).await;
        *entry.last_accessed_at.lock().await = Instant::now();
        let generation = self.generation.load(Ordering::Acquire);
        {
            let cached = entry.value.lock().await;
            if let Some(cached) = cached.as_ref() {
                if cached.generation != generation
                    || cached.refreshed_at.elapsed() >= LIBRARY_PAGE_CACHE_TTL
                {
                    self.schedule_refresh();
                }
                return Ok((*cached.page).clone());
            }
        }

        let _compute_guard = entry.compute_lock.lock().await;
        let generation = self.generation.load(Ordering::Acquire);
        {
            let cached = entry.value.lock().await;
            if let Some(cached) = cached.as_ref() {
                if cached.generation != generation
                    || cached.refreshed_at.elapsed() >= LIBRARY_PAGE_CACHE_TTL
                {
                    self.schedule_refresh();
                }
                return Ok((*cached.page).clone());
            }
        }

        let page = service.load_library_page(&entry.request).await?;
        *entry.value.lock().await = Some(CachedLibraryPage {
            generation,
            refreshed_at: Instant::now(),
            page: Arc::new(page.clone()),
        });
        Ok(page)
    }

    async fn refresh_entries(&self, service: &CatalogService) {
        let entries = self
            .entries
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut recently_accessed = Vec::with_capacity(entries.len());
        for entry in entries {
            let last_accessed_at = *entry.last_accessed_at.lock().await;
            recently_accessed.push((last_accessed_at, entry));
        }
        let entries = take_recent_entries(recently_accessed, MAX_LIBRARY_PAGE_REFRESH_ENTRIES);
        for entry in entries {
            let Ok(_compute_guard) = entry.compute_lock.try_lock() else {
                continue;
            };
            let generation = self.generation.load(Ordering::Acquire);
            let cached = entry.value.lock().await;
            if cached.as_ref().is_some_and(|cached| {
                cached.generation == generation
                    && cached.refreshed_at.elapsed() < LIBRARY_PAGE_CACHE_TTL
            }) {
                continue;
            }
            drop(cached);
            match service.load_library_page(&entry.request).await {
                Ok(page) if self.generation.load(Ordering::Acquire) == generation => {
                    *entry.value.lock().await = Some(CachedLibraryPage {
                        generation,
                        refreshed_at: Instant::now(),
                        page: Arc::new(page),
                    });
                }
                Ok(_) => self.schedule_refresh(),
                Err(error) => tracing::debug!(%error, "library page cache refresh failed"),
            }
        }
    }
}

fn take_recent_entries<T>(mut entries: Vec<(Instant, T)>, limit: usize) -> Vec<T> {
    entries.sort_unstable_by(|(left, _), (right, _)| right.cmp(left));
    entries
        .into_iter()
        .take(limit)
        .map(|(_, entry)| entry)
        .collect()
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CatalogItemCounts {
    pub movie_count: i64,
    pub series_count: i64,
    pub episode_count: i64,
    pub box_set_count: i64,
    pub item_count: i64,
}

impl CatalogService {
    pub fn new(database: Database, access: MediaAccessService) -> Self {
        let search_flights = SearchFlightRegistry::new();
        let library_page_cache =
            LibraryPageCache::new(database.clone(), access.clone(), search_flights.clone());
        Self {
            database,
            access,
            library_page_cache,
            search_flights,
        }
    }

    pub(crate) fn invalidate_library_pages(&self) {
        self.library_page_cache.invalidate();
        self.database.invalidate_recommendation_rating_cache();
    }

    pub async fn count_item_types(
        &self,
        principal: AccessPrincipal,
        user_id: &str,
        is_favorite: Option<bool>,
    ) -> Result<CatalogItemCounts, CatalogError> {
        let library_ids = self.access.accessible_library_ids(principal).await?;
        let counts = self
            .database
            .count_catalog_item_types(&library_ids, user_id, is_favorite)
            .await?;
        Ok(CatalogItemCounts {
            movie_count: counts.movie_count,
            series_count: counts.series_count,
            episode_count: counts.episode_count,
            box_set_count: counts.box_set_count,
            item_count: counts.item_count,
        })
    }

    pub(crate) async fn count_library_root_items(
        &self,
        library_ids: &[String],
    ) -> Result<HashMap<String, CatalogItemCounts>, CatalogError> {
        let counts = self
            .database
            .count_catalog_root_items_by_library(library_ids)
            .await?;
        Ok(counts
            .into_iter()
            .map(|(library_id, count)| {
                (
                    library_id,
                    CatalogItemCounts {
                        movie_count: count.movie_count,
                        series_count: count.series_count,
                        episode_count: count.episode_count,
                        box_set_count: count.box_set_count,
                        item_count: count.item_count,
                    },
                )
            })
            .collect())
    }

    pub async fn list_library_items(
        &self,
        principal: AccessPrincipal,
        library_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        let Some(library) = self.database.find_library(library_id).await? else {
            return Err(CatalogError::LibraryNotFound);
        };
        if !library.is_enabled {
            return Err(CatalogError::LibraryNotFound);
        }
        if !self.access.can_view_library(principal, library_id).await? {
            return Err(CatalogError::AccessDenied);
        }
        let (total, rows) = tokio::try_join!(
            self.database.count_catalog_items(Some(library_id)),
            self.database
                .list_catalog_rows(Some(library_id), offset, limit),
        )?;
        let mut items = assemble_items(rows);
        self.populate_episode_counts(&mut items).await?;
        Ok(CatalogPage {
            items,
            total,
            offset,
            limit,
        })
    }

    pub async fn list_library_items_filtered(
        &self,
        principal: AccessPrincipal,
        library_id: &str,
        filter: &CatalogFilter,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        let Some(library) = self.database.find_library(library_id).await? else {
            return Err(CatalogError::LibraryNotFound);
        };
        if !library.is_enabled {
            return Err(CatalogError::LibraryNotFound);
        }
        if !self.access.can_view_library(principal, library_id).await? {
            return Err(CatalogError::AccessDenied);
        }
        let request = LibraryPageRequest {
            principal,
            library_id: library_id.to_owned(),
            filter: filter.clone(),
            offset,
            limit,
        };
        return self.library_page_cache.get_or_load(self, request).await;
    }

    async fn load_library_page(
        &self,
        request: &LibraryPageRequest,
    ) -> Result<CatalogPage, CatalogError> {
        let library_ids = vec![request.library_id.clone()];
        let user_id = request.principal.user_id.to_string();
        let filter = &request.filter;
        let query = CatalogFilterQuery {
            library_ids: &library_ids,
            user_id: &user_id,
            item_types: &filter.item_types,
            excluded_item_types: &filter.excluded_item_types,
            item_ids: filter.item_ids.as_deref(),
            person_id: filter.person_id.as_deref(),
            media_source_ids: filter.media_source_ids.as_deref(),
            years: &filter.years,
            is_played: filter.is_played,
            is_favorite: filter.is_favorite,
            metadata_pending: filter.metadata_pending,
            sort_by: match filter.sort_by {
                CatalogSort::Name => StorageCatalogSort::Name,
                CatalogSort::DateCreated => StorageCatalogSort::DateCreated,
                CatalogSort::PremiereDate => StorageCatalogSort::PremiereDate,
                CatalogSort::Rating => StorageCatalogSort::Rating,
            },
            descending: filter.descending,
            offset: request.offset,
            limit: request.limit,
        };
        let (rows, total) = self.database.list_filtered_catalog_rows(&query).await?;
        let mut items = assemble_items(rows);
        self.populate_item_details_and_episode_counts(&mut items)
            .await?;
        Ok(CatalogPage {
            items,
            total,
            offset: request.offset,
            limit: request.limit,
        })
    }

    pub async fn list_all_items_filtered(
        &self,
        principal: AccessPrincipal,
        filter: &CatalogFilter,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        let library_ids = self.access.accessible_library_ids(principal).await?;
        let user_id = principal.user_id.to_string();
        let query = CatalogFilterQuery {
            library_ids: &library_ids,
            user_id: &user_id,
            item_types: &filter.item_types,
            excluded_item_types: &filter.excluded_item_types,
            item_ids: filter.item_ids.as_deref(),
            person_id: filter.person_id.as_deref(),
            media_source_ids: filter.media_source_ids.as_deref(),
            years: &filter.years,
            is_played: filter.is_played,
            is_favorite: filter.is_favorite,
            metadata_pending: filter.metadata_pending,
            sort_by: match filter.sort_by {
                CatalogSort::Name => StorageCatalogSort::Name,
                CatalogSort::DateCreated => StorageCatalogSort::DateCreated,
                CatalogSort::PremiereDate => StorageCatalogSort::PremiereDate,
                CatalogSort::Rating => StorageCatalogSort::Rating,
            },
            descending: filter.descending,
            offset,
            limit,
        };
        let (rows, total) = self.database.list_filtered_catalog_rows(&query).await?;
        let mut items = assemble_items(rows);
        self.populate_item_details_and_episode_counts(&mut items)
            .await?;
        Ok(CatalogPage {
            items,
            total,
            offset,
            limit,
        })
    }

    pub async fn list_children(
        &self,
        principal: AccessPrincipal,
        parent_id: &str,
        item_type: &str,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        if !self.access.can_view_item(principal, parent_id).await? {
            return Err(CatalogError::AccessDenied);
        }
        let total = self
            .database
            .count_catalog_children(parent_id, item_type)
            .await?;
        let rows = self
            .database
            .list_catalog_children(parent_id, item_type, offset, limit)
            .await?;
        let mut items = assemble_items(rows);
        if matches!(item_type, "SEASON" | "EPISODE") {
            self.populate_item_details_and_episode_counts(&mut items)
                .await?;
        } else {
            self.populate_episode_counts(&mut items).await?;
        }
        Ok(CatalogPage {
            items,
            total,
            offset,
            limit,
        })
    }

    pub async fn list_series_episodes(
        &self,
        principal: AccessPrincipal,
        series_id: &str,
        season_id: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        if !self.access.can_view_item(principal, series_id).await? {
            return Err(CatalogError::AccessDenied);
        }
        // A few Emby clients (including Yamby) use the selected season ID in
        // the `/Shows/{id}/Episodes` path, even though the protocol names
        // that path parameter `seriesId`. Resolve that compatibility shape to
        // the season's parent series and keep the season filter on the
        // selected season. Standard requests continue to use the supplied
        // series ID unchanged.
        let route_item = assemble_items(self.database.find_catalog_rows(series_id).await?)
            .into_iter()
            .next();
        let (resolved_series_id, resolved_season_id) = match route_item {
            Some(item) if item.item_type == "SEASON" => (
                item.parent_id.unwrap_or_else(|| series_id.to_owned()),
                Some(series_id.to_owned()),
            ),
            _ => (series_id.to_owned(), season_id.map(str::to_owned)),
        };
        if let Some(season_id) = resolved_season_id.as_deref() {
            let season = self
                .database
                .find_catalog_rows(season_id)
                .await
                .map(assemble_items)?
                .into_iter()
                .next()
                .filter(|item| {
                    item.item_type == "SEASON"
                        && item.parent_id.as_deref() == Some(resolved_series_id.as_str())
                });
            let Some(_) = season else {
                return Err(CatalogError::LibraryNotFound);
            };
        }

        let (item_ids, total) = self
            .database
            .list_series_episode_ids(
                &resolved_series_id,
                resolved_season_id.as_deref(),
                offset,
                limit,
            )
            .await?;
        let rows = self.database.list_catalog_rows_by_ids(&item_ids).await?;
        let items_by_id = assemble_items(rows)
            .into_iter()
            .map(|item| (item.id.clone(), item))
            .collect::<HashMap<_, _>>();
        let mut items = item_ids
            .iter()
            .filter_map(|item_id| items_by_id.get(item_id).cloned())
            .collect::<Vec<_>>();
        self.populate_item_details(&mut items).await?;
        Ok(CatalogPage {
            items,
            total,
            offset,
            limit,
        })
    }

    pub async fn list_collection_items(
        &self,
        principal: AccessPrincipal,
        collection_item_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        if !self
            .access
            .can_view_item(principal, collection_item_id)
            .await?
        {
            return Err(CatalogError::AccessDenied);
        }
        let library_ids = if principal.is_admin {
            None
        } else {
            Some(self.access.accessible_library_ids(principal).await?)
        };
        let (member_ids, total) = self
            .database
            .list_collection_member_ids_page(
                collection_item_id,
                library_ids.as_deref(),
                offset,
                limit,
            )
            .await?;
        let rows = self.database.list_catalog_rows_by_ids(&member_ids).await?;
        let mut items_by_id = assemble_items(rows)
            .into_iter()
            .map(|item| (item.id.clone(), item))
            .collect::<HashMap<_, _>>();
        let details = self
            .database
            .list_catalog_details_by_ids(&member_ids)
            .await?;
        let mut items = Vec::with_capacity(member_ids.len());
        for member_id in member_ids {
            if let Some(mut item) = items_by_id.remove(&member_id) {
                if let Some(detail) = details.get(&member_id) {
                    apply_catalog_detail(&mut item, detail);
                }
                items.push(item);
            }
        }
        Ok(CatalogPage {
            items,
            total,
            offset,
            limit,
        })
    }

    pub async fn list_next_up(
        &self,
        principal: AccessPrincipal,
        user_id: &str,
        series_id: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        self.list_progress_items(principal, user_id, series_id, offset, limit, &["EPISODE"])
            .await
    }

    pub async fn list_continue_watching(
        &self,
        principal: AccessPrincipal,
        user_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        let library_ids = self.access.accessible_library_ids(principal).await?;
        self.list_continue_watching_for_library_ids(&library_ids, user_id, offset, limit)
            .await
    }

    pub(crate) async fn list_continue_watching_for_library_ids(
        &self,
        library_ids: &[String],
        user_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        let played_percent = self.database.user_played_percent(user_id).await?;
        let (_, minimum_ticks) = self.database.resume_settings().await?;
        let item_types = ["MOVIE", "EPISODE"];
        let total = self
            .database
            .count_resume_items(
                user_id,
                library_ids,
                &item_types,
                played_percent,
                minimum_ticks,
            )
            .await?;
        let rows = self
            .database
            .list_resume_items(&ResumeItemsQuery {
                user_id,
                library_ids,
                item_types: &item_types,
                played_percent,
                minimum_ticks,
                offset,
                limit,
            })
            .await?;
        Ok(CatalogPage {
            items: assemble_items(rows),
            total,
            offset,
            limit,
        })
    }

    async fn list_progress_items(
        &self,
        principal: AccessPrincipal,
        user_id: &str,
        series_id: Option<&str>,
        offset: i64,
        limit: i64,
        item_types: &[&str],
    ) -> Result<CatalogPage, CatalogError> {
        let library_ids = self.access.accessible_library_ids(principal).await?;
        let total = self
            .database
            .count_progress_items(user_id, &library_ids, item_types, series_id)
            .await?;
        let rows = self
            .database
            .list_progress_items(user_id, &library_ids, item_types, series_id, offset, limit)
            .await?;
        Ok(CatalogPage {
            items: assemble_items(rows),
            total,
            offset,
            limit,
        })
    }

    pub async fn list_recently_added(
        &self,
        principal: AccessPrincipal,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        let library_ids = self.access.accessible_library_ids(principal).await?;
        self.list_recently_added_for_library_ids(&library_ids, offset, limit)
            .await
    }

    pub(crate) async fn list_recently_added_for_library_ids(
        &self,
        library_ids: &[String],
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        let (item_ids, total) = self
            .database
            .list_recent_catalog_item_ids(library_ids, offset, limit)
            .await?;
        let rows = self.database.list_catalog_rows_by_ids(&item_ids).await?;
        let mut items = assemble_items(rows);
        let details = self.database.list_catalog_details_by_ids(&item_ids).await?;
        for item in &mut items {
            if let Some(detail) = details.get(&item.id) {
                apply_catalog_detail(item, detail);
            }
        }
        let items = reorder_catalog_items(items, &item_ids);
        Ok(CatalogPage {
            items,
            total,
            offset,
            limit,
        })
    }

    pub async fn list_recently_added_by_library(
        &self,
        principal: AccessPrincipal,
        limit: i64,
    ) -> Result<Vec<(String, Vec<CatalogItem>)>, CatalogError> {
        let library_ids = self.access.accessible_library_ids(principal).await?;
        self.list_recently_added_by_library_ids(&library_ids, limit)
            .await
    }

    pub(crate) async fn list_recently_added_by_library_ids(
        &self,
        library_ids: &[String],
        limit: i64,
    ) -> Result<Vec<(String, Vec<CatalogItem>)>, CatalogError> {
        let rows = self
            .database
            .list_recent_catalog_rows_by_library(library_ids, limit)
            .await?;
        let mut items = assemble_items(rows);
        self.populate_episode_counts(&mut items).await?;
        let mut grouped = BTreeMap::<String, Vec<CatalogItem>>::new();
        for item in items {
            grouped
                .entry(item.library_id.clone())
                .or_default()
                .push(item);
        }
        Ok(grouped.into_iter().collect())
    }

    pub async fn list_recommended(
        &self,
        principal: AccessPrincipal,
        user_id: &str,
        limit: i64,
    ) -> Result<Vec<CatalogItem>, CatalogError> {
        let library_ids = self.access.accessible_library_ids(principal).await?;
        self.list_recommended_for_library_ids(&library_ids, user_id, limit)
            .await
    }

    pub(crate) async fn list_recommended_for_library_ids(
        &self,
        library_ids: &[String],
        user_id: &str,
        limit: i64,
    ) -> Result<Vec<CatalogItem>, CatalogError> {
        let rows = self
            .database
            .list_recommended_catalog_rows(user_id, library_ids, 0, RECOMMENDATION_CANDIDATE_POOL)
            .await?;
        let mut items = assemble_items(rows);
        self.populate_episode_counts(&mut items).await?;
        Ok(daily_recommendation_items(
            items,
            user_id,
            current_day_bucket(),
            usize::try_from(limit).unwrap_or(0),
        ))
    }

    async fn populate_episode_counts(&self, items: &mut [CatalogItem]) -> Result<(), CatalogError> {
        let item_ids = items
            .iter()
            .filter(|item| item.item_type == "SERIES" || item.item_type == "SEASON")
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();
        let episode_counts = self.database.list_episode_counts(&item_ids).await?;
        let details = self.database.list_catalog_details_by_ids(&item_ids).await?;
        for item in &mut *items {
            if let Some(detail) = details.get(&item.id) {
                item.season_count = Some(detail.season_count);
                item.series_name = detail.series_name.clone();
            }
            item.episode_count = episode_counts.get(&item.id).copied();
        }
        Ok(())
    }

    async fn populate_item_details_and_episode_counts(
        &self,
        items: &mut [CatalogItem],
    ) -> Result<(), CatalogError> {
        let item_ids = items.iter().map(|item| item.id.clone()).collect::<Vec<_>>();
        let episode_item_ids = items
            .iter()
            .filter(|item| item.item_type == "SERIES" || item.item_type == "SEASON")
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();
        let (details, episode_counts) = tokio::try_join!(
            self.database.list_catalog_details_by_ids(&item_ids),
            self.database.list_episode_counts(&episode_item_ids),
        )?;
        for item in &mut *items {
            if let Some(detail) = details.get(&item.id) {
                apply_catalog_detail(item, detail);
                item.season_count = Some(detail.season_count);
                item.series_name = detail.series_name.clone();
            }
            item.episode_count = episode_counts.get(&item.id).copied();
        }
        Ok(())
    }

    async fn populate_item_details(&self, items: &mut [CatalogItem]) -> Result<(), CatalogError> {
        let item_ids = items.iter().map(|item| item.id.clone()).collect::<Vec<_>>();
        let details = self.database.list_catalog_details_by_ids(&item_ids).await?;
        for item in items {
            if let Some(detail) = details.get(&item.id) {
                apply_catalog_detail(item, detail);
            }
        }
        Ok(())
    }

    pub async fn find_item(
        &self,
        principal: AccessPrincipal,
        item_id: &str,
    ) -> Result<Option<CatalogItem>, CatalogError> {
        if !self.access.can_view_item(principal, item_id).await? {
            return Ok(None);
        }
        let rows = self.database.find_catalog_rows(item_id).await?;
        let Some(mut item) = assemble_items(rows).into_iter().next() else {
            return Ok(None);
        };
        if let Some(detail) = self.database.find_catalog_detail(item_id).await? {
            apply_catalog_detail(&mut item, &detail);
        }
        self.populate_chapters(std::slice::from_mut(&mut item))
            .await?;
        Ok(Some(item))
    }

    pub(crate) async fn find_items(
        &self,
        principal: AccessPrincipal,
        item_ids: &[String],
    ) -> Result<HashMap<String, CatalogItem>, CatalogError> {
        if item_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let allowed_libraries = if principal.is_admin {
            None
        } else {
            Some(
                self.access
                    .accessible_library_ids(principal)
                    .await?
                    .into_iter()
                    .collect::<HashSet<_>>(),
            )
        };
        let mut items = assemble_items(self.database.list_catalog_rows_by_ids(item_ids).await?);
        if let Some(allowed_libraries) = allowed_libraries {
            items.retain(|item| allowed_libraries.contains(&item.library_id));
        }
        self.populate_item_details(&mut items).await?;
        self.populate_chapters(&mut items).await?;
        Ok(items
            .into_iter()
            .map(|item| (item.id.clone(), item))
            .collect())
    }

    pub async fn find_item_by_media_source_id(
        &self,
        principal: AccessPrincipal,
        media_source_id: &str,
    ) -> Result<Option<CatalogItem>, CatalogError> {
        // Emby proxy lookups only need the source-bearing catalog rows. Avoid
        // loading detail metadata here; the normal item lookup retains the
        // richer response for clients addressing the item ID directly.
        let Some(item_id) = self
            .database
            .find_item_id_by_media_source_id(media_source_id)
            .await?
        else {
            return Ok(None);
        };
        if !self.access.can_view_item(principal, &item_id).await? {
            return Ok(None);
        }
        let rows = self.database.find_catalog_rows(&item_id).await?;
        Ok(assemble_items(rows).into_iter().next())
    }

    pub async fn populate_chapters(&self, items: &mut [CatalogItem]) -> Result<(), CatalogError> {
        let source_ids = items
            .iter()
            .flat_map(|item| item.media_sources.iter().map(|source| source.id.clone()))
            .collect::<Vec<_>>();
        let chapters = self
            .database
            .list_media_chapters_by_source_ids(&source_ids)
            .await?;
        for item in items {
            for source in &mut item.media_sources {
                source.chapters = chapters
                    .get(&source.id)
                    .map(|source_chapters| {
                        source_chapters.iter().map(CatalogChapter::from).collect()
                    })
                    .unwrap_or_default();
            }
        }
        Ok(())
    }

    pub async fn populate_image_tags(&self, items: &mut [CatalogItem]) -> Result<(), CatalogError> {
        let item_ids = items.iter().map(|item| item.id.clone()).collect::<Vec<_>>();
        let tags = self
            .database
            .list_catalog_image_tags_by_ids(&item_ids)
            .await?;
        for item in &mut *items {
            item.fanart_image_tags.clear();
            let Some(item_tags) = tags.get(&item.id) else {
                continue;
            };
            for tag in item_tags {
                if tag.image_type != "FANART" && tag.image_index != 0 {
                    continue;
                }
                match tag.image_type.as_str() {
                    "POSTER" => item.poster_image_tag = Some(tag.id.clone()),
                    "FANART" => item.fanart_image_tags.push(tag.id.clone()),
                    "THUMB" => item.thumb_image_tag = Some(tag.id.clone()),
                    "LOGO" => item.logo_image_tag = Some(tag.id.clone()),
                    "BANNER" => item.banner_image_tag = Some(tag.id.clone()),
                    "DISC" => item.disc_image_tag = Some(tag.id.clone()),
                    "ART" => item.art_image_tag = Some(tag.id.clone()),
                    "WALLPAPER" => item.wallpaper_image_tag = Some(tag.id.clone()),
                    _ => {}
                }
            }
            item.fanart_image_tag = item.fanart_image_tags.first().cloned();
        }
        let series_ids = items
            .iter()
            .filter(|item| matches!(item.item_type.as_str(), "SEASON" | "EPISODE"))
            .filter_map(|item| item.series_id.clone().or_else(|| item.parent_id.clone()))
            .collect::<Vec<_>>();
        if !series_ids.is_empty() {
            let series_tags = self
                .database
                .list_catalog_image_tags_by_ids(&series_ids)
                .await?;
            for item in items
                .iter_mut()
                .filter(|item| matches!(item.item_type.as_str(), "SEASON" | "EPISODE"))
            {
                let Some(series_id) = item.series_id.clone().or_else(|| item.parent_id.clone())
                else {
                    continue;
                };
                let Some(tags) = series_tags.get(&series_id) else {
                    continue;
                };
                let mut fanart = Vec::new();
                for tag in tags {
                    match tag.image_type.as_str() {
                        "POSTER" if tag.image_index == 0 => {
                            item.series_primary_image_tag = Some(tag.id.clone())
                        }
                        "FANART" if tag.image_index == 0 => fanart.push(tag.id.clone()),
                        "LOGO" if tag.image_index == 0 => {
                            item.series_logo_image_tag = Some(tag.id.clone())
                        }
                        "THUMB" if tag.image_index == 0 => {
                            item.series_thumb_image_tag = Some(tag.id.clone())
                        }
                        _ => {}
                    }
                }
                item.series_fanart_image_tags = fanart;
            }
        }
        Ok(())
    }

    pub async fn search_items(
        &self,
        principal: AccessPrincipal,
        query: &str,
        like_query: &str,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        let library_ids = if principal.is_admin {
            None
        } else {
            Some(self.access.accessible_library_ids(principal).await?)
        };
        let key = SearchFlightKey {
            user_id: principal.user_id.to_string(),
            is_admin: principal.is_admin,
            library_ids: library_ids.clone(),
            query: query.to_owned(),
            like_query: like_query.to_owned(),
            offset,
            limit,
        };
        let (key, flight) = self.search_flights.begin(key).await;
        match flight {
            SearchFlightHandle::Leader(flight) => {
                let service = self.clone();
                let registry = self.search_flights.clone();
                let task_key = key.clone();
                let task_flight = flight.clone();
                let task_query = query.to_owned();
                let task_like_query = like_query.to_owned();
                let task_library_ids = library_ids.clone();
                let task = tokio::spawn(async move {
                    let result = service
                        .search_items_uncached(
                            &task_query,
                            &task_like_query,
                            task_library_ids.as_deref(),
                            offset,
                            limit,
                        )
                        .await;
                    let page = result.as_ref().ok().map(|page| Arc::new(page.clone()));
                    registry.finish(&task_key, &task_flight, page).await;
                    result
                });
                match task.await {
                    Ok(result) => result,
                    Err(error) => {
                        self.search_flights.finish(&key, &flight, None).await;
                        Err(CatalogError::Storage(StorageError::Serialization(format!(
                            "search worker stopped: {error}"
                        ))))
                    }
                }
            }
            SearchFlightHandle::Waiter(flight) => {
                if let Some(page) = flight.wait().await {
                    return Ok((*page).clone());
                }
                self.search_items_uncached(query, like_query, library_ids.as_deref(), offset, limit)
                    .await
            }
            SearchFlightHandle::Bypass => {
                self.search_items_uncached(query, like_query, library_ids.as_deref(), offset, limit)
                    .await
            }
        }
    }

    async fn search_items_uncached(
        &self,
        query: &str,
        like_query: &str,
        library_ids: Option<&[String]>,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        let (ids, total) = self
            .database
            .search_catalog_item_ids(query, like_query, library_ids, offset, limit)
            .await?;
        let rows = self.database.list_catalog_rows_by_ids(&ids).await?;
        let items_by_id = assemble_items(rows)
            .into_iter()
            .map(|item| (item.id.clone(), item))
            .collect::<HashMap<_, _>>();
        let details = self.database.list_catalog_details_by_ids(&ids).await?;
        let mut items = Vec::with_capacity(ids.len());
        for item_id in ids {
            let Some(mut item) = items_by_id.get(&item_id).cloned() else {
                continue;
            };
            if let Some(detail) = details.get(&item_id) {
                apply_catalog_detail(&mut item, detail);
            }
            items.push(item);
        }
        Ok(CatalogPage {
            items,
            total,
            offset,
            limit,
        })
    }
}

pub fn normalize_search_query(value: &str) -> Option<String> {
    let tokens = value
        .split_whitespace()
        .filter(|token| !token.is_empty())
        .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
        .collect::<Vec<_>>();
    if tokens.is_empty() {
        return None;
    }
    // A multi-word search should narrow results to items containing every
    // token. The LIKE fallback already treats the full input as a phrase;
    // keeping FTS on the same AND semantics prevents broad matches such as
    // "Reference Movie 40" from returning every title containing "Movie".
    Some(tokens.join(" AND "))
}

pub fn normalize_search_like_query(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let escaped = value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    Some(format!("%{escaped}%"))
}

#[derive(Clone, Debug, PartialEq)]
pub struct CatalogPage {
    pub items: Vec<CatalogItem>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CatalogItem {
    pub id: String,
    pub library_id: String,
    pub item_type: String,
    pub parent_id: Option<String>,
    pub series_id: Option<String>,
    pub series_name: Option<String>,
    pub season_number: Option<i64>,
    pub episode_number: Option<i64>,
    pub title: String,
    pub sort_title: String,
    pub original_title: Option<String>,
    pub overview: Option<String>,
    pub premiere_date: Option<String>,
    pub last_air_date: Option<String>,
    pub status: Option<String>,
    pub original_language: Option<String>,
    pub provider_ids: BTreeMap<String, String>,
    pub season_count: Option<i64>,
    pub episode_count: Option<i64>,
    pub production_year: Option<i64>,
    pub rating: Option<f64>,
    pub rating_source: Option<String>,
    pub runtime_ticks: Option<i64>,
    pub poster_image_tag: Option<String>,
    pub series_primary_image_tag: Option<String>,
    pub series_fanart_image_tags: Vec<String>,
    pub series_logo_image_tag: Option<String>,
    pub series_thumb_image_tag: Option<String>,
    pub fanart_image_tag: Option<String>,
    pub fanart_image_tags: Vec<String>,
    pub thumb_image_tag: Option<String>,
    pub logo_image_tag: Option<String>,
    pub banner_image_tag: Option<String>,
    pub disc_image_tag: Option<String>,
    pub art_image_tag: Option<String>,
    pub wallpaper_image_tag: Option<String>,
    pub media_sources: Vec<CatalogSource>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogSource {
    pub id: String,
    pub source_kind: String,
    pub container: Option<String>,
    pub size: Option<i64>,
    pub external_url: Option<String>,
    pub edition_name: Option<String>,
    pub quality_label: Option<String>,
    pub bitrate: Option<i64>,
    pub duration_ticks: Option<i64>,
    pub is_default: bool,
    pub probe_status: String,
    pub streams: Vec<CatalogStream>,
    pub chapters: Vec<CatalogChapter>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogChapter {
    pub start_position_ticks: i64,
    pub name: Option<String>,
    pub marker_type: String,
    pub chapter_index: i64,
}

impl From<&StoredMediaChapter> for CatalogChapter {
    fn from(chapter: &StoredMediaChapter) -> Self {
        Self {
            start_position_ticks: chapter.start_position_ticks,
            name: chapter.name.clone(),
            marker_type: chapter.marker_type.clone(),
            chapter_index: chapter.chapter_index,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogStream {
    pub index: i64,
    pub stream_type: String,
    pub codec: Option<String>,
    pub language: Option<String>,
    pub title: Option<String>,
    pub is_external: bool,
    pub is_default: bool,
    pub is_forced: bool,
    pub details: std::collections::BTreeMap<String, serde_json::Value>,
}

fn assemble_items(rows: Vec<StoredCatalogRow>) -> Vec<CatalogItem> {
    let mut items = Vec::new();
    for row in rows {
        let item_index = match items
            .iter()
            .position(|item: &CatalogItem| item.id == row.item_id)
        {
            Some(index) => index,
            None => {
                items.push(CatalogItem {
                    id: row.item_id.clone(),
                    library_id: row.library_id.clone(),
                    item_type: row.item_type.clone(),
                    parent_id: row.parent_id.clone(),
                    series_id: row.series_id.clone(),
                    series_name: None,
                    season_number: row.season_number,
                    episode_number: row.episode_number,
                    title: row.title.clone(),
                    sort_title: row.sort_title.clone(),
                    original_title: row.original_title.clone(),
                    overview: row.overview.clone(),
                    premiere_date: None,
                    last_air_date: None,
                    status: None,
                    original_language: None,
                    provider_ids: BTreeMap::new(),
                    season_count: None,
                    episode_count: None,
                    production_year: row.production_year,
                    rating: row.rating,
                    rating_source: row.rating_source.clone(),
                    runtime_ticks: row.runtime_ticks,
                    poster_image_tag: row.poster_image_tag.clone(),
                    series_primary_image_tag: None,
                    series_fanart_image_tags: Vec::new(),
                    series_logo_image_tag: None,
                    series_thumb_image_tag: None,
                    fanart_image_tag: row.fanart_image_tag.clone(),
                    fanart_image_tags: row.fanart_image_tag.clone().into_iter().collect(),
                    thumb_image_tag: row.thumb_image_tag.clone(),
                    logo_image_tag: row.logo_image_tag.clone(),
                    banner_image_tag: None,
                    disc_image_tag: None,
                    art_image_tag: None,
                    wallpaper_image_tag: None,
                    media_sources: Vec::new(),
                });
                items.len() - 1
            }
        };
        let Some(source_id) = row.source_id else {
            continue;
        };
        let item = &mut items[item_index];
        let source_index = match item
            .media_sources
            .iter()
            .position(|source| source.id == source_id)
        {
            Some(index) => index,
            None => {
                item.media_sources.push(CatalogSource {
                    id: source_id,
                    source_kind: row.source_kind.unwrap_or_else(|| "LOCAL_FILE".to_owned()),
                    container: row.container.clone(),
                    size: row.size,
                    external_url: row.external_url.clone(),
                    edition_name: row.edition_name.clone(),
                    quality_label: row.quality_label.clone(),
                    bitrate: row.bitrate,
                    duration_ticks: row.duration_ticks,
                    is_default: row.is_default.unwrap_or(false),
                    probe_status: row.probe_status.unwrap_or_else(|| "PENDING".to_owned()),
                    streams: Vec::new(),
                    chapters: Vec::new(),
                });
                item.media_sources.len() - 1
            }
        };
        let Some(stream_id) = row.stream_id else {
            continue;
        };
        let source = &mut item.media_sources[source_index];
        if source
            .streams
            .iter()
            .any(|stream| stream.index == row.stream_index.unwrap_or(-1))
        {
            continue;
        }
        source.streams.push(CatalogStream {
            index: row.stream_index.unwrap_or(source.streams.len() as i64),
            stream_type: row.stream_type.unwrap_or_else(|| "UNKNOWN".to_owned()),
            codec: row.codec,
            language: row.language,
            title: row.stream_title,
            is_external: row.stream_is_external.unwrap_or(false),
            is_default: row.stream_is_default.unwrap_or(false),
            is_forced: row.stream_is_forced.unwrap_or(false),
            details: row
                .stream_details_json
                .as_deref()
                .and_then(|value| serde_json::from_str(value).ok())
                .unwrap_or_default(),
        });
        let _ = stream_id;
    }
    items
}

fn reorder_catalog_items(items: Vec<CatalogItem>, item_ids: &[String]) -> Vec<CatalogItem> {
    let mut items_by_id = items
        .into_iter()
        .map(|item| (item.id.clone(), item))
        .collect::<HashMap<_, _>>();
    item_ids
        .iter()
        .filter_map(|item_id| items_by_id.remove(item_id))
        .collect()
}

fn apply_catalog_detail(item: &mut CatalogItem, detail: &StoredCatalogDetail) {
    item.series_name = detail.series_name.clone();
    item.premiere_date = detail.premiere_date.clone();
    item.last_air_date = detail.last_air_date.clone();
    item.status = detail.status.clone();
    item.original_language = detail.original_language.clone();
    item.provider_ids = provider_ids_from_json(detail.provider_ids_json.as_deref());
    if item.item_type == "SERIES" || item.item_type == "SEASON" {
        item.season_count = Some(detail.season_count);
        item.episode_count = Some(detail.episode_count);
    }
}

fn provider_ids_from_json(raw: Option<&str>) -> BTreeMap<String, String> {
    raw.and_then(|value| {
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(value).ok()
    })
    .map(|object| {
        object
            .into_iter()
            .filter_map(|(name, value)| {
                let id = value
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| value.as_i64().map(|value| value.to_string()))?;
                (!name.trim().is_empty() && !id.trim().is_empty()).then_some((name, id))
            })
            .collect()
    })
    .unwrap_or_default()
}

#[derive(Debug)]
pub enum CatalogError {
    LibraryNotFound,
    AccessDenied,
    Storage(StorageError),
}

impl fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LibraryNotFound => formatter.write_str("library not found"),
            Self::AccessDenied => formatter.write_str("library access denied"),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for CatalogError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::LibraryNotFound | Self::AccessDenied => None,
            Self::Storage(error) => Some(error),
        }
    }
}

impl From<StorageError> for CatalogError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<AccessError> for CatalogError {
    fn from(error: AccessError) -> Self {
        match error {
            AccessError::Storage(error) => Self::Storage(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        time::{Duration, Instant},
    };

    use crate::application::recommendations::daily_recommendation_items;

    use super::{
        CatalogItem, MAX_LIBRARY_PAGE_REFRESH_ENTRIES, SearchFlightHandle, SearchFlightKey,
        SearchFlightRegistry, reorder_catalog_items, take_recent_entries,
    };

    fn catalog_item(id: &str) -> CatalogItem {
        CatalogItem {
            id: id.to_owned(),
            library_id: "library-1".to_owned(),
            item_type: "MOVIE".to_owned(),
            parent_id: None,
            series_id: None,
            series_name: None,
            season_number: None,
            episode_number: None,
            title: id.to_owned(),
            sort_title: id.to_owned(),
            original_title: None,
            overview: None,
            premiere_date: None,
            last_air_date: None,
            status: None,
            original_language: None,
            provider_ids: BTreeMap::new(),
            season_count: None,
            episode_count: None,
            production_year: None,
            rating: None,
            rating_source: None,
            runtime_ticks: None,
            poster_image_tag: None,
            series_primary_image_tag: None,
            series_fanart_image_tags: Vec::new(),
            series_logo_image_tag: None,
            series_thumb_image_tag: None,
            fanart_image_tag: None,
            fanart_image_tags: Vec::new(),
            thumb_image_tag: None,
            logo_image_tag: None,
            banner_image_tag: None,
            disc_image_tag: None,
            art_image_tag: None,
            wallpaper_image_tag: None,
            media_sources: Vec::new(),
        }
    }

    #[test]
    fn batched_recent_items_keep_recent_order() {
        let requested_ids = ["recent-1".to_owned(), "recent-2".to_owned()];
        let items = vec![catalog_item("recent-2"), catalog_item("recent-1")];

        let ordered = reorder_catalog_items(items, &requested_ids);

        assert_eq!(
            ordered
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["recent-1", "recent-2"]
        );
    }

    #[test]
    fn daily_recommendations_are_stable_for_one_day_and_change_next_day() {
        let items = || (1..=6).collect::<Vec<_>>();

        let day_one = daily_recommendation_items(items(), "user-1", 20, 3);
        let same_day = daily_recommendation_items(items(), "user-1", 20, 3);
        let next_day = daily_recommendation_items(items(), "user-1", 21, 3);

        assert_eq!(day_one, same_day);
        assert_ne!(day_one, next_day);
    }

    #[test]
    fn library_page_refresh_is_bounded_to_recent_entries() {
        let now = Instant::now();
        let entries = (0..(MAX_LIBRARY_PAGE_REFRESH_ENTRIES + 1))
            .map(|index| {
                (
                    now - Duration::from_secs(index as u64),
                    format!("page-{index}"),
                )
            })
            .collect();

        let selected = take_recent_entries(entries, MAX_LIBRARY_PAGE_REFRESH_ENTRIES);

        assert_eq!(selected.len(), MAX_LIBRARY_PAGE_REFRESH_ENTRIES);
        assert_eq!(selected.first().map(String::as_str), Some("page-0"));
        assert!(!selected.iter().any(|page| page == "page-64"));
    }

    #[tokio::test]
    async fn failed_search_flight_does_not_leave_waiters_blocked() {
        let registry = SearchFlightRegistry::new();
        let key = SearchFlightKey {
            user_id: "user-1".to_owned(),
            is_admin: false,
            library_ids: Some(vec!["library-1".to_owned()]),
            query: "query".to_owned(),
            like_query: "%query%".to_owned(),
            offset: 0,
            limit: 50,
        };
        let (key, handle) = registry.begin(key).await;
        let SearchFlightHandle::Leader(flight) = handle else {
            panic!("first request must become the search flight leader");
        };

        registry.finish(&key, &flight, None).await;

        let result = tokio::time::timeout(std::time::Duration::from_millis(50), flight.wait())
            .await
            .expect("a failed search flight must wake waiters");
        assert!(result.is_none());
    }
}
