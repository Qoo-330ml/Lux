use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::{
    auth::cache::ExpiringCache,
    domain::{
        content_restrictions::{
            ContentRestrictionClassification, ContentRestrictionPolicy,
            content_classification_is_allowed, content_restriction_policy_is_restrictive,
        },
        ids::UserId,
    },
    storage::{
        AccessChangeEpochs, AccessEpoch, Database, GLOBAL_COLLECTION_LIBRARY_ID, StorageError,
        StoredPlaybackSource, UserContentRestrictions,
    },
};

/// How long a cached playback access answer stays usable.
///
/// Emby clients and the Lux web player report progress every ~10 s, so one minute removes about six
/// repeated checks from a session. The project owner accepted this bound for revocation lag; the
/// permission writes in `storage` still invalidate the affected user immediately, so the TTL only
/// covers changes no storage function observes (see the notes on [`MediaAccessService`]).
const PLAYBACK_ACCESS_CACHE_TTL: Duration = Duration::from_secs(60);

/// Upper bound on cached answers.
///
/// The realistic working set is one entry per actively playing (user, item, library) triple, but the
/// map is bounded so a long-running server cannot grow it without limit. Expired entries are dropped
/// lazily; when the map is full the entry closest to expiring makes room.
const PLAYBACK_ACCESS_CACHE_CAPACITY: usize = 1024;

/// Source of `Instant`s for the playback access cache.
///
/// Production always uses [`Instant::now`]. Tests swap in a hand-advanced clock so the TTL can be
/// checked without sleeping for a minute.
type AccessCacheClock = Arc<dyn Fn() -> Instant + Send + Sync>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AccessPrincipal {
    pub user_id: Option<UserId>,
    pub is_admin: bool,
}

impl AccessPrincipal {
    pub const fn new(user_id: UserId, is_admin: bool) -> Self {
        Self {
            user_id: Some(user_id),
            is_admin,
        }
    }

    pub const fn server_admin(target_user_id: Option<UserId>) -> Self {
        Self {
            user_id: target_user_id,
            is_admin: true,
        }
    }

    pub fn user_id_string(self) -> String {
        self.user_id.map_or_else(String::new, |id| id.to_string())
    }
}

/// The per-user parental-control policy, resolved once per cache window.
///
/// Only the policy is cached. Library access is still checked per request: the access layer must
/// never answer from a value that a database write outside the storage API could have invalidated,
/// and library access rows are exactly such an input (the frozen end-to-end tests change them with
/// direct SQL). Per-item certification rows are likewise read per item, so a rescan is visible
/// immediately.
#[derive(Clone)]
struct CachedRestrictionPolicy {
    restrictions: Option<UserContentRestrictions>,
    /// Captured before the policy row was read; a hit is only served while it is still current.
    epoch: AccessEpoch,
}

/// Answers the access questions that every request asks.
///
/// The service is also the home of the access caches in the application, both guarded by the change
/// epochs stored next to the writes (`crate::storage::AccessChangeEpochs`):
///
/// * the playback progress path re-asks "may this user view this item" every ~10 s for the whole
///   session, so [`Self::can_view_item_cached`] and [`Self::can_view_item_in_library_cached`] may
///   reuse an earlier "allowed" answer for up to [`PLAYBACK_ACCESS_CACHE_TTL`]. Those two entry
///   points are **only** for the playback progress handlers: every other endpoint must use the
///   uncached checks so an administrative change is visible on its very next request;
/// * the per-user parental-control policy is cached for [`crate::auth::cache::CACHE_TTL`] because
///   every item check reads it. Library access, item certifications and administrator visibility
///   are still read per request.
///
/// Denials are never cached by the playback cache, so a newly granted permission takes effect
/// immediately. Anything that does not go through one of the epoch-bumping writes (an item's
/// content-restriction index being rebuilt, for example) can lag until the TTL expires.
#[derive(Clone)]
pub struct MediaAccessService {
    database: Database,
    playback_access_cache: PlaybackAccessCache,
    restriction_cache: Arc<ExpiringCache<CachedRestrictionPolicy>>,
}

impl MediaAccessService {
    pub fn new(database: Database) -> Self {
        Self {
            database,
            playback_access_cache: PlaybackAccessCache::new(),
            restriction_cache: Arc::new(ExpiringCache::default()),
        }
    }

    /// Reads the user's parental-control policy, reusing it while its epochs are unchanged.
    ///
    /// The epoch is captured *before* the policy row is read; when it moved while the query ran the
    /// policy is returned but not cached, so a stale answer can never be stored.
    async fn restriction_policy(
        &self,
        user_id: UserId,
    ) -> Result<Arc<CachedRestrictionPolicy>, AccessError> {
        let user_id = user_id.to_string();
        let epochs = self.database.access_changes();
        if let Some(entry) = self
            .restriction_cache
            .lookup(user_id.as_bytes(), Instant::now())
        {
            let policy = entry.value();
            if policy.epoch == epochs.snapshot(&user_id) {
                return Ok(policy);
            }
        }
        let epoch = epochs.snapshot(&user_id);
        let restrictions = self.database.user_content_restrictions(&user_id).await?;
        if epochs.snapshot(&user_id) == epoch {
            self.restriction_cache.store(
                user_id.clone().into_bytes(),
                CachedRestrictionPolicy {
                    restrictions: restrictions.clone(),
                    epoch,
                },
                Instant::now(),
            );
        }
        Ok(Arc::new(CachedRestrictionPolicy {
            restrictions,
            epoch,
        }))
    }

    pub async fn can_view_library(
        &self,
        principal: AccessPrincipal,
        library_id: &str,
    ) -> Result<bool, AccessError> {
        if library_id == GLOBAL_COLLECTION_LIBRARY_ID {
            return Ok(false);
        }
        if principal.is_admin {
            return Ok(true);
        }
        let Some(user_id) = principal.user_id else {
            return Ok(false);
        };
        Ok(self
            .database
            .has_user_library_access(&user_id.to_string(), library_id)
            .await?)
    }

    pub async fn can_view_item(
        &self,
        principal: AccessPrincipal,
        item_id: &str,
    ) -> Result<bool, AccessError> {
        let library_id = self.database.find_item_library_id(item_id).await?;
        self.can_view_item_in_library(principal, item_id, library_id.as_deref())
            .await
    }

    /// Same check as [`Self::can_view_item`] for callers that already resolved the item library.
    ///
    /// `library_id` is this request's `Database::find_item_library_id` result for `item_id`, so
    /// `None` means the item is not visible and access is denied without repeating the lookup.
    /// Playback callbacks need the library id anyway, and without this entry point they resolve
    /// the same library twice per event.
    pub async fn can_view_item_in_library(
        &self,
        principal: AccessPrincipal,
        item_id: &str,
        library_id: Option<&str>,
    ) -> Result<bool, AccessError> {
        let Some(library_id) = library_id else {
            return Ok(false);
        };
        if library_id == GLOBAL_COLLECTION_LIBRARY_ID {
            return Ok(false);
        }
        if principal.is_admin {
            return Ok(true);
        }
        if !self.can_view_library(principal, library_id).await? {
            return Ok(false);
        }
        self.can_view_item_content(principal, item_id).await
    }

    /// The playback-progress variant of [`Self::can_view_item`].
    ///
    /// Only the playback progress handlers may call this: it may answer "allowed" from the short
    /// playback cache instead of asking the database. It resolves the item library like
    /// [`Self::can_view_item`] does, so it saves exactly the library-access and content-restriction
    /// queries and keeps the item lookup.
    pub async fn can_view_item_cached(
        &self,
        principal: AccessPrincipal,
        item_id: &str,
    ) -> Result<bool, AccessError> {
        if principal.is_admin || principal.user_id.is_none() {
            return self.can_view_item(principal, item_id).await;
        }
        let library_id = self.database.find_item_library_id(item_id).await?;
        self.can_view_item_in_library_cached(principal, item_id, library_id.as_deref())
            .await
    }

    /// The playback-progress variant of [`Self::can_view_item_in_library`].
    ///
    /// Only the playback progress handlers may call this: a recent "allowed" answer for the same
    /// (user, item, library) may be reused for up to [`PLAYBACK_ACCESS_CACHE_TTL`], while a "denied"
    /// answer is always re-checked. Administrators, the global collection library and an unresolved
    /// library never enter the cache, because the uncached check answers them without a query.
    pub async fn can_view_item_in_library_cached(
        &self,
        principal: AccessPrincipal,
        item_id: &str,
        library_id: Option<&str>,
    ) -> Result<bool, AccessError> {
        let Some(library_id) = library_id else {
            return Ok(false);
        };
        if library_id == GLOBAL_COLLECTION_LIBRARY_ID || principal.is_admin {
            return self
                .can_view_item_in_library(principal, item_id, Some(library_id))
                .await;
        }
        let Some(user_id) = principal.user_id else {
            return self
                .can_view_item_in_library(principal, item_id, Some(library_id))
                .await;
        };
        let key = PlaybackAccessKey {
            user_id: user_id.to_string(),
            item_id: item_id.to_owned(),
            library_id: library_id.to_owned(),
        };
        let epochs = self.database.access_changes();
        if self.playback_access_cache.is_fresh(epochs, &key) {
            return Ok(true);
        }
        let allowed = self
            .can_view_item_in_library(principal, item_id, Some(library_id))
            .await?;
        if allowed {
            self.playback_access_cache.remember(epochs, key);
        }
        Ok(allowed)
    }

    /// Checks only the per-user content restrictions. The caller has already confirmed that the
    /// item exists and that the principal may view its library.
    async fn can_view_item_content(
        &self,
        principal: AccessPrincipal,
        item_id: &str,
    ) -> Result<bool, AccessError> {
        if principal.is_admin {
            return Ok(true);
        }
        let Some(user_id) = principal.user_id else {
            return Ok(true);
        };
        let policy = self.restriction_policy(user_id).await?;
        let Some(restrictions) = policy.restrictions.as_ref() else {
            return Ok(true);
        };
        let policy = ContentRestrictionPolicy {
            rating_system: &restrictions.rating_system,
            maximum_rating: restrictions.maximum_rating.as_deref(),
            allow_unrated: restrictions.allow_unrated,
            allowed_tags: &restrictions.allowed_tags,
            blocked_tags: &restrictions.blocked_tags,
        };
        if !content_restriction_policy_is_restrictive(&policy) {
            return Ok(true);
        }
        let classifications = self
            .database
            .media_content_restriction_classifications_for_item(item_id)
            .await?;
        if classifications.is_empty() {
            return Ok(false);
        }
        Ok(classifications
            .into_iter()
            .all(|(certification_code, tags, indexed)| {
                content_classification_is_allowed(
                    &policy,
                    &ContentRestrictionClassification {
                        certification_code: certification_code.as_deref(),
                        tags: &tags,
                        indexed,
                    },
                )
            }))
    }

    pub async fn accessible_library_ids(
        &self,
        principal: AccessPrincipal,
    ) -> Result<Vec<String>, AccessError> {
        if principal.is_admin {
            return Ok(self.database.list_enabled_media_library_ids().await?);
        }
        let Some(user_id) = principal.user_id else {
            return Ok(Vec::new());
        };
        Ok(self
            .database
            .list_accessible_library_ids(&user_id.to_string())
            .await?)
    }

    /// Resolves accessible library ids for several users with one bounded batch.
    ///
    /// Administrators see every enabled media library, exactly like [`Self::accessible_library_ids`];
    /// the remaining users are resolved in one batched query.
    pub async fn accessible_library_ids_for_users(
        &self,
        users: &[(String, bool)],
    ) -> Result<std::collections::HashMap<String, Vec<String>>, AccessError> {
        if users.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        let admin_ids = users
            .iter()
            .filter(|(_, is_admin)| *is_admin)
            .map(|(user_id, _)| user_id.clone())
            .collect::<Vec<_>>();
        let member_ids = users
            .iter()
            .filter(|(_, is_admin)| !*is_admin)
            .map(|(user_id, _)| user_id.clone())
            .collect::<Vec<_>>();
        let mut accessible = std::collections::HashMap::with_capacity(users.len());
        if !admin_ids.is_empty() {
            let enabled_library_ids = self.database.list_enabled_media_library_ids().await?;
            for user_id in admin_ids {
                accessible.insert(user_id, enabled_library_ids.clone());
            }
        }
        if !member_ids.is_empty() {
            let member_accessible = self
                .database
                .list_accessible_library_ids_for_users(&member_ids)
                .await?;
            for user_id in member_ids {
                accessible.insert(
                    user_id.clone(),
                    member_accessible.get(&user_id).cloned().unwrap_or_default(),
                );
            }
        }
        Ok(accessible)
    }

    pub(crate) async fn authorized_playback_source(
        &self,
        principal: AccessPrincipal,
        item_id: &str,
        source_id: Option<&str>,
    ) -> Result<Option<StoredPlaybackSource>, AccessError> {
        let Some(source) = self
            .database
            .find_authorized_playback_source(
                item_id,
                source_id,
                &principal.user_id_string(),
                principal.is_admin,
            )
            .await?
        else {
            return Ok(None);
        };
        // Streaming and downloads reuse this lookup; without the content check they would
        // bypass the parental-control policy that item detail and subtitles enforce.
        if !self.can_view_item_content(principal, item_id).await? {
            return Ok(None);
        }
        Ok(Some(source))
    }
}

/// Identifies one cached playback access answer.
///
/// The user is part of the key, so one user's answer can never be served to another; the library is
/// part of it so an item that moved between libraries is re-checked under its new library.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct PlaybackAccessKey {
    user_id: String,
    item_id: String,
    library_id: String,
}

#[derive(Clone, Copy, Debug)]
struct PlaybackAccessEntry {
    expires_at: Instant,
    epoch: AccessEpoch,
}

/// A bounded, lazily cleaned map of "this user may view this item" answers.
///
/// Clones share one map, so every handler and worker that holds the service sees the same answers
/// and the same invalidations. The map is guarded by a `std` mutex that is never held across an
/// `await`, and it needs neither a background task nor a timer: entries expire by timestamp and are
/// only removed when they are read, overwritten, or when the map needs room.
#[derive(Clone)]
struct PlaybackAccessCache {
    entries: Arc<Mutex<HashMap<PlaybackAccessKey, PlaybackAccessEntry>>>,
    clock: AccessCacheClock,
    ttl: Duration,
    capacity: usize,
}

impl PlaybackAccessCache {
    fn new() -> Self {
        Self::with_clock_and_capacity(Arc::new(Instant::now), PLAYBACK_ACCESS_CACHE_CAPACITY)
    }

    fn with_clock_and_capacity(clock: AccessCacheClock, capacity: usize) -> Self {
        Self {
            entries: Arc::new(Mutex::new(HashMap::new())),
            clock,
            ttl: PLAYBACK_ACCESS_CACHE_TTL,
            capacity,
        }
    }

    /// Reports whether a usable answer for `key` is present, dropping it when it is stale.
    ///
    /// A stale entry is removed on read, which keeps the invalidation lazy: a permission write only
    /// has to move a counter, not walk the map.
    fn is_fresh(&self, epochs: &AccessChangeEpochs, key: &PlaybackAccessKey) -> bool {
        let now = (self.clock)();
        let epoch = epochs.snapshot(&key.user_id);
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match entries.get(key) {
            Some(entry) if entry.expires_at > now && entry.epoch == epoch => true,
            Some(_) => {
                entries.remove(key);
                false
            }
            None => false,
        }
    }

    /// Stores an "allowed" answer until it expires or the change epochs move.
    fn remember(&self, epochs: &AccessChangeEpochs, key: PlaybackAccessKey) {
        let now = (self.clock)();
        let epoch = epochs.snapshot(&key.user_id);
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if entries.len() >= self.capacity {
            // Drop everything that already expired, then the answer closest to expiring, so one
            // insert can never push the map past its capacity.
            entries.retain(|_, entry| entry.expires_at > now);
            if entries.len() >= self.capacity
                && let Some(soonest) = entries
                    .iter()
                    .min_by_key(|(_, entry)| entry.expires_at)
                    .map(|(key, _)| key.clone())
            {
                entries.remove(&soonest);
            }
        }
        entries.insert(
            key,
            PlaybackAccessEntry {
                expires_at: now + self.ttl,
                epoch,
            },
        );
    }

    /// The number of stored entries; only the capacity test needs to look inside.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

#[cfg(test)]
impl MediaAccessService {
    fn with_playback_access_clock(mut self, clock: AccessCacheClock) -> Self {
        self.playback_access_cache.clock = clock;
        self
    }

    fn with_playback_access_capacity(mut self, capacity: usize) -> Self {
        self.playback_access_cache.capacity = capacity;
        self
    }

    fn cached_playback_access_entries(&self) -> usize {
        self.playback_access_cache.len()
    }
}

#[derive(Debug)]
pub enum AccessError {
    Storage(StorageError),
}

impl fmt::Display for AccessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for AccessError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Storage(error) => Some(error),
        }
    }
}

impl From<StorageError> for AccessError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };

    use crate::{
        application::libraries::LibraryService,
        auth::users::UserStore,
        config::Config,
        library::LibraryKind,
        storage::{Database, NewUserContentRestrictions, UserContentRestrictionsCasResult},
    };

    use super::{AccessCacheClock, AccessPrincipal, MediaAccessService, PLAYBACK_ACCESS_CACHE_TTL};

    /// A database with one non-admin viewer, one library, and one classified movie.
    struct PlaybackCacheFixture {
        _directory: tempfile::TempDir,
        database: Database,
        access: MediaAccessService,
        principal: AccessPrincipal,
        library_id: String,
        second_library_id: String,
    }

    async fn playback_cache_fixture() -> Result<PlaybackCacheFixture, Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        })
        .await?;
        let user = UserStore::new(database.clone())?
            .create_user("playback-cache-viewer", "Viewer", "test password", false)
            .await?;
        let libraries = LibraryService::new(database.clone());
        let library = libraries
            .create_library("Playback cache", LibraryKind::Movie, false)
            .await?;
        let library_id = library.id.to_string();
        database
            .set_user_library_access(&user.id.to_string(), &library_id, true)
            .await?;
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title,
                 identification_status, has_available_source
             ) VALUES ('cached-movie', ?, 'MOVIE', 'Cached', 'cached', 'LOCAL_CONFIRMED', 1)",
        )
        .bind(&library_id)
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO media_content_restriction_index (item_id, certification_code)
             VALUES ('cached-movie', 'R')",
        )
        .execute(database.pool())
        .await?;
        // A second granted library keeps `library_access_mode = 'SELECTED'` when one grant is
        // revoked, which is how an administrator usually narrows access.
        let second_library = libraries
            .create_library("Playback cache second", LibraryKind::Movie, false)
            .await?;
        let second_library_id = second_library.id.to_string();
        database
            .set_user_library_access(&user.id.to_string(), &second_library_id, true)
            .await?;
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title,
                 identification_status, has_available_source
             ) VALUES ('cached-second-movie', ?, 'MOVIE', 'Second', 'second', 'LOCAL_CONFIRMED', 1)",
        )
        .bind(&second_library_id)
        .execute(database.pool())
        .await?;
        let access = MediaAccessService::new(database.clone());
        Ok(PlaybackCacheFixture {
            _directory: directory,
            database,
            access,
            principal: AccessPrincipal::new(user.id, false),
            library_id,
            second_library_id,
        })
    }

    /// A hand-advanced clock, so the TTL test does not sleep for a minute.
    #[derive(Clone)]
    struct TestClock(Arc<Mutex<Instant>>);

    impl TestClock {
        fn new() -> Self {
            Self(Arc::new(Mutex::new(Instant::now())))
        }

        fn reader(&self) -> AccessCacheClock {
            let current = Arc::clone(&self.0);
            Arc::new(move || *current.lock().expect("test clock is not poisoned"))
        }

        fn advance(&self, duration: Duration) {
            *self.0.lock().expect("test clock is not poisoned") += duration;
        }
    }

    #[tokio::test]
    async fn playback_access_cache_skips_the_repeated_allowed_check()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = playback_cache_fixture().await?;
        fixture.database.reset_query_count();
        assert!(
            fixture
                .access
                .can_view_item_in_library_cached(
                    fixture.principal,
                    "cached-movie",
                    Some(&fixture.library_id)
                )
                .await?
        );
        let uncached_queries = fixture.database.query_count();
        assert_eq!(
            uncached_queries, 2,
            "the first playback check reads library access and the content-restriction policy"
        );

        fixture.database.reset_query_count();
        assert!(
            fixture
                .access
                .can_view_item_in_library_cached(
                    fixture.principal,
                    "cached-movie",
                    Some(&fixture.library_id)
                )
                .await?
        );
        assert_eq!(
            fixture.database.query_count(),
            0,
            "a repeated allowed check within the TTL must not query the database"
        );
        Ok(())
    }

    #[tokio::test]
    async fn playback_access_cache_never_caches_a_denial() -> Result<(), Box<dyn std::error::Error>>
    {
        let fixture = playback_cache_fixture().await?;
        fixture
            .database
            .replace_user_library_access_mode(&fixture.principal.user_id_string(), "NONE", &[])
            .await?;

        for attempt in 1..=2 {
            fixture.database.reset_query_count();
            assert!(
                !fixture
                    .access
                    .can_view_item_in_library_cached(
                        fixture.principal,
                        "cached-movie",
                        Some(&fixture.library_id)
                    )
                    .await?
            );
            assert!(
                fixture.database.query_count() > 0,
                "attempt {attempt} was denied, and a denial must never be cached"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn playback_access_cache_expires_after_the_ttl() -> Result<(), Box<dyn std::error::Error>>
    {
        let fixture = playback_cache_fixture().await?;
        let clock = TestClock::new();
        let access = fixture
            .access
            .clone()
            .with_playback_access_clock(clock.reader());
        assert!(
            access
                .can_view_item_in_library_cached(
                    fixture.principal,
                    "cached-movie",
                    Some(&fixture.library_id)
                )
                .await?
        );

        fixture.database.reset_query_count();
        assert!(
            access
                .can_view_item_in_library_cached(
                    fixture.principal,
                    "cached-movie",
                    Some(&fixture.library_id)
                )
                .await?
        );
        assert_eq!(fixture.database.query_count(), 0, "the cache is still warm");

        clock.advance(PLAYBACK_ACCESS_CACHE_TTL + Duration::from_millis(1));
        fixture.database.reset_query_count();
        assert!(
            access
                .can_view_item_in_library_cached(
                    fixture.principal,
                    "cached-movie",
                    Some(&fixture.library_id)
                )
                .await?
        );
        assert!(
            fixture.database.query_count() > 0,
            "an expired entry must be re-checked against the database"
        );
        Ok(())
    }

    #[tokio::test]
    async fn revoking_library_access_invalidates_the_cached_answer_immediately()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = playback_cache_fixture().await?;
        assert!(
            fixture
                .access
                .can_view_item_in_library_cached(
                    fixture.principal,
                    "cached-second-movie",
                    Some(&fixture.second_library_id)
                )
                .await?
        );
        fixture.database.reset_query_count();
        assert!(
            fixture
                .access
                .can_view_item_in_library_cached(
                    fixture.principal,
                    "cached-second-movie",
                    Some(&fixture.second_library_id)
                )
                .await?
        );
        assert_eq!(fixture.database.query_count(), 0, "the cache is warm");

        fixture
            .database
            .set_user_library_access(
                &fixture.principal.user_id_string(),
                &fixture.second_library_id,
                false,
            )
            .await?;
        assert!(
            !fixture
                .access
                .can_view_item_in_library_cached(
                    fixture.principal,
                    "cached-second-movie",
                    Some(&fixture.second_library_id)
                )
                .await?,
            "a revoked library must not keep serving the cached allow"
        );
        Ok(())
    }

    #[tokio::test]
    async fn tightening_content_restrictions_invalidates_the_cached_answer_immediately()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = playback_cache_fixture().await?;
        assert!(
            fixture
                .access
                .can_view_item_in_library_cached(
                    fixture.principal,
                    "cached-movie",
                    Some(&fixture.library_id)
                )
                .await?
        );

        let restricted = fixture
            .database
            .compare_and_swap_user_content_restrictions(
                &fixture.principal.user_id_string(),
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
            restricted,
            UserContentRestrictionsCasResult::Updated(_)
        ));
        assert!(
            !fixture
                .access
                .can_view_item_in_library_cached(
                    fixture.principal,
                    "cached-movie",
                    Some(&fixture.library_id)
                )
                .await?,
            "a new content policy must revoke the cached allow immediately"
        );
        Ok(())
    }

    #[tokio::test]
    async fn playback_access_cache_never_shares_entries_between_users()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = playback_cache_fixture().await?;
        let other_user = UserStore::new(fixture.database.clone())?
            .create_user("playback-cache-other", "Other", "test password", false)
            .await?;
        // New users default to `library_access_mode = 'ALL'`, so deny this one explicitly.
        fixture
            .database
            .replace_user_library_access_mode(&other_user.id.to_string(), "NONE", &[])
            .await?;
        let other_principal = AccessPrincipal::new(other_user.id, false);
        assert!(
            fixture
                .access
                .can_view_item_in_library_cached(
                    fixture.principal,
                    "cached-movie",
                    Some(&fixture.library_id)
                )
                .await?
        );

        fixture.database.reset_query_count();
        assert!(
            !fixture
                .access
                .can_view_item_in_library_cached(
                    other_principal,
                    "cached-movie",
                    Some(&fixture.library_id)
                )
                .await?,
            "a second user without that library must not inherit the cached allow"
        );
        assert!(
            fixture.database.query_count() > 0,
            "the second user's answer must come from the database"
        );

        fixture.database.reset_query_count();
        assert!(
            fixture
                .access
                .can_view_item_in_library_cached(
                    fixture.principal,
                    "cached-movie",
                    Some(&fixture.library_id)
                )
                .await?
        );
        assert_eq!(
            fixture.database.query_count(),
            0,
            "the first user's entry must survive the second user's denied check"
        );
        Ok(())
    }

    #[tokio::test]
    async fn playback_access_cache_is_bounded() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = playback_cache_fixture().await?;
        let access = fixture.access.clone().with_playback_access_capacity(2);
        for item_id in ["bounded-a", "bounded-b", "bounded-c"] {
            assert!(
                access
                    .can_view_item_in_library_cached(
                        fixture.principal,
                        item_id,
                        Some(&fixture.library_id)
                    )
                    .await?
            );
        }
        assert_eq!(
            access.cached_playback_access_entries(),
            2,
            "the cache must never grow past its configured capacity"
        );
        Ok(())
    }

    #[tokio::test]
    async fn administrators_and_collection_items_stay_uncached()
    -> Result<(), Box<dyn std::error::Error>> {
        let fixture = playback_cache_fixture().await?;
        let admin = AccessPrincipal::new(fixture.principal.user_id.expect("viewer id"), true);
        assert!(
            fixture
                .access
                .can_view_item_in_library_cached(admin, "cached-movie", Some(&fixture.library_id))
                .await?
        );
        assert!(
            !fixture
                .access
                .can_view_item_in_library_cached(
                    fixture.principal,
                    "cached-movie",
                    Some(crate::storage::GLOBAL_COLLECTION_LIBRARY_ID)
                )
                .await?
        );
        assert_eq!(
            fixture.access.cached_playback_access_entries(),
            0,
            "administrators and the global collection library never need a cache entry"
        );
        Ok(())
    }

    #[tokio::test]
    async fn batched_accessible_library_ids_match_single_principals()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        })
        .await?;
        let users = UserStore::new(database.clone())?;
        let admin = users
            .create_user("batch-scope-admin", "Admin", "test password", true)
            .await?;
        let all_user = users
            .create_user("batch-scope-all", "All", "test password", false)
            .await?;
        let selected_user = users
            .create_user("batch-scope-selected", "Selected", "test password", false)
            .await?;
        let libraries = LibraryService::new(database.clone());
        libraries
            .create_library("Batch scope A", LibraryKind::Movie, false)
            .await?;
        let library_b = libraries
            .create_library("Batch scope B", LibraryKind::Movie, false)
            .await?;
        database
            .set_user_library_access(
                &selected_user.id.to_string(),
                &library_b.id.to_string(),
                true,
            )
            .await?;
        let service = MediaAccessService::new(database.clone());
        let requests = vec![
            (admin.id.to_string(), true),
            (all_user.id.to_string(), false),
            (selected_user.id.to_string(), false),
        ];
        let batched = service.accessible_library_ids_for_users(&requests).await?;
        for (user_id, is_admin) in &requests {
            let single = service
                .accessible_library_ids(AccessPrincipal::new(
                    (*user_id).parse::<crate::domain::ids::UserId>()?,
                    *is_admin,
                ))
                .await?;
            assert_eq!(
                batched.get(user_id).cloned().unwrap_or_default(),
                single,
                "batched accessible libraries must match the single lookup for {user_id}"
            );
        }
        assert_eq!(
            batched.get(&admin.id.to_string()).map(Vec::len),
            Some(2),
            "administrators see every enabled media library"
        );
        assert_eq!(
            batched.get(&selected_user.id.to_string()).map(Vec::len),
            Some(1),
            "selected users see only their granted libraries"
        );
        database.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn content_policy_denies_direct_item_access_and_inherits_parent_restrictions()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        })
        .await?;
        let user = UserStore::new(database.clone())?
            .create_user("direct-policy-viewer", "Viewer", "test password", false)
            .await?;
        let library = LibraryService::new(database.clone())
            .create_library("Direct policy series", LibraryKind::Series, false)
            .await?;
        let user_id = user.id.to_string();
        let library_id = library.id.to_string();
        database
            .set_user_library_access(&user_id, &library_id, true)
            .await?;
        for (item_id, item_type, parent_id, series_id, certification) in [
            ("policy-series", "SERIES", None, None, "R"),
            (
                "policy-season",
                "SEASON",
                Some("policy-series"),
                Some("policy-series"),
                "G",
            ),
            (
                "policy-episode",
                "EPISODE",
                Some("policy-season"),
                Some("policy-series"),
                "G",
            ),
        ] {
            sqlx::query(
                "INSERT INTO media_items (
                     id, library_id, item_type, parent_id, series_id, title,
                     sort_title, identification_status, has_available_source
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, 'LOCAL_CONFIRMED', 1)",
            )
            .bind(item_id)
            .bind(&library_id)
            .bind(item_type)
            .bind(parent_id)
            .bind(series_id)
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
        }
        sqlx::query(
            "INSERT INTO media_content_restriction_tags (item_id, tag)
             VALUES ('policy-episode', 'violent')",
        )
        .execute(database.pool())
        .await?;

        let access = MediaAccessService::new(database.clone());
        let principal = AccessPrincipal::new(user.id, false);
        assert!(access.can_view_item(principal, "policy-episode").await?);

        database.reset_query_count();
        assert!(access.can_view_item(principal, "policy-episode").await?);
        assert_eq!(
            database.query_count(),
            2,
            "a warm policy cache re-reads only the item library and the library access, not the policy"
        );

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
        assert!(!access.can_view_item(principal, "policy-episode").await?);

        sqlx::query(
            "UPDATE media_content_restriction_index
             SET certification_code = 'PG' WHERE item_id = 'policy-series'",
        )
        .execute(database.pool())
        .await?;
        assert!(access.can_view_item(principal, "policy-episode").await?);
        let tag_restriction = database
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
            tag_restriction,
            UserContentRestrictionsCasResult::Updated(_)
        ));
        assert!(!access.can_view_item(principal, "policy-episode").await?);

        sqlx::query("DELETE FROM media_content_restriction_index WHERE item_id = 'policy-series'")
            .execute(database.pool())
            .await?;
        assert!(!access.can_view_item(principal, "policy-episode").await?);
        Ok(())
    }

    #[tokio::test]
    async fn authorized_playback_source_respects_content_restrictions()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        })
        .await?;
        let user = UserStore::new(database.clone())?
            .create_user("playback-policy-viewer", "Viewer", "test password", false)
            .await?;
        let library = LibraryService::new(database.clone())
            .create_library("Playback policy", LibraryKind::Movie, false)
            .await?;
        let user_id = user.id.to_string();
        let library_id = library.id.to_string();
        database
            .set_user_library_access(&user_id, &library_id, true)
            .await?;
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title,
                 identification_status, has_available_source
             ) VALUES ('policy-movie', ?, 'MOVIE', 'Policy', 'policy', 'LOCAL_CONFIRMED', 1)",
        )
        .bind(&library_id)
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO media_content_restriction_index (item_id, certification_code)
             VALUES ('policy-movie', 'R')",
        )
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO library_roots (
                 id, library_id, canonical_path, display_path, is_available, is_writable
             ) VALUES ('policy-root', ?, '/media/policy', '/media/policy', 1, 1)",
        )
        .bind(&library_id)
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO filesystem_entries (
                 id, library_root_id, relative_path, entry_kind, size, modified_at,
                 last_seen_generation, is_missing
             ) VALUES ('policy-entry', 'policy-root', 'Policy.mkv', 'FILE', 1, 1, 'gen', 0)",
        )
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO media_sources (id, item_id, source_kind, filesystem_entry_id, is_default)
             VALUES ('policy-source', 'policy-movie', 'LOCAL_FILE', 'policy-entry', 1)",
        )
        .execute(database.pool())
        .await?;

        let access = MediaAccessService::new(database.clone());
        let principal = AccessPrincipal::new(user.id, false);
        assert!(access.can_view_item(principal, "policy-movie").await?);
        assert!(
            access
                .authorized_playback_source(principal, "policy-movie", None)
                .await?
                .is_some()
        );

        let restricted = database
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
            restricted,
            UserContentRestrictionsCasResult::Updated(_)
        ));
        assert!(!access.can_view_item(principal, "policy-movie").await?);
        assert!(
            access
                .authorized_playback_source(principal, "policy-movie", None)
                .await?
                .is_none(),
            "playback authorization must not bypass content restrictions"
        );
        Ok(())
    }

    async fn library_fixture() -> Result<
        (
            Database,
            MediaAccessService,
            AccessPrincipal,
            String,
            (String, String),
        ),
        Box<dyn std::error::Error>,
    > {
        let directory = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        })
        .await?;
        Box::leak(Box::new(directory));
        let user = UserStore::new(database.clone())?
            .create_user("access-cache-viewer", "Viewer", "test password", false)
            .await?;
        let libraries = LibraryService::new(database.clone());
        let first = libraries
            .create_library("Access cache A", LibraryKind::Movie, false)
            .await?;
        let second = libraries
            .create_library("Access cache B", LibraryKind::Movie, false)
            .await?;
        let service = MediaAccessService::new(database.clone());
        Ok((
            database,
            service,
            AccessPrincipal::new(user.id, false),
            user.id.to_string(),
            (first.id.to_string(), second.id.to_string()),
        ))
    }

    #[tokio::test]
    async fn library_access_changes_apply_to_the_very_next_request()
    -> Result<(), Box<dyn std::error::Error>> {
        let (database, access, principal, user_id, (first, second)) = library_fixture().await?;

        // A fresh user sees every enabled library; a selection then narrows the set.
        assert_eq!(access.accessible_library_ids(principal).await?.len(), 2);
        database
            .set_user_library_access(&user_id, &first, true)
            .await?;
        assert_eq!(
            access.accessible_library_ids(principal).await?,
            vec![first.clone()]
        );

        database
            .set_user_library_access(&user_id, &second, true)
            .await?;
        let refreshed = access.accessible_library_ids(principal).await?;
        assert!(
            refreshed.contains(&second),
            "granting a library must be visible to the very next request"
        );

        // Revoking it again is equally immediate.
        database
            .set_user_library_access(&user_id, &second, false)
            .await?;
        assert!(
            !access
                .accessible_library_ids(principal)
                .await?
                .contains(&second)
        );
        Ok(())
    }

    #[tokio::test]
    async fn library_access_changes_deny_the_very_next_item_check()
    -> Result<(), Box<dyn std::error::Error>> {
        let (database, access, principal, user_id, (first, second)) = library_fixture().await?;
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title,
                 identification_status, has_available_source
             ) VALUES ('access-cache-movie', ?, 'MOVIE', 'Cached', 'cached', 'LOCAL_CONFIRMED', 1)",
        )
        .bind(&first)
        .execute(database.pool())
        .await?;
        // A selection keeps the user in SELECTED mode, so revoking one library below narrows the
        // set instead of falling back to "all enabled libraries".
        database
            .set_user_library_access(&user_id, &second, true)
            .await?;
        database
            .set_user_library_access(&user_id, &first, true)
            .await?;
        assert!(
            access
                .can_view_item(principal, "access-cache-movie")
                .await?
        );

        // Revoking the library must deny the item on the very next check, cache or not.
        database
            .set_user_library_access(&user_id, &first, false)
            .await?;
        assert!(
            !access
                .can_view_item(principal, "access-cache-movie")
                .await?
        );
        Ok(())
    }

    #[tokio::test]
    async fn library_disable_denies_the_very_next_request() -> Result<(), Box<dyn std::error::Error>>
    {
        use crate::storage::LibrarySettingsUpdate;

        let (database, access, principal, _user_id, (first, _second)) = library_fixture().await?;
        assert!(access.can_view_library(principal, &first).await?);

        database
            .update_library_settings(
                &first,
                LibrarySettingsUpdate {
                    name: None,
                    kind: None,
                    is_enabled: Some(false),
                    realtime_watch_enabled: None,
                    realtime_metadata_auto_match_enabled: None,
                    reconciliation_schedule: None,
                    metadata_schedule: None,
                    scan_concurrency: None,
                    probe_concurrency: None,
                    scraper_id: None,
                    scrapers: None,
                    chapter_source_id: None,
                    media_strategy_json: None,
                },
            )
            .await?;
        assert!(
            !access.can_view_library(principal, &first).await?,
            "disabling a library must deny access on the very next request"
        );
        Ok(())
    }

    #[tokio::test]
    async fn resolved_library_access_check_skips_the_item_lookup()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        })
        .await?;
        let user = UserStore::new(database.clone())?
            .create_user("resolved-library-viewer", "Viewer", "test password", false)
            .await?;
        let library = LibraryService::new(database.clone())
            .create_library("Resolved library", LibraryKind::Movie, false)
            .await?;
        let library_id = library.id.to_string();
        database
            .set_user_library_access(&user.id.to_string(), &library_id, true)
            .await?;
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title,
                 identification_status, has_available_source
             ) VALUES ('resolved-movie', ?, 'MOVIE', 'Resolved', 'resolved', 'LOCAL_CONFIRMED', 1)",
        )
        .bind(&library_id)
        .execute(database.pool())
        .await?;

        let access = MediaAccessService::new(database.clone());
        let principal = AccessPrincipal::new(user.id, false);
        // The first call fills the access context; the counts below measure the steady state.
        assert!(
            access
                .can_view_item_in_library(principal, "resolved-movie", Some(&library_id))
                .await?
        );
        database.reset_query_count();
        assert!(
            access
                .can_view_item_in_library(principal, "resolved-movie", Some(&library_id))
                .await?
        );
        assert_eq!(
            database.query_count(),
            1,
            "a warm policy cache checks library access but does not re-read the policy"
        );

        database.reset_query_count();
        assert!(access.can_view_item(principal, "resolved-movie").await?);
        assert_eq!(
            database.query_count(),
            2,
            "can_view_item re-resolves the item library; the resolved variant must not"
        );

        database.reset_query_count();
        assert!(
            !access
                .can_view_item_in_library(principal, "resolved-movie", None)
                .await?,
            "an unresolved library denies access exactly like a hidden item"
        );
        assert_eq!(
            database.query_count(),
            0,
            "denying an unresolved library must not fall back to the item lookup"
        );
        Ok(())
    }
}
