use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use tokio::sync::{Mutex, mpsc};

use super::{
    catalog::{CatalogError, CatalogItem, CatalogService},
    libraries::{LibraryService, LibraryServiceError, LibraryView},
};

const HOME_SHARED_CACHE_TTL: Duration = Duration::from_secs(60);
const HOME_REFRESH_DEBOUNCE: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HomeSharedSnapshot {
    pub(crate) latest_groups: Vec<(String, Vec<CatalogItem>)>,
    pub(crate) views: Vec<LibraryView>,
}

#[derive(Debug)]
pub(crate) enum HomeError {
    Catalog(CatalogError),
    Libraries(LibraryServiceError),
}

impl std::fmt::Display for HomeError {
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

struct CachedSharedSnapshot {
    generation: u64,
    refreshed_at: Instant,
    snapshot: Arc<HomeSharedSnapshot>,
}

struct HomeServiceInner {
    catalog: CatalogService,
    libraries: LibraryService,
    generation: AtomicU64,
    shared: Mutex<Option<CachedSharedSnapshot>>,
    compute_lock: Mutex<()>,
    refresh_tx: mpsc::Sender<()>,
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
            compute_lock: Mutex::new(()),
            refresh_tx,
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
                if let Err(error) = (Self { inner }).refresh_shared_snapshot().await {
                    tracing::warn!(%error, "failed to refresh shared home snapshot");
                }
            }
        });
        let service = Self { inner };
        service.schedule_refresh();
        service
    }

    pub(crate) async fn shared_snapshot(&self) -> Result<Arc<HomeSharedSnapshot>, HomeError> {
        let generation = self.inner.generation.load(Ordering::Acquire);
        {
            let cached = self.inner.shared.lock().await;
            if let Some(cached) = cached.as_ref() {
                if cached.generation == generation
                    && cached.refreshed_at.elapsed() < HOME_SHARED_CACHE_TTL
                {
                    return Ok(cached.snapshot.clone());
                }
                self.schedule_refresh();
                return Ok(cached.snapshot.clone());
            }
        }

        self.refresh_shared_snapshot().await
    }

    pub(crate) fn invalidate(&self) {
        self.inner.generation.fetch_add(1, Ordering::AcqRel);
        self.schedule_refresh();
    }

    fn schedule_refresh(&self) {
        let _ = self.inner.refresh_tx.try_send(());
    }

    async fn refresh_shared_snapshot(&self) -> Result<Arc<HomeSharedSnapshot>, HomeError> {
        let _compute_guard = self.inner.compute_lock.lock().await;
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
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::HomeService;
    use crate::{
        application::{
            access::MediaAccessService, catalog::CatalogService, libraries::LibraryService,
        },
        config::Config,
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
            CatalogService::new(database.clone(), access),
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
}
