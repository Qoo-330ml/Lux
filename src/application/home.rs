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
const HOME_INVALIDATION_DEBOUNCE: Duration = Duration::from_millis(100);
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
    snapshot: Arc<CachedHomeSnapshot>,
}

struct CachedHomeSnapshot {
    recently_added: CatalogPage,
    recommended: Vec<CatalogItem>,
    latest_groups: Vec<(String, Vec<CatalogItem>)>,
    views: Vec<LibraryView>,
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

#[derive(Default)]
struct ScanInvalidationState {
    scheduled: bool,
    dirty: bool,
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
    scan_invalidation: Mutex<ScanInvalidationState>,
    scan_refresh_epoch: AtomicU64,
    invalidation_debounce_pending: AtomicBool,
    #[cfg(test)]
    invalidation_notification_count: AtomicU64,
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
            scan_invalidation: Mutex::new(ScanInvalidationState::default()),
            scan_refresh_epoch: AtomicU64::new(0),
            invalidation_debounce_pending: AtomicBool::new(false),
            #[cfg(test)]
            invalidation_notification_count: AtomicU64::new(0),
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
                (Self { inner }).refresh_cached_entries(false).await;
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
        let cached = self.cached_snapshot(principal, &library_ids).await?;
        let user_id = principal.user_id.to_string();
        let continue_watching = self
            .inner
            .catalog
            .list_home_continue_watching_for_library_ids(&library_ids, &user_id, 0, 10)
            .await
            .map_err(HomeError::Catalog)?;
        Ok(Arc::new(HomeSnapshot {
            continue_watching,
            recently_added: cached.recently_added.clone(),
            recommended: cached.recommended.clone(),
            latest_groups: cached.latest_groups.clone(),
            views: cached.views.clone(),
        }))
    }

    async fn cached_snapshot(
        &self,
        principal: AccessPrincipal,
        library_ids: &[String],
    ) -> Result<Arc<CachedHomeSnapshot>, HomeError> {
        let key = HomeCacheKey {
            user_id: principal.user_id.to_string(),
            is_admin: principal.is_admin,
            library_ids: library_ids.to_vec(),
        };
        let entry = self.entry(key, principal).await;
        loop {
            let generation = self.inner.generation.load(Ordering::Acquire);
            {
                let cached = entry.value.lock().await;
                if let Some(cached) = cached.as_ref()
                    && cached.generation == generation
                {
                    if cached.refreshed_at.elapsed() >= HOME_USER_CACHE_TTL {
                        self.schedule_refresh();
                    }
                    return Ok(cached.snapshot.clone());
                }
            }

            let _compute_guard = entry.compute_lock.lock().await;
            let generation = self.inner.generation.load(Ordering::Acquire);
            let scan_refresh_epoch = self.inner.scan_refresh_epoch.load(Ordering::Acquire);
            {
                let cached = entry.value.lock().await;
                if let Some(cached) = cached.as_ref()
                    && cached.generation == generation
                {
                    if cached.refreshed_at.elapsed() >= HOME_USER_CACHE_TTL {
                        self.schedule_refresh();
                    }
                    return Ok(cached.snapshot.clone());
                }
            }

            let snapshot = Arc::new(
                self.build_cached_snapshot(principal, library_ids, true)
                    .await?,
            );
            if self.inner.generation.load(Ordering::Acquire) != generation
                || self.inner.scan_refresh_epoch.load(Ordering::Acquire) != scan_refresh_epoch
            {
                continue;
            }
            *entry.value.lock().await = Some(CachedSnapshot {
                generation,
                refreshed_at: Instant::now(),
                snapshot: snapshot.clone(),
            });
            return Ok(snapshot);
        }
    }

    pub(crate) fn invalidate(&self) {
        Self::invalidate_now(&self.inner);
    }

    fn invalidate_now(inner: &Arc<HomeServiceInner>) {
        // The generation and catalog cache are invalidated synchronously so a
        // snapshot requested immediately after a change cannot reuse stale data.
        // Only the refresh-worker wakeup is coalesced for a short burst.
        inner.generation.fetch_add(1, Ordering::AcqRel);
        inner.catalog.invalidate_library_pages();
        if inner
            .invalidation_debounce_pending
            .swap(true, Ordering::AcqRel)
        {
            return;
        }

        #[cfg(test)]
        inner
            .invalidation_notification_count
            .fetch_add(1, Ordering::Relaxed);
        inner.invalidation_notify.notify_waiters();
        (HomeService {
            inner: Arc::clone(inner),
        })
        .schedule_refresh();
        let first_generation = inner.generation.load(Ordering::Acquire);
        let weak_inner = Arc::downgrade(inner);
        tokio::spawn(async move {
            tokio::time::sleep(HOME_INVALIDATION_DEBOUNCE).await;
            let Some(inner) = weak_inner.upgrade() else {
                return;
            };
            let generation_changed = inner.generation.load(Ordering::Acquire) != first_generation;
            inner
                .invalidation_debounce_pending
                .store(false, Ordering::Release);
            if !generation_changed {
                return;
            }
            #[cfg(test)]
            inner
                .invalidation_notification_count
                .fetch_add(1, Ordering::Relaxed);
            inner.invalidation_notify.notify_waiters();
            (HomeService { inner }).schedule_refresh();
        });
    }

    pub(crate) async fn invalidate_scan_batch(&self) {
        let mut state = self.inner.scan_invalidation.lock().await;
        state.scheduled = true;
        state.dirty = true;
        drop(state);
        self.inner.catalog.invalidate_library_pages();
    }

    pub(crate) async fn flush_scan_invalidation(&self) {
        let should_refresh = {
            let mut state = self.inner.scan_invalidation.lock().await;
            if !state.scheduled {
                false
            } else {
                let dirty = state.dirty;
                state.scheduled = false;
                state.dirty = false;
                dirty
            }
        };
        if should_refresh {
            self.inner.scan_refresh_epoch.fetch_add(1, Ordering::AcqRel);
            self.refresh_cached_entries(true).await;
        }
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

    async fn refresh_shared_snapshot(
        &self,
        force: bool,
    ) -> Result<Arc<HomeSharedSnapshot>, HomeError> {
        let _compute_guard = self.inner.shared_compute_lock.lock().await;
        let generation = self.inner.generation.load(Ordering::Acquire);
        {
            let cached = self.inner.shared.lock().await;
            if !force
                && let Some(cached) = cached.as_ref()
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

    async fn refresh_cached_entries(&self, force: bool) {
        if let Err(error) = self.refresh_shared_snapshot(force).await {
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
            let compute_guard = if force {
                Some(entry.compute_lock.lock().await)
            } else {
                entry.compute_lock.try_lock().ok()
            };
            let Some(_compute_guard) = compute_guard else {
                continue;
            };
            let generation = self.inner.generation.load(Ordering::Acquire);
            let scan_refresh_epoch = self.inner.scan_refresh_epoch.load(Ordering::Acquire);
            let cached = entry.value.lock().await;
            if !force
                && cached.as_ref().is_some_and(|cached| {
                    cached.generation == generation
                        && cached.refreshed_at.elapsed() < HOME_USER_CACHE_TTL
                })
            {
                continue;
            }
            drop(cached);
            let notified = self.inner.invalidation_notify.notified();
            let result = tokio::select! {
                result = self.build_cached_snapshot(entry.principal, &entry.library_ids, false) => Some(result),
                _ = notified => None,
            };
            match result {
                Some(Ok(snapshot))
                    if self.inner.generation.load(Ordering::Acquire) == generation
                        && self.inner.scan_refresh_epoch.load(Ordering::Acquire)
                            == scan_refresh_epoch =>
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

    async fn build_cached_snapshot(
        &self,
        principal: AccessPrincipal,
        accessible_library_ids: &[String],
        require_fresh_shared: bool,
    ) -> Result<CachedHomeSnapshot, HomeError> {
        let shared = if require_fresh_shared {
            self.refresh_shared_snapshot(false).await?
        } else {
            self.shared_snapshot().await?
        };
        let user_id = principal.user_id.to_string();
        let (recently_added, recommended, views) = tokio::try_join!(
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
                    .order_views_for_user(
                        &user_id,
                        principal.is_admin,
                        accessible_library_ids,
                        shared.views.clone(),
                    )
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
        Ok(CachedHomeSnapshot {
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
        self.refresh_shared_snapshot(false).await
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::atomic::Ordering, time::Duration};

    use super::{HOME_INVALIDATION_DEBOUNCE, HomeService};
    use crate::{
        application::{
            access::{AccessPrincipal, MediaAccessService},
            catalog::CatalogService,
            libraries::LibraryService,
            scanner::LibraryScanner,
            setup::SetupService,
        },
        config::Config,
        domain::ids::UserId,
        library::LibraryKind,
        storage::{Database, NewPlaybackEvent},
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
            .cached_snapshot(first_user, &[])
            .await
            .expect("first user snapshot");
        let reused = home
            .cached_snapshot(first_user, &[])
            .await
            .expect("reused user snapshot");
        let isolated = home
            .cached_snapshot(second_user, &[])
            .await
            .expect("second user snapshot");

        assert!(std::ptr::eq(first.as_ref(), reused.as_ref()));
        assert!(!std::ptr::eq(first.as_ref(), isolated.as_ref()));
    }

    #[tokio::test]
    async fn invalidation_rebuilds_user_home_snapshot_before_returning_it() {
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
        let principal = AccessPrincipal::new(UserId::new(), false);

        let first = home
            .cached_snapshot(principal, &[])
            .await
            .expect("first user snapshot");
        home.invalidate();
        let refreshed = home
            .cached_snapshot(principal, &[])
            .await
            .expect("invalidated user snapshot");

        assert!(!std::ptr::eq(first.as_ref(), refreshed.as_ref()));
    }

    #[tokio::test]
    async fn scan_invalidations_keep_user_snapshot_until_the_final_flush() {
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
        let principal = AccessPrincipal::new(UserId::new(), false);
        let first = home
            .cached_snapshot(principal, &[])
            .await
            .expect("initial user snapshot");

        home.invalidate_scan_batch().await;
        assert_eq!(home.inner.generation.load(Ordering::Acquire), 0);

        let during_scan = home
            .cached_snapshot(principal, &[])
            .await
            .expect("user snapshot during scan");
        assert!(std::ptr::eq(first.as_ref(), during_scan.as_ref()));

        home.invalidate_scan_batch().await;
        assert_eq!(home.inner.generation.load(Ordering::Acquire), 0);

        home.flush_scan_invalidation().await;
        assert_eq!(home.inner.generation.load(Ordering::Acquire), 0);

        let after_scan = home
            .cached_snapshot(principal, &[])
            .await
            .expect("user snapshot after scan");
        assert!(!std::ptr::eq(first.as_ref(), after_scan.as_ref()));

        home.flush_scan_invalidation().await;
        assert_eq!(home.inner.generation.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn rapid_invalidations_share_a_short_notification_window() {
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

        home.invalidate();
        home.invalidate();

        assert_eq!(home.inner.generation.load(Ordering::Acquire), 2);
        assert_eq!(
            home.inner
                .invalidation_notification_count
                .load(Ordering::Relaxed),
            1
        );
        assert!(
            home.inner
                .invalidation_debounce_pending
                .load(Ordering::Acquire)
        );

        tokio::time::sleep(HOME_INVALIDATION_DEBOUNCE + Duration::from_millis(25)).await;
        assert!(
            !home
                .inner
                .invalidation_debounce_pending
                .load(Ordering::Acquire)
        );
        assert_eq!(
            home.inner
                .invalidation_notification_count
                .load(Ordering::Relaxed),
            2
        );
    }

    #[tokio::test]
    async fn continue_watching_is_read_fresh_on_cached_home_snapshot() {
        let temp_dir = tempfile::tempdir().expect("temporary directory should be available");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("test address"),
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await.expect("database");
        let setup = SetupService::new(database.clone()).expect("setup service");
        let admin = setup
            .complete("Admin", "Admin", "correct password")
            .await
            .expect("admin user");
        let access = MediaAccessService::new(database.clone());
        let libraries = LibraryService::new(database.clone());
        let library = libraries
            .create_library("Movies", LibraryKind::Movie, false)
            .await
            .expect("movie library");
        let root = temp_dir.path().join("Movies");
        tokio::fs::create_dir_all(&root).await.expect("movie root");
        tokio::fs::write(root.join("Fresh Resume Movie 2024.mkv"), b"video")
            .await
            .expect("movie file");
        libraries
            .add_root(library.id, root.to_str().expect("utf8 movie root"))
            .await
            .expect("movie root registration");
        LibraryScanner::new(database.clone())
            .scan_movie_library(library.id)
            .await
            .expect("movie scan");
        let item_id: String =
            sqlx::query_scalar("SELECT id FROM media_items WHERE title = 'Fresh Resume Movie'")
                .fetch_one(database.pool())
                .await
                .expect("scanned movie");
        let source_id: String =
            sqlx::query_scalar("SELECT id FROM media_sources WHERE item_id = ?")
                .bind(&item_id)
                .fetch_one(database.pool())
                .await
                .expect("movie source");
        sqlx::query("UPDATE media_sources SET duration_ticks = ? WHERE id = ?")
            .bind(2_000_000_000_i64)
            .bind(&source_id)
            .execute(database.pool())
            .await
            .expect("movie duration");
        sqlx::query(
            "INSERT INTO server_settings (key, value) VALUES ('resume_min_ticks', '0')
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .execute(database.pool())
        .await
        .expect("resume minimum");

        let home = HomeService::new(
            CatalogService::new(database.clone(), access.clone()),
            libraries,
        );
        let principal = AccessPrincipal::new(admin.id, true);
        let library_ids = access
            .accessible_library_ids(principal)
            .await
            .expect("accessible libraries");
        let first = home
            .snapshot(principal, library_ids.clone())
            .await
            .expect("initial home snapshot");
        assert!(first.continue_watching.items.is_empty());

        let user_id = admin.id.to_string();
        database
            .record_playback_event(NewPlaybackEvent {
                user_id: &user_id,
                item_id: &item_id,
                media_source_id: Some(&source_id),
                play_session_id: "fresh-home-test",
                device_id: "test",
                client: Some("test"),
                device_name: Some("test"),
                client_version: None,
                device_type: Some("test"),
                remote_ip: None,
                state: "PLAYING",
                position_ticks: 1_000_000_000,
                duration_ticks: Some(2_000_000_000),
                played_percent: 90,
                is_paused: false,
            })
            .await
            .expect("playback state");

        let second = home
            .snapshot(principal, library_ids)
            .await
            .expect("fresh home snapshot");
        assert_eq!(second.continue_watching.items[0].id, item_id);
    }

    #[tokio::test]
    async fn home_continue_watching_keeps_only_latest_incomplete_episode_per_series() {
        let temp_dir = tempfile::tempdir().expect("temporary directory should be available");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("test address"),
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await.expect("database");
        let admin = SetupService::new(database.clone())
            .expect("setup service")
            .complete("Admin", "Admin", "correct password")
            .await
            .expect("admin user");
        let access = MediaAccessService::new(database.clone());
        let libraries = LibraryService::new(database.clone());
        let library = libraries
            .create_library("Mixed", LibraryKind::Mixed, false)
            .await
            .expect("mixed library");
        let user_id = admin.id.to_string();

        let series_a_id = uuid::Uuid::now_v7().to_string();
        let series_b_id = uuid::Uuid::now_v7().to_string();
        for (series_id, title, sort_title) in [
            (&series_a_id, "Series A", "series a"),
            (&series_b_id, "Series B", "series b"),
        ] {
            sqlx::query(
                "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title, identification_status
                 ) VALUES (?, ?, 'SERIES', ?, ?, 'LOCAL_CONFIRMED')",
            )
            .bind(series_id)
            .bind(library.id.to_string())
            .bind(title)
            .bind(sort_title)
            .execute(database.pool())
            .await
            .expect("series item");
        }

        let fixtures = [
            (
                Some(series_a_id.as_str()),
                "EPISODE",
                Some(1_i64),
                Some(8_i64),
                "Series A S01E08",
                300_i64,
            ),
            (
                Some(series_a_id.as_str()),
                "EPISODE",
                Some(2),
                Some(1),
                "Series A S02E01",
                400,
            ),
            (
                Some(series_a_id.as_str()),
                "EPISODE",
                Some(2),
                Some(4),
                "Series A S02E04",
                200,
            ),
            (
                Some(series_b_id.as_str()),
                "EPISODE",
                Some(1),
                Some(1),
                "Series B S01E01",
                350,
            ),
            (None, "MOVIE", None, None, "Standalone Movie", 500),
            (None, "MOVIE", None, None, "Another Movie", 450),
        ];
        let mut item_ids = Vec::new();
        for (series_id, item_type, season_number, episode_number, title, last_played_at) in fixtures
        {
            let item_id = uuid::Uuid::now_v7().to_string();
            sqlx::query(
                "INSERT INTO media_items (
                    id, library_id, item_type, series_id, season_number,
                    episode_number, title, sort_title, runtime_ticks,
                    identification_status, has_available_source
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 36000000000, 'LOCAL_CONFIRMED', 1)",
            )
            .bind(&item_id)
            .bind(library.id.to_string())
            .bind(item_type)
            .bind(series_id)
            .bind(season_number)
            .bind(episode_number)
            .bind(title)
            .bind(title.to_lowercase())
            .execute(database.pool())
            .await
            .expect("episode item");
            sqlx::query(
                "INSERT INTO user_item_state (
                    user_id, item_id, position_ticks, is_played, last_played_at
                 ) VALUES (?, ?, 6000000000, 0, ?)",
            )
            .bind(&user_id)
            .bind(&item_id)
            .bind(last_played_at)
            .execute(database.pool())
            .await
            .expect("episode resume state");
            item_ids.push(item_id);
        }

        let home = HomeService::new(
            CatalogService::new(database.clone(), access.clone()),
            libraries,
        );
        let principal = AccessPrincipal::new(admin.id, true);
        let library_ids = access
            .accessible_library_ids(principal)
            .await
            .expect("accessible libraries");
        let snapshot = home
            .snapshot(principal, library_ids)
            .await
            .expect("home snapshot");

        let items = &snapshot.continue_watching.items;
        assert_eq!(snapshot.continue_watching.total, 4);
        assert_eq!(items.len(), 4);
        assert!(items.iter().any(|item| item.id == item_ids[2]));
        assert!(items.iter().any(|item| item.id == item_ids[3]));
        assert!(items.iter().any(|item| item.id == item_ids[4]));
        assert!(items.iter().any(|item| item.id == item_ids[5]));
        assert!(!items.iter().any(|item| item.id == item_ids[0]));
        assert!(!items.iter().any(|item| item.id == item_ids[1]));

        let emby_resume = CatalogService::new(database, access)
            .list_continue_watching(principal, &user_id, 0, 10)
            .await
            .expect("Emby resume items");
        assert_eq!(emby_resume.total, 6);
        assert_eq!(emby_resume.items.len(), 6);
    }
}
