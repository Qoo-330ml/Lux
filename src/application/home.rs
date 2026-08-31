use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use tokio::sync::{Mutex, Notify, mpsc};

use crate::application::{
    access::AccessPrincipal,
    catalog::{CatalogError, CatalogItem, CatalogPage, CatalogService},
    libraries::{LibraryService, LibraryServiceError, LibraryView},
};

const HOME_USER_CACHE_TTL: Duration = Duration::from_secs(15);
const HOME_SHARED_CACHE_TTL: Duration = Duration::from_secs(60);
const HOME_REFRESH_DEBOUNCE: Duration = Duration::from_secs(2);
const MAX_HOME_CACHE_ENTRIES: usize = 256;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HomeSnapshot {
    pub(crate) continue_watching: CatalogPage,
    pub(crate) recently_added: CatalogPage,
    pub(crate) recommended: Vec<CatalogItem>,
    pub(crate) latest_groups: Vec<(String, Vec<CatalogItem>)>,
    pub(crate) views: Vec<LibraryView>,
}

#[derive(Debug)]
pub(crate) enum HomeError {
    Catalog(CatalogError),
    Libraries(LibraryServiceError),
}

impl fmt::Display for HomeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Catalog(error) => error.fmt(formatter),
            Self::Libraries(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for HomeError {}

impl From<CatalogError> for HomeError {
    fn from(error: CatalogError) -> Self {
        Self::Catalog(error)
    }
}

impl From<LibraryServiceError> for HomeError {
    fn from(error: LibraryServiceError) -> Self {
        Self::Libraries(error)
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct HomeCacheKey {
    user_id: String,
    is_admin: bool,
    library_ids: Vec<String>,
}

struct HomeCacheEntry {
    principal: AccessPrincipal,
    library_ids: Vec<String>,
    value: Mutex<Option<CachedSnapshot>>,
    compute_lock: Mutex<()>,
}

struct CachedSnapshot {
    generation: u64,
    refreshed_at: Instant,
    snapshot: Arc<HomeSnapshot>,
}

struct HomeSharedSnapshot {
    latest_groups: Vec<(String, Vec<CatalogItem>)>,
    views: Vec<LibraryView>,
}

struct CachedSharedSnapshot {
    generation: u64,
    refreshed_at: Instant,
    snapshot: Arc<HomeSharedSnapshot>,
}

struct HomeServiceInner {
    catalog: CatalogService,
    libraries: LibraryService,
    generation: AtomicU64,
    entries: Mutex<HashMap<HomeCacheKey, Arc<HomeCacheEntry>>>,
    shared: Mutex<Option<CachedSharedSnapshot>>,
    shared_compute_lock: Mutex<()>,
    refresh_tx: mpsc::Sender<()>,
    refresh_pending: AtomicBool,
    invalidation_notify: Notify,
}

#[derive(Clone)]
pub(crate) struct HomeService {
    inner: Arc<HomeServiceInner>,
}

impl HomeService {
    pub(crate) fn new(catalog: CatalogService, libraries: LibraryService) -> Self {
        let (refresh_tx, mut refresh_rx) = mpsc::channel(1);
        let inner = Arc::new(HomeServiceInner {
            catalog,
            libraries,
            generation: AtomicU64::new(0),
            shared: Mutex::new(None),
            entries: Mutex::new(HashMap::new()),
            shared_compute_lock: Mutex::new(()),
            refresh_tx,
            refresh_pending: AtomicBool::new(false),
            invalidation_notify: Notify::new(),
        });
        let worker_inner = Arc::downgrade(&inner);
        tokio::spawn(async move {
            while refresh_rx.recv().await.is_some() {
                while let Ok(Some(())) =
                    tokio::time::timeout(HOME_REFRESH_DEBOUNCE, refresh_rx.recv()).await
                {
                }
                let Some(inner) = worker_inner.upgrade() else {
                    break;
                };
                inner.refresh_pending.store(false, Ordering::Release);
                (Self { inner }).refresh_cached_entries().await;
            }
        });
        let service = Self { inner };
        service.schedule_refresh();
        service
    }

    pub(crate) async fn snapshot(
        &self,
        principal: AccessPrincipal,
        mut library_ids: Vec<String>,
    ) -> Result<Arc<HomeSnapshot>, HomeError> {
        library_ids.sort_unstable();
        library_ids.dedup();
        let key = HomeCacheKey {
            user_id: principal.user_id.to_string(),
            is_admin: principal.is_admin,
            library_ids: library_ids.clone(),
        };
        let entry = self.entry(key, principal).await;
        let generation = self.inner.generation.load(Ordering::Acquire);
        {
            let cached = entry.value.lock().await;
            if let Some(cached) = cached.as_ref() {
                if cached.generation != generation
                    || cached.refreshed_at.elapsed() >= HOME_USER_CACHE_TTL
                {
                    self.schedule_refresh();
                }
                return Ok(cached.snapshot.clone());
            }
        }

        let _compute_guard = entry.compute_lock.lock().await;
        let generation = self.inner.generation.load(Ordering::Acquire);
        {
            let cached = entry.value.lock().await;
            if let Some(cached) = cached.as_ref() {
                if cached.generation != generation
                    || cached.refreshed_at.elapsed() >= HOME_USER_CACHE_TTL
                {
                    self.schedule_refresh();
                }
                return Ok(cached.snapshot.clone());
            }
        }

        let snapshot = Arc::new(self.build_snapshot(principal, &library_ids).await?);
        *entry.value.lock().await = Some(CachedSnapshot {
            generation,
            refreshed_at: Instant::now(),
            snapshot: snapshot.clone(),
        });
        Ok(snapshot)
    }

    pub(crate) fn invalidate(&self) {
        self.inner.generation.fetch_add(1, Ordering::AcqRel);
        self.inner.catalog.invalidate_library_pages();
        self.inner.invalidation_notify.notify_waiters();
        self.schedule_refresh();
    }

    fn schedule_refresh(&self) {
        if self.inner.refresh_pending.swap(true, Ordering::AcqRel) {
            return;
        }
        if self.inner.refresh_tx.try_send(()).is_err() {
            self.inner.refresh_pending.store(false, Ordering::Release);
        }
    }

    async fn entry(&self, key: HomeCacheKey, principal: AccessPrincipal) -> Arc<HomeCacheEntry> {
        let mut entries = self.inner.entries.lock().await;
        if !entries.contains_key(&key) && entries.len() >= MAX_HOME_CACHE_ENTRIES {
            entries.clear();
        }
        let library_ids = key.library_ids.clone();
        entries
            .entry(key)
            .or_insert_with(|| {
                Arc::new(HomeCacheEntry {
                    principal,
                    library_ids,
                    value: Mutex::new(None),
                    compute_lock: Mutex::new(()),
                })
            })
            .clone()
    }

    async fn refresh_shared_snapshot(&self) -> Result<Arc<HomeSharedSnapshot>, HomeError> {
        let _compute_guard = self.inner.shared_compute_lock.lock().await;
        let generation = self.inner.generation.load(Ordering::Acquire);
        {
            let cached = self.inner.shared.lock().await;
            if let Some(cached) = cached.as_ref()
                && cached.generation == generation
                && cached.refreshed_at.elapsed() < HOME_SHARED_CACHE_TTL
            {
                return Ok(cached.snapshot.clone());
            }
        }

        let views = self.inner.libraries.list_libraries().await?;
        let enabled_library_ids = views
            .iter()
            .filter(|view| view.library.is_enabled)
            .map(|view| view.library.id.to_string())
            .collect::<Vec<_>>();
        let latest_groups = self
            .inner
            .catalog
            .list_recently_added_by_library_ids(&enabled_library_ids, 12)
            .await?;
        let snapshot = Arc::new(HomeSharedSnapshot {
            latest_groups,
            views: views
                .into_iter()
                .filter(|view| view.library.is_enabled)
                .collect(),
        });
        let generation_changed = self.inner.generation.load(Ordering::Acquire) != generation;
        *self.inner.shared.lock().await = Some(CachedSharedSnapshot {
            generation,
            refreshed_at: Instant::now(),
            snapshot: snapshot.clone(),
        });
        if generation_changed {
            self.schedule_refresh();
        }
        Ok(snapshot)
    }

    async fn refresh_cached_entries(&self) {
        if let Err(error) = self.refresh_shared_snapshot().await {
            tracing::warn!(%error, "failed to refresh shared home snapshot");
        }
        let entries = self
            .inner
            .entries
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for entry in entries {
            let Ok(_compute_guard) = entry.compute_lock.try_lock() else {
                continue;
            };
            let generation = self.inner.generation.load(Ordering::Acquire);
            let cached = entry.value.lock().await;
            if cached.as_ref().is_some_and(|cached| {
                cached.generation == generation
                    && cached.refreshed_at.elapsed() < HOME_USER_CACHE_TTL
            }) {
                continue;
            }
            drop(cached);
            let notified = self.inner.invalidation_notify.notified();
            let result = tokio::select! {
                result = self.build_snapshot(entry.principal, &entry.library_ids) => Some(result),
                _ = notified => None,
            };
            match result {
                Some(Ok(snapshot))
                    if self.inner.generation.load(Ordering::Acquire) == generation =>
                {
                    *entry.value.lock().await = Some(CachedSnapshot {
                        generation,
                        refreshed_at: Instant::now(),
                        snapshot: Arc::new(snapshot),
                    });
                }
                Some(Ok(_)) | None => self.schedule_refresh(),
                Some(Err(error)) => tracing::debug!(%error, "home cache refresh failed"),
            }
        }
    }

    async fn build_snapshot(
        &self,
        principal: AccessPrincipal,
        accessible_library_ids: &[String],
    ) -> Result<HomeSnapshot, HomeError> {
        let shared = self.shared_snapshot().await?;
        let user_id = principal.user_id.to_string();
        let (continue_watching, recently_added, recommended, views) = tokio::try_join!(
            async {
                self.inner
                    .catalog
                    .list_continue_watching_for_library_ids(accessible_library_ids, &user_id, 0, 10)
                    .await
                    .map_err(HomeError::Catalog)
            },
            async {
                self.inner
                    .catalog
                    .list_recently_added_for_library_ids(accessible_library_ids, 0, 12)
                    .await
                    .map_err(HomeError::Catalog)
            },
            async {
                self.inner
                    .catalog
                    .list_recommended_for_library_ids(accessible_library_ids, &user_id, 7)
                    .await
                    .map_err(HomeError::Catalog)
            },
            async {
                self.inner
                    .libraries
                    .order_views_for_user(&user_id, accessible_library_ids, shared.views.clone())
                    .await
                    .map_err(HomeError::Libraries)
            },
        )?;
        let accessible_library_ids = accessible_library_ids
            .iter()
            .cloned()
            .collect::<HashSet<_>>();
        let latest_groups = shared
            .latest_groups
            .iter()
            .filter(|(library_id, _)| accessible_library_ids.contains(library_id))
            .cloned()
            .collect();
        Ok(HomeSnapshot {
            continue_watching,
            recently_added,
            recommended,
            latest_groups,
            views,
        })
    }

    async fn shared_snapshot(&self) -> Result<Arc<HomeSharedSnapshot>, HomeError> {
        let generation = self.inner.generation.load(Ordering::Acquire);
        {
            let cached = self.inner.shared.lock().await;
            if let Some(cached) = cached.as_ref() {
                if cached.generation == generation {
                    if cached.refreshed_at.elapsed() >= HOME_SHARED_CACHE_TTL {
                        self.schedule_refresh();
                    }
                    return Ok(cached.snapshot.clone());
                }
                self.schedule_refresh();
                return Ok(cached.snapshot.clone());
            }
        }
        self.refresh_shared_snapshot().await
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::HomeService;
    use crate::{
        application::{
            access::{AccessPrincipal, MediaAccessService},
            catalog::CatalogService,
            libraries::LibraryService,
        },
        config::Config,
        domain::ids::UserId,
        storage::Database,
    };

    #[tokio::test]
    async fn shared_home_snapshot_stays_available_during_refresh() {
        let temp_dir = tempfile::tempdir().expect("temporary directory should be available");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("test address"),
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await.expect("database");
        let access = MediaAccessService::new(database.clone());
        let home = HomeService::new(
            CatalogService::new(database.clone(), access.clone()),
            LibraryService::new(database),
        );

        let first = home.shared_snapshot().await.expect("first snapshot");
        home.invalidate();
        let stale = home.shared_snapshot().await.expect("stale snapshot");

        assert!(std::ptr::eq(first.as_ref(), stale.as_ref()));

        tokio::time::sleep(Duration::from_secs(2) + Duration::from_millis(100)).await;
        let refreshed = home.shared_snapshot().await.expect("refreshed snapshot");
        assert!(!std::ptr::eq(first.as_ref(), refreshed.as_ref()));
    }

    #[tokio::test]
    async fn user_home_snapshot_cache_is_reused_but_isolated_by_principal() {
        let temp_dir = tempfile::tempdir().expect("temporary directory should be available");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("test address"),
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await.expect("database");
        let access = MediaAccessService::new(database.clone());
        let home = HomeService::new(
            CatalogService::new(database.clone(), access.clone()),
            LibraryService::new(database),
        );
        let first_user = AccessPrincipal::new(UserId::new(), false);
        let second_user = AccessPrincipal::new(UserId::new(), false);

        let first = home
            .snapshot(first_user, Vec::new())
            .await
            .expect("first user snapshot");
        let reused = home
            .snapshot(first_user, Vec::new())
            .await
            .expect("reused user snapshot");
        let isolated = home
            .snapshot(second_user, Vec::new())
            .await
            .expect("second user snapshot");

        assert!(std::ptr::eq(first.as_ref(), reused.as_ref()));
        assert!(!std::ptr::eq(first.as_ref(), isolated.as_ref()));
    }
}
