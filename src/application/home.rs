use std::{
    collections::HashMap,
    fmt,
    sync::{
        Arc, Mutex as StdMutex,
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
use crate::storage::{AccessEpoch, UserContentRestrictions};

const HOME_USER_CACHE_TTL: Duration = Duration::from_secs(15);
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
    content_restriction_version: Option<i64>,
}

struct HomeCacheEntry {
    principal: AccessPrincipal,
    library_ids: Vec<String>,
    content_restrictions: Option<UserContentRestrictions>,
    value: Mutex<Option<CachedSnapshot>>,
    compute_lock: Mutex<()>,
}

struct CachedSnapshot {
    /// Directory-level generation the snapshot was built under; any scan, metadata or library
    /// change moves it and therefore invalidates every user.
    generation: u64,
    /// The owning user's own generation; only that user's state changes move it.
    user_generation: u64,
    /// Permission counters the snapshot was built under, so a revoked library or a tightened
    /// policy invalidates the cached home without a dedicated call site.
    access_epoch: AccessEpoch,
    refreshed_at: Instant,
    snapshot: Arc<HomeSnapshot>,
}

impl CachedSnapshot {
    fn matches(&self, generation: u64, user_generation: u64, access_epoch: AccessEpoch) -> bool {
        self.generation == generation
            && self.user_generation == user_generation
            && self.access_epoch == access_epoch
    }
}

#[derive(Default)]
struct ScanInvalidationState {
    scheduled: bool,
    dirty: bool,
    refreshing: bool,
}

struct HomeServiceInner {
    catalog: CatalogService,
    libraries: LibraryService,
    /// Directory-level generation: scans, metadata edits and library lifecycle changes.
    generation: AtomicU64,
    /// Per-user generations for state only that user can see (progress, played, favorites,
    /// preferences). Never held across an await, so a plain mutex keeps invalidation synchronous.
    user_generations: StdMutex<HashMap<String, u64>>,
    entries: Mutex<HashMap<HomeCacheKey, Arc<HomeCacheEntry>>>,
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
            user_generations: StdMutex::new(HashMap::new()),
            entries: Mutex::new(HashMap::new()),
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

    /// Returns the user's home snapshot, serving it from the per-user cache when possible.
    ///
    /// Every rail lives in the same cached snapshot as the recommendations, so a cache hit runs no
    /// SQL for the rails at all.
    pub(crate) async fn snapshot(
        &self,
        principal: AccessPrincipal,
        mut library_ids: Vec<String>,
    ) -> Result<Arc<HomeSnapshot>, HomeError> {
        library_ids.sort_unstable();
        library_ids.dedup();
        let stage_started = std::time::Instant::now();
        let content_restrictions = self
            .inner
            .catalog
            .content_restrictions_for_principal(principal)
            .await?;
        tracing::debug!(
            stage = "restrictions",
            elapsed_ms = stage_started.elapsed().as_millis(),
            "home snapshot stage"
        );
        self.cached_home_snapshot(principal, &library_ids, content_restrictions)
            .await
    }

    pub(crate) async fn carousel(
        &self,
        principal: AccessPrincipal,
        mut library_ids: Vec<String>,
    ) -> Result<Vec<CatalogItem>, HomeError> {
        library_ids.sort_unstable();
        library_ids.dedup();
        let content_restrictions = self
            .inner
            .catalog
            .content_restrictions_for_principal(principal)
            .await?;
        Ok(self
            .cached_home_snapshot(principal, &library_ids, content_restrictions)
            .await?
            .recommended
            .clone())
    }

    #[cfg(test)]
    async fn cached_snapshot(
        &self,
        principal: AccessPrincipal,
        library_ids: &[String],
    ) -> Result<Arc<HomeSnapshot>, HomeError> {
        let content_restrictions = self
            .inner
            .catalog
            .content_restrictions_for_principal(principal)
            .await?;
        self.cached_home_snapshot(principal, library_ids, content_restrictions)
            .await
    }

    /// Returns the cached home snapshot for this principal, building it under the entry lock when
    /// any of the directory generation, the user's own generation or the permission epoch moved.
    async fn cached_home_snapshot(
        &self,
        principal: AccessPrincipal,
        library_ids: &[String],
        content_restrictions: Option<UserContentRestrictions>,
    ) -> Result<Arc<HomeSnapshot>, HomeError> {
        let user_id = principal.user_id_string();
        let key = HomeCacheKey {
            user_id: user_id.clone(),
            is_admin: principal.is_admin,
            library_ids: library_ids.to_vec(),
            content_restriction_version: content_restrictions
                .as_ref()
                .map(|restrictions| restrictions.version),
        };
        let entry = self
            .entry(key, principal, content_restrictions.clone())
            .await;
        loop {
            let generation = self.inner.generation.load(Ordering::Acquire);
            let user_generation = self.user_generation(&user_id);
            let access_epoch = self.access_epoch(&user_id);
            if let Some(snapshot) = self
                .cached_value(&entry, generation, user_generation, access_epoch)
                .await
            {
                return Ok(snapshot);
            }

            let _compute_guard = entry.compute_lock.lock().await;
            let generation = self.inner.generation.load(Ordering::Acquire);
            let scan_refresh_epoch = self.inner.scan_refresh_epoch.load(Ordering::Acquire);
            let user_generation = self.user_generation(&user_id);
            let access_epoch = self.access_epoch(&user_id);
            if let Some(snapshot) = self
                .cached_value(&entry, generation, user_generation, access_epoch)
                .await
            {
                return Ok(snapshot);
            }

            let snapshot = Arc::new(
                self.build_home_snapshot(principal, library_ids, content_restrictions.as_ref())
                    .await?,
            );
            if self.inner.generation.load(Ordering::Acquire) != generation
                || self.inner.scan_refresh_epoch.load(Ordering::Acquire) != scan_refresh_epoch
                || self.user_generation(&user_id) != user_generation
                || self.access_epoch(&user_id) != access_epoch
            {
                continue;
            }
            *entry.value.lock().await = Some(CachedSnapshot {
                generation,
                user_generation,
                access_epoch,
                refreshed_at: Instant::now(),
                snapshot: snapshot.clone(),
            });
            return Ok(snapshot);
        }
    }

    /// Returns the cached snapshot when it still matches every generation, scheduling a background
    /// refresh once it is older than the TTL so the TTL is only a fallback for missed
    /// invalidations rather than the normal refresh path.
    async fn cached_value(
        &self,
        entry: &HomeCacheEntry,
        generation: u64,
        user_generation: u64,
        access_epoch: AccessEpoch,
    ) -> Option<Arc<HomeSnapshot>> {
        let cached = entry.value.lock().await;
        let cached = cached.as_ref()?;
        if !cached.matches(generation, user_generation, access_epoch) {
            return None;
        }
        if cached.refreshed_at.elapsed() >= HOME_USER_CACHE_TTL {
            self.schedule_refresh();
        }
        Some(cached.snapshot.clone())
    }

    fn user_generation(&self, user_id: &str) -> u64 {
        self.inner
            .user_generations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(user_id)
            .copied()
            .unwrap_or_default()
    }

    fn access_epoch(&self, user_id: &str) -> AccessEpoch {
        self.inner.catalog.access_change_epochs().snapshot(user_id)
    }

    /// Directory-level invalidation for changes every user can see: scans, metadata edits, media
    /// merges and deletions, library lifecycle and server-wide settings.
    ///
    /// This is the conservative default: when a change cannot be proven to be private to one user,
    /// classify it here so no cached home keeps a stale rail.
    pub(crate) fn invalidate_directory(&self) {
        Self::invalidate_directory_now(&self.inner);
    }

    /// User-level invalidation for state only the owning user can see: playback progress, played
    /// and favorite flags, account preferences and library order.
    ///
    /// Only this user's cached snapshots become stale. Other users keep their caches and the global
    /// library page cache is left alone; the affected entry is rebuilt lazily on that user's next
    /// home request, so no background recompute is scheduled either.
    pub(crate) fn invalidate_user(&self, user_id: &str) {
        let mut generations = self
            .inner
            .user_generations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let generation = generations.entry(user_id.to_owned()).or_insert(0);
        *generation = generation.wrapping_add(1);
    }

    fn invalidate_directory_now(inner: &Arc<HomeServiceInner>) {
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

    pub(crate) async fn flush_scan_invalidation(&self) -> bool {
        let should_refresh = {
            let mut state = self.inner.scan_invalidation.lock().await;
            if state.refreshing || (!state.scheduled && !state.dirty) {
                false
            } else {
                state.scheduled = false;
                state.dirty = false;
                state.refreshing = true;
                true
            }
        };
        if !should_refresh {
            return false;
        }

        let scan_refresh_epoch = self
            .inner
            .scan_refresh_epoch
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        let generation = self.inner.generation.load(Ordering::Acquire);
        let refreshed = self.refresh_cached_entries(true).await;
        let versions_unchanged = self.inner.generation.load(Ordering::Acquire) == generation
            && self.inner.scan_refresh_epoch.load(Ordering::Acquire) == scan_refresh_epoch;

        let mut state = self.inner.scan_invalidation.lock().await;
        state.refreshing = false;
        if !refreshed || !versions_unchanged || state.scheduled || state.dirty {
            state.scheduled = true;
            state.dirty = true;
            return false;
        }
        true
    }

    fn schedule_refresh(&self) {
        if self.inner.refresh_pending.swap(true, Ordering::AcqRel) {
            return;
        }
        if self.inner.refresh_tx.try_send(()).is_err() {
            self.inner.refresh_pending.store(false, Ordering::Release);
        }
    }

    async fn entry(
        &self,
        key: HomeCacheKey,
        principal: AccessPrincipal,
        content_restrictions: Option<UserContentRestrictions>,
    ) -> Arc<HomeCacheEntry> {
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
                    content_restrictions,
                    value: Mutex::new(None),
                    compute_lock: Mutex::new(()),
                })
            })
            .clone()
    }

    async fn refresh_cached_entries(&self, force: bool) -> bool {
        let generation = self.inner.generation.load(Ordering::Acquire);
        let scan_refresh_epoch = self.inner.scan_refresh_epoch.load(Ordering::Acquire);
        if force
            && (self.inner.generation.load(Ordering::Acquire) != generation
                || self.inner.scan_refresh_epoch.load(Ordering::Acquire) != scan_refresh_epoch)
        {
            return false;
        }
        let entries = self
            .inner
            .entries
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut refreshed_all = true;
        for entry in entries {
            let user_id = entry.principal.user_id_string();
            let compute_guard = if force {
                Some(entry.compute_lock.lock().await)
            } else {
                entry.compute_lock.try_lock().ok()
            };
            let Some(_compute_guard) = compute_guard else {
                if force {
                    refreshed_all = false;
                }
                continue;
            };
            let entry_generation = self.inner.generation.load(Ordering::Acquire);
            let entry_user_generation = self.user_generation(&user_id);
            let entry_access_epoch = self.access_epoch(&user_id);
            let entry_scan_refresh_epoch = self.inner.scan_refresh_epoch.load(Ordering::Acquire);
            if force
                && (entry_generation != generation
                    || entry_scan_refresh_epoch != scan_refresh_epoch)
            {
                refreshed_all = false;
                continue;
            }
            let cached = entry.value.lock().await;
            if !force
                && cached.as_ref().is_some_and(|cached| {
                    cached.matches(entry_generation, entry_user_generation, entry_access_epoch)
                        && cached.refreshed_at.elapsed() < HOME_USER_CACHE_TTL
                })
            {
                continue;
            }
            drop(cached);
            let notified = self.inner.invalidation_notify.notified();
            let result = tokio::select! {
                result = self.build_home_snapshot(
                    entry.principal,
                    &entry.library_ids,
                    entry.content_restrictions.as_ref(),
                ) => Some(result),
                _ = notified => None,
            };
            match result {
                Some(Ok(snapshot))
                    if self.inner.generation.load(Ordering::Acquire) == entry_generation
                        && self.inner.scan_refresh_epoch.load(Ordering::Acquire)
                            == entry_scan_refresh_epoch
                        && self.user_generation(&user_id) == entry_user_generation
                        && self.access_epoch(&user_id) == entry_access_epoch =>
                {
                    *entry.value.lock().await = Some(CachedSnapshot {
                        generation: entry_generation,
                        user_generation: entry_user_generation,
                        access_epoch: entry_access_epoch,
                        refreshed_at: Instant::now(),
                        snapshot: Arc::new(snapshot),
                    });
                }
                Some(Ok(_)) | None => {
                    refreshed_all = false;
                    self.schedule_refresh();
                }
                Some(Err(error)) => {
                    refreshed_all = false;
                    tracing::debug!(%error, "home cache refresh failed");
                }
            }
        }
        refreshed_all
            && self.inner.generation.load(Ordering::Acquire) == generation
            && self.inner.scan_refresh_epoch.load(Ordering::Acquire) == scan_refresh_epoch
    }

    /// Builds every rail the cached snapshot serves. It runs in the background refresh worker or
    /// under the entry's compute lock, never inline on a cache hit.
    async fn build_home_snapshot(
        &self,
        principal: AccessPrincipal,
        accessible_library_ids: &[String],
        content_restrictions: Option<&UserContentRestrictions>,
    ) -> Result<HomeSnapshot, HomeError> {
        let user_id = principal.user_id_string();
        let stage_started = Instant::now();
        let (recommended, continue_watching, recently_added, latest_groups, views) = tokio::try_join!(
            async {
                let started = Instant::now();
                let result = self
                    .inner
                    .catalog
                    .list_recommended_for_library_ids_with_policy(
                        accessible_library_ids,
                        &user_id,
                        content_restrictions,
                        7,
                    )
                    .await;
                tracing::debug!(
                    stage = "recommended",
                    elapsed_ms = started.elapsed().as_millis(),
                    "home section"
                );
                result.map_err(HomeError::Catalog)
            },
            async {
                let started = Instant::now();
                let result = self
                    .inner
                    .catalog
                    .list_home_continue_watching_for_library_ids(
                        accessible_library_ids,
                        &user_id,
                        content_restrictions,
                        0,
                        10,
                    )
                    .await;
                tracing::debug!(
                    stage = "continue_watching",
                    elapsed_ms = started.elapsed().as_millis(),
                    "home section"
                );
                result.map_err(HomeError::Catalog)
            },
            async {
                let started = Instant::now();
                let result = self
                    .inner
                    .catalog
                    .list_recently_added_for_library_ids(
                        accessible_library_ids,
                        content_restrictions,
                        0,
                        12,
                    )
                    .await;
                tracing::debug!(
                    stage = "recently_added",
                    elapsed_ms = started.elapsed().as_millis(),
                    "home section"
                );
                result.map_err(HomeError::Catalog)
            },
            async {
                let started = Instant::now();
                let result = self
                    .inner
                    .catalog
                    .list_recently_added_by_library_ids(
                        accessible_library_ids,
                        content_restrictions,
                        12,
                    )
                    .await;
                tracing::debug!(
                    stage = "latest_by_library",
                    elapsed_ms = started.elapsed().as_millis(),
                    "home section"
                );
                result.map_err(HomeError::Catalog)
            },
            async {
                let started = Instant::now();
                let result = self.inner.libraries.list_libraries().await;
                tracing::debug!(
                    stage = "libraries",
                    elapsed_ms = started.elapsed().as_millis(),
                    "home section"
                );
                result.map_err(HomeError::Libraries)
            },
        )?;
        tracing::debug!(
            stage = "sections",
            elapsed_ms = stage_started.elapsed().as_millis(),
            "home snapshot stage"
        );
        let views = self
            .inner
            .libraries
            .order_views_for_user(&user_id, principal.is_admin, accessible_library_ids, views)
            .await?;
        Ok(HomeSnapshot {
            continue_watching,
            recently_added,
            recommended,
            latest_groups,
            views,
        })
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
        auth::users::UserStore,
        config::Config,
        domain::ids::UserId,
        library::LibraryKind,
        storage::{
            Database, NewPlaybackEvent, NewUserContentRestrictions,
            UserContentRestrictionsCasResult,
        },
    };

    #[tokio::test]
    async fn carousel_cache_is_reused_but_isolated_by_principal() {
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
    async fn carousel_cache_isolated_by_content_restriction_version() {
        let temp_dir = tempfile::tempdir().expect("temporary directory should be available");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("test address"),
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await.expect("database");
        let user = UserStore::new(database.clone())
            .expect("user store")
            .create_user(
                "home-policy-cache-viewer",
                "Home Policy Cache Viewer",
                "test password",
                false,
            )
            .await
            .expect("regular user");
        let principal = AccessPrincipal::new(user.id, false);
        let access = MediaAccessService::new(database.clone());
        let home = HomeService::new(
            CatalogService::new(database.clone(), access),
            LibraryService::new(database.clone()),
        );

        let before_policy = home
            .cached_snapshot(principal, &[])
            .await
            .expect("initial policy snapshot");
        let policy = database
            .compare_and_swap_user_content_restrictions(
                &user.id.to_string(),
                0,
                NewUserContentRestrictions {
                    rating_system: "MPA",
                    maximum_rating: Some("PG-13"),
                    allow_unrated: false,
                    allowed_tags: &[],
                    blocked_tags: &[],
                },
            )
            .await
            .expect("save content restriction policy");
        assert!(matches!(
            policy,
            UserContentRestrictionsCasResult::Updated(_)
        ));

        let after_policy = home
            .cached_snapshot(principal, &[])
            .await
            .expect("updated policy snapshot");

        assert!(!std::ptr::eq(before_policy.as_ref(), after_policy.as_ref()));
    }

    #[tokio::test]
    async fn directory_invalidation_rebuilds_cached_snapshot_before_returning_it() {
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
        home.invalidate_directory();
        let refreshed = home
            .cached_snapshot(principal, &[])
            .await
            .expect("invalidated user snapshot");

        assert!(!std::ptr::eq(first.as_ref(), refreshed.as_ref()));
    }

    #[tokio::test]
    async fn directory_invalidation_invalidates_every_user() {
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
        let first_user = AccessPrincipal::new(UserId::new(), false);
        let second_user = AccessPrincipal::new(UserId::new(), false);

        let first_user_before = home
            .cached_snapshot(first_user, &[])
            .await
            .expect("first user snapshot");
        let second_user_before = home
            .cached_snapshot(second_user, &[])
            .await
            .expect("second user snapshot");

        home.invalidate_directory();

        let first_user_after = home
            .cached_snapshot(first_user, &[])
            .await
            .expect("first user snapshot after directory change");
        let second_user_after = home
            .cached_snapshot(second_user, &[])
            .await
            .expect("second user snapshot after directory change");

        assert!(!std::ptr::eq(
            first_user_before.as_ref(),
            first_user_after.as_ref()
        ));
        assert!(!std::ptr::eq(
            second_user_before.as_ref(),
            second_user_after.as_ref()
        ));
    }

    #[tokio::test]
    async fn user_invalidation_only_invalidates_that_user() {
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
        let first_user = AccessPrincipal::new(UserId::new(), false);
        let second_user = AccessPrincipal::new(UserId::new(), false);
        let first_user_id = first_user.user_id_string();

        let first_user_before = home
            .cached_snapshot(first_user, &[])
            .await
            .expect("first user snapshot");
        let second_user_before = home
            .cached_snapshot(second_user, &[])
            .await
            .expect("second user snapshot");

        home.invalidate_user(&first_user_id);

        assert_eq!(
            home.inner.generation.load(Ordering::Acquire),
            0,
            "a per-user change must not move the directory generation"
        );
        let first_user_after = home
            .cached_snapshot(first_user, &[])
            .await
            .expect("first user snapshot after own change");
        let second_user_after = home
            .cached_snapshot(second_user, &[])
            .await
            .expect("second user snapshot after another user's change");

        assert!(!std::ptr::eq(
            first_user_before.as_ref(),
            first_user_after.as_ref()
        ));
        assert!(
            std::ptr::eq(second_user_before.as_ref(), second_user_after.as_ref()),
            "another user's state change must keep this user's cached home snapshot"
        );
    }

    #[tokio::test]
    async fn rails_on_a_cache_hit_run_no_sql() {
        let temp_dir = tempfile::tempdir().expect("temporary directory should be available");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("test address"),
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await.expect("database");
        let access = MediaAccessService::new(database.clone());
        let home = HomeService::new(
            CatalogService::new(database.clone(), access),
            LibraryService::new(database.clone()),
        );
        // The administrator principal reads no content restrictions, so a cache hit performs no
        // query at all: every rail comes from the cached snapshot.
        let principal = AccessPrincipal::new(UserId::new(), true);

        let first = home
            .snapshot(principal, Vec::new())
            .await
            .expect("initial home snapshot");
        database.reset_query_count();
        let cached = home
            .snapshot(principal, Vec::new())
            .await
            .expect("cached home snapshot");

        assert!(std::ptr::eq(first.as_ref(), cached.as_ref()));
        assert_eq!(
            database.query_count(),
            0,
            "a home cache hit must not query the rails again"
        );
    }

    #[tokio::test]
    async fn library_access_change_invalidates_that_users_home_cache() {
        let temp_dir = tempfile::tempdir().expect("temporary directory should be available");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("test address"),
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await.expect("database");
        let user = UserStore::new(database.clone())
            .expect("user store")
            .create_user(
                "home-access-cache-viewer",
                "Home Access Cache Viewer",
                "test password",
                false,
            )
            .await
            .expect("regular user");
        let other = UserStore::new(database.clone())
            .expect("user store")
            .create_user(
                "home-access-cache-other",
                "Home Access Cache Other",
                "test password",
                false,
            )
            .await
            .expect("regular user");
        let library = LibraryService::new(database.clone())
            .create_library("Access Cache", LibraryKind::Movie, false)
            .await
            .expect("library");
        let library_id = library.id.to_string();
        let access = MediaAccessService::new(database.clone());
        let home = HomeService::new(
            CatalogService::new(database.clone(), access),
            LibraryService::new(database.clone()),
        );
        let principal = AccessPrincipal::new(user.id, false);
        let other_principal = AccessPrincipal::new(other.id, false);
        let library_ids = vec![library_id.clone()];

        let before = home
            .cached_snapshot(principal, &library_ids)
            .await
            .expect("initial home snapshot");
        let other_before = home
            .cached_snapshot(other_principal, &library_ids)
            .await
            .expect("other user home snapshot");

        // The permission write bumps the storage-level counters the access cache already uses, so
        // the home cache must treat the cached snapshot as stale without an extra call site.
        database
            .set_user_library_access(&user.id.to_string(), &library_id, false)
            .await
            .expect("library access write");

        let after = home
            .cached_snapshot(principal, &library_ids)
            .await
            .expect("home snapshot after access change");
        let other_after = home
            .cached_snapshot(other_principal, &library_ids)
            .await
            .expect("other user home snapshot after access change");

        assert!(!std::ptr::eq(before.as_ref(), after.as_ref()));
        assert!(
            std::ptr::eq(other_before.as_ref(), other_after.as_ref()),
            "one user's library access change must not invalidate another user's home cache"
        );
    }

    #[tokio::test]
    async fn library_deletion_invalidates_every_home_cache() {
        let temp_dir = tempfile::tempdir().expect("temporary directory should be available");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("test address"),
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await.expect("database");
        let user_store = UserStore::new(database.clone()).expect("user store");
        let first = user_store
            .create_user(
                "home-deletion-cache-first",
                "Home Deletion Cache First",
                "test password",
                false,
            )
            .await
            .expect("first user");
        let second = user_store
            .create_user(
                "home-deletion-cache-second",
                "Home Deletion Cache Second",
                "test password",
                false,
            )
            .await
            .expect("second user");
        let library = LibraryService::new(database.clone())
            .create_library("Deleted Cache", LibraryKind::Movie, false)
            .await
            .expect("library");
        let library_id = library.id.to_string();
        let access = MediaAccessService::new(database.clone());
        let home = HomeService::new(
            CatalogService::new(database.clone(), access),
            LibraryService::new(database.clone()),
        );
        let first_user = AccessPrincipal::new(first.id, false);
        let second_user = AccessPrincipal::new(second.id, false);
        let library_ids = vec![library_id.clone()];

        let first_before = home
            .cached_snapshot(first_user, &library_ids)
            .await
            .expect("first user snapshot");
        let second_before = home
            .cached_snapshot(second_user, &library_ids)
            .await
            .expect("second user snapshot");

        // Deleting a library moves the global access epoch: every user's visible libraries change.
        assert!(
            database
                .delete_library(&library_id)
                .await
                .expect("delete library")
        );

        let first_after = home
            .cached_snapshot(first_user, &library_ids)
            .await
            .expect("first user snapshot after deletion");
        let second_after = home
            .cached_snapshot(second_user, &library_ids)
            .await
            .expect("second user snapshot after deletion");

        assert!(!std::ptr::eq(first_before.as_ref(), first_after.as_ref()));
        assert!(!std::ptr::eq(second_before.as_ref(), second_after.as_ref()));
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

        assert!(home.flush_scan_invalidation().await);
        assert_eq!(home.inner.generation.load(Ordering::Acquire), 0);

        let after_scan = home
            .cached_snapshot(principal, &[])
            .await
            .expect("user snapshot after scan");
        assert!(!std::ptr::eq(first.as_ref(), after_scan.as_ref()));

        assert!(!home.flush_scan_invalidation().await);
        assert_eq!(home.inner.generation.load(Ordering::Acquire), 0);
    }

    #[tokio::test]
    async fn scan_flush_without_pending_invalidation_is_a_noop() {
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

        assert!(!home.flush_scan_invalidation().await);

        let after_noop = home
            .cached_snapshot(principal, &[])
            .await
            .expect("user snapshot after no-op flush");
        assert!(std::ptr::eq(first.as_ref(), after_noop.as_ref()));
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

        home.invalidate_directory();
        home.invalidate_directory();

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
    async fn playback_progress_only_invalidates_the_playing_users_home_cache() {
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
        let other_user = UserStore::new(database.clone())
            .expect("user store")
            .create_user(
                "home-progress-other",
                "Home Progress Other",
                "test password",
                false,
            )
            .await
            .expect("regular user");
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
        let playing = AccessPrincipal::new(admin.id, true);
        let other = AccessPrincipal::new(other_user.id, false);
        let library_ids = vec![library.id.to_string()];
        let playing_before = home
            .snapshot(playing, library_ids.clone())
            .await
            .expect("initial playing user snapshot");
        let other_before = home
            .snapshot(other, library_ids.clone())
            .await
            .expect("initial other user snapshot");
        assert!(playing_before.continue_watching.items.is_empty());
        assert!(other_before.continue_watching.items.is_empty());

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
                previous_state: None,
                previous_position_ticks: None,
                renew_admission: false,
            })
            .await
            .expect("playback state");
        // What the progress handlers do once the write commits: invalidate the playing user only.
        home.invalidate_user(&user_id);

        let playing_after = home
            .snapshot(playing, library_ids.clone())
            .await
            .expect("fresh playing user snapshot");
        let other_after = home
            .snapshot(other, library_ids)
            .await
            .expect("other user snapshot after another user's playback");

        assert_eq!(playing_after.continue_watching.items[0].id, item_id);
        assert!(!std::ptr::eq(
            playing_before.as_ref(),
            playing_after.as_ref()
        ));
        assert!(
            std::ptr::eq(other_before.as_ref(), other_after.as_ref()),
            "one user's playback must not invalidate another user's cached home"
        );
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
        assert_eq!(emby_resume.total, 4);
        assert_eq!(emby_resume.items.len(), 4);
        assert!(emby_resume.items.iter().any(|item| item.id == item_ids[2]));
        assert!(emby_resume.items.iter().any(|item| item.id == item_ids[3]));
        assert!(emby_resume.items.iter().any(|item| item.id == item_ids[4]));
        assert!(emby_resume.items.iter().any(|item| item.id == item_ids[5]));
        assert!(!emby_resume.items.iter().any(|item| item.id == item_ids[0]));
        assert!(!emby_resume.items.iter().any(|item| item.id == item_ids[1]));
    }
}
