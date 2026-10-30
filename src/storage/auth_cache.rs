//! Invalidation epochs for the process-local authentication and access caches.
//!
//! [`crate::auth`] caches resolved principals and [`crate::application::access`] caches per-user
//! access contexts. A short TTL keeps those caches fresh, but a TTL alone would let a revoked
//! token, a disabled account or a changed access rule stay effective for up to one TTL. Every
//! storage write whose result those decisions depend on therefore bumps one of the counters below,
//! and a cached value is only reused while the epochs it was built from are still current.
//!
//! Correctness rules for the write sites:
//!
//! * Bump *after* the write is committed, never before.
//! * A cache fill captures [`AuthCacheEpochs::write_fence`] before its read and only stores the
//!   value when the fence is unchanged afterwards, so a write that committed during the read can
//!   never be hidden by a value built from pre-write data.
//! * Bump for every write that can change a principal (revocation, disable, delete, permission or
//!   password change, access rule change) or an access context (library access, content
//!   restrictions, library visibility).
//!
//! The epochs are plain in-memory counters: one atomic increment per write, one load per cache
//! lookup, nothing persisted.

use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

/// Upper bound for [`AuthCacheEpochs::users`]; the map holds one entry per user that changed, so it
/// is bounded by the user count in practice. Clearing it on overflow only makes cached values
/// stale, which is the safe direction.
const MAX_USER_EPOCH_ENTRIES: usize = 16_384;

/// The epochs a cached authentication or access value was built from.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct AuthCacheEpochSnapshot {
    /// Advanced whenever any session or access token is revoked or deleted.
    pub(crate) credentials: u64,
    /// Advanced whenever an access (blacklist) rule is created, enabled, disabled or deleted.
    pub(crate) rules: u64,
    /// Advanced whenever a media library becomes visible or invisible to principals.
    pub(crate) libraries: u64,
    /// The generation of the user this snapshot belongs to.
    pub(crate) user: u64,
}

/// Process-wide epoch counters shared by every clone of one [`super::Database`].
#[derive(Debug, Default)]
pub(crate) struct AuthCacheEpochs {
    credentials: AtomicU64,
    rules: AtomicU64,
    libraries: AtomicU64,
    /// Bumped by every invalidation, so a cache fill can prove that no write raced its read.
    writes: AtomicU64,
    users: Mutex<HashMap<String, u64>>,
}

impl AuthCacheEpochs {
    /// Snapshots the epochs a cache read is about to build its value from.
    ///
    /// Callers must take the snapshot *after* the database read and only store it when the
    /// [`Self::write_fence`] they captured before the read is still current, so a write that
    /// committed while the read was in flight can never leave a stale value behind.
    pub(crate) fn snapshot(&self, user_id: &str) -> AuthCacheEpochSnapshot {
        AuthCacheEpochSnapshot {
            credentials: self.credentials.load(Ordering::SeqCst),
            rules: self.rules.load(Ordering::SeqCst),
            libraries: self.libraries.load(Ordering::SeqCst),
            user: self.user_epoch(user_id),
        }
    }

    /// A fingerprint of every invalidation, captured before a cache-filling read.
    pub(crate) fn write_fence(&self) -> u64 {
        self.writes.load(Ordering::SeqCst)
    }

    /// Whether no invalidation happened since `fence` was captured.
    pub(crate) fn fence_is_current(&self, fence: u64) -> bool {
        self.writes.load(Ordering::SeqCst) == fence
    }

    /// Whether a cached principal of `user_id` is still current.
    pub(crate) fn principal_is_current(
        &self,
        snapshot: AuthCacheEpochSnapshot,
        user_id: &str,
    ) -> bool {
        snapshot.credentials == self.credentials.load(Ordering::SeqCst)
            && snapshot.rules == self.rules.load(Ordering::SeqCst)
            && snapshot.user == self.user_epoch(user_id)
    }

    /// Whether a cached access context of `user_id` is still current.
    pub(crate) fn access_is_current(
        &self,
        snapshot: AuthCacheEpochSnapshot,
        user_id: &str,
    ) -> bool {
        snapshot.libraries == self.libraries.load(Ordering::SeqCst)
            && snapshot.user == self.user_epoch(user_id)
    }

    /// Invalidates every cached principal: a session or access token was revoked.
    pub(crate) fn invalidate_credentials(&self) {
        self.credentials.fetch_add(1, Ordering::SeqCst);
        self.writes.fetch_add(1, Ordering::SeqCst);
    }

    /// Invalidates every cached principal: an access rule changed, including global rules.
    pub(crate) fn invalidate_rules(&self) {
        self.rules.fetch_add(1, Ordering::SeqCst);
        self.writes.fetch_add(1, Ordering::SeqCst);
    }

    /// Invalidates every cached access context: a library became visible or invisible.
    pub(crate) fn invalidate_libraries(&self) {
        self.libraries.fetch_add(1, Ordering::SeqCst);
        self.writes.fetch_add(1, Ordering::SeqCst);
    }

    /// Invalidates everything cached for one user.
    pub(crate) fn invalidate_user(&self, user_id: &str) {
        self.writes.fetch_add(1, Ordering::SeqCst);
        let mut users = self
            .users
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if users.len() >= MAX_USER_EPOCH_ENTRIES && !users.contains_key(user_id) {
            // Dropping the whole map invalidates every entry, which is always safe.
            users.clear();
        }
        let epoch = users.entry(user_id.to_owned()).or_insert(0);
        *epoch = epoch.wrapping_add(1);
    }

    fn user_epoch(&self, user_id: &str) -> u64 {
        let users = self
            .users
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        users.get(user_id).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::AuthCacheEpochs;

    #[test]
    fn user_invalidations_only_advance_the_affected_user() {
        let epochs = AuthCacheEpochs::default();
        let before = epochs.snapshot("user-a");

        epochs.invalidate_user("user-b");

        assert!(epochs.principal_is_current(before, "user-a"));
        assert!(epochs.access_is_current(before, "user-a"));
        let after = epochs.snapshot("user-b");
        assert!(!epochs.principal_is_current(before, "user-b"));
        assert!(!epochs.access_is_current(before, "user-b"));
        assert_eq!(after.user, 1);
    }

    #[test]
    fn credential_and_rule_invalidations_only_affect_principals() {
        let epochs = AuthCacheEpochs::default();
        let before = epochs.snapshot("user-a");

        epochs.invalidate_credentials();
        assert!(!epochs.principal_is_current(before, "user-a"));
        assert!(epochs.access_is_current(before, "user-a"));

        let before = epochs.snapshot("user-a");
        epochs.invalidate_rules();
        assert!(!epochs.principal_is_current(before, "user-a"));
        assert!(epochs.access_is_current(before, "user-a"));
    }

    #[test]
    fn library_invalidations_only_affect_access_contexts() {
        let epochs = AuthCacheEpochs::default();
        let before = epochs.snapshot("user-a");

        epochs.invalidate_libraries();

        assert!(epochs.principal_is_current(before, "user-a"));
        assert!(!epochs.access_is_current(before, "user-a"));
    }

    #[test]
    fn every_invalidation_advances_the_write_fence() {
        let epochs = AuthCacheEpochs::default();
        let fence = epochs.write_fence();
        assert!(epochs.fence_is_current(fence));

        epochs.invalidate_user("user-a");
        assert!(!epochs.fence_is_current(fence));

        let fence = epochs.write_fence();
        epochs.invalidate_credentials();
        assert!(!epochs.fence_is_current(fence));

        let fence = epochs.write_fence();
        epochs.invalidate_rules();
        assert!(!epochs.fence_is_current(fence));

        let fence = epochs.write_fence();
        epochs.invalidate_libraries();
        assert!(!epochs.fence_is_current(fence));
    }

    #[test]
    fn overflow_drops_the_user_map_and_invalidates_every_user() {
        let epochs = AuthCacheEpochs::default();
        epochs.invalidate_user("user-a");
        let before = epochs.snapshot("user-a");
        assert_eq!(before.user, 1);

        {
            let mut users = epochs.users.lock().expect("user epochs lock");
            users.clear();
            for index in 0..super::MAX_USER_EPOCH_ENTRIES {
                users.insert(format!("filler-{index}"), 1);
            }
        }
        epochs.invalidate_user("user-b");

        assert!(
            !epochs.access_is_current(before, "user-a"),
            "clearing the map must invalidate the previous generations"
        );
    }
}
