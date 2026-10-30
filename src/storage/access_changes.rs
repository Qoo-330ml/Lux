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
//! registry keeps no SQL and no data, only two counters per user plus one global counter.

use std::{
    collections::HashMap,
    sync::{
        Mutex,
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
}

#[cfg(test)]
mod tests {
    use super::AccessChangeEpochs;

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
}
