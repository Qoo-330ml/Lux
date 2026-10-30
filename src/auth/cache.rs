//! Bounded, short-lived cache shared by the request authentication and media access caches.
//!
//! The Web session resolver, the Emby token resolver and [`crate::application::access`] all repeat
//! the same small database reads on every request. The TTL below removes those round trips for
//! back-to-back requests (a browser opens many connections per page), while
//! [`crate::storage::AuthCacheEpochs`] keeps the result correct: every credential, user, rule and
//! library write bumps an epoch, and a cached value is only reused while its snapshot is current.
//!
//! Secrets and decisions are never cached. The cached payload is data (a session row, a user, a
//! rule list, an access context) and every request re-evaluates the address-dependent rules with
//! the address the access-IP resolver produced.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crate::storage::AuthCacheEpochSnapshot;

/// How long a cached value may be reused. Short enough that a missed invalidation would still
/// self-heal quickly; the epoch hooks are what make revocations immediate.
pub(crate) const CACHE_TTL: Duration = Duration::from_secs(5);

/// Upper bound for one cache; pruning drops expired entries first and only clears the whole map
/// when a burst fills it faster than the TTL drains it.
pub(crate) const CACHE_MAX_ENTRIES: usize = 4_096;

/// A cache entry: the resolved value plus what is needed to prove it is still current.
pub(crate) struct ExpiringCacheEntry<V> {
    value: Arc<V>,
    user_id: String,
    snapshot: AuthCacheEpochSnapshot,
    stored_at: Instant,
}

impl<V> Clone for ExpiringCacheEntry<V> {
    fn clone(&self) -> Self {
        Self {
            value: Arc::clone(&self.value),
            user_id: self.user_id.clone(),
            snapshot: self.snapshot,
            stored_at: self.stored_at,
        }
    }
}

impl<V> ExpiringCacheEntry<V> {
    pub(crate) fn value(&self) -> Arc<V> {
        Arc::clone(&self.value)
    }

    pub(crate) fn user_id(&self) -> &str {
        &self.user_id
    }

    pub(crate) fn snapshot(&self) -> AuthCacheEpochSnapshot {
        self.snapshot
    }
}

/// Cache key to [`ExpiringCacheEntry`] map with a TTL and a hard entry bound.
pub(crate) struct ExpiringCache<V> {
    entries: Mutex<HashMap<Vec<u8>, ExpiringCacheEntry<V>>>,
}

impl<V> Default for ExpiringCache<V> {
    fn default() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }
}

impl<V> ExpiringCache<V> {
    /// Returns the entry for `key` when it is younger than [`CACHE_TTL`].
    ///
    /// The caller must still verify [`ExpiringCacheEntry::snapshot`] against the database epochs;
    /// this method only enforces the age bound.
    pub(crate) fn lookup(&self, key: &[u8], now: Instant) -> Option<ExpiringCacheEntry<V>> {
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let entry = entries.get(key)?;
        (now.saturating_duration_since(entry.stored_at) < CACHE_TTL).then(|| entry.clone())
    }

    /// Inserts or replaces `key`. `user_id` and `snapshot` are what the caller compares against
    /// [`crate::storage::AuthCacheEpochs`] on the next lookup.
    pub(crate) fn store(
        &self,
        key: Vec<u8>,
        user_id: String,
        snapshot: AuthCacheEpochSnapshot,
        value: V,
        now: Instant,
    ) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if entries.len() >= CACHE_MAX_ENTRIES {
            entries.retain(|_, entry| now.saturating_duration_since(entry.stored_at) < CACHE_TTL);
            if entries.len() >= CACHE_MAX_ENTRIES {
                entries.clear();
            }
        }
        entries.insert(
            key,
            ExpiringCacheEntry {
                value: Arc::new(value),
                user_id,
                snapshot,
                stored_at: now,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };

    use crate::storage::AuthCacheEpochs;

    use super::{CACHE_MAX_ENTRIES, CACHE_TTL, ExpiringCache};

    fn epochs() -> AuthCacheEpochs {
        AuthCacheEpochs::default()
    }

    #[test]
    fn entries_are_reused_until_the_ttl_expires() {
        let cache: ExpiringCache<u32> = ExpiringCache::default();
        let epochs = epochs();
        let now = Instant::now();
        cache.store(
            b"token".to_vec(),
            "user".to_owned(),
            epochs.snapshot("user"),
            7,
            now,
        );

        let hit = cache
            .lookup(b"token", now + Duration::from_secs(1))
            .expect("entry within the TTL");
        assert_eq!(*hit.value(), 7);
        assert!(epochs.principal_is_current(hit.snapshot(), hit.user_id()));

        assert!(
            cache.lookup(b"token", now + CACHE_TTL).is_none(),
            "an entry at the TTL boundary must be reloaded"
        );
    }

    #[test]
    fn user_writes_make_a_cached_principal_stale() {
        let cache: ExpiringCache<u32> = ExpiringCache::default();
        let epochs = epochs();
        let now = Instant::now();
        cache.store(
            b"token".to_vec(),
            "user".to_owned(),
            epochs.snapshot("user"),
            7,
            now,
        );
        epochs.invalidate_user("user");

        let entry = cache.lookup(b"token", now).expect("entry is still young");
        assert!(!epochs.principal_is_current(entry.snapshot(), entry.user_id()));
    }

    #[test]
    fn filling_the_cache_beyond_its_bound_drops_old_entries() {
        let cache: ExpiringCache<u32> = ExpiringCache::default();
        let epochs = epochs();
        let now = Instant::now();
        for index in 0..=CACHE_MAX_ENTRIES {
            let key = format!("token-{index}").into_bytes();
            cache.store(key, "user".to_owned(), epochs.snapshot("user"), 1, now);
        }

        let entries = cache.entries.lock().expect("cache lock");
        assert!(
            entries.len() <= CACHE_MAX_ENTRIES,
            "the cache must stay bounded"
        );
        assert!(
            !entries.contains_key(b"token-0".as_slice()),
            "the oldest entry is dropped once the cache is full"
        );
    }

    #[test]
    fn lookups_share_one_cached_value() {
        let cache: ExpiringCache<Vec<u8>> = ExpiringCache::default();
        let epochs = epochs();
        let now = Instant::now();
        cache.store(
            b"token".to_vec(),
            "user".to_owned(),
            epochs.snapshot("user"),
            vec![1, 2, 3],
            now,
        );

        let first = cache.lookup(b"token", now).expect("entry");
        let second = cache.lookup(b"token", now).expect("entry");
        assert_eq!(*first.value(), vec![1, 2, 3]);
        assert!(Arc::ptr_eq(&first.value(), &second.value()));
    }
}
