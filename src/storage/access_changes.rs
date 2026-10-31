//! Change counters that tell caches when a permission write invalidated them.
//!
//! The playback progress path is the one place that caches an access decision (see
//! `crate::application::access`), because an Emby client or the Lux web player repeats the same
//! "may this user view this item" check every ~10 s for the whole session. A short TTL alone would
//! let a revoked library or a tightened parental policy keep playing for up to that TTL, so every
//! storage write that can change the answer bumps a counter here and the cache treats a counter
//! mismatch as a miss.
//!
//! The counters live next to the writes instead of next to the cache on purpose: every writer goes
//! through these storage functions, so a new call site cannot silently forget to invalidate. The
//! registry keeps no SQL and no data, only two counters per user plus one global counter and one
//! counter per access setting (currently the trusted-proxy CIDR list).

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

/// The counters a cached access decision was computed under.
///
/// Two counters instead of one keep per-user invalidation cheap (no scan of every cached entry)
/// while still letting a library-wide change invalidate every user at once.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct AccessEpoch {
    /// Bumped by a change that can affect every user, such as a library being disabled or deleted.
    pub(crate) global: u64,
    /// Bumped by a change that only affects the user that owns this epoch.
    pub(crate) user: u64,
}

/// Monotonic change counters owned by [`crate::storage::Database`].
///
/// A `Database` clone shares the same counters, so a write observed through any clone invalidates
/// entries created through any other clone. Nothing here is persisted: the counters only have to
/// be comparable within the lifetime of one process.
#[derive(Debug, Default)]
pub(crate) struct AccessChangeEpochs {
    global: AtomicU64,
    users: Mutex<HashMap<String, u64>>,
    /// Advanced by a write to a setting that the access-IP resolver reads.
    trusted_proxy_cidrs: AtomicU64,
}

impl AccessChangeEpochs {
    /// Reads the counters a cache entry for `user_id` must match to stay usable.
    pub(crate) fn snapshot(&self, user_id: &str) -> AccessEpoch {
        let global = self.global.load(Ordering::Relaxed);
        let users = self
            .users
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        AccessEpoch {
            global,
            user: users.get(user_id).copied().unwrap_or_default(),
        }
    }

    /// Marks every cached decision for `user_id` as stale.
    ///
    /// Every mutation of `user_library_access`, of the user's library access mode, or of the user's
    /// content restrictions must call this, including the copy and preset paths.
    pub(crate) fn invalidate_user(&self, user_id: &str) {
        let mut users = self
            .users
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let epoch = users.entry(user_id.to_owned()).or_insert(0);
        *epoch = epoch.wrapping_add(1);
    }

    /// Marks every cached decision for every user as stale.
    ///
    /// Used by the library lifecycle writes: `Database::has_user_library_access` also requires the
    /// library to be enabled and not deleted, so disabling or deleting one invalidates the cached
    /// answer of every user that could see it.
    pub(crate) fn invalidate_all(&self) {
        self.global.fetch_add(1, Ordering::Relaxed);
    }

    /// The counter a cached trusted-proxy CIDR list must match to stay usable.
    ///
    /// Callers snapshot this *before* reading the setting and only publish the value while the
    /// counter still matches, so a write that commits during the read cannot be cached over.
    pub(crate) fn trusted_proxy_cidrs_epoch(&self) -> u64 {
        self.trusted_proxy_cidrs.load(Ordering::SeqCst)
    }

    /// Marks every cached trusted-proxy CIDR list as stale.
    ///
    /// The access-IP resolver reads this list for every protected request that carries a forwarded
    /// chain, so the raw JSON and its normalized form are cached per instance. Every writer of the
    /// setting must call this *after* its write committed: reads that start once the writer returned
    /// then see the new counter and refill from the database.
    pub(crate) fn invalidate_trusted_proxy_cidrs(&self) {
        self.trusted_proxy_cidrs.fetch_add(1, Ordering::SeqCst);
    }
}

/// One instance's cached copy of the trusted-proxy CIDR setting.
///
/// The parsed and normalized list is cached because the access-IP resolver needs it on every
/// protected request that arrives with a forwarded chain, while administrators change it rarely.
/// The value is keyed by the database instance it was read from plus
/// [`AccessChangeEpochs::trusted_proxy_cidrs_epoch`] at read time:
///
/// * a write bumps the epoch, so the next request misses and refills - the same "bump after the
///   commit, store under the snapshot" contract the other caches in this crate follow;
/// * two databases (or two tests running in parallel) never share an entry, so neither can serve
///   the other's setting.
///
/// The cache returns the normalized list, exactly what a database read returns; it does not change
/// which forwarded values the resolver accepts.
#[derive(Clone, Debug, Default)]
pub(crate) struct TrustedProxyCidrCache {
    cached: Arc<Mutex<Option<CachedTrustedProxyCidrs>>>,
}

#[derive(Debug)]
struct CachedTrustedProxyCidrs {
    /// `Database::server_id`, so an entry is only reused by the database it was read from.
    database: String,
    /// The epoch the value was read under.
    epoch: u64,
    /// The normalized list the database returned.
    values: Vec<String>,
}

impl TrustedProxyCidrCache {
    /// The cached list, when it was read from `database` under `epoch`.
    pub(crate) fn get(&self, database: &str, epoch: u64) -> Option<Vec<String>> {
        let cached = self
            .cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = cached.as_ref()?;
        (entry.epoch == epoch && entry.database == database).then(|| entry.values.clone())
    }

    /// Publishes a freshly read list for `database` under `epoch`.
    ///
    /// Callers must only pass an epoch that is still current; a value stored under an outdated
    /// epoch is unreachable but would make the invariant harder to audit, so the caller checks
    /// [`AccessChangeEpochs::trusted_proxy_cidrs_epoch`] again before calling this.
    pub(crate) fn insert(&self, database: &str, epoch: u64, values: Vec<String>) {
        let mut cached = self
            .cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *cached = Some(CachedTrustedProxyCidrs {
            database: database.to_owned(),
            epoch,
            values,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{AccessChangeEpochs, TrustedProxyCidrCache};

    #[test]
    fn user_invalidation_only_moves_that_users_epoch() {
        let epochs = AccessChangeEpochs::default();
        let before = epochs.snapshot("epoch-user-a");
        epochs.invalidate_user("epoch-user-b");

        assert_eq!(epochs.snapshot("epoch-user-a"), before);
        assert_eq!(
            epochs.snapshot("epoch-user-b").user,
            before.user + 1,
            "invalidating one user must not move another user's epoch"
        );
        assert_eq!(
            epochs.snapshot("epoch-user-b").global,
            before.global,
            "a per-user invalidation must not move the global epoch"
        );
    }

    #[test]
    fn global_invalidation_moves_every_users_epoch() {
        let epochs = AccessChangeEpochs::default();
        let before = epochs.snapshot("epoch-user-a");
        epochs.invalidate_all();

        let after = epochs.snapshot("epoch-user-a");
        assert_eq!(after.global, before.global + 1);
        assert_eq!(after.user, before.user);
        assert_ne!(after, before);
    }

    #[test]
    fn trusted_proxy_invalidation_advances_only_its_own_epoch() {
        let epochs = AccessChangeEpochs::default();
        let before = epochs.trusted_proxy_cidrs_epoch();
        let before_snapshot = epochs.snapshot("epoch-user-a");

        epochs.invalidate_trusted_proxy_cidrs();

        assert_eq!(epochs.trusted_proxy_cidrs_epoch(), before + 1);
        assert_eq!(
            epochs.snapshot("epoch-user-a"),
            before_snapshot,
            "a trusted-proxy write must not invalidate per-user access decisions"
        );
    }

    #[test]
    fn trusted_proxy_cache_only_serves_the_epoch_and_database_it_was_stored_for() {
        let cache = TrustedProxyCidrCache::default();
        assert_eq!(cache.get("database-a", 7), None);

        cache.insert("database-a", 7, vec!["10.0.0.0/8".to_owned()]);

        assert_eq!(
            cache.get("database-a", 7),
            Some(vec!["10.0.0.0/8".to_owned()])
        );
        assert_eq!(
            cache.get("database-a", 8),
            None,
            "a write bumped the epoch, so the entry must be a miss"
        );
        assert_eq!(
            cache.get("database-b", 7),
            None,
            "another database must never see this entry"
        );
    }

    #[test]
    fn a_clone_of_the_trusted_proxy_cache_shares_its_entries() {
        let cache = TrustedProxyCidrCache::default();
        let clone = cache.clone();
        cache.insert("database-a", 1, vec!["192.0.2.0/24".to_owned()]);

        assert_eq!(
            clone.get("database-a", 1),
            Some(vec!["192.0.2.0/24".to_owned()])
        );
    }
}
