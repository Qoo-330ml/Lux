use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use tokio::sync::{Mutex, Notify, mpsc};

use crate::application::{
    access::{AccessPrincipal, MediaAccessService},
    catalog::{CatalogError, CatalogItem, CatalogPage, CatalogService},
    libraries::{LibraryService, LibraryServiceError, LibraryView},
};

// Home queries can take several seconds on a large library. Keep a complete
// snapshot usable while a newer one is being prepared, and wait until a burst
// of scanner invalidations has gone quiet before refreshing it.
const HOME_USER_CACHE_TTL: Duration = Duration::from_secs(15);
const HOME_SHARED_CACHE_TTL: Duration = Duration::from_secs(60);
const HOME_REFRESH_DEBOUNCE: Duration = Duration::from_secs(2);

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
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
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
}

struct HomeCacheEntry {
    principal: AccessPrincipal,
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
    access: MediaAccessService,
    generation: AtomicU64,
    entries: Mutex<HashMap<HomeCacheKey, Arc<HomeCacheEntry>>>,
    shared: Mutex<Option<CachedSharedSnapshot>>,
    shared_compute_lock: Mutex<()>,
    refresh_tx: mpsc::Sender<()>,
    invalidation_notify: Notify,
}

#[derive(Clone)]
pub(crate) struct HomeService {
    inner: Arc<HomeServiceInner>,
}

impl HomeService {
    pub(crate) fn new(
        catalog: CatalogService,
        libraries: LibraryService,
        access: MediaAccessService,
    ) -> Self {
        let (refresh_tx, mut refresh_rx) = mpsc::channel(1);
        let inner = Arc::new(HomeServiceInner {
            catalog,
            libraries,
            access,
            generation: AtomicU64::new(0),
            entries: Mutex::new(HashMap::new()),
            shared: Mutex::new(None),
            shared_compute_lock: Mutex::new(()),
            refresh_tx,
            invalidation_notify: Notify::new(),
        });
        let worker_inner = Arc::downgrade(&inner);
        tokio::spawn(async move {
            while refresh_rx.recv().await.is_some() {
                // Reset the debounce window whenever another invalidation
                // arrives. A large scan can emit many small updates; doing a
                // full home calculation between those updates only competes
                // with the scan for the same database and I/O resources.
                while let Ok(Some(())) =
                    tokio::time::timeout(HOME_REFRESH_DEBOUNCE, refresh_rx.recv()).await
                {
                }
                let Some(inner) = worker_inner.upgrade() else {
                    break;
                };
                Self { inner }.refresh_cached_entries().await;
            }
        });
        let service = Self { inner };
        service.schedule_refresh();
        service
    }

    pub(crate) async fn snapshot(
        &self,
        principal: AccessPrincipal,
    ) -> Result<Arc<HomeSnapshot>, HomeError> {
        let key = HomeCacheKey {
            user_id: principal.user_id.to_string(),
            is_admin: principal.is_admin,
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
                // A complete old snapshot is preferable to making the user
                // wait for a database-wide recalculation. The refresh worker
                // will converge on the latest generation in the background.
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

        let snapshot = Arc::new(self.build_snapshot(principal).await?);
        let generation_changed = self.inner.generation.load(Ordering::Acquire) != generation;
        *entry.value.lock().await = Some(CachedSnapshot {
            generation,
            refreshed_at: Instant::now(),
            snapshot: snapshot.clone(),
        });
        if generation_changed {
            // Keep this complete snapshot available while a queued background
            // refresh calculates one for the newer generation.
            self.schedule_refresh();
        }
        Ok(snapshot)
    }

    pub(crate) fn invalidate(&self) {
        self.inner.generation.fetch_add(1, Ordering::AcqRel);
        self.inner.invalidation_notify.notify_waiters();
        self.schedule_refresh();
    }

    async fn entry(&self, key: HomeCacheKey, principal: AccessPrincipal) -> Arc<HomeCacheEntry> {
        let mut entries = self.inner.entries.lock().await;
        entries
            .entry(key)
            .or_insert_with(|| {
                Arc::new(HomeCacheEntry {
                    principal,
                    value: Mutex::new(None),
                    compute_lock: Mutex::new(()),
                })
            })
            .clone()
    }

    fn schedule_refresh(&self) {
        let _ = self.inner.refresh_tx.try_send(());
    }

    async fn prewarm(&self) -> Result<(), HomeError> {
        self.refresh_shared_cache().await.map(|_| ())
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
                // A scan may invalidate the shared snapshot repeatedly. The
                // foreground request should use the last complete snapshot
                // while the worker converges on the latest generation.
                self.schedule_refresh();
                return Ok(cached.snapshot.clone());
            }
        }

        // No snapshot exists yet (for example immediately after startup), so
        // wait for one complete calculation. If a scan invalidates it while
        // it is being built, refresh_shared_cache still returns that complete
        // snapshot for this foreground request and queues a newer refresh.
        self.refresh_shared_cache().await
    }

    async fn refresh_shared_cache(&self) -> Result<Arc<HomeSharedSnapshot>, HomeError> {
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

        let snapshot = Arc::new(self.build_shared_snapshot().await?);
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
        if let Err(error) = self.prewarm().await {
            tracing::warn!(%error, "failed to prewarm shared home cache");
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
            {
                let cached = entry.value.lock().await;
                if cached.as_ref().is_some_and(|cached| {
                    cached.generation == generation
                        && cached.refreshed_at.elapsed() < HOME_USER_CACHE_TTL
                }) {
                    continue;
                }
            }
            let notified = self.inner.invalidation_notify.notified();
            let result = tokio::select! {
                result = self.build_snapshot(entry.principal) => Some(result),
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
                Some(Err(_)) => {}
            }
        }
    }

    async fn build_snapshot(&self, principal: AccessPrincipal) -> Result<HomeSnapshot, HomeError> {
        let (shared_result, accessible_library_ids_result) = tokio::join!(
            self.shared_snapshot(),
            self.inner.access.accessible_library_ids(principal),
        );
        let shared = shared_result?;
        let accessible_library_ids = accessible_library_ids_result.map_err(CatalogError::from)?;
        let user_id = principal.user_id.to_string();
        let (continue_watching_result, recently_added_result, recommended_result) = tokio::join!(
            self.inner.catalog.list_continue_watching_for_library_ids(
                &accessible_library_ids,
                &user_id,
                0,
                10,
            ),
            self.inner
                .catalog
                .list_recently_added_for_library_ids(&accessible_library_ids, 0, 12),
            self.inner.catalog.list_recommended_for_library_ids(
                &accessible_library_ids,
                &user_id,
                12,
            ),
        );
        let accessible_library_ids = accessible_library_ids.into_iter().collect::<HashSet<_>>();
        let views = shared
            .views
            .iter()
            .filter(|view| {
                view.library.is_enabled
                    && accessible_library_ids.contains(&view.library.id.to_string())
            })
            .cloned()
            .collect();
        let latest_groups = shared
            .latest_groups
            .iter()
            .filter(|(library_id, _)| accessible_library_ids.contains(library_id))
            .cloned()
            .collect();

        Ok(HomeSnapshot {
            continue_watching: continue_watching_result?,
            recently_added: recently_added_result?,
            recommended: recommended_result?,
            latest_groups,
            views,
        })
    }

    async fn build_shared_snapshot(&self) -> Result<HomeSharedSnapshot, HomeError> {
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
        Ok(HomeSharedSnapshot {
            latest_groups,
            views: views
                .into_iter()
                .filter(|view| view.library.is_enabled)
                .collect(),
        })
    }

    #[cfg(test)]
    async fn shared_cache_ready(&self) -> bool {
        let generation = self.inner.generation.load(Ordering::Acquire);
        self.inner
            .shared
            .lock()
            .await
            .as_ref()
            .is_some_and(|cached| cached.generation == generation)
    }
}

#[cfg(test)]
mod tests {
    use super::{HOME_REFRESH_DEBOUNCE, HomeService};
    use crate::{
        application::{
            access::MediaAccessService, catalog::CatalogService, libraries::LibraryService,
        },
        config::Config,
        storage::Database,
    };

    #[tokio::test]
    async fn empty_home_snapshot_is_served_while_invalidation_refreshes() {
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
            access,
        );
        let principal = crate::application::access::AccessPrincipal::new(
            crate::domain::ids::UserId::new(),
            true,
        );

        let first = home.snapshot(principal).await.expect("first snapshot");
        let second = home.snapshot(principal).await.expect("cached snapshot");
        assert!(std::ptr::eq(first.as_ref(), second.as_ref()));

        home.invalidate();
        let stale = home.snapshot(principal).await.expect("stale snapshot");
        assert!(std::ptr::eq(first.as_ref(), stale.as_ref()));

        tokio::time::sleep(HOME_REFRESH_DEBOUNCE + std::time::Duration::from_millis(100)).await;
        let refreshed = home.snapshot(principal).await.expect("refreshed snapshot");
        assert!(!std::ptr::eq(first.as_ref(), refreshed.as_ref()));
    }

    #[tokio::test]
    async fn invalidation_prewarms_shared_home_without_a_user_entry() {
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
            access,
        );

        home.invalidate();
        tokio::time::sleep(HOME_REFRESH_DEBOUNCE + std::time::Duration::from_millis(50)).await;

        assert!(home.shared_cache_ready().await);
    }
}
