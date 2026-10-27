use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt,
    path::Path,
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
        RECOMMENDATION_CANDIDATE_POOL, current_recommendation_batch_key,
        daily_recommendation_items, recommendation_library_scope_key,
    },
    storage::{
        CatalogFilterQuery, CatalogSearchQuery, CatalogSort as StorageCatalogSort, Database,
        GLOBAL_COLLECTION_LIBRARY_ID, ResumeItemsQuery, StorageError, StoredCatalogDetail,
        StoredCatalogRow, StoredMediaChapter, UserContentRestrictions,
    },
};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CatalogFilter {
    pub item_types: Vec<String>,
    pub excluded_item_types: Vec<String>,
    pub item_ids: Option<Vec<String>>,
    pub person_id: Option<String>,
    pub media_source_ids: Option<Vec<String>>,
    pub provider_id_equals: Option<Vec<(String, String)>>,
    pub years: Vec<i64>,
    pub is_played: Option<bool>,
    pub is_favorite: Option<bool>,
    pub min_date_last_saved: Option<i64>,
    pub metadata_pending: bool,
    pub sort_by: CatalogSort,
    pub descending: bool,
    pub include_total: bool,
}

impl Default for CatalogFilter {
    fn default() -> Self {
        Self {
            item_types: Vec::new(),
            excluded_item_types: Vec::new(),
            item_ids: None,
            person_id: None,
            media_source_ids: None,
            provider_id_equals: None,
            years: Vec::new(),
            is_played: None,
            is_favorite: None,
            min_date_last_saved: None,
            metadata_pending: false,
            sort_by: CatalogSort::default(),
            descending: false,
            include_total: true,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum CatalogParentScope {
    Root,
    Item(String),
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum CatalogSort {
    #[default]
    Name,
    SortName,
    DateCreated,
    PremiereDate,
    ProductionYear,
    Rating,
}

#[derive(Clone)]
pub struct CatalogService {
    database: Database,
    access: MediaAccessService,
    library_page_cache: Arc<LibraryPageCache>,
    search_flights: Arc<SearchFlightRegistry>,
    recommendation_compute_lock: Arc<Mutex<()>>,
    read_model_mode: ReadModelMode,
}

/// Read-model rollout switch. `on` stays unavailable until shadow parity is proven.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadModelMode {
    Off,
    Shadow,
}

impl ReadModelMode {
    pub fn from_env() -> Self {
        match std::env::var("LUX_READ_MODEL_MODE")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "shadow" => Self::Shadow,
            _ => Self::Off,
        }
    }
}

const LIBRARY_PAGE_CACHE_TTL: Duration = Duration::from_secs(15);
const LIBRARY_PAGE_REFRESH_DEBOUNCE: Duration = Duration::from_secs(2);
const MAX_LIBRARY_PAGE_CACHE_ENTRIES: usize = 256;
// Cold pages are refreshed on demand; the background worker keeps only hot pages warm.
const MAX_LIBRARY_PAGE_REFRESH_ENTRIES: usize = 64;
const MAX_SEARCH_FLIGHTS: usize = 256;
const DEFAULT_SEARCH_ITEM_TYPES: [&str; 2] = ["MOVIE", "SERIES"];

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct SearchFlightKey {
    user_id: String,
    is_admin: bool,
    library_ids: Option<Vec<String>>,
    query: String,
    like_query: String,
    item_types: Vec<String>,
    content_restriction_version: Option<i64>,
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
    parent_scope: Option<CatalogParentScope>,
    offset: i64,
    limit: i64,
    content_restriction_version: Option<i64>,
}

#[derive(Clone)]
struct LibraryPageRequest {
    principal: AccessPrincipal,
    library_id: String,
    filter: CatalogFilter,
    parent_scope: Option<CatalogParentScope>,
    offset: i64,
    limit: i64,
    content_restrictions: Option<UserContentRestrictions>,
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
                    recommendation_compute_lock: Arc::new(Mutex::new(())),
                    read_model_mode: ReadModelMode::Off,
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
            user_id: request.principal.user_id_string(),
            is_admin: request.principal.is_admin,
            library_id: request.library_id.clone(),
            filter: request.filter.clone(),
            parent_scope: request.parent_scope.clone(),
            offset: request.offset,
            limit: request.limit,
            content_restriction_version: request
                .content_restrictions
                .as_ref()
                .map(|restrictions| restrictions.version),
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
    pub fn with_read_model_mode(mut self, mode: ReadModelMode) -> Self {
        self.read_model_mode = mode;
        self
    }

    pub fn new(database: Database, access: MediaAccessService) -> Self {
        let search_flights = SearchFlightRegistry::new();
        let library_page_cache =
            LibraryPageCache::new(database.clone(), access.clone(), search_flights.clone());
        Self {
            database,
            access,
            library_page_cache,
            search_flights,
            recommendation_compute_lock: Arc::new(Mutex::new(())),
            read_model_mode: ReadModelMode::Off,
        }
    }

    #[cfg(test)]
    pub(crate) fn recommendation_compute_lock(&self) -> Arc<Mutex<()>> {
        self.recommendation_compute_lock.clone()
    }

    pub(crate) fn invalidate_library_pages(&self) {
        self.library_page_cache.invalidate();
    }

    pub async fn list_global_collection_cards(
        &self,
        principal: AccessPrincipal,
        offset: i64,
        limit: i64,
    ) -> Result<GlobalCollectionPage, CatalogError> {
        self.list_global_collection_cards_ordered(principal, offset, limit, false)
            .await
    }

    /// Counts the global collections the principal can see without building any card.
    pub async fn count_global_collection_cards(
        &self,
        principal: AccessPrincipal,
    ) -> Result<i64, CatalogError> {
        let content_restrictions = self.content_restrictions_for_principal(principal).await?;
        self.count_global_collection_cards_with_restrictions(
            principal,
            content_restrictions.as_ref(),
        )
        .await
    }

    /// Counts the visible global collections with already resolved content restrictions.
    ///
    /// Listings that batch user access snapshots already hold the restrictions, so they can skip
    /// the extra per-user restriction lookup.
    pub(crate) async fn count_global_collection_cards_with_restrictions(
        &self,
        principal: AccessPrincipal,
        content_restrictions: Option<&UserContentRestrictions>,
    ) -> Result<i64, CatalogError> {
        Ok(self
            .database
            .count_visible_global_collections_with_restrictions(
                &principal.user_id_string(),
                principal.is_admin,
                None,
                content_restrictions,
            )
            .await?)
    }

    pub async fn list_recent_global_collection_cards(
        &self,
        principal: AccessPrincipal,
        offset: i64,
        limit: i64,
    ) -> Result<GlobalCollectionPage, CatalogError> {
        self.list_global_collection_cards_ordered(principal, offset, limit, true)
            .await
    }

    async fn list_global_collection_cards_ordered(
        &self,
        principal: AccessPrincipal,
        offset: i64,
        limit: i64,
        recently_updated_first: bool,
    ) -> Result<GlobalCollectionPage, CatalogError> {
        let content_restrictions = self.content_restrictions_for_principal(principal).await?;
        let (stored_cards, total) = if recently_updated_first {
            self.database
                .list_recent_global_collection_cards_page_with_restrictions(
                    &principal.user_id_string(),
                    principal.is_admin,
                    offset,
                    limit,
                    content_restrictions.as_ref(),
                )
                .await?
        } else {
            self.database
                .list_global_collection_cards_page_with_restrictions(
                    &principal.user_id_string(),
                    principal.is_admin,
                    offset,
                    limit,
                    content_restrictions.as_ref(),
                )
                .await?
        };
        let item_ids = stored_cards
            .iter()
            .map(|card| card.item_id.clone())
            .collect::<Vec<_>>();
        let mut items_by_id =
            assemble_items(self.database.list_catalog_rows_by_ids(&item_ids).await?)
                .into_iter()
                .map(|item| (item.id.clone(), item))
                .collect::<HashMap<_, _>>();
        let details = self.database.list_catalog_details_by_ids(&item_ids).await?;
        let mut cards = Vec::with_capacity(stored_cards.len());
        for stored in stored_cards {
            let Some(mut item) = items_by_id.remove(&stored.item_id) else {
                continue;
            };
            if let Some(detail) = details.get(&stored.item_id) {
                apply_catalog_detail(&mut item, detail);
            }
            if item.poster_image_tag.is_none() {
                item.poster_image_tag = stored.poster_image_tag;
            }
            cards.push(GlobalCollectionCard {
                item,
                member_count: stored.member_count,
                poster_item_id: stored.poster_item_id,
            });
        }
        Ok(GlobalCollectionPage {
            cards,
            total,
            offset,
            limit,
        })
    }

    pub async fn find_global_collection_card(
        &self,
        principal: AccessPrincipal,
        collection_item_id: &str,
    ) -> Result<Option<GlobalCollectionCard>, CatalogError> {
        let Some(mut item) = self.find_item(principal, collection_item_id).await? else {
            return Ok(None);
        };
        if item.library_id != GLOBAL_COLLECTION_LIBRARY_ID || item.item_type != "BOX_SET" {
            return Ok(None);
        }
        let content_restrictions = self.content_restrictions_for_principal(principal).await?;
        let (member_ids, member_count) = self
            .database
            .list_global_collection_member_ids_page_with_restrictions(
                collection_item_id,
                &principal.user_id_string(),
                principal.is_admin,
                0,
                1,
                content_restrictions.as_ref(),
            )
            .await?;
        if member_count == 0 {
            return Ok(None);
        }
        let mut poster_item_id = None;
        if item.poster_image_tag.is_none() && !member_ids.is_empty() {
            let work_sources = self
                .global_collection_sources_for_works(
                    principal,
                    &member_ids,
                    content_restrictions.as_ref(),
                )
                .await?;
            if let Some(source) = work_sources.first().and_then(|work| {
                work.copies
                    .iter()
                    .find(|copy| copy.poster_image_tag.is_some())
            }) {
                item.poster_image_tag = source.poster_image_tag.clone();
                poster_item_id = Some(source.item_id.clone());
            }
        } else if item.poster_image_tag.is_some() {
            poster_item_id = Some(item.id.clone());
        }
        Ok(Some(GlobalCollectionCard {
            item,
            member_count,
            poster_item_id,
        }))
    }

    pub async fn list_global_collection_members(
        &self,
        principal: AccessPrincipal,
        collection_item_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<GlobalCollectionMembersPage, CatalogError> {
        let content_restrictions = self.content_restrictions_for_principal(principal).await?;
        let (member_ids, total) = self
            .database
            .list_global_collection_member_ids_page_with_restrictions(
                collection_item_id,
                &principal.user_id_string(),
                principal.is_admin,
                offset,
                limit,
                content_restrictions.as_ref(),
            )
            .await?;
        if total == 0 {
            return Err(CatalogError::AccessDenied);
        }
        let mut page = self
            .catalog_page_from_ids(member_ids.clone(), total, offset, limit)
            .await?;
        let mut sources_by_work = self
            .global_collection_sources_for_works(
                principal,
                &member_ids,
                content_restrictions.as_ref(),
            )
            .await?
            .into_iter()
            .map(|work| (work.work_item_id.clone(), work))
            .collect::<HashMap<_, _>>();
        let mut members = Vec::with_capacity(page.items.len());
        for item in page.items.drain(..) {
            let source_group = sources_by_work.remove(&item.id);
            members.push(GlobalCollectionMember {
                item,
                sources: source_group
                    .as_ref()
                    .map(|group| group.copies.clone())
                    .unwrap_or_default(),
                preferred_source_item_id: source_group
                    .as_ref()
                    .and_then(|group| group.preferred_source_item_id.clone()),
                preferred_media_source_id: source_group
                    .and_then(|group| group.preferred_media_source_id),
            });
        }
        Ok(GlobalCollectionMembersPage {
            members,
            total: page.total,
            offset,
            limit,
        })
    }

    pub async fn global_collection_playback_sources(
        &self,
        principal: AccessPrincipal,
        work_item_id: &str,
    ) -> Result<Option<GlobalCollectionPlaybackSources>, CatalogError> {
        let content_restrictions = self.content_restrictions_for_principal(principal).await?;
        let work_sources = self
            .global_collection_sources_for_works(
                principal,
                &[work_item_id.to_owned()],
                content_restrictions.as_ref(),
            )
            .await?;
        let Some(work_sources) = work_sources.into_iter().next() else {
            return Ok(None);
        };
        let mut copies = Vec::with_capacity(work_sources.copies.len());
        for source_copy in work_sources.copies {
            let Some(mut item) = self.find_item(principal, &source_copy.item_id).await? else {
                continue;
            };
            let available_source_ids = source_copy
                .versions
                .iter()
                .map(|version| version.id.as_str())
                .collect::<HashSet<_>>();
            item.media_sources
                .retain(|source| available_source_ids.contains(source.id.as_str()));
            if !item.media_sources.is_empty() {
                copies.push(item);
            }
        }
        let preferred_source_item_id = work_sources
            .preferred_source_item_id
            .filter(|preferred_id| copies.iter().any(|item| item.id == *preferred_id));
        let preferred_media_source_id =
            work_sources.preferred_media_source_id.filter(|source_id| {
                copies.iter().any(|item| {
                    item.media_sources
                        .iter()
                        .any(|source| source.id == *source_id)
                })
            });
        Ok(Some(GlobalCollectionPlaybackSources {
            copies,
            preferred_source_item_id,
            preferred_media_source_id,
        }))
    }

    pub async fn set_global_collection_source_preference(
        &self,
        principal: AccessPrincipal,
        collection_item_id: &str,
        work_item_id: &str,
        source_item_id: &str,
        media_source_id: Option<&str>,
    ) -> Result<(), CatalogError> {
        let content_restrictions = self.content_restrictions_for_principal(principal).await?;
        let saved = self
            .database
            .set_global_collection_source_preference_with_restrictions(
                crate::storage::GlobalCollectionSourcePreference {
                    user_id: &principal.user_id_string(),
                    is_admin: principal.is_admin,
                    collection_item_id,
                    work_item_id,
                    source_item_id,
                    media_source_id,
                },
                content_restrictions.as_ref(),
            )
            .await?;
        if saved {
            Ok(())
        } else {
            Err(CatalogError::AccessDenied)
        }
    }

    async fn global_collection_sources_for_works(
        &self,
        principal: AccessPrincipal,
        work_item_ids: &[String],
        content_restrictions: Option<&UserContentRestrictions>,
    ) -> Result<Vec<GlobalCollectionWorkSources>, CatalogError> {
        let stored_sources = self
            .database
            .list_global_collection_sources_for_works_with_restrictions(
                work_item_ids,
                &principal.user_id_string(),
                principal.is_admin,
                content_restrictions,
            )
            .await?;
        let mut groups = Vec::<GlobalCollectionWorkSources>::new();
        let mut group_indices = HashMap::<String, usize>::new();
        let mut copy_indices = HashMap::<(String, String), usize>::new();
        for source in stored_sources {
            let work_index = match group_indices.get(&source.work_item_id).copied() {
                Some(index) => index,
                None => {
                    let index = groups.len();
                    group_indices.insert(source.work_item_id.clone(), index);
                    groups.push(GlobalCollectionWorkSources {
                        work_item_id: source.work_item_id.clone(),
                        copies: Vec::new(),
                        preferred_source_item_id: None,
                        preferred_media_source_id: None,
                    });
                    index
                }
            };
            let group = &mut groups[work_index];
            if let Some(preferred_source_item_id) = source.preferred_source_item_id.clone() {
                group.preferred_source_item_id = Some(preferred_source_item_id);
                group.preferred_media_source_id = source.preferred_media_source_id.clone();
            }
            let copy_key = (source.work_item_id.clone(), source.source_item_id.clone());
            let copy_index = match copy_indices.get(&copy_key).copied() {
                Some(index) => index,
                None => {
                    let index = group.copies.len();
                    copy_indices.insert(copy_key, index);
                    group.copies.push(GlobalCollectionSourceCopy {
                        item_id: source.source_item_id.clone(),
                        library_id: source.library_id.clone(),
                        library_name: source.library_name.clone(),
                        title: source.title.clone(),
                        poster_image_tag: source.poster_image_tag.clone(),
                        versions: Vec::new(),
                    });
                    index
                }
            };
            if let Some(id) = source.media_source_id {
                let version = GlobalCollectionSourceVersion {
                    id,
                    edition_name: source.edition_name,
                    quality_label: source.quality_label,
                    container: source.container,
                };
                let versions = &mut group.copies[copy_index].versions;
                if !versions.iter().any(|existing| existing.id == version.id) {
                    versions.push(version);
                }
            }
        }
        Ok(groups)
    }

    pub async fn count_item_types(
        &self,
        principal: AccessPrincipal,
        user_id: &str,
        is_favorite: Option<bool>,
    ) -> Result<CatalogItemCounts, CatalogError> {
        let library_ids = self.access.accessible_library_ids(principal).await?;
        let content_restrictions = self.content_restrictions_for_principal(principal).await?;
        let counts = self
            .database
            .count_catalog_item_types(
                &library_ids,
                user_id,
                is_favorite,
                content_restrictions.as_ref(),
            )
            .await?;
        let global_collection_count = self
            .database
            .count_visible_global_collections_with_restrictions(
                &principal.user_id_string(),
                principal.is_admin,
                is_favorite,
                content_restrictions.as_ref(),
            )
            .await?;
        Ok(CatalogItemCounts {
            movie_count: counts.movie_count,
            series_count: counts.series_count,
            episode_count: counts.episode_count,
            box_set_count: counts.box_set_count + global_collection_count,
            item_count: counts.item_count + global_collection_count,
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

    pub(crate) async fn list_unplayed_episode_counts(
        &self,
        user_id: &str,
        item_ids: &[String],
    ) -> Result<HashMap<String, i64>, CatalogError> {
        Ok(self
            .database
            .list_unplayed_episode_counts(user_id, item_ids)
            .await?)
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
        let content_restrictions = self.content_restrictions_for_principal(principal).await?;
        self.populate_episode_counts(&mut items, content_restrictions.as_ref())
            .await?;
        Ok(CatalogPage::new(items, total, offset, limit))
    }

    pub async fn list_library_items_filtered(
        &self,
        principal: AccessPrincipal,
        library_id: &str,
        filter: &CatalogFilter,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        self.list_library_items_in_scope(principal, library_id, filter, None, offset, limit)
            .await
    }

    pub async fn list_library_items_in_scope(
        &self,
        principal: AccessPrincipal,
        library_id: &str,
        filter: &CatalogFilter,
        parent_scope: Option<CatalogParentScope>,
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
        let content_restrictions = if principal.is_admin {
            None
        } else {
            self.database
                .user_content_restrictions(&principal.user_id_string())
                .await?
        };
        let request = LibraryPageRequest {
            principal,
            library_id: library_id.to_owned(),
            filter: filter.clone(),
            parent_scope,
            offset,
            limit,
            content_restrictions,
        };
        return self.library_page_cache.get_or_load(self, request).await;
    }

    async fn compare_read_model_page(
        &self,
        query: &CatalogFilterQuery<'_>,
        rows: &[StoredCatalogRow],
        total: i64,
    ) {
        let sort = match query.sort_by {
            StorageCatalogSort::Name | StorageCatalogSort::SortName => {
                crate::storage::ReadModelSort::SortTitle
            }
            StorageCatalogSort::DateCreated => crate::storage::ReadModelSort::EffectiveAddedAt,
            StorageCatalogSort::Rating => crate::storage::ReadModelSort::Rating,
            StorageCatalogSort::ProductionYear => crate::storage::ReadModelSort::ProductionYear,
            StorageCatalogSort::PremiereDate => return,
        };
        // The read model has no per-user/person/provider joins yet; only compare the default page.
        if query.library_ids.len() != 1
            || query.person_id.is_some()
            || query.media_source_ids.is_some()
            || query.provider_id_equals.is_some()
            || !query.years.is_empty()
            || query.is_played.is_some()
            || query.is_favorite.is_some()
            || query.min_date_last_saved.is_some()
            || query.metadata_pending
            || query.parent_id_scope.is_some()
            || query.content_restrictions.is_some()
        {
            return;
        }
        let limit = if query.include_total {
            query.limit
        } else {
            query.limit.saturating_add(1)
        };
        // The legacy query joins sources/streams, so one item can appear on several rows.
        let mut seen = std::collections::HashSet::new();
        let legacy = rows
            .iter()
            .map(|row| row.item_id.as_str())
            .filter(|item_id| seen.insert(*item_id))
            .collect::<Vec<_>>();
        match self
            .database
            .shadow_catalog_library_page(&crate::storage::ReadModelPageQuery {
                library_id: &query.library_ids[0],
                item_types: query.item_types,
                excluded_item_types: query.excluded_item_types,
                sort,
                descending: query.descending,
                offset: query.offset,
                limit,
                include_total: query.include_total,
            })
            .await
        {
            Ok((ids, shadow_total)) => {
                let shadow = ids.iter().map(String::as_str).collect::<Vec<_>>();
                if legacy != shadow {
                    tracing::warn!(
                        library_id = %query.library_ids[0],
                        legacy_count = legacy.len(),
                        shadow_count = shadow.len(),
                        "catalog read-model shadow page differs"
                    );
                }
                if query.include_total && total != shadow_total {
                    tracing::warn!(
                        library_id = %query.library_ids[0],
                        legacy_total = total,
                        shadow_total,
                        "catalog read-model shadow total differs"
                    );
                }
            }
            Err(error) => {
                tracing::warn!(%error, "catalog read-model shadow query failed");
            }
        }
    }

    async fn load_library_page(
        &self,
        request: &LibraryPageRequest,
    ) -> Result<CatalogPage, CatalogError> {
        let library_ids = vec![request.library_id.clone()];
        let user_id = request.principal.user_id_string();
        let filter = &request.filter;
        let query = CatalogFilterQuery {
            library_ids: &library_ids,
            user_id: &user_id,
            item_types: &filter.item_types,
            excluded_item_types: &filter.excluded_item_types,
            item_ids: filter.item_ids.as_deref(),
            person_id: filter.person_id.as_deref(),
            media_source_ids: filter.media_source_ids.as_deref(),
            provider_id_equals: filter.provider_id_equals.as_deref(),
            years: &filter.years,
            is_played: filter.is_played,
            is_favorite: filter.is_favorite,
            min_date_last_saved: filter.min_date_last_saved,
            metadata_pending: filter.metadata_pending,
            parent_id_scope: request.parent_scope.as_ref().map(|scope| match scope {
                CatalogParentScope::Root => None,
                CatalogParentScope::Item(item_id) => Some(item_id.as_str()),
            }),
            content_restrictions: request.content_restrictions.as_ref(),
            sort_by: match filter.sort_by {
                CatalogSort::Name => StorageCatalogSort::Name,
                CatalogSort::SortName => StorageCatalogSort::SortName,
                CatalogSort::DateCreated => StorageCatalogSort::DateCreated,
                CatalogSort::PremiereDate => StorageCatalogSort::PremiereDate,
                CatalogSort::ProductionYear => StorageCatalogSort::ProductionYear,
                CatalogSort::Rating => StorageCatalogSort::Rating,
            },
            descending: filter.descending,
            include_total: filter.include_total,
            offset: request.offset,
            limit: request.limit,
        };
        let (rows, total) = self.database.list_filtered_catalog_rows(&query).await?;
        if self.read_model_mode == ReadModelMode::Shadow {
            self.compare_read_model_page(&query, &rows, total).await;
        }
        let mut items = assemble_items(rows);
        self.populate_item_details_and_episode_counts(
            &mut items,
            request.content_restrictions.as_ref(),
        )
        .await?;
        if filter.include_total {
            Ok(CatalogPage::new(
                items,
                total,
                request.offset,
                request.limit,
            ))
        } else {
            let has_next = items.len() as i64 > request.limit;
            if has_next {
                items.truncate(request.limit as usize);
            }
            Ok(CatalogPage::without_total(
                items,
                request.offset,
                request.limit,
                has_next,
            ))
        }
    }

    pub async fn list_all_items_filtered(
        &self,
        principal: AccessPrincipal,
        filter: &CatalogFilter,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        let library_ids = self.access.accessible_library_ids(principal).await?;
        let user_id = principal.user_id_string();
        let content_restrictions = if principal.is_admin {
            None
        } else {
            self.database.user_content_restrictions(&user_id).await?
        };
        let query = CatalogFilterQuery {
            library_ids: &library_ids,
            user_id: &user_id,
            item_types: &filter.item_types,
            excluded_item_types: &filter.excluded_item_types,
            item_ids: filter.item_ids.as_deref(),
            person_id: filter.person_id.as_deref(),
            media_source_ids: filter.media_source_ids.as_deref(),
            provider_id_equals: filter.provider_id_equals.as_deref(),
            years: &filter.years,
            is_played: filter.is_played,
            is_favorite: filter.is_favorite,
            min_date_last_saved: filter.min_date_last_saved,
            metadata_pending: filter.metadata_pending,
            parent_id_scope: None,
            content_restrictions: content_restrictions.as_ref(),
            sort_by: match filter.sort_by {
                CatalogSort::Name => StorageCatalogSort::Name,
                CatalogSort::SortName => StorageCatalogSort::SortName,
                CatalogSort::DateCreated => StorageCatalogSort::DateCreated,
                CatalogSort::PremiereDate => StorageCatalogSort::PremiereDate,
                CatalogSort::ProductionYear => StorageCatalogSort::ProductionYear,
                CatalogSort::Rating => StorageCatalogSort::Rating,
            },
            descending: filter.descending,
            include_total: filter.include_total,
            offset,
            limit,
        };
        let (rows, total) = self.database.list_filtered_catalog_rows(&query).await?;
        let mut items = assemble_items(rows);
        self.populate_item_details_and_episode_counts(&mut items, content_restrictions.as_ref())
            .await?;
        if filter.include_total {
            Ok(CatalogPage::new(items, total, offset, limit))
        } else {
            let has_next = items.len() as i64 > limit;
            if has_next {
                items.truncate(limit as usize);
            }
            Ok(CatalogPage::without_total(items, offset, limit, has_next))
        }
    }

    pub async fn list_children(
        &self,
        principal: AccessPrincipal,
        parent_id: &str,
        item_types: &str,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        self.list_children_ordered(principal, parent_id, item_types, None, offset, limit)
            .await
    }

    pub async fn list_children_with_sort(
        &self,
        principal: AccessPrincipal,
        parent_id: &str,
        item_types: &str,
        sort: (CatalogSort, bool),
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        self.list_children_ordered(principal, parent_id, item_types, Some(sort), offset, limit)
            .await
    }

    async fn list_children_ordered(
        &self,
        principal: AccessPrincipal,
        parent_id: &str,
        item_types: &str,
        sort: Option<(CatalogSort, bool)>,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        if !self.access.can_view_item(principal, parent_id).await? {
            return Err(CatalogError::AccessDenied);
        }
        let content_restrictions = self.content_restrictions_for_principal(principal).await?;
        let total = self
            .database
            .count_catalog_children(parent_id, item_types, content_restrictions.as_ref())
            .await?;
        let storage_sort = sort.map(|(sort_by, descending)| {
            let sort_by = match sort_by {
                CatalogSort::Name => StorageCatalogSort::Name,
                CatalogSort::SortName => StorageCatalogSort::SortName,
                CatalogSort::DateCreated => StorageCatalogSort::DateCreated,
                CatalogSort::PremiereDate => StorageCatalogSort::PremiereDate,
                CatalogSort::ProductionYear => StorageCatalogSort::ProductionYear,
                CatalogSort::Rating => StorageCatalogSort::Rating,
            };
            (sort_by, descending)
        });
        let rows = self
            .database
            .list_catalog_children(
                parent_id,
                item_types,
                storage_sort,
                content_restrictions.as_ref(),
                offset,
                limit,
            )
            .await?;
        let mut items = assemble_items(rows);
        if matches!(item_types, "SEASON" | "EPISODE") {
            self.populate_item_details_and_episode_counts(
                &mut items,
                content_restrictions.as_ref(),
            )
            .await?;
        } else {
            self.populate_episode_counts(&mut items, content_restrictions.as_ref())
                .await?;
        }
        Ok(CatalogPage::new(items, total, offset, limit))
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
        let content_restrictions = self.content_restrictions_for_principal(principal).await?;
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
                content_restrictions.as_ref(),
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
        Ok(CatalogPage::new(items, total, offset, limit))
    }

    pub async fn list_collection_items(
        &self,
        principal: AccessPrincipal,
        collection_item_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        let collection_library_id = self
            .database
            .find_item_library_id(collection_item_id)
            .await?;
        if collection_library_id.as_deref() == Some(GLOBAL_COLLECTION_LIBRARY_ID) {
            let content_restrictions = self.content_restrictions_for_principal(principal).await?;
            let (member_ids, total) = self
                .database
                .list_global_collection_member_ids_page_with_restrictions(
                    collection_item_id,
                    &principal.user_id_string(),
                    principal.is_admin,
                    offset,
                    limit,
                    content_restrictions.as_ref(),
                )
                .await?;
            if total == 0 {
                return Err(CatalogError::AccessDenied);
            }
            return self
                .catalog_page_from_ids(member_ids, total, offset, limit)
                .await;
        }
        let Some(collection_library_id) = collection_library_id else {
            return Err(CatalogError::AccessDenied);
        };
        if self
            .database
            .is_collection_container(collection_item_id)
            .await?
        {
            if !self
                .access
                .can_view_library(principal, &collection_library_id)
                .await?
            {
                return Err(CatalogError::AccessDenied);
            }
        } else if !self
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
        let content_restrictions = self.content_restrictions_for_principal(principal).await?;
        let (member_ids, total) = self
            .database
            .list_collection_member_ids_page(
                collection_item_id,
                library_ids.as_deref(),
                content_restrictions.as_ref(),
                offset,
                limit,
            )
            .await?;
        self.catalog_page_from_ids(member_ids, total, offset, limit)
            .await
    }

    async fn catalog_page_from_ids(
        &self,
        member_ids: Vec<String>,
        total: i64,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
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
        Ok(CatalogPage::new(items, total, offset, limit))
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

    pub(crate) async fn set_resume_hidden(
        &self,
        principal: AccessPrincipal,
        item_id: &str,
        hidden: bool,
    ) -> Result<Option<crate::storage::StoredUserItemState>, CatalogError> {
        if !self.access.can_view_item(principal, item_id).await? {
            return Err(CatalogError::AccessDenied);
        }
        let user_id = principal.user_id_string();
        self.database
            .set_user_item_resume_hidden(&user_id, item_id, hidden)
            .await?;
        Ok(self
            .database
            .find_user_item_state(&user_id, item_id)
            .await?)
    }

    pub async fn list_continue_watching(
        &self,
        principal: AccessPrincipal,
        user_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        let library_ids = self.access.accessible_library_ids(principal).await?;
        let content_restrictions = if principal.is_admin {
            None
        } else {
            self.database
                .user_content_restrictions(&principal.user_id_string())
                .await?
        };
        self.list_continue_watching_for_library_ids(
            &library_ids,
            user_id,
            content_restrictions.as_ref(),
            offset,
            limit,
        )
        .await
    }

    pub async fn list_home_continue_watching(
        &self,
        principal: AccessPrincipal,
        user_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        let library_ids = self.access.accessible_library_ids(principal).await?;
        let content_restrictions = if principal.is_admin {
            None
        } else {
            self.database
                .user_content_restrictions(&principal.user_id_string())
                .await?
        };
        self.list_continue_watching_for_library_ids_with_grouping(
            &library_ids,
            user_id,
            offset,
            limit,
            true,
            content_restrictions.as_ref(),
        )
        .await
    }

    pub(crate) async fn list_continue_watching_for_library_ids(
        &self,
        library_ids: &[String],
        user_id: &str,
        content_restrictions: Option<&UserContentRestrictions>,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        self.list_continue_watching_for_library_ids_with_grouping(
            library_ids,
            user_id,
            offset,
            limit,
            true,
            content_restrictions,
        )
        .await
    }

    pub(crate) async fn list_home_continue_watching_for_library_ids(
        &self,
        library_ids: &[String],
        user_id: &str,
        content_restrictions: Option<&UserContentRestrictions>,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        self.list_continue_watching_for_library_ids_with_grouping(
            library_ids,
            user_id,
            offset,
            limit,
            true,
            content_restrictions,
        )
        .await
    }

    pub(crate) async fn content_restrictions_for_principal(
        &self,
        principal: AccessPrincipal,
    ) -> Result<Option<UserContentRestrictions>, CatalogError> {
        if principal.is_admin {
            return Ok(None);
        }
        Ok(self
            .database
            .user_content_restrictions(&principal.user_id_string())
            .await?)
    }

    async fn list_continue_watching_for_library_ids_with_grouping(
        &self,
        library_ids: &[String],
        user_id: &str,
        offset: i64,
        limit: i64,
        include_next_unplayed_episode: bool,
        content_restrictions: Option<&UserContentRestrictions>,
    ) -> Result<CatalogPage, CatalogError> {
        let (played_percent, minimum_ticks) = self.database.home_resume_settings(user_id).await?;
        let query = ResumeItemsQuery {
            user_id,
            library_ids,
            item_types: &["MOVIE", "EPISODE", "VIDEO"],
            content_restrictions,
            played_percent,
            minimum_ticks,
            offset,
            limit,
        };
        let (rows, total) = if include_next_unplayed_episode {
            self.database
                .list_home_resume_items_with_total(&query, true)
                .await?
        } else {
            self.database
                .list_resume_items_with_total(&query, true)
                .await?
        };
        Ok(CatalogPage::new(assemble_items(rows), total, offset, limit))
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
        Ok(CatalogPage::new(assemble_items(rows), total, offset, limit))
    }

    pub async fn list_recently_added(
        &self,
        principal: AccessPrincipal,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        let library_ids = self.access.accessible_library_ids(principal).await?;
        let content_restrictions = self.content_restrictions_for_principal(principal).await?;
        self.list_recently_added_for_library_ids(
            &library_ids,
            content_restrictions.as_ref(),
            offset,
            limit,
        )
        .await
    }

    pub(crate) async fn list_recently_added_for_library_ids(
        &self,
        library_ids: &[String],
        content_restrictions: Option<&UserContentRestrictions>,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        self.list_recently_added_for_library_ids_with_filter(
            library_ids,
            content_restrictions,
            offset,
            limit,
            false,
        )
        .await
    }

    pub(crate) async fn list_recently_added_movies_and_series_for_library_ids(
        &self,
        library_ids: &[String],
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        self.list_recently_added_for_library_ids_with_filter(library_ids, None, offset, limit, true)
            .await
    }

    async fn list_recently_added_for_library_ids_with_filter(
        &self,
        library_ids: &[String],
        content_restrictions: Option<&UserContentRestrictions>,
        offset: i64,
        limit: i64,
        media_only: bool,
    ) -> Result<CatalogPage, CatalogError> {
        let (item_ids, total) = self
            .database
            .list_recent_catalog_item_ids(
                library_ids,
                offset,
                limit,
                media_only,
                content_restrictions,
            )
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
        Ok(CatalogPage::new(items, total, offset, limit))
    }

    pub async fn list_recently_added_by_library(
        &self,
        principal: AccessPrincipal,
        limit: i64,
    ) -> Result<Vec<(String, Vec<CatalogItem>)>, CatalogError> {
        let library_ids = self.access.accessible_library_ids(principal).await?;
        let content_restrictions = self.content_restrictions_for_principal(principal).await?;
        self.list_recently_added_by_library_ids(&library_ids, content_restrictions.as_ref(), limit)
            .await
    }

    pub(crate) async fn list_recently_added_by_library_ids(
        &self,
        library_ids: &[String],
        content_restrictions: Option<&UserContentRestrictions>,
        limit: i64,
    ) -> Result<Vec<(String, Vec<CatalogItem>)>, CatalogError> {
        let rows = self
            .database
            .list_recent_catalog_rows_by_library_with_restrictions(
                library_ids,
                limit,
                content_restrictions,
            )
            .await?;
        let mut items = assemble_items(rows);
        self.populate_episode_counts(&mut items, content_restrictions)
            .await?;
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
        let content_restrictions = self.content_restrictions_for_principal(principal).await?;
        self.list_recommended_for_library_ids_with_policy(
            &library_ids,
            user_id,
            content_restrictions.as_ref(),
            limit,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn list_recommended_for_library_ids(
        &self,
        library_ids: &[String],
        user_id: &str,
        limit: i64,
    ) -> Result<Vec<CatalogItem>, CatalogError> {
        self.list_recommended_for_library_ids_with_policy(library_ids, user_id, None, limit)
            .await
    }

    pub(crate) async fn list_recommended_for_library_ids_with_policy(
        &self,
        library_ids: &[String],
        user_id: &str,
        content_restrictions: Option<&UserContentRestrictions>,
        limit: i64,
    ) -> Result<Vec<CatalogItem>, CatalogError> {
        if library_ids.is_empty() {
            return Ok(Vec::new());
        }
        let batch_key = current_recommendation_batch_key();
        let mut library_scope_key = recommendation_library_scope_key(library_ids);
        if let Some(restrictions) = content_restrictions {
            library_scope_key.push_str(&format!(
                "\u{001e}content-policy-version={}",
                restrictions.version
            ));
        }
        if let Some(item_ids) = self
            .database
            .find_recommendation_daily_batch(user_id, &library_scope_key, batch_key)
            .await?
        {
            let rows = self.database.list_catalog_rows_by_ids(&item_ids).await?;
            let mut items = reorder_catalog_items(assemble_items(rows), &item_ids);
            items.retain(|item| {
                library_ids
                    .iter()
                    .any(|library_id| library_id == &item.library_id)
            });
            self.populate_episode_counts(&mut items, content_restrictions)
                .await?;
            items.truncate(usize::try_from(limit).unwrap_or(0));
            return Ok(items);
        }

        let item_ids = self
            .compute_recommendation_daily_batch(
                user_id,
                library_ids,
                &library_scope_key,
                batch_key,
                content_restrictions.cloned(),
            )
            .await?;
        let rows = self.database.list_catalog_rows_by_ids(&item_ids).await?;
        let mut items = reorder_catalog_items(assemble_items(rows), &item_ids);
        items.retain(|item| {
            library_ids
                .iter()
                .any(|library_id| library_id == &item.library_id)
        });
        self.populate_episode_counts(&mut items, content_restrictions)
            .await?;
        Ok(items
            .into_iter()
            .take(usize::try_from(limit).unwrap_or(0))
            .collect())
    }

    /// Scores the full catalog once and persists the daily batch.
    ///
    /// The scoring query is expensive on large libraries, so the work runs in a detached task:
    /// home-cache refreshes are cancelled whenever the catalog is invalidated (every scan
    /// batch), and dropping the caller mid-query would otherwise discard the result before it is
    /// saved, leaving the orphaned PostgreSQL backend running and the next refresh starting the
    /// same query again. Tasks are serialized and re-check the stored batch, so a burst of
    /// refreshes costs one scoring pass instead of one per request.
    async fn compute_recommendation_daily_batch(
        &self,
        user_id: &str,
        library_ids: &[String],
        library_scope_key: &str,
        batch_key: i64,
        content_restrictions: Option<UserContentRestrictions>,
    ) -> Result<Vec<String>, CatalogError> {
        let service = self.clone();
        let user_id = user_id.to_owned();
        let library_ids = library_ids.to_vec();
        let library_scope_key = library_scope_key.to_owned();
        tokio::spawn(async move {
            let _compute_guard = service.recommendation_compute_lock.lock().await;
            if let Some(item_ids) = service
                .database
                .find_recommendation_daily_batch(&user_id, &library_scope_key, batch_key)
                .await?
            {
                return Ok(item_ids);
            }

            // Global recommendation statistics are refreshed by ScheduledTaskService. This
            // request path must remain read-first so a new user's home page cannot wait on the
            // full refresh transaction while a migration or scan is writing to the database.
            let rows = service
                .database
                .list_recommended_catalog_rows_with_restrictions(
                    &user_id,
                    &library_ids,
                    0,
                    RECOMMENDATION_CANDIDATE_POOL,
                    content_restrictions.as_ref(),
                )
                .await?;
            let items = daily_recommendation_items(
                assemble_items(rows),
                &user_id,
                batch_key,
                RECOMMENDATION_CANDIDATE_POOL as usize,
            );
            let item_ids = items.iter().map(|item| item.id.clone()).collect::<Vec<_>>();
            let inserted = service
                .database
                .save_recommendation_daily_batch(&user_id, &library_scope_key, batch_key, &item_ids)
                .await?;
            if inserted {
                return Ok(item_ids);
            }
            Ok(service
                .database
                .find_recommendation_daily_batch(&user_id, &library_scope_key, batch_key)
                .await?
                .unwrap_or(item_ids))
        })
        .await
        .map_err(|error| {
            CatalogError::Storage(StorageError::Serialization(format!(
                "recommendation batch task failed: {error}"
            )))
        })?
    }

    async fn populate_episode_counts(
        &self,
        items: &mut [CatalogItem],
        content_restrictions: Option<&UserContentRestrictions>,
    ) -> Result<(), CatalogError> {
        let item_ids = items
            .iter()
            .filter(|item| item.item_type == "SERIES" || item.item_type == "SEASON")
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();
        let episode_counts = self
            .database
            .list_episode_counts(&item_ids, content_restrictions)
            .await?;
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
        content_restrictions: Option<&UserContentRestrictions>,
    ) -> Result<(), CatalogError> {
        let item_ids = items.iter().map(|item| item.id.clone()).collect::<Vec<_>>();
        let episode_item_ids = items
            .iter()
            .filter(|item| item.item_type == "SERIES" || item.item_type == "SEASON")
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();
        let (details, episode_counts) = tokio::try_join!(
            self.database.list_catalog_details_by_ids(&item_ids),
            self.database
                .list_episode_counts(&episode_item_ids, content_restrictions),
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
        let Some(library_id) = self.database.find_item_library_id(item_id).await? else {
            return Ok(None);
        };
        let rows = self.database.find_catalog_rows(item_id).await?;
        let Some(mut item) = assemble_items(rows).into_iter().next() else {
            return Ok(None);
        };
        if library_id == GLOBAL_COLLECTION_LIBRARY_ID {
            let content_restrictions = self.content_restrictions_for_principal(principal).await?;
            let visible = match item.item_type.as_str() {
                "BOX_SET" => {
                    let (_, total) = self
                        .database
                        .list_global_collection_member_ids_page_with_restrictions(
                            item_id,
                            &principal.user_id_string(),
                            principal.is_admin,
                            0,
                            1,
                            content_restrictions.as_ref(),
                        )
                        .await?;
                    total > 0
                }
                "MOVIE" => {
                    let sources = self
                        .global_collection_sources_for_works(
                            principal,
                            std::slice::from_ref(&item.id),
                            content_restrictions.as_ref(),
                        )
                        .await?;
                    sources.iter().any(|work| !work.copies.is_empty())
                }
                _ => false,
            };
            if !visible {
                return Ok(None);
            }
        } else if !self.access.can_view_item(principal, item_id).await? {
            if item.item_type.as_str() != "BOX_SET"
                || !self.database.is_collection_container(item_id).await?
                || !self.access.can_view_library(principal, &library_id).await?
            {
                return Ok(None);
            }
            let library_ids = if principal.is_admin {
                None
            } else {
                Some(self.access.accessible_library_ids(principal).await?)
            };
            let content_restrictions = self.content_restrictions_for_principal(principal).await?;
            let (_, visible_member_count) = self
                .database
                .list_collection_member_ids_page(
                    item_id,
                    library_ids.as_deref(),
                    content_restrictions.as_ref(),
                    0,
                    1,
                )
                .await?;
            if visible_member_count == 0 {
                return Ok(None);
            }
        }
        if let Some(detail) = self.database.find_catalog_detail(item_id).await? {
            apply_catalog_detail(&mut item, &detail);
        }
        if matches!(item.item_type.as_str(), "SERIES" | "SEASON") {
            let content_restrictions = self.content_restrictions_for_principal(principal).await?;
            let episode_counts = self
                .database
                .list_episode_counts(
                    std::slice::from_ref(&item.id),
                    content_restrictions.as_ref(),
                )
                .await?;
            item.episode_count = episode_counts.get(&item.id).copied();
        }
        self.populate_chapters(std::slice::from_mut(&mut item))
            .await?;
        Ok(Some(item))
    }

    pub(crate) async fn strm_item_filesystem_paths(
        &self,
        items: &[CatalogItem],
    ) -> Result<HashMap<String, String>, CatalogError> {
        let source_to_item = items
            .iter()
            .filter_map(|item| {
                let source = item
                    .media_sources
                    .iter()
                    .find(|source| source.is_default)
                    .or_else(|| item.media_sources.first())?;
                (source.source_kind == "STRM_URL").then(|| (source.id.clone(), item.id.clone()))
            })
            .collect::<HashMap<_, _>>();
        if source_to_item.is_empty() {
            return Ok(HashMap::new());
        }

        let source_ids = source_to_item.keys().cloned().collect::<Vec<_>>();
        let stored_paths = self
            .database
            .list_strm_source_paths_by_ids(&source_ids)
            .await?;
        let mut item_paths = HashMap::new();
        for stored_path in stored_paths {
            let Some(item_id) = source_to_item.get(&stored_path.source_id) else {
                continue;
            };
            let root = std::path::Path::new(&stored_path.root_path);
            let relative = std::path::Path::new(&stored_path.relative_path);
            if !root.is_absolute()
                || relative.as_os_str().is_empty()
                || !relative
                    .components()
                    .all(|component| matches!(component, std::path::Component::Normal(_)))
            {
                continue;
            }
            let full_path = root.join(relative);
            if !full_path.starts_with(root) {
                continue;
            }
            let Some(full_path) = full_path.to_str() else {
                continue;
            };
            item_paths.insert(item_id.clone(), full_path.to_owned());
        }
        Ok(item_paths)
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
        self.populate_image_tags(&mut items).await?;
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
        let item_types = DEFAULT_SEARCH_ITEM_TYPES
            .iter()
            .map(|item_type| (*item_type).to_owned())
            .collect();
        self.search_items_with_types(principal, query, like_query, item_types, offset, limit)
            .await
    }

    pub async fn search_items_with_types(
        &self,
        principal: AccessPrincipal,
        query: &str,
        like_query: &str,
        item_types: Vec<String>,
        offset: i64,
        limit: i64,
    ) -> Result<CatalogPage, CatalogError> {
        let library_ids = if principal.is_admin {
            None
        } else {
            Some(self.access.accessible_library_ids(principal).await?)
        };
        let content_restrictions = if principal.is_admin {
            None
        } else {
            self.database
                .user_content_restrictions(&principal.user_id_string())
                .await?
        };
        let key = SearchFlightKey {
            user_id: principal.user_id_string(),
            is_admin: principal.is_admin,
            library_ids: library_ids.clone(),
            query: query.to_owned(),
            like_query: like_query.to_owned(),
            item_types: item_types.clone(),
            content_restriction_version: content_restrictions
                .as_ref()
                .map(|restrictions| restrictions.version),
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
                let task_item_types = item_types.clone();
                let task_content_restrictions = content_restrictions.clone();
                let task = tokio::spawn(async move {
                    let result = service
                        .search_items_uncached(CatalogSearchQuery {
                            query: &task_query,
                            like_query: &task_like_query,
                            item_types: &task_item_types,
                            library_ids: task_library_ids.as_deref(),
                            offset,
                            limit,
                            content_restrictions: task_content_restrictions.as_ref(),
                        })
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
                self.search_items_uncached(CatalogSearchQuery {
                    query,
                    like_query,
                    item_types: &item_types,
                    library_ids: library_ids.as_deref(),
                    offset,
                    limit,
                    content_restrictions: content_restrictions.as_ref(),
                })
                .await
            }
            SearchFlightHandle::Bypass => {
                self.search_items_uncached(CatalogSearchQuery {
                    query,
                    like_query,
                    item_types: &item_types,
                    library_ids: library_ids.as_deref(),
                    offset,
                    limit,
                    content_restrictions: content_restrictions.as_ref(),
                })
                .await
            }
        }
    }

    async fn search_items_uncached(
        &self,
        search: CatalogSearchQuery<'_>,
    ) -> Result<CatalogPage, CatalogError> {
        let (ids, total) = self.database.search_catalog_item_ids(&search).await?;
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
        Ok(CatalogPage::new(items, total, search.offset, search.limit))
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
    // The SQL side declares `ESCAPE '!'`, which is portable across SQLite and PostgreSQL
    // (a backslash is a plain character in SQLite LIKE).
    let escaped = value
        .replace('!', "!!")
        .replace('%', "!%")
        .replace('_', "!_");
    Some(format!("%{escaped}%"))
}

#[derive(Clone, Debug, PartialEq)]
pub struct CatalogPage {
    pub items: Vec<CatalogItem>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
    pub has_next: bool,
    pub total_included: bool,
}

impl CatalogPage {
    pub fn new(items: Vec<CatalogItem>, total: i64, offset: i64, limit: i64) -> Self {
        let has_next = offset.saturating_add(items.len() as i64) < total;
        Self {
            items,
            total,
            offset,
            limit,
            has_next,
            total_included: true,
        }
    }

    pub fn without_total(items: Vec<CatalogItem>, offset: i64, limit: i64, has_next: bool) -> Self {
        Self {
            items,
            total: 0,
            offset,
            limit,
            has_next,
            total_included: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CatalogItemProjection {
    #[default]
    Full,
    Card,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GlobalCollectionCard {
    pub item: CatalogItem,
    pub member_count: i64,
    pub poster_item_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GlobalCollectionPage {
    pub cards: Vec<GlobalCollectionCard>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GlobalCollectionMember {
    pub item: CatalogItem,
    pub sources: Vec<GlobalCollectionSourceCopy>,
    pub preferred_source_item_id: Option<String>,
    pub preferred_media_source_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GlobalCollectionMembersPage {
    pub members: Vec<GlobalCollectionMember>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GlobalCollectionPlaybackSources {
    pub copies: Vec<CatalogItem>,
    pub preferred_source_item_id: Option<String>,
    pub preferred_media_source_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GlobalCollectionSourceCopy {
    pub item_id: String,
    pub library_id: String,
    pub library_name: String,
    pub title: String,
    pub poster_image_tag: Option<String>,
    pub versions: Vec<GlobalCollectionSourceVersion>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GlobalCollectionSourceVersion {
    pub id: String,
    pub edition_name: Option<String>,
    pub quality_label: Option<String>,
    pub container: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
struct GlobalCollectionWorkSources {
    work_item_id: String,
    copies: Vec<GlobalCollectionSourceCopy>,
    preferred_source_item_id: Option<String>,
    preferred_media_source_id: Option<String>,
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
    pub added_at: i64,
    pub updated_at: i64,
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
    pub file_name: Option<String>,
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

impl CatalogSource {
    /// Version label without the part marker; parts of one file set share it.
    pub fn version_key(&self) -> String {
        self.split_part().0
    }

    /// Part number (`cd1` -> 1) when this source is one part of a multi-part version.
    pub fn part_index(&self) -> Option<u32> {
        self.split_part().1
    }

    fn split_part(&self) -> (String, Option<u32>) {
        use crate::application::media_matching::split_part_marker;
        let (label, part) = split_part_marker(self.edition_name.as_deref().unwrap_or_default());
        let quality = self
            .quality_label
            .as_deref()
            .unwrap_or_default()
            .to_lowercase();
        let label = label.to_lowercase();
        if part.is_some() {
            return (format!("{label}|{quality}"), part);
        }
        // Plain `Movie.1080p.cd1.mkv` files carry the marker only in the file name. The rest
        // of the file name tells their versions apart (`Movie.4K.cd1.mkv` is another one).
        let stem = self
            .file_name
            .as_deref()
            .map(|name| name.rsplit_once('.').map_or(name, |(stem, _)| stem))
            .unwrap_or_default();
        match split_part_marker(stem) {
            (stem_key, Some(part)) => (
                format!("{label}|{quality}|{}", stem_key.to_lowercase()),
                Some(part),
            ),
            (_, None) => (format!("{label}|{quality}"), None),
        }
    }
}

/// Number of parts that make up the version `source` belongs to (1 for a single file).
pub fn catalog_source_part_count(sources: &[CatalogSource], source: &CatalogSource) -> usize {
    if source.part_index().is_none() {
        return 1;
    }
    let key = source.version_key();
    sources
        .iter()
        .filter(|candidate| candidate.part_index().is_some() && candidate.version_key() == key)
        .count()
        .max(1)
}

/// Whether `source` belongs to the version of the default source: the default itself or,
/// for a multi-part default, one of its other parts.
pub fn belongs_to_default_version(sources: &[CatalogSource], source: &CatalogSource) -> bool {
    if source.is_default {
        return true;
    }
    let Some(default) = sources.iter().find(|candidate| candidate.is_default) else {
        return false;
    };
    default.part_index().is_some()
        && source.part_index().is_some()
        && default.version_key() == source.version_key()
}

/// Keeps the parts of one version together, in part order, and makes sure only the
/// first part of a version can be the default source.
fn arrange_source_parts(sources: &mut Vec<CatalogSource>) {
    if sources.len() < 2 {
        return;
    }
    let mut groups: Vec<(String, Vec<CatalogSource>)> = Vec::new();
    for source in sources.drain(..) {
        let key = source.version_key();
        match groups.iter_mut().find(|(group_key, members)| {
            *group_key == key
                && members.iter().any(|member| member.part_index().is_some())
                && source.part_index().is_some()
        }) {
            Some((_, members)) => members.push(source),
            None => groups.push((key, vec![source])),
        }
    }
    for (_, members) in &mut groups {
        members.sort_by_key(|member| member.part_index().unwrap_or(0));
    }
    let default_group = groups
        .iter()
        .position(|(_, members)| members.iter().any(|member| member.is_default));
    for (_, members) in &mut groups {
        for member in members.iter_mut() {
            member.is_default = false;
        }
    }
    let default_group = default_group.unwrap_or(0);
    if let Some(first) = groups
        .get_mut(default_group)
        .and_then(|(_, members)| members.first_mut())
    {
        first.is_default = true;
    }
    sources.extend(groups.into_iter().flat_map(|(_, members)| members));
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
                    series_name: row.series_name.clone(),
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
                    added_at: row.added_at,
                    updated_at: row.updated_at,
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
                    file_name: catalog_source_file_name(row.source_relative_path.as_deref()),
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
    for item in &mut items {
        arrange_source_parts(&mut item.media_sources);
    }
    items
}

fn catalog_source_file_name(relative_path: Option<&str>) -> Option<String> {
    Path::new(relative_path?)
        .file_name()?
        .to_str()
        .map(str::to_owned)
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
        CatalogItem, CatalogSource, MAX_LIBRARY_PAGE_REFRESH_ENTRIES, SearchFlightHandle,
        SearchFlightKey, SearchFlightRegistry, arrange_source_parts, belongs_to_default_version,
        catalog_source_file_name, catalog_source_part_count, reorder_catalog_items,
        take_recent_entries,
    };

    fn catalog_source(id: &str, edition: Option<&str>, is_default: bool) -> CatalogSource {
        CatalogSource {
            id: id.to_owned(),
            source_kind: "LOCAL_FILE".to_owned(),
            file_name: Some(format!("{id}.mkv")),
            container: None,
            size: None,
            external_url: None,
            edition_name: edition.map(str::to_owned),
            quality_label: None,
            bitrate: None,
            duration_ticks: None,
            is_default,
            probe_status: "READY".to_owned(),
            streams: Vec::new(),
            chapters: Vec::new(),
        }
    }

    fn file_source(
        id: &str,
        file_name: &str,
        quality: Option<&str>,
        is_default: bool,
    ) -> CatalogSource {
        CatalogSource {
            file_name: Some(file_name.to_owned()),
            quality_label: quality.map(str::to_owned),
            ..catalog_source(id, None, is_default)
        }
    }

    #[test]
    fn search_like_patterns_escape_wildcards_with_the_sql_escape_character() {
        assert_eq!(
            super::normalize_search_like_query("100%"),
            Some("%100!%%".to_owned())
        );
        assert_eq!(
            super::normalize_search_like_query("a_b"),
            Some("%a!_b%".to_owned())
        );
        assert_eq!(
            super::normalize_search_like_query("a!b"),
            Some("%a!!b%".to_owned())
        );
        assert_eq!(super::normalize_search_like_query("   "), None);
    }

    #[test]
    fn two_multi_part_versions_named_only_by_file_name_stay_apart() {
        // The filename parser leaves edition_name empty here; the resolution is the only
        // difference and must keep the two versions apart.
        let mut sources = vec![
            file_source("hd-cd1", "Movie.1080p.cd1.mkv", Some("1080p"), false),
            file_source("uhd-cd1", "Movie.4K.cd1.mkv", Some("4K"), false),
            file_source("hd-cd2", "Movie.1080p.cd2.mkv", Some("1080p"), false),
            file_source("uhd-cd2", "Movie.4K.cd2.mkv", Some("4K"), true),
        ];
        arrange_source_parts(&mut sources);
        let ids = sources
            .iter()
            .map(|source| source.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["hd-cd1", "hd-cd2", "uhd-cd1", "uhd-cd2"]);
        let defaults = sources
            .iter()
            .filter(|source| source.is_default)
            .map(|source| source.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(defaults, ["uhd-cd1"], "the default stays in the 4K version");
        assert_eq!(catalog_source_part_count(&sources, &sources[0]), 2);
        assert_eq!(catalog_source_part_count(&sources, &sources[2]), 2);

        // Without a quality label the rest of the file name still tells them apart.
        let mut sources = vec![
            file_source("a-cd1", "Movie Theatrical cd1.mkv", None, true),
            file_source("b-cd1", "Movie Extended cd1.mkv", None, false),
            file_source("a-cd2", "Movie Theatrical cd2.mkv", None, false),
            file_source("b-cd2", "Movie Extended cd2.mkv", None, false),
        ];
        arrange_source_parts(&mut sources);
        let ids = sources
            .iter()
            .map(|source| source.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["a-cd1", "a-cd2", "b-cd1", "b-cd2"]);
    }

    #[test]
    fn a_complete_single_file_reports_one_part_next_to_a_multi_part_version() {
        let mut sources = vec![
            file_source("whole", "Movie.mkv", None, true),
            file_source("part-1", "Movie.cd1.mkv", None, false),
            file_source("part-2", "Movie.cd2.mkv", None, false),
        ];
        arrange_source_parts(&mut sources);
        let whole = sources
            .iter()
            .find(|source| source.id == "whole")
            .expect("whole file");
        assert_eq!(catalog_source_part_count(&sources, whole), 1);
        let part = sources
            .iter()
            .find(|source| source.id == "part-1")
            .expect("first part");
        assert_eq!(catalog_source_part_count(&sources, part), 2);
        assert!(belongs_to_default_version(&sources, whole));
        assert!(!belongs_to_default_version(&sources, part));
    }

    #[test]
    fn parts_of_one_version_stay_together_and_only_the_first_can_be_default() {
        let mut sources = vec![
            catalog_source("a-cd2", Some("A 4K cd2"), false),
            catalog_source("b", Some("B"), false),
            catalog_source("a-cd1", Some("A 4K cd1"), true),
            catalog_source("a-cd3", Some("A 4K cd3"), false),
            catalog_source("plain-cd2", None, false),
        ];
        // Mirror the file-name fallback: the plain file's stem carries the marker.
        sources[4].file_name = Some("Movie cd2.mkv".to_owned());
        arrange_source_parts(&mut sources);
        let ids = sources
            .iter()
            .map(|source| source.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["a-cd1", "a-cd2", "a-cd3", "b", "plain-cd2"]);
        let defaults = sources
            .iter()
            .filter(|source| source.is_default)
            .map(|source| source.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(defaults, ["a-cd1"]);
        assert_eq!(catalog_source_part_count(&sources, &sources[0]), 3);
        assert_eq!(catalog_source_part_count(&sources, &sources[3]), 1);
    }

    #[test]
    fn a_later_part_marked_default_moves_the_default_to_the_first_part() {
        let mut sources = vec![
            catalog_source("x-cd1", Some("X cd1"), false),
            catalog_source("x-cd2", Some("X cd2"), true),
            catalog_source("y", Some("Y"), false),
        ];
        arrange_source_parts(&mut sources);
        let defaults = sources
            .iter()
            .filter(|source| source.is_default)
            .map(|source| source.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(defaults, ["x-cd1"]);
    }

    #[test]
    fn items_without_parts_keep_their_order_and_single_default() {
        let mut sources = vec![
            catalog_source("one", Some("Directors Cut"), false),
            catalog_source("two", None, true),
            catalog_source("three", Some("4K"), true),
        ];
        arrange_source_parts(&mut sources);
        let ids = sources
            .iter()
            .map(|source| source.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["one", "two", "three"]);
        let defaults = sources
            .iter()
            .filter(|source| source.is_default)
            .map(|source| source.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(defaults, ["two"]);
    }

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
            added_at: 0,
            updated_at: 0,
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
    fn catalog_source_file_name_uses_only_the_last_path_component() {
        assert_eq!(
            catalog_source_file_name(Some("Series/Season 01/Show.S01E01.2160p.WEB-DL.mkv",)),
            Some("Show.S01E01.2160p.WEB-DL.mkv".to_owned())
        );
        assert_eq!(catalog_source_file_name(None), None);
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
            item_types: vec!["MOVIE".to_owned(), "SERIES".to_owned()],
            content_restriction_version: None,
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

    #[tokio::test]
    async fn resume_visibility_is_user_scoped_and_filtered_before_series_ranking()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::{
            application::{
                access::{AccessPrincipal, MediaAccessService},
                libraries::LibraryService,
            },
            auth::users::UserStore,
            config::Config,
            library::LibraryKind,
            storage::Database,
        };
        let directory = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        })
        .await?;
        let users = UserStore::new(database.clone())?;
        let owner = users
            .create_user("resume-owner", "Owner", "test password", true)
            .await?;
        let other = users
            .create_user("resume-other", "Other", "test password", true)
            .await?;
        let library = LibraryService::new(database.clone())
            .create_library("Series", LibraryKind::Series, false)
            .await?;
        let series_id = uuid::Uuid::now_v7().to_string();
        sqlx::query("INSERT INTO media_items (id, library_id, item_type, title, sort_title, identification_status)
                     VALUES (?, ?, 'SERIES', 'Series', 'series', 'LOCAL_CONFIRMED')")
            .bind(&series_id).bind(library.id.to_string()).execute(database.pool()).await?;
        let mut episodes = Vec::new();
        for number in [1_i64, 2] {
            let item_id = uuid::Uuid::now_v7().to_string();
            sqlx::query("INSERT INTO media_items (id, library_id, item_type, series_id, season_number,
                         episode_number, title, sort_title, runtime_ticks, identification_status, has_available_source)
                         VALUES (?, ?, 'EPISODE', ?, 1, ?, 'Episode', 'episode', 36000000000, 'LOCAL_CONFIRMED', 1)")
                .bind(&item_id).bind(library.id.to_string()).bind(&series_id).bind(number)
                .execute(database.pool()).await?;
            for user_id in [owner.id, other.id] {
                sqlx::query("INSERT INTO user_item_state (user_id, item_id, position_ticks, is_favorite, last_played_at)
                             VALUES (?, ?, 6000000000, 1, 12345)")
                    .bind(user_id.to_string()).bind(&item_id).execute(database.pool()).await?;
            }
            episodes.push(item_id);
        }
        let catalog =
            super::CatalogService::new(database.clone(), MediaAccessService::new(database.clone()));
        let principal = AccessPrincipal::new(owner.id, true);
        // The marked episode must not suppress the earlier eligible episode.
        let hidden = catalog
            .set_resume_hidden(principal, &episodes[1], true)
            .await?
            .ok_or("missing state")?;
        let duplicate = catalog
            .set_resume_hidden(principal, &episodes[1], true)
            .await?
            .ok_or("missing state")?;
        assert_eq!(hidden, duplicate);
        assert_eq!(hidden.position_ticks, 6_000_000_000);
        assert!(hidden.is_favorite);
        assert!(!hidden.is_played);
        let page = catalog
            .list_continue_watching(principal, &owner.id.to_string(), 0, 1)
            .await?;
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].id, episodes[0]);
        let other_page = catalog
            .list_continue_watching(
                AccessPrincipal::new(other.id, true),
                &other.id.to_string(),
                0,
                1,
            )
            .await?;
        assert_eq!(other_page.items[0].id, episodes[1]);
        catalog
            .set_resume_hidden(principal, &episodes[0], true)
            .await?;
        let empty = catalog
            .list_continue_watching(principal, &owner.id.to_string(), 10, 1)
            .await?;
        assert_eq!(empty.total, 0);
        assert!(empty.items.is_empty());
        let restored = catalog
            .set_resume_hidden(principal, &episodes[1], false)
            .await?
            .ok_or("missing state")?;
        assert_eq!(restored.position_ticks, hidden.position_ticks);
        assert_eq!(restored.last_played_at, hidden.last_played_at);
        let page = catalog
            .list_continue_watching(principal, &owner.id.to_string(), 0, 1)
            .await?;
        assert_eq!(page.items[0].id, episodes[1]);
        let empty = catalog
            .list_continue_watching(principal, &owner.id.to_string(), 10, 1)
            .await?;
        assert_eq!(empty.total, 1);
        database.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn global_collection_catalog_uses_visible_source_libraries()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::{
            application::{access::AccessPrincipal, libraries::LibraryService},
            auth::users::UserStore,
            config::Config,
            library::LibraryKind,
            storage::{
                Database, GLOBAL_COLLECTION_LIBRARY_ID, NewUserContentRestrictions,
                UserContentRestrictionsCasResult,
            },
        };

        let directory = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let libraries = LibraryService::new(database.clone());
        let visible_library = libraries
            .create_library("Visible Movies", LibraryKind::Movie, false)
            .await?;
        let hidden_library = libraries
            .create_library("Hidden Movies", LibraryKind::Movie, false)
            .await?;
        let user = UserStore::new(database.clone())?
            .create_user("catalog-viewer", "Catalog Viewer", "test password", false)
            .await?;
        database
            .set_user_library_access(&user.id.to_string(), &visible_library.id.to_string(), true)
            .await?;

        for (item_id, library_id, movie_id, collection_id, collection_title, year) in [
            (
                "catalog-visible-movie",
                visible_library.id.to_string(),
                "movie-visible",
                "catalog-series",
                "Catalog Series",
                2020,
            ),
            (
                "catalog-hidden-movie",
                hidden_library.id.to_string(),
                "movie-hidden",
                "catalog-series",
                "Catalog Series",
                2020,
            ),
            (
                "catalog-unindexed-movie",
                visible_library.id.to_string(),
                "movie-unindexed",
                "catalog-series",
                "Catalog Series",
                2021,
            ),
            (
                "catalog-visible-restricted-copy",
                visible_library.id.to_string(),
                "movie-visible",
                "catalog-series",
                "Catalog Series",
                2020,
            ),
            (
                "catalog-restricted-only-movie",
                visible_library.id.to_string(),
                "movie-restricted",
                "restricted-series",
                "Restricted Series",
                2024,
            ),
            (
                "catalog-restricted-only-movie-two",
                visible_library.id.to_string(),
                "movie-restricted-two",
                "restricted-series",
                "Restricted Series",
                2025,
            ),
        ] {
            sqlx::query(
                "INSERT INTO media_items (
                     id, library_id, item_type, title, sort_title, production_year,
                     identification_status, has_available_source
                 ) VALUES (?, ?, 'MOVIE', ?, ?, ?, 'ONLINE_CONFIRMED', 1)",
            )
            .bind(item_id)
            .bind(library_id)
            .bind(item_id)
            .bind(item_id)
            .bind(year)
            .execute(database.pool())
            .await?;
            sqlx::query(
                "INSERT INTO collection_item_references (
                     item_id, movie_provider, movie_provider_id, collection_provider,
                     collection_provider_id, collection_title, sort_order
                 ) VALUES (?, 'tmdb', ?, 'tmdb', ?, ?, ?)",
            )
            .bind(item_id)
            .bind(movie_id)
            .bind(collection_id)
            .bind(collection_title)
            .bind(year)
            .execute(database.pool())
            .await?;
        }
        for job in database.list_pending_global_collection_syncs(10).await? {
            database
                .sync_global_collection(
                    &job.collection_provider,
                    &job.collection_provider_id,
                    job.generation,
                )
                .await?;
        }
        sqlx::query(
            "INSERT INTO item_images (id, item_id, image_type, image_index, local_path)
             VALUES ('catalog-visible-poster', 'catalog-visible-movie', 'POSTER', 0, '/visible.jpg'),
                    ('catalog-hidden-poster', 'catalog-hidden-movie', 'POSTER', 0, '/hidden.jpg'),
                    ('catalog-restricted-poster', 'catalog-visible-restricted-copy', 'POSTER', 0, '/restricted.jpg')",
        )
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO media_content_restriction_index (item_id, certification_code)
             VALUES ('catalog-visible-movie', 'PG'),
                    ('catalog-visible-restricted-copy', 'R'),
                    ('catalog-restricted-only-movie', 'R'),
                    ('catalog-restricted-only-movie-two', 'R')",
        )
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO media_content_restriction_tags (item_id, tag)
             VALUES ('catalog-visible-movie', 'family'),
                    ('catalog-visible-restricted-copy', 'family'),
                    ('catalog-visible-restricted-copy', 'violent'),
                    ('catalog-restricted-only-movie', 'family'),
                    ('catalog-restricted-only-movie', 'violent'),
                    ('catalog-restricted-only-movie-two', 'family'),
                    ('catalog-restricted-only-movie-two', 'violent')",
        )
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO media_sources (id, item_id, source_kind, is_default, probe_status)
             VALUES ('catalog-visible-source', 'catalog-visible-movie', 'LOCAL_FILE', 1, 'READY'),
                    ('catalog-restricted-source', 'catalog-visible-restricted-copy', 'LOCAL_FILE', 1, 'READY')",
        )
        .execute(database.pool())
        .await?;
        let allowed_tags = vec!["family".to_owned()];
        let blocked_tags = vec!["violent".to_owned()];
        let policy = database
            .compare_and_swap_user_content_restrictions(
                &user.id.to_string(),
                0,
                NewUserContentRestrictions {
                    rating_system: "MPA",
                    maximum_rating: Some("PG-13"),
                    allow_unrated: false,
                    allowed_tags: &allowed_tags,
                    blocked_tags: &blocked_tags,
                },
            )
            .await?;
        assert!(matches!(
            policy,
            UserContentRestrictionsCasResult::Updated(_)
        ));
        let collection_item_id: String = sqlx::query_scalar(
            "SELECT item_id FROM collections WHERE library_id = ? AND provider_id = 'catalog-series'",
        )
        .bind(GLOBAL_COLLECTION_LIBRARY_ID)
        .fetch_one(database.pool())
        .await?;
        let restricted_work_item_id: String = sqlx::query_scalar(
            "SELECT work_item_id FROM collection_work_items WHERE movie_provider_id = 'movie-restricted'",
        )
        .fetch_one(database.pool())
        .await?;
        let restricted_collection_item_id: String = sqlx::query_scalar(
            "SELECT item_id FROM collections WHERE library_id = ? AND provider_id = 'restricted-series'",
        )
        .bind(GLOBAL_COLLECTION_LIBRARY_ID)
        .fetch_one(database.pool())
        .await?;
        let hidden_work_item_id: String = sqlx::query_scalar(
            "SELECT work_item_id FROM collection_work_items WHERE movie_provider_id = 'movie-hidden'",
        )
        .fetch_one(database.pool())
        .await?;

        let access = crate::application::access::MediaAccessService::new(database.clone());
        let catalog = super::CatalogService::new(database.clone(), access.clone());
        let principal = AccessPrincipal::new(user.id, false);
        let physical_library_ids = access.accessible_library_ids(principal).await?;
        assert_eq!(physical_library_ids, vec![visible_library.id.to_string()]);
        assert!(
            !access
                .can_view_library(principal, GLOBAL_COLLECTION_LIBRARY_ID)
                .await?
        );

        let page = catalog
            .list_global_collection_cards(principal, 0, 10)
            .await?;
        assert_eq!(page.total, 1);
        assert_eq!(page.cards.len(), 1);
        assert_eq!(page.cards[0].item.id, collection_item_id);
        assert_eq!(page.cards[0].member_count, 1);
        assert_eq!(
            page.cards[0].poster_item_id.as_deref(),
            Some("catalog-visible-movie")
        );
        assert_eq!(
            page.cards[0].item.poster_image_tag.as_deref(),
            Some("catalog-visible-poster")
        );
        let counts = catalog
            .count_item_types(principal, &user.id.to_string(), None)
            .await?;
        assert_eq!(counts.box_set_count, 1);
        let recent_page = catalog
            .list_recent_global_collection_cards(principal, 0, 10)
            .await?;
        assert_eq!(recent_page.total, 1);
        assert_eq!(recent_page.cards[0].member_count, 1);

        assert!(
            catalog
                .find_item(principal, &collection_item_id)
                .await?
                .is_some()
        );
        assert!(
            catalog
                .find_item(principal, &hidden_work_item_id)
                .await?
                .is_none()
        );
        let members = catalog
            .list_collection_items(principal, &collection_item_id, 0, 10)
            .await?;
        assert_eq!(members.total, 1);
        assert_eq!(members.items.len(), 1);
        assert_eq!(members.items[0].library_id, GLOBAL_COLLECTION_LIBRARY_ID);
        let detailed_card = catalog
            .find_global_collection_card(principal, &collection_item_id)
            .await?
            .ok_or("expected a visible global collection")?;
        assert_eq!(
            detailed_card.poster_item_id.as_deref(),
            Some("catalog-visible-movie")
        );
        let detailed_members = catalog
            .list_global_collection_members(principal, &collection_item_id, 0, 10)
            .await?;
        assert_eq!(detailed_members.total, 1);
        assert_eq!(detailed_members.members[0].sources.len(), 1);
        assert_eq!(
            detailed_members.members[0].sources[0].item_id,
            "catalog-visible-movie"
        );
        assert!(matches!(
            catalog
                .set_global_collection_source_preference(
                    principal,
                    &collection_item_id,
                    &detailed_members.members[0].item.id,
                    "catalog-visible-restricted-copy",
                    None,
                )
                .await,
            Err(super::CatalogError::AccessDenied)
        ));
        let playback_sources = catalog
            .global_collection_playback_sources(principal, &detailed_members.members[0].item.id)
            .await?
            .ok_or("expected visible playback sources")?;
        // This fixture has no filesystem entries, so visible metadata works have no playable copy.
        assert!(playback_sources.copies.is_empty());
        assert!(
            catalog
                .global_collection_playback_sources(principal, &restricted_work_item_id)
                .await?
                .is_none()
        );
        assert!(matches!(
            catalog
                .list_global_collection_members(principal, &restricted_collection_item_id, 0, 10)
                .await,
            Err(super::CatalogError::AccessDenied)
        ));
        assert!(matches!(
            catalog
                .find_global_collection_card(principal, &restricted_collection_item_id)
                .await,
            Ok(None)
        ));

        let admin_principal = AccessPrincipal::new(user.id, true);
        assert!(
            catalog
                .content_restrictions_for_principal(admin_principal)
                .await?
                .is_none()
        );
        let (_, raw_admin_total) = database
            .list_global_collection_cards_page(&user.id.to_string(), true, 0, 10)
            .await?;
        assert_eq!(raw_admin_total, 2);
        let admin_page = catalog
            .list_global_collection_cards(admin_principal, 0, 10)
            .await?;
        assert_eq!(admin_page.total, 2);
        assert_eq!(admin_page.cards[0].member_count, 3);
        let admin_counts = catalog
            .count_item_types(admin_principal, &user.id.to_string(), None)
            .await?;
        assert_eq!(admin_counts.box_set_count, 2);

        let unrestricted_user = UserStore::new(database.clone())?
            .create_user(
                "catalog-unrestricted",
                "Catalog Unrestricted",
                "test password",
                false,
            )
            .await?;
        database
            .set_user_library_access(
                &unrestricted_user.id.to_string(),
                &visible_library.id.to_string(),
                true,
            )
            .await?;
        let unrestricted_page = catalog
            .list_global_collection_cards(AccessPrincipal::new(unrestricted_user.id, false), 0, 10)
            .await?;
        assert_eq!(unrestricted_page.total, 2);
        assert_eq!(unrestricted_page.cards[0].member_count, 2);
        let unrestricted_counts = catalog
            .count_item_types(
                AccessPrincipal::new(unrestricted_user.id, false),
                &unrestricted_user.id.to_string(),
                None,
            )
            .await?;
        assert_eq!(unrestricted_counts.box_set_count, 2);
        let open_policy = database
            .compare_and_swap_user_content_restrictions(
                &unrestricted_user.id.to_string(),
                0,
                NewUserContentRestrictions {
                    rating_system: "MPA",
                    maximum_rating: None,
                    allow_unrated: true,
                    allowed_tags: &[],
                    blocked_tags: &[],
                },
            )
            .await?;
        assert!(matches!(
            open_policy,
            UserContentRestrictionsCasResult::Updated(_)
        ));
        let open_policy_page = catalog
            .list_global_collection_cards(AccessPrincipal::new(unrestricted_user.id, false), 0, 10)
            .await?;
        assert_eq!(open_policy_page.total, 2);
        let open_policy_counts = catalog
            .count_item_types(
                AccessPrincipal::new(unrestricted_user.id, false),
                &unrestricted_user.id.to_string(),
                None,
            )
            .await?;
        assert_eq!(open_policy_counts.box_set_count, 2);
        Ok(())
    }

    #[tokio::test]
    async fn manual_collection_items_apply_content_restrictions_before_pagination()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::{
            application::{access::AccessPrincipal, libraries::LibraryService},
            auth::users::UserStore,
            config::Config,
            library::LibraryKind,
            storage::{Database, NewUserContentRestrictions},
        };

        let directory = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        })
        .await?;
        let library = LibraryService::new(database.clone())
            .create_library("Manual Collection Movies", LibraryKind::Movie, false)
            .await?;
        let restricted_user = UserStore::new(database.clone())?
            .create_user("manual-collection-viewer", "Viewer", "test password", false)
            .await?;
        let unrestricted_user = UserStore::new(database.clone())?
            .create_user(
                "manual-collection-open",
                "Open Viewer",
                "test password",
                false,
            )
            .await?;
        for user_id in [restricted_user.id, unrestricted_user.id] {
            database
                .set_user_library_access(&user_id.to_string(), &library.id.to_string(), true)
                .await?;
        }

        for (item_id, item_type, title) in [
            ("manual-collection", "BOX_SET", "Manual Collection"),
            ("manual-visible-movie", "MOVIE", "Allowed movie"),
            ("manual-restricted-movie", "MOVIE", "Restricted movie"),
            ("manual-unindexed-movie", "MOVIE", "Unindexed movie"),
            ("manual-parent-series", "SERIES", "Restricted series"),
        ] {
            sqlx::query(
                "INSERT INTO media_items (
                     id, library_id, item_type, title, sort_title,
                     identification_status, has_available_source
                 ) VALUES (?, ?, ?, ?, ?, 'LOCAL_CONFIRMED', 1)",
            )
            .bind(item_id)
            .bind(library.id.to_string())
            .bind(item_type)
            .bind(title)
            .bind(title.to_lowercase())
            .execute(database.pool())
            .await?;
        }
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, series_id, episode_number, title, sort_title,
                 identification_status, has_available_source
             ) VALUES (?, ?, 'EPISODE', 'manual-parent-series', 1,
                       'Allowed rated episode', 'allowed rated episode', 'LOCAL_CONFIRMED', 1)",
        )
        .bind("manual-parent-restricted-episode")
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title,
                 identification_status, has_available_source
             ) VALUES ('manual-hidden-collection', ?, 'BOX_SET', 'Hidden Collection',
                       'hidden collection', 'LOCAL_CONFIRMED', 1)",
        )
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO collections (id, item_id, library_id, provider, provider_id, title)
             VALUES ('manual-collection-row', 'manual-collection', ?, 'manual',
                     'manual-collection', 'Manual Collection'),
                    ('manual-hidden-collection-row', 'manual-hidden-collection', ?, 'manual',
                     'manual-hidden-collection', 'Hidden Collection')",
        )
        .bind(library.id.to_string())
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO collection_items (collection_id, item_id, sort_order)
             VALUES ('manual-collection-row', 'manual-restricted-movie', 1),
                    ('manual-collection-row', 'manual-unindexed-movie', 2),
                    ('manual-collection-row', 'manual-parent-restricted-episode', 3),
                    ('manual-collection-row', 'manual-visible-movie', 4)",
        )
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO collection_items (collection_id, item_id, sort_order)
             VALUES ('manual-hidden-collection-row', 'manual-restricted-movie', 1),
                    ('manual-hidden-collection-row', 'manual-unindexed-movie', 2),
                    ('manual-hidden-collection-row', 'manual-parent-restricted-episode', 3)",
        )
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO media_content_restriction_index (item_id, certification_code)
             VALUES ('manual-collection', 'PG'),
                    ('manual-visible-movie', 'PG'),
                    ('manual-restricted-movie', 'R'),
                    ('manual-parent-series', 'R'),
                    ('manual-parent-restricted-episode', 'PG')",
        )
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO media_content_restriction_tags (item_id, tag)
             VALUES ('manual-collection', 'family'),
                    ('manual-visible-movie', 'family'),
                    ('manual-restricted-movie', 'family'),
                    ('manual-restricted-movie', 'violent'),
                    ('manual-parent-series', 'family'),
                    ('manual-parent-restricted-episode', 'family')",
        )
        .execute(database.pool())
        .await?;
        database
            .compare_and_swap_user_content_restrictions(
                &restricted_user.id.to_string(),
                0,
                NewUserContentRestrictions {
                    rating_system: "MPA",
                    maximum_rating: Some("PG-13"),
                    allow_unrated: false,
                    allowed_tags: &["family".to_owned()],
                    blocked_tags: &["violent".to_owned()],
                },
            )
            .await?;

        let catalog = super::CatalogService::new(
            database.clone(),
            crate::application::access::MediaAccessService::new(database.clone()),
        );
        let direct_access = crate::application::access::MediaAccessService::new(database.clone());
        assert!(
            !direct_access
                .can_view_item(
                    AccessPrincipal::new(restricted_user.id, false),
                    "manual-collection",
                )
                .await?
        );
        assert!(
            catalog
                .find_item(
                    AccessPrincipal::new(restricted_user.id, false),
                    "manual-collection",
                )
                .await?
                .is_some()
        );
        let page = catalog
            .list_collection_items(
                AccessPrincipal::new(restricted_user.id, false),
                "manual-collection",
                0,
                1,
            )
            .await?;
        assert_eq!(page.total, 1);
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].id, "manual-visible-movie");
        assert!(
            catalog
                .find_item(
                    AccessPrincipal::new(restricted_user.id, false),
                    "manual-hidden-collection",
                )
                .await?
                .is_none()
        );

        let unrestricted_page = catalog
            .list_collection_items(
                AccessPrincipal::new(unrestricted_user.id, false),
                "manual-collection",
                0,
                10,
            )
            .await?;
        assert_eq!(unrestricted_page.total, 4);
        assert_eq!(unrestricted_page.items.len(), 4);
        let admin_page = catalog
            .list_collection_items(
                AccessPrincipal::new(restricted_user.id, true),
                "manual-collection",
                0,
                10,
            )
            .await?;
        assert_eq!(admin_page.total, 4);
        assert_eq!(admin_page.items.len(), 4);

        database.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn hierarchy_child_lists_apply_content_restrictions_before_pagination()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::{
            application::{access::AccessPrincipal, libraries::LibraryService},
            auth::users::UserStore,
            config::Config,
            library::LibraryKind,
            storage::{Database, NewUserContentRestrictions},
        };

        let directory = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        })
        .await?;
        let library = LibraryService::new(database.clone())
            .create_library("Hierarchy Restrictions", LibraryKind::Series, false)
            .await?;
        let restricted_user = UserStore::new(database.clone())?
            .create_user(
                "hierarchy-restriction-viewer",
                "Viewer",
                "test password",
                false,
            )
            .await?;
        let unrestricted_user = UserStore::new(database.clone())?
            .create_user(
                "hierarchy-open-viewer",
                "Open Viewer",
                "test password",
                false,
            )
            .await?;
        for user_id in [restricted_user.id, unrestricted_user.id] {
            database
                .set_user_library_access(&user_id.to_string(), &library.id.to_string(), true)
                .await?;
        }

        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status,
                 has_available_source
             ) VALUES ('hierarchy-series', ?, 'SERIES', 'Series', 'series',
                       'LOCAL_CONFIRMED', 1)",
        )
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;
        for (season_id, season_number, title) in [
            ("hierarchy-visible-season", 1_i64, "Visible season"),
            ("hierarchy-restricted-season", 2_i64, "Restricted season"),
        ] {
            sqlx::query(
                "INSERT INTO media_items (
                     id, library_id, item_type, parent_id, series_id, season_number,
                     title, sort_title, identification_status, has_available_source
                 ) VALUES (?, ?, 'SEASON', 'hierarchy-series', 'hierarchy-series', ?, ?, ?,
                           'LOCAL_CONFIRMED', 1)",
            )
            .bind(season_id)
            .bind(library.id.to_string())
            .bind(season_number)
            .bind(title)
            .bind(title.to_lowercase())
            .execute(database.pool())
            .await?;
        }
        for (episode_id, season_id, season_number, episode_number) in [
            (
                "hierarchy-restricted-episode",
                "hierarchy-visible-season",
                1_i64,
                1_i64,
            ),
            (
                "hierarchy-unindexed-episode",
                "hierarchy-visible-season",
                1,
                2,
            ),
            (
                "hierarchy-visible-episode",
                "hierarchy-visible-season",
                1,
                3,
            ),
            (
                "hierarchy-parent-restricted-episode",
                "hierarchy-restricted-season",
                2,
                1,
            ),
        ] {
            sqlx::query(
                "INSERT INTO media_items (
                     id, library_id, item_type, parent_id, series_id, season_number,
                     episode_number, title, sort_title, identification_status,
                     has_available_source
                 ) VALUES (?, ?, 'EPISODE', ?, 'hierarchy-series', ?, ?, ?, ?,
                           'LOCAL_CONFIRMED', 1)",
            )
            .bind(episode_id)
            .bind(library.id.to_string())
            .bind(season_id)
            .bind(season_number)
            .bind(episode_number)
            .bind(episode_id)
            .bind(episode_id)
            .execute(database.pool())
            .await?;
        }
        sqlx::query(
            "INSERT INTO media_content_restriction_index (item_id, certification_code)
             VALUES ('hierarchy-series', 'PG'),
                    ('hierarchy-visible-season', 'PG'),
                    ('hierarchy-restricted-season', 'R'),
                    ('hierarchy-restricted-episode', 'R'),
                    ('hierarchy-visible-episode', 'PG'),
                    ('hierarchy-parent-restricted-episode', 'PG')",
        )
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO media_content_restriction_tags (item_id, tag)
             VALUES ('hierarchy-series', 'family'),
                    ('hierarchy-visible-season', 'family'),
                    ('hierarchy-restricted-season', 'family'),
                    ('hierarchy-restricted-episode', 'family'),
                    ('hierarchy-restricted-episode', 'violent'),
                    ('hierarchy-visible-episode', 'family'),
                    ('hierarchy-parent-restricted-episode', 'family')",
        )
        .execute(database.pool())
        .await?;
        database
            .compare_and_swap_user_content_restrictions(
                &restricted_user.id.to_string(),
                0,
                NewUserContentRestrictions {
                    rating_system: "MPA",
                    maximum_rating: Some("PG-13"),
                    allow_unrated: false,
                    allowed_tags: &["family".to_owned()],
                    blocked_tags: &["violent".to_owned()],
                },
            )
            .await?;

        let catalog = super::CatalogService::new(
            database.clone(),
            crate::application::access::MediaAccessService::new(database.clone()),
        );
        let restricted_principal = AccessPrincipal::new(restricted_user.id, false);
        let seasons = catalog
            .list_children(restricted_principal, "hierarchy-series", "SEASON", 0, 10)
            .await?;
        assert_eq!(seasons.total, 1);
        assert_eq!(seasons.items[0].id, "hierarchy-visible-season");
        let children = catalog
            .list_children(
                restricted_principal,
                "hierarchy-visible-season",
                "EPISODE",
                0,
                1,
            )
            .await?;
        assert_eq!(children.total, 1);
        assert_eq!(children.items[0].id, "hierarchy-visible-episode");
        let series_episodes = catalog
            .list_series_episodes(
                restricted_principal,
                "hierarchy-series",
                Some("hierarchy-visible-season"),
                0,
                1,
            )
            .await?;
        assert_eq!(series_episodes.total, 1);
        assert_eq!(series_episodes.items[0].id, "hierarchy-visible-episode");
        let parent_restricted = catalog
            .list_series_episodes(
                restricted_principal,
                "hierarchy-series",
                Some("hierarchy-restricted-season"),
                0,
                1,
            )
            .await?;
        assert_eq!(parent_restricted.total, 0);
        assert!(parent_restricted.items.is_empty());

        let unrestricted_principal = AccessPrincipal::new(unrestricted_user.id, false);
        let unrestricted_seasons = catalog
            .list_children(unrestricted_principal, "hierarchy-series", "SEASON", 0, 10)
            .await?;
        assert_eq!(unrestricted_seasons.total, 2);
        let unrestricted_episodes = catalog
            .list_series_episodes(
                unrestricted_principal,
                "hierarchy-series",
                Some("hierarchy-visible-season"),
                0,
                10,
            )
            .await?;
        assert_eq!(unrestricted_episodes.total, 3);
        let admin_episodes = catalog
            .list_series_episodes(
                AccessPrincipal::new(restricted_user.id, true),
                "hierarchy-series",
                Some("hierarchy-visible-season"),
                0,
                10,
            )
            .await?;
        assert_eq!(admin_episodes.total, 3);

        database.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn content_restricted_catalog_counts_and_episode_counts_match_visible_items()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::{
            application::{access::AccessPrincipal, libraries::LibraryService},
            auth::users::UserStore,
            config::Config,
            library::LibraryKind,
            storage::{Database, NewUserContentRestrictions},
        };

        let directory = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        })
        .await?;
        let libraries = LibraryService::new(database.clone());
        let library = libraries
            .create_library("Content Count", LibraryKind::Series, false)
            .await?;
        let root_path = directory.path().join("media-root");
        tokio::fs::create_dir_all(&root_path).await?;
        libraries
            .add_root(library.id, root_path.to_str().ok_or("non-utf8 path")?)
            .await?;
        let root_id: String =
            sqlx::query_scalar("SELECT id FROM library_roots WHERE library_id = ?")
                .bind(library.id.to_string())
                .fetch_one(database.pool())
                .await?;
        let restricted_user = UserStore::new(database.clone())?
            .create_user("content-count-viewer", "Viewer", "test password", false)
            .await?;
        let open_user = UserStore::new(database.clone())?
            .create_user("content-count-open", "Open Viewer", "test password", false)
            .await?;
        for user_id in [restricted_user.id, open_user.id] {
            database
                .set_user_library_access(&user_id.to_string(), &library.id.to_string(), true)
                .await?;
        }

        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status,
                 has_available_source
             ) VALUES ('content-count-series', ?, 'SERIES', 'Series', 'series',
                       'LOCAL_CONFIRMED', 1)",
        )
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, parent_id, series_id, season_number,
                 title, sort_title, identification_status, has_available_source
             ) VALUES ('content-count-season', ?, 'SEASON', 'content-count-series',
                       'content-count-series', 1, 'Season', 'season', 'LOCAL_CONFIRMED', 1)",
        )
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;
        for (item_id, episode_number) in [
            ("content-count-restricted", 1_i64),
            ("content-count-unindexed", 2_i64),
            ("content-count-visible", 3_i64),
        ] {
            sqlx::query(
                "INSERT INTO media_items (
                     id, library_id, item_type, parent_id, series_id, season_number,
                     episode_number, title, sort_title, identification_status,
                     has_available_source
                 ) VALUES (?, ?, 'EPISODE', 'content-count-season', 'content-count-series',
                           1, ?, ?, ?, 'LOCAL_CONFIRMED', 1)",
            )
            .bind(item_id)
            .bind(library.id.to_string())
            .bind(episode_number)
            .bind(item_id)
            .bind(item_id)
            .execute(database.pool())
            .await?;
            let entry_id = format!("entry-{item_id}");
            let source_id = format!("source-{item_id}");
            sqlx::query(
                "INSERT INTO filesystem_entries (
                     id, library_root_id, relative_path, entry_kind, size, modified_at,
                     last_seen_generation
                 ) VALUES (?, ?, ?, 'FILE', 1, 1, 'content-count-generation')",
            )
            .bind(&entry_id)
            .bind(&root_id)
            .bind(format!("{item_id}.mkv"))
            .execute(database.pool())
            .await?;
            sqlx::query(
                "INSERT INTO media_sources (
                     id, item_id, source_kind, filesystem_entry_id, is_default, probe_status
                 ) VALUES (?, ?, 'LOCAL_FILE', ?, 1, 'READY')",
            )
            .bind(&source_id)
            .bind(item_id)
            .bind(&entry_id)
            .execute(database.pool())
            .await?;
        }
        sqlx::query(
            "INSERT INTO media_content_restriction_index (item_id, certification_code)
             VALUES ('content-count-series', 'PG'),
                    ('content-count-season', 'PG'),
                    ('content-count-restricted', 'R'),
                    ('content-count-visible', 'PG')",
        )
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO media_content_restriction_tags (item_id, tag)
             VALUES ('content-count-series', 'family'),
                    ('content-count-season', 'family'),
                    ('content-count-restricted', 'family'),
                    ('content-count-restricted', 'violent'),
                    ('content-count-visible', 'family')",
        )
        .execute(database.pool())
        .await?;
        for (collection_id, item_id, title, member_id) in [
            (
                "content-count-box-visible",
                "content-count-box-visible-item",
                "Visible Collection",
                "content-count-visible",
            ),
            (
                "content-count-box-hidden",
                "content-count-box-hidden-item",
                "Restricted Collection",
                "content-count-restricted",
            ),
        ] {
            sqlx::query(
                "INSERT INTO media_items (
                     id, library_id, item_type, title, sort_title, identification_status,
                     has_available_source
                 ) VALUES (?, ?, 'BOX_SET', ?, ?, 'LOCAL_CONFIRMED', 1)",
            )
            .bind(item_id)
            .bind(library.id.to_string())
            .bind(title)
            .bind(title.to_lowercase())
            .execute(database.pool())
            .await?;
            sqlx::query(
                "INSERT INTO collections (id, item_id, library_id, provider, provider_id, title)
                 VALUES (?, ?, ?, 'manual', ?, ?)",
            )
            .bind(collection_id)
            .bind(item_id)
            .bind(library.id.to_string())
            .bind(collection_id)
            .bind(title)
            .execute(database.pool())
            .await?;
            sqlx::query(
                "INSERT INTO collection_items (collection_id, item_id, sort_order)
                 VALUES (?, ?, 0)",
            )
            .bind(collection_id)
            .bind(member_id)
            .execute(database.pool())
            .await?;
        }
        database
            .compare_and_swap_user_content_restrictions(
                &restricted_user.id.to_string(),
                0,
                NewUserContentRestrictions {
                    rating_system: "MPA",
                    maximum_rating: Some("PG-13"),
                    allow_unrated: false,
                    allowed_tags: &["family".to_owned()],
                    blocked_tags: &["violent".to_owned()],
                },
            )
            .await?;

        let catalog = super::CatalogService::new(
            database.clone(),
            crate::application::access::MediaAccessService::new(database.clone()),
        );
        let restricted_principal = AccessPrincipal::new(restricted_user.id, false);
        let restricted_counts = catalog
            .count_item_types(restricted_principal, &restricted_user.id.to_string(), None)
            .await?;
        assert_eq!(restricted_counts.series_count, 1);
        assert_eq!(restricted_counts.episode_count, 1);
        assert_eq!(restricted_counts.box_set_count, 1);
        assert_eq!(restricted_counts.item_count, 4);
        let seasons = catalog
            .list_children(
                restricted_principal,
                "content-count-series",
                "SEASON",
                0,
                10,
            )
            .await?;
        assert_eq!(seasons.items[0].episode_count, Some(1));
        let library_items = catalog
            .list_library_items_filtered(
                restricted_principal,
                &library.id.to_string(),
                &super::CatalogFilter::default(),
                0,
                10,
            )
            .await?;
        let series_card = library_items
            .items
            .iter()
            .find(|item| item.id == "content-count-series")
            .ok_or("visible series card should remain in the library page")?;
        assert_eq!(series_card.episode_count, Some(1));
        let series = catalog
            .find_item(restricted_principal, "content-count-series")
            .await?
            .ok_or("visible series should remain accessible")?;
        assert_eq!(series.episode_count, Some(1));
        let season = catalog
            .find_item(restricted_principal, "content-count-season")
            .await?
            .ok_or("visible season should remain accessible")?;
        assert_eq!(season.episode_count, Some(1));

        let open_principal = AccessPrincipal::new(open_user.id, false);
        let open_counts = catalog
            .count_item_types(open_principal, &open_user.id.to_string(), None)
            .await?;
        assert_eq!(open_counts.episode_count, 3);
        assert_eq!(open_counts.box_set_count, 2);
        assert_eq!(open_counts.item_count, 7);
        let admin_counts = catalog
            .count_item_types(
                AccessPrincipal::new(restricted_user.id, true),
                &restricted_user.id.to_string(),
                None,
            )
            .await?;
        assert_eq!(admin_counts.episode_count, 3);
        assert_eq!(admin_counts.box_set_count, 2);
        assert_eq!(admin_counts.item_count, 7);

        let open_policy = database
            .compare_and_swap_user_content_restrictions(
                &open_user.id.to_string(),
                0,
                NewUserContentRestrictions {
                    rating_system: "MPA",
                    maximum_rating: None,
                    allow_unrated: true,
                    allowed_tags: &[],
                    blocked_tags: &[],
                },
            )
            .await?;
        assert!(matches!(
            open_policy,
            crate::storage::UserContentRestrictionsCasResult::Updated(_)
        ));
        let open_policy_counts = catalog
            .count_item_types(open_principal, &open_user.id.to_string(), None)
            .await?;
        assert_eq!(open_policy_counts.episode_count, 3);
        assert_eq!(open_policy_counts.box_set_count, 2);
        assert_eq!(open_policy_counts.item_count, 7);

        database.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn library_page_applies_user_policy_and_uses_policy_version_in_cache_key()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::{
            application::{access::AccessPrincipal, libraries::LibraryService},
            auth::users::UserStore,
            config::Config,
            library::LibraryKind,
            storage::{Database, NewUserContentRestrictions, UserContentRestrictionsCasResult},
        };

        let directory = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        })
        .await?;
        let user = UserStore::new(database.clone())?
            .create_user("policy-viewer", "Policy Viewer", "test password", false)
            .await?;
        let library = LibraryService::new(database.clone())
            .create_library("Restricted movies", LibraryKind::Movie, false)
            .await?;
        let user_id = user.id.to_string();
        let library_id = library.id.to_string();
        database
            .set_user_library_access(&user_id, &library_id, true)
            .await?;
        for item_id in ["policy-visible", "policy-blocked-after-update"] {
            sqlx::query(
                "INSERT INTO media_items (
                     id, library_id, item_type, title, sort_title,
                     identification_status, has_available_source
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', 1)",
            )
            .bind(item_id)
            .bind(&library_id)
            .bind(item_id)
            .bind(item_id)
            .execute(database.pool())
            .await?;
            sqlx::query(
                "INSERT INTO media_content_restriction_index (item_id, certification_code)
                 VALUES (?, 'PG')",
            )
            .bind(item_id)
            .execute(database.pool())
            .await?;
        }
        sqlx::query(
            "INSERT INTO media_content_restriction_tags (item_id, tag)
             VALUES ('policy-visible', 'family'),
                    ('policy-blocked-after-update', 'violent')",
        )
        .execute(database.pool())
        .await?;

        let initial = database
            .compare_and_swap_user_content_restrictions(
                &user_id,
                0,
                NewUserContentRestrictions {
                    rating_system: "MPA",
                    maximum_rating: Some("PG-13"),
                    allow_unrated: false,
                    allowed_tags: &[],
                    blocked_tags: &[],
                },
            )
            .await?;
        assert!(matches!(
            initial,
            UserContentRestrictionsCasResult::Updated(_)
        ));

        let catalog = super::CatalogService::new(
            database.clone(),
            crate::application::access::MediaAccessService::new(database.clone()),
        );
        let principal = AccessPrincipal::new(user.id, false);
        let filter = super::CatalogFilter::default();
        let initial_page = catalog
            .list_library_items_filtered(principal, &library_id, &filter, 0, 10)
            .await?;
        assert_eq!(initial_page.total, 2);

        let updated = database
            .compare_and_swap_user_content_restrictions(
                &user_id,
                1,
                NewUserContentRestrictions {
                    rating_system: "MPA",
                    maximum_rating: Some("PG-13"),
                    allow_unrated: false,
                    allowed_tags: &[],
                    blocked_tags: &["violent".to_owned()],
                },
            )
            .await?;
        assert!(matches!(
            updated,
            UserContentRestrictionsCasResult::Updated(_)
        ));
        let updated_page = catalog
            .list_library_items_filtered(principal, &library_id, &filter, 0, 10)
            .await?;
        assert_eq!(updated_page.total, 1);
        assert_eq!(updated_page.items[0].id, "policy-visible");
        Ok(())
    }

    #[tokio::test]
    async fn library_search_applies_content_restrictions_before_pagination()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::{
            application::{access::AccessPrincipal, libraries::LibraryService},
            auth::users::UserStore,
            config::Config,
            library::LibraryKind,
            storage::{Database, NewUserContentRestrictions, UserContentRestrictionsCasResult},
        };

        let directory = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        })
        .await?;
        let user = UserStore::new(database.clone())?
            .create_user(
                "search-policy-viewer",
                "Search Viewer",
                "test password",
                false,
            )
            .await?;
        let library = LibraryService::new(database.clone())
            .create_library("Search policy movies", LibraryKind::Movie, false)
            .await?;
        let user_id = user.id.to_string();
        let library_id = library.id.to_string();
        database
            .set_user_library_access(&user_id, &library_id, true)
            .await?;
        for (item_id, title, certification, tags) in [
            (
                "search-policy-visible",
                "Policy Visible Film",
                "PG",
                &["approved"][..],
            ),
            (
                "search-policy-hidden",
                "Policy Hidden Film",
                "R",
                &["approved"][..],
            ),
            (
                "search-policy-blocked-tag",
                "Policy Blocked Tag Film",
                "PG",
                &["approved", "violent"][..],
            ),
        ] {
            sqlx::query(
                "INSERT INTO media_items (
                     id, library_id, item_type, title, sort_title,
                     identification_status, has_available_source
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', 1)",
            )
            .bind(item_id)
            .bind(&library_id)
            .bind(title)
            .bind(title)
            .execute(database.pool())
            .await?;
            sqlx::query(
                "INSERT INTO media_content_restriction_index (item_id, certification_code)
                 VALUES (?, ?)",
            )
            .bind(item_id)
            .bind(certification)
            .execute(database.pool())
            .await?;
            for tag in tags {
                sqlx::query(
                    "INSERT INTO media_content_restriction_tags (item_id, tag) VALUES (?, ?)",
                )
                .bind(item_id)
                .bind(tag)
                .execute(database.pool())
                .await?;
            }
        }
        let catalog = super::CatalogService::new(
            database.clone(),
            crate::application::access::MediaAccessService::new(database.clone()),
        );
        let principal = AccessPrincipal::new(user.id, false);
        let item_types = vec!["MOVIE".to_owned()];
        let before_policy = catalog
            .search_items_with_types(principal, "policy", "%Policy%", item_types.clone(), 0, 10)
            .await?;
        assert_eq!(before_policy.total, 3);

        let allowed_tags = vec!["approved".to_owned()];
        let blocked_tags = vec!["violent".to_owned()];
        let policy = database
            .compare_and_swap_user_content_restrictions(
                &user_id,
                0,
                NewUserContentRestrictions {
                    rating_system: "MPA",
                    maximum_rating: Some("PG-13"),
                    allow_unrated: false,
                    allowed_tags: &allowed_tags,
                    blocked_tags: &blocked_tags,
                },
            )
            .await?;
        assert!(matches!(
            policy,
            UserContentRestrictionsCasResult::Updated(_)
        ));
        let restricted_page = catalog
            .search_items_with_types(principal, "policy", "%Policy%", item_types, 0, 1)
            .await?;
        assert_eq!(restricted_page.total, 1);
        assert_eq!(restricted_page.items.len(), 1);
        assert_eq!(restricted_page.items[0].id, "search-policy-visible");
        let like_fallback = catalog
            .search_items_with_types(principal, "oli", "%oli%", vec!["MOVIE".to_owned()], 0, 10)
            .await?;
        assert_eq!(like_fallback.total, 1);
        assert_eq!(like_fallback.items[0].id, "search-policy-visible");
        let past_end = catalog
            .search_items_with_types(
                principal,
                "policy",
                "%Policy%",
                vec!["MOVIE".to_owned()],
                1,
                1,
            )
            .await?;
        assert!(past_end.items.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn continue_watching_applies_content_restrictions_before_pagination()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::{
            application::{access::AccessPrincipal, libraries::LibraryService},
            auth::users::UserStore,
            config::Config,
            library::LibraryKind,
            storage::{Database, NewUserContentRestrictions, UserContentRestrictionsCasResult},
        };

        let directory = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        })
        .await?;
        let user = UserStore::new(database.clone())?
            .create_user(
                "resume-policy-viewer",
                "Resume Viewer",
                "test password",
                false,
            )
            .await?;
        let library = LibraryService::new(database.clone())
            .create_library("Resume policy", LibraryKind::Series, false)
            .await?;
        let user_id = user.id.to_string();
        let library_id = library.id.to_string();
        database
            .set_user_library_access(&user_id, &library_id, true)
            .await?;

        for (item_id, certification) in [
            ("resume-policy-visible-movie", "PG"),
            ("resume-policy-hidden-movie", "R"),
            ("resume-policy-open-series", "PG"),
            ("resume-policy-blocked-series", "R"),
        ] {
            let item_type = if item_id.ends_with("series") {
                "SERIES"
            } else {
                "MOVIE"
            };
            sqlx::query(
                "INSERT INTO media_items (
                     id, library_id, item_type, title, sort_title, runtime_ticks,
                     identification_status, has_available_source
                 ) VALUES (?, ?, ?, ?, ?, 36000000000, 'LOCAL_CONFIRMED', 1)",
            )
            .bind(item_id)
            .bind(&library_id)
            .bind(item_type)
            .bind(item_id)
            .bind(item_id)
            .execute(database.pool())
            .await?;
            sqlx::query(
                "INSERT INTO media_content_restriction_index (item_id, certification_code)
                 VALUES (?, ?)",
            )
            .bind(item_id)
            .bind(certification)
            .execute(database.pool())
            .await?;
            if item_type == "MOVIE" {
                sqlx::query(
                    "INSERT INTO user_item_state (
                         user_id, item_id, is_played, position_ticks, last_played_at
                     ) VALUES (?, ?, 0, 6000000000, 30)",
                )
                .bind(&user_id)
                .bind(item_id)
                .execute(database.pool())
                .await?;
            }
        }
        sqlx::query(
            "INSERT INTO media_content_restriction_tags (item_id, tag)
             VALUES ('resume-policy-hidden-movie', 'violent')",
        )
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, runtime_ticks,
                 identification_status, has_available_source
             ) VALUES (
                 'resume-policy-unindexed-movie', ?, 'MOVIE',
                 'Unindexed Movie', 'unindexed movie', 36000000000,
                 'LOCAL_CONFIRMED', 1
             )",
        )
        .bind(&library_id)
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO user_item_state (
                 user_id, item_id, is_played, position_ticks, last_played_at
             ) VALUES (?, 'resume-policy-unindexed-movie', 0, 6000000000, 25)",
        )
        .bind(&user_id)
        .execute(database.pool())
        .await?;

        for (series_id, prefix) in [
            ("resume-policy-open-series", "resume-policy-open"),
            ("resume-policy-blocked-series", "resume-policy-blocked"),
        ] {
            for (episode_number, played) in [(1_i64, true), (2_i64, false)] {
                let item_id = format!("{prefix}-episode-{episode_number}");
                sqlx::query(
                    "INSERT INTO media_items (
                         id, library_id, item_type, series_id, season_number,
                         episode_number, title, sort_title, runtime_ticks,
                         identification_status, has_available_source
                     ) VALUES (?, ?, 'EPISODE', ?, 1, ?, ?, ?, 36000000000, 'LOCAL_CONFIRMED', 1)",
                )
                .bind(&item_id)
                .bind(&library_id)
                .bind(series_id)
                .bind(episode_number)
                .bind(&item_id)
                .bind(&item_id)
                .execute(database.pool())
                .await?;
                sqlx::query(
                    "INSERT INTO media_content_restriction_index (item_id, certification_code)
                     VALUES (?, 'PG')",
                )
                .bind(&item_id)
                .execute(database.pool())
                .await?;
                if played {
                    sqlx::query(
                        "INSERT INTO user_item_state (
                             user_id, item_id, is_played, position_ticks, last_played_at
                         ) VALUES (?, ?, 1, 36000000000, 20)",
                    )
                    .bind(&user_id)
                    .bind(&item_id)
                    .execute(database.pool())
                    .await?;
                }
            }
        }

        let catalog = super::CatalogService::new(
            database.clone(),
            crate::application::access::MediaAccessService::new(database.clone()),
        );
        let principal = AccessPrincipal::new(user.id, false);
        let before_policy = catalog
            .list_continue_watching(principal, &user_id, 0, 10)
            .await?;
        assert_eq!(
            before_policy.total,
            5,
            "before policy: {:?}",
            before_policy
                .items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>()
        );

        let blocked_tags = vec!["violent".to_owned()];
        let policy = database
            .compare_and_swap_user_content_restrictions(
                &user_id,
                0,
                NewUserContentRestrictions {
                    rating_system: "MPA",
                    maximum_rating: Some("PG-13"),
                    allow_unrated: false,
                    allowed_tags: &[],
                    blocked_tags: &blocked_tags,
                },
            )
            .await?;
        assert!(matches!(
            policy,
            UserContentRestrictionsCasResult::Updated(_)
        ));

        let restricted_page = catalog
            .list_continue_watching(principal, &user_id, 0, 1)
            .await?;
        assert_eq!(restricted_page.total, 2);
        assert_eq!(restricted_page.items.len(), 1);
        assert_eq!(restricted_page.items[0].id, "resume-policy-visible-movie");
        let home_page = catalog
            .list_home_continue_watching(principal, &user_id, 0, 10)
            .await?;
        assert_eq!(home_page.total, 2);
        assert!(
            home_page
                .items
                .iter()
                .any(|item| item.id == "resume-policy-visible-movie")
        );
        assert!(
            home_page
                .items
                .iter()
                .any(|item| item.id == "resume-policy-open-episode-2")
        );
        let home = crate::application::home::HomeService::new(
            catalog.clone(),
            LibraryService::new(database.clone()),
        );
        let snapshot = home.snapshot(principal, vec![library_id.clone()]).await?;
        assert_eq!(snapshot.continue_watching.total, 2);
        assert!(snapshot.continue_watching.items.iter().all(|item| {
            item.id != "resume-policy-hidden-movie"
                && item.id != "resume-policy-unindexed-movie"
                && item.id != "resume-policy-blocked-episode-2"
        }));
        let second_page = catalog
            .list_continue_watching(principal, &user_id, 1, 1)
            .await?;
        assert_eq!(second_page.total, 2);
        assert_eq!(second_page.items.len(), 1);
        assert_eq!(second_page.items[0].id, "resume-policy-open-episode-2");
        Ok(())
    }

    #[tokio::test]
    async fn recommendations_apply_content_restrictions_before_candidate_pool()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::{
            application::{access::AccessPrincipal, libraries::LibraryService},
            auth::users::UserStore,
            config::Config,
            library::LibraryKind,
            storage::{Database, NewUserContentRestrictions, UserContentRestrictionsCasResult},
        };

        let directory = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        })
        .await?;
        let user = UserStore::new(database.clone())?
            .create_user(
                "recommendation-policy-viewer",
                "Recommendation Viewer",
                "test password",
                false,
            )
            .await?;
        let library = LibraryService::new(database.clone())
            .create_library("Recommendation policy", LibraryKind::Movie, false)
            .await?;
        let user_id = user.id.to_string();
        let library_id = library.id.to_string();
        database
            .set_user_library_access(&user_id, &library_id, true)
            .await?;
        let mut candidates = (0..60)
            .map(|index| {
                (
                    format!("recommendation-policy-hidden-{index:02}"),
                    "R",
                    1_000_000_000_i64,
                )
            })
            .collect::<Vec<_>>();
        candidates.push(("recommendation-policy-visible".to_owned(), "PG", 0));
        for (item_id, certification, added_at) in candidates {
            sqlx::query(
                "INSERT INTO media_items (
                     id, library_id, item_type, title, sort_title,
                     identification_status, has_available_source, added_at
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', 1, ?)",
            )
            .bind(&item_id)
            .bind(&library_id)
            .bind(&item_id)
            .bind(&item_id)
            .bind(added_at)
            .execute(database.pool())
            .await?;
            sqlx::query(
                "INSERT INTO media_content_restriction_index (item_id, certification_code)
                 VALUES (?, ?)",
            )
            .bind(&item_id)
            .bind(certification)
            .execute(database.pool())
            .await?;
        }
        let catalog = super::CatalogService::new(
            database.clone(),
            crate::application::access::MediaAccessService::new(database.clone()),
        );
        let principal = AccessPrincipal::new(user.id, false);
        let before_policy = catalog.list_recommended(principal, &user_id, 10).await?;
        assert_eq!(before_policy.len(), 10);
        assert!(
            before_policy
                .iter()
                .all(|item| item.id != "recommendation-policy-visible")
        );

        let policy = database
            .compare_and_swap_user_content_restrictions(
                &user_id,
                0,
                NewUserContentRestrictions {
                    rating_system: "MPA",
                    maximum_rating: Some("PG-13"),
                    allow_unrated: false,
                    allowed_tags: &[],
                    blocked_tags: &[],
                },
            )
            .await?;
        assert!(matches!(
            policy,
            UserContentRestrictionsCasResult::Updated(_)
        ));
        let restricted = catalog.list_recommended(principal, &user_id, 10).await?;
        assert_eq!(restricted.len(), 1);
        assert_eq!(restricted[0].id, "recommendation-policy-visible");

        let admin = AccessPrincipal::new(user.id, true);
        let admin_recommendations = catalog.list_recommended(admin, &user_id, 10).await?;
        assert_eq!(admin_recommendations.len(), 10);
        assert_eq!(
            admin_recommendations
                .iter()
                .map(|item| &item.id)
                .collect::<Vec<_>>(),
            before_policy
                .iter()
                .map(|item| &item.id)
                .collect::<Vec<_>>()
        );

        let unrestricted_policy = database
            .compare_and_swap_user_content_restrictions(
                &user_id,
                1,
                NewUserContentRestrictions {
                    rating_system: "MPA",
                    maximum_rating: None,
                    allow_unrated: true,
                    allowed_tags: &[],
                    blocked_tags: &[],
                },
            )
            .await?;
        assert!(matches!(
            unrestricted_policy,
            UserContentRestrictionsCasResult::Updated(_)
        ));
        let unrestricted_recommendations =
            catalog.list_recommended(principal, &user_id, 10).await?;
        assert_eq!(unrestricted_recommendations.len(), 10);
        assert_eq!(
            unrestricted_recommendations
                .iter()
                .map(|item| &item.id)
                .collect::<Vec<_>>(),
            before_policy
                .iter()
                .map(|item| &item.id)
                .collect::<Vec<_>>()
        );
        Ok(())
    }

    #[tokio::test]
    async fn home_recently_added_applies_content_restrictions_before_pagination()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::{
            application::{access::AccessPrincipal, libraries::LibraryService},
            auth::users::UserStore,
            config::Config,
            library::LibraryKind,
            storage::{Database, NewUserContentRestrictions, UserContentRestrictionsCasResult},
        };

        let directory = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        })
        .await?;
        let user = UserStore::new(database.clone())?
            .create_user(
                "recent-policy-viewer",
                "Recent Policy Viewer",
                "test password",
                false,
            )
            .await?;
        let library = LibraryService::new(database.clone())
            .create_library("Recent policy", LibraryKind::Movie, false)
            .await?;
        let user_id = user.id.to_string();
        let library_id = library.id.to_string();
        database
            .set_user_library_access(&user_id, &library_id, true)
            .await?;
        for (item_id, certification, added_at) in [
            ("recent-policy-hidden", Some("R"), 30_i64),
            ("recent-policy-unindexed", None, 20_i64),
            ("recent-policy-visible", Some("PG"), 10_i64),
        ] {
            sqlx::query(
                "INSERT INTO media_items (
                     id, library_id, item_type, title, sort_title,
                     identification_status, has_available_source, added_at
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', 1, ?)",
            )
            .bind(item_id)
            .bind(&library_id)
            .bind(item_id)
            .bind(item_id)
            .bind(added_at)
            .execute(database.pool())
            .await?;
            if let Some(certification) = certification {
                sqlx::query(
                    "INSERT INTO media_content_restriction_index (item_id, certification_code)
                     VALUES (?, ?)",
                )
                .bind(item_id)
                .bind(certification)
                .execute(database.pool())
                .await?;
            }
        }
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title,
                 identification_status, has_available_source, added_at
             ) VALUES (
                 'recent-policy-blocked-series', ?, 'SERIES',
                 'Blocked Series', 'blocked series', 'LOCAL_CONFIRMED', 0, 40
             )",
        )
        .bind(&library_id)
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, parent_id, series_id,
                 title, sort_title, identification_status, has_available_source, added_at
             ) VALUES (
                 'recent-policy-blocked-episode', ?, 'EPISODE',
                 'recent-policy-blocked-series', 'recent-policy-blocked-series',
                 'Blocked Episode', 'blocked episode', 'LOCAL_CONFIRMED', 1, 50
             )",
        )
        .bind(&library_id)
        .execute(database.pool())
        .await?;
        for (item_id, certification) in [
            ("recent-policy-blocked-series", "R"),
            ("recent-policy-blocked-episode", "PG"),
        ] {
            sqlx::query(
                "INSERT INTO media_content_restriction_index (item_id, certification_code)
                 VALUES (?, ?)",
            )
            .bind(item_id)
            .bind(certification)
            .execute(database.pool())
            .await?;
        }
        for (item_id, tag) in [
            ("recent-policy-hidden", "violent"),
            ("recent-policy-visible", "family"),
            ("recent-policy-blocked-series", "family"),
            ("recent-policy-blocked-episode", "family"),
        ] {
            sqlx::query("INSERT INTO media_content_restriction_tags (item_id, tag) VALUES (?, ?)")
                .bind(item_id)
                .bind(tag)
                .execute(database.pool())
                .await?;
        }
        let allowed_tags = vec!["family".to_owned()];
        let blocked_tags = vec!["violent".to_owned()];
        let policy = database
            .compare_and_swap_user_content_restrictions(
                &user_id,
                0,
                NewUserContentRestrictions {
                    rating_system: "MPA",
                    maximum_rating: Some("PG-13"),
                    allow_unrated: false,
                    allowed_tags: &allowed_tags,
                    blocked_tags: &blocked_tags,
                },
            )
            .await?;
        assert!(matches!(
            policy,
            UserContentRestrictionsCasResult::Updated(_)
        ));

        let catalog = super::CatalogService::new(
            database.clone(),
            crate::application::access::MediaAccessService::new(database.clone()),
        );
        let principal = AccessPrincipal::new(user.id, false);
        let recent = catalog.list_recently_added(principal, 0, 1).await?;
        assert_eq!(recent.total, 1);
        assert_eq!(recent.items.len(), 1);
        assert_eq!(recent.items[0].id, "recent-policy-visible");

        let home = crate::application::home::HomeService::new(
            catalog.clone(),
            LibraryService::new(database.clone()),
        );
        let snapshot = home.snapshot(principal, vec![library_id.clone()]).await?;
        assert_eq!(snapshot.recently_added.total, 1);
        assert_eq!(snapshot.recently_added.items.len(), 1);
        assert_eq!(snapshot.recently_added.items[0].id, "recent-policy-visible");
        assert_eq!(snapshot.latest_groups.len(), 1);
        assert_eq!(snapshot.latest_groups[0].1.len(), 1);
        assert_eq!(snapshot.latest_groups[0].1[0].id, "recent-policy-visible");

        let admin_page = catalog
            .list_recently_added(AccessPrincipal::new(user.id, true), 0, 1)
            .await?;
        assert_eq!(admin_page.total, 5);

        let unconfigured_user = UserStore::new(database.clone())?
            .create_user(
                "recent-policy-unconfigured",
                "Recent Policy Unconfigured",
                "test password",
                false,
            )
            .await?;
        database
            .set_user_library_access(&unconfigured_user.id.to_string(), &library_id, true)
            .await?;
        let unconfigured_principal = AccessPrincipal::new(unconfigured_user.id, false);
        let unconfigured_page = catalog
            .list_recently_added(unconfigured_principal, 0, 1)
            .await?;
        assert_eq!(unconfigured_page.total, 5);

        let unrestricted_policy = database
            .compare_and_swap_user_content_restrictions(
                &unconfigured_user.id.to_string(),
                0,
                NewUserContentRestrictions {
                    rating_system: "MPA",
                    maximum_rating: None,
                    allow_unrated: true,
                    allowed_tags: &[],
                    blocked_tags: &[],
                },
            )
            .await?;
        assert!(matches!(
            unrestricted_policy,
            UserContentRestrictionsCasResult::Updated(_)
        ));
        let unrestricted_page = catalog
            .list_recently_added(unconfigured_principal, 0, 1)
            .await?;
        assert_eq!(unrestricted_page.total, 5);
        Ok(())
    }
}
