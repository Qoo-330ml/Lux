use std::{
    collections::HashMap,
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

const HOME_CACHE_TTL: Duration = Duration::from_secs(5);
const HOME_REFRESH_DEBOUNCE: Duration = Duration::from_millis(300);

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

struct HomeServiceInner {
    catalog: CatalogService,
    libraries: LibraryService,
    access: MediaAccessService,
    generation: AtomicU64,
    entries: Mutex<HashMap<HomeCacheKey, Arc<HomeCacheEntry>>>,
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
            refresh_tx,
            invalidation_notify: Notify::new(),
        });
        let worker_inner = Arc::downgrade(&inner);
        tokio::spawn(async move {
            while refresh_rx.recv().await.is_some() {
                tokio::time::sleep(HOME_REFRESH_DEBOUNCE).await;
                while refresh_rx.try_recv().is_ok() {}
                let Some(inner) = worker_inner.upgrade() else {
                    break;
                };
                Self { inner }.refresh_cached_entries().await;
            }
        });
        Self { inner }
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
            if let Some(cached) = cached.as_ref()
                && cached.generation == generation
            {
                if cached.refreshed_at.elapsed() >= HOME_CACHE_TTL {
                    self.schedule_refresh();
                }
                return Ok(cached.snapshot.clone());
            }
        }

        let _compute_guard = entry.compute_lock.lock().await;
        let generation = self.inner.generation.load(Ordering::Acquire);
        {
            let cached = entry.value.lock().await;
            if let Some(cached) = cached.as_ref()
                && cached.generation == generation
                && cached.refreshed_at.elapsed() < HOME_CACHE_TTL
            {
                return Ok(cached.snapshot.clone());
            }
        }

        let snapshot = Arc::new(self.build_snapshot(principal).await?);
        if self.inner.generation.load(Ordering::Acquire) == generation {
            *entry.value.lock().await = Some(CachedSnapshot {
                generation,
                refreshed_at: Instant::now(),
                snapshot: snapshot.clone(),
            });
        } else {
            // The foreground request is allowed to finish. A queued background
            // refresh will calculate a snapshot for the newer generation.
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

    async fn refresh_cached_entries(&self) {
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
                        && cached.refreshed_at.elapsed() < HOME_CACHE_TTL
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
        let accessible_library_ids = self
            .inner
            .access
            .accessible_library_ids(principal)
            .await
            .map_err(CatalogError::from)?;
        let user_id = principal.user_id.to_string();
        let (
            continue_watching_result,
            recently_added_result,
            recommended_result,
            latest_groups_result,
            views_result,
        ) = tokio::join!(
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
            self.inner
                .catalog
                .list_recently_added_by_library_ids(&accessible_library_ids, 12),
            self.inner.libraries.list_libraries(),
        );
        let accessible_library_ids = accessible_library_ids
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        let views = views_result?
            .into_iter()
            .filter(|view| {
                view.library.is_enabled
                    && accessible_library_ids.contains(&view.library.id.to_string())
            })
            .collect();

        Ok(HomeSnapshot {
            continue_watching: continue_watching_result?,
            recently_added: recently_added_result?,
            recommended: recommended_result?,
            latest_groups: latest_groups_result?,
            views,
        })
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
    async fn empty_home_snapshot_is_cached_and_invalidated() {
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
        tokio::time::sleep(HOME_REFRESH_DEBOUNCE + std::time::Duration::from_millis(50)).await;
        let third = home.snapshot(principal).await.expect("refreshed snapshot");
        assert!(!std::ptr::eq(first.as_ref(), third.as_ref()));
    }
}
