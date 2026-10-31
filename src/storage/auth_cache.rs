//! Invalidation epochs for the cached request principals.
//!
//! [`crate::auth`] caches the credential of a resolved request (the Web session row, or the Emby
//! token with its device identity). A short TTL keeps that cache fresh, but a TTL alone would let a
//! revoked token, a disabled account or a deleted user stay effective for up to one TTL. Every
//! storage write whose result such a decision depends on therefore bumps one of the counters below,
//! and a cached principal is only reused while the snapshot it was built under is still current.
//!
//! Correctness rules for the write sites:
//!
//! * Bump *after* the write is committed, never before.
//! * The cache captures [`AuthCacheEpochs::principal_snapshot`] *before* its database read and only
//!   stores the value when that snapshot is still current afterwards, so a write that committed
//!   while the read was in flight can never leave a stale value behind.
//!
//! The counters are plain in-memory values: one atomic increment per write, one load per cache
//! lookup, nothing persisted. Library visibility and per-user access decisions are tracked
//! separately by [`super::AccessChangeEpochs`], which the media access caches use.

use std::sync::atomic::{AtomicU64, Ordering};

/// The epochs a cached principal was built under. All three are global counters: a principal does
/// not know its user before the credential is read, so the per-user generation is folded into one
/// counter. User writes are rare, so the extra misses cost nothing measurable.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct AuthCacheEpochSnapshot {
    /// Advanced whenever any session or access token is revoked.
    pub(crate) credentials: u64,
    /// Advanced whenever an access (blacklist) rule is created, enabled, disabled or deleted.
    pub(crate) rules: u64,
    /// Advanced whenever a user row changes in a way authentication depends on.
    pub(crate) users: u64,
}

/// Process-wide epoch counters shared by every clone of one [`super::Database`].
#[derive(Debug, Default)]
pub(crate) struct AuthCacheEpochs {
    credentials: AtomicU64,
    rules: AtomicU64,
    users: AtomicU64,
}

impl AuthCacheEpochs {
    /// Snapshots the epochs a cache fill is about to read under.
    ///
    /// Callers must take the snapshot *before* the database read and store it with the value; the
    /// entry is only reused while the snapshot is still current.
    pub(crate) fn principal_snapshot(&self) -> AuthCacheEpochSnapshot {
        AuthCacheEpochSnapshot {
            credentials: self.credentials.load(Ordering::SeqCst),
            rules: self.rules.load(Ordering::SeqCst),
            users: self.users.load(Ordering::SeqCst),
        }
    }

    /// Whether a cached principal built under `snapshot` is still current.
    pub(crate) fn principal_is_current(&self, snapshot: AuthCacheEpochSnapshot) -> bool {
        snapshot == self.principal_snapshot()
    }

    /// Invalidates every cached principal: a session or access token was revoked.
    pub(crate) fn invalidate_credentials(&self) {
        self.credentials.fetch_add(1, Ordering::SeqCst);
    }

    /// Invalidates every cached principal: an access rule changed, including global rules.
    ///
    /// The resolvers read the rule snapshot per request, so a rule change is visible immediately
    /// either way; the counter additionally drops cached credentials, so no cached value can
    /// outlive a rule change even if a future caller starts caching rule-derived state.
    pub(crate) fn invalidate_rules(&self) {
        self.rules.fetch_add(1, Ordering::SeqCst);
    }

    /// Invalidates every cached principal: a user row changed (disable, delete, permissions,
    /// password, profile).
    pub(crate) fn invalidate_users(&self) {
        self.users.fetch_add(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::AuthCacheEpochs;

    #[test]
    fn credential_and_rule_invalidations_advance_the_principal_epoch() {
        let epochs = AuthCacheEpochs::default();
        let before = epochs.principal_snapshot();
        assert!(epochs.principal_is_current(before));

        epochs.invalidate_credentials();
        assert!(!epochs.principal_is_current(before));

        let before = epochs.principal_snapshot();
        epochs.invalidate_rules();
        assert!(!epochs.principal_is_current(before));
    }

    #[test]
    fn user_invalidations_advance_the_principal_epoch() {
        let epochs = AuthCacheEpochs::default();
        let before = epochs.principal_snapshot();

        epochs.invalidate_users();

        assert!(!epochs.principal_is_current(before));
    }

    #[test]
    fn a_fresh_snapshot_of_an_unchanged_registry_stays_current() {
        let epochs = AuthCacheEpochs::default();
        epochs.invalidate_users();
        let snapshot = epochs.principal_snapshot();

        assert!(epochs.principal_is_current(snapshot));
        assert!(epochs.principal_is_current(epochs.principal_snapshot()));
    }
}
