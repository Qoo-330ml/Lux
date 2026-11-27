use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

const WINDOW: Duration = Duration::from_secs(60);
const MAX_FAILURES: u8 = 5;
const MAX_ENTRIES: usize = 10_000;
const CLEANUP_INTERVAL: Duration = Duration::from_secs(10);
const DEVICE_PAIRING_WINDOW: Duration = Duration::from_secs(60);
const DEVICE_PAIRING_MAX_ENTRIES: usize = 10_000;
pub const DEVICE_PAIRING_CREATE_LIMIT: u32 = 10;
pub const DEVICE_PAIRING_REDEEM_LIMIT: u32 = 10;
pub const DEVICE_PAIRING_RETRY_AFTER_SECONDS: u64 = 60;

#[derive(Clone)]
pub struct LoginRateLimiter {
    state: Arc<Mutex<LoginRateLimiterState>>,
}

impl Default for LoginRateLimiter {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(LoginRateLimiterState {
                attempts: HashMap::new(),
                last_cleanup: Instant::now(),
            })),
        }
    }
}

impl LoginRateLimiter {
    /// Reserves one attempt for `key`; `false` means the window budget is exhausted.
    ///
    /// Checking the budget and counting the attempt happen inside one lock acquisition. The old
    /// split between a check and a separate `record_failure` let a burst of concurrent guesses
    /// observe the same pre-increment count and pass together, so a window could admit far more
    /// than [`MAX_FAILURES`] password verifications.
    ///
    /// The guard is released before the caller runs Argon2; a caller that never reached the
    /// credential check must give the reservation back with [`Self::release_attempt`], and a
    /// successful login clears the window with [`Self::record_success`].
    pub async fn try_acquire(&self, key: &str) -> bool {
        let mut state = self.state.lock().await;
        let now = Instant::now();
        maybe_cleanup(&mut state, now);
        let key = key_digest(key);
        if let Some(attempt) = state.attempts.get_mut(&key) {
            if now.saturating_duration_since(attempt.started_at) >= WINDOW {
                attempt.started_at = now;
                attempt.failures = 0;
            }
            if attempt.failures >= MAX_FAILURES {
                return false;
            }
            attempt.failures = attempt.failures.saturating_add(1);
            return true;
        }
        if state.attempts.len() >= MAX_ENTRIES {
            // A saturated table must never deny an account that has no entry yet: otherwise a
            // flood of unique usernames would lock every legitimate sign-in out. Unknown keys
            // simply stay untracked until the periodic sweep frees capacity, and releasing such
            // a reservation is a no-op.
            return true;
        }
        state.attempts.insert(
            key,
            LoginAttempt {
                started_at: now,
                failures: 1,
            },
        );
        true
    }

    /// Rolls back one reservation made by [`Self::try_acquire`] after a server-side failure.
    ///
    /// Only one unit is released, so a storage fault cannot erase the failures other attempts
    /// already accumulated in the same window, and an untracked key (a saturated table fails
    /// open) stays a no-op. A window that already expired has nothing left to roll back.
    pub async fn release_attempt(&self, key: &str) {
        let mut state = self.state.lock().await;
        let now = Instant::now();
        maybe_cleanup(&mut state, now);
        let key = key_digest(key);
        let Some(attempt) = state.attempts.get_mut(&key) else {
            return;
        };
        if now.saturating_duration_since(attempt.started_at) >= WINDOW {
            state.attempts.remove(&key);
            return;
        }
        attempt.failures = attempt.failures.saturating_sub(1);
        if attempt.failures == 0 {
            state.attempts.remove(&key);
        }
    }

    /// Clears the window after a successful authentication.
    pub async fn record_success(&self, key: &str) {
        let mut state = self.state.lock().await;
        let now = Instant::now();
        maybe_cleanup(&mut state, now);
        state.attempts.remove(&key_digest(key));
    }
}

#[derive(Clone)]
pub struct DevicePairingRateLimiter {
    state: Arc<Mutex<DevicePairingRateLimiterState>>,
}

impl Default for DevicePairingRateLimiter {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(DevicePairingRateLimiterState {
                attempts: HashMap::new(),
                last_cleanup: Instant::now(),
            })),
        }
    }
}

impl DevicePairingRateLimiter {
    pub async fn try_acquire(&self, key: &str, limit: u32) -> bool {
        let mut state = self.state.lock().await;
        let now = Instant::now();
        cleanup_device_pairing_attempts(&mut state, now);
        let key = key_digest(key);
        if let Some(attempt) = state.attempts.get_mut(&key) {
            if now.saturating_duration_since(attempt.started_at) >= DEVICE_PAIRING_WINDOW {
                attempt.started_at = now;
                attempt.count = 0;
            }
            if attempt.count >= limit {
                return false;
            }
            attempt.count = attempt.count.saturating_add(1);
            return true;
        }
        if state.attempts.len() >= DEVICE_PAIRING_MAX_ENTRIES {
            return false;
        }
        state.attempts.insert(
            key,
            DevicePairingAttempt {
                started_at: now,
                count: 1,
            },
        );
        true
    }
}

struct LoginRateLimiterState {
    attempts: HashMap<[u8; 32], LoginAttempt>,
    last_cleanup: Instant,
}

struct LoginAttempt {
    started_at: Instant,
    failures: u8,
}

struct DevicePairingRateLimiterState {
    attempts: HashMap<[u8; 32], DevicePairingAttempt>,
    last_cleanup: Instant,
}

struct DevicePairingAttempt {
    started_at: Instant,
    count: u32,
}

fn maybe_cleanup(state: &mut LoginRateLimiterState, now: Instant) {
    if now.saturating_duration_since(state.last_cleanup) < CLEANUP_INTERVAL {
        return;
    }
    state
        .attempts
        .retain(|_, attempt| now.saturating_duration_since(attempt.started_at) < WINDOW);
    state.last_cleanup = now;
}

fn cleanup_device_pairing_attempts(state: &mut DevicePairingRateLimiterState, now: Instant) {
    if now.saturating_duration_since(state.last_cleanup) < CLEANUP_INTERVAL {
        return;
    }
    state.attempts.retain(|_, attempt| {
        now.saturating_duration_since(attempt.started_at) < DEVICE_PAIRING_WINDOW
    });
    state.last_cleanup = now;
}

fn key_digest(key: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    hasher.finalize().into()
}

const REQUEST_WINDOW: Duration = Duration::from_secs(60);
const REQUEST_MAX_ENTRIES: usize = 8192;
/// Generous per-peer budget: a player fetching HLS segments and artwork bursts stays well below
/// it, while a runaway client cannot monopolise the server.
pub const DEFAULT_REQUESTS_PER_MINUTE: u32 = 2_400;
/// Administrator danmaku searches allowed per session inside [`DANMAKU_SEARCH_WINDOW`].
pub const DANMAKU_SEARCH_REQUESTS_PER_WINDOW: u32 = 2;
/// Sliding window of [`DanmakuSearchRateLimiter`]; ADR-052 P5 asks for at most two searches per
/// second and per administrator session because every search reaches an upstream provider.
pub const DANMAKU_SEARCH_WINDOW: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct RequestRateLimiter {
    state: Arc<std::sync::Mutex<RequestRateLimiterState>>,
    limit: u32,
    window: Duration,
}

impl Default for RequestRateLimiter {
    fn default() -> Self {
        Self::new(DEFAULT_REQUESTS_PER_MINUTE)
    }
}

impl RequestRateLimiter {
    pub fn new(limit: u32) -> Self {
        Self::with_window(limit, REQUEST_WINDOW)
    }

    /// Builds a limiter with a shorter or longer window than the default minute.
    pub fn with_window(limit: u32, window: Duration) -> Self {
        Self {
            state: Arc::new(std::sync::Mutex::new(RequestRateLimiterState {
                entries: HashMap::new(),
                last_cleanup: Instant::now(),
            })),
            limit: limit.max(1),
            window,
        }
    }

    /// Counts one request for `key`; returns false once the window budget is exhausted.
    pub async fn allow(&self, key: &str) -> bool {
        self.allow_digest(xxhash_rust::xxh3::xxh3_128(key.as_bytes()))
    }

    /// Counts one request for `ip` without heap-formatting the address string.
    pub fn allow_ip(&self, ip: std::net::IpAddr) -> bool {
        let digest = match ip {
            std::net::IpAddr::V4(v4) => {
                let [a, b, c, d] = v4.octets();
                xxhash_rust::xxh3::xxh3_128(&[4, a, b, c, d])
            }
            std::net::IpAddr::V6(v6) => {
                let mut buf = [0_u8; 17];
                buf[0] = 6;
                buf[1..].copy_from_slice(&v6.octets());
                xxhash_rust::xxh3::xxh3_128(&buf)
            }
        };
        self.allow_digest(digest)
    }

    fn allow_digest(&self, key: u128) -> bool {
        let now = Instant::now();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if now.saturating_duration_since(state.last_cleanup) >= self.window {
            state
                .entries
                .retain(|_, entry| now.saturating_duration_since(entry.started_at) < self.window);
            state.last_cleanup = now;
        }
        if let Some(entry) = state.entries.get_mut(&key) {
            if now.saturating_duration_since(entry.started_at) >= self.window {
                *entry = RequestWindow {
                    started_at: now,
                    count: 1,
                };
                return true;
            }
            if entry.count >= self.limit {
                return false;
            }
            entry.count = entry.count.saturating_add(1);
            return true;
        }
        if state.entries.len() >= REQUEST_MAX_ENTRIES {
            return false;
        }
        state.entries.insert(
            key,
            RequestWindow {
                started_at: now,
                count: 1,
            },
        );
        true
    }
}

/// Per-administrator-session budget for the manual danmaku search endpoint (ADR-052 P5).
///
/// The key is the caller's session material, so two administrators never share a budget; the raw
/// key is hashed before it is stored and never logged.
#[derive(Clone)]
pub struct DanmakuSearchRateLimiter(RequestRateLimiter);

impl Default for DanmakuSearchRateLimiter {
    fn default() -> Self {
        Self::new(DANMAKU_SEARCH_REQUESTS_PER_WINDOW, DANMAKU_SEARCH_WINDOW)
    }
}

impl DanmakuSearchRateLimiter {
    pub fn new(limit: u32, window: Duration) -> Self {
        Self(RequestRateLimiter::with_window(limit, window))
    }

    /// Counts one search for `key`; returns false once the window budget is exhausted.
    pub async fn allow(&self, key: &str) -> bool {
        self.0.allow(key).await
    }
}

struct RequestRateLimiterState {
    entries: HashMap<u128, RequestWindow>,
    last_cleanup: Instant,
}

struct RequestWindow {
    started_at: Instant,
    count: u32,
}

#[cfg(test)]
mod tests {
    use super::{
        CLEANUP_INTERVAL, DanmakuSearchRateLimiter, DevicePairingRateLimiter, LoginAttempt,
        LoginRateLimiter, MAX_ENTRIES, MAX_FAILURES, RequestRateLimiter, WINDOW, key_digest,
    };
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };
    use tokio::sync::Barrier;

    #[tokio::test]
    async fn unique_failure_keys_are_bounded_and_a_full_table_does_not_deny_new_keys() {
        let limiter = LoginRateLimiter::default();
        let active_key = "active-user";
        assert!(limiter.try_acquire(active_key).await);
        for index in 0..MAX_ENTRIES.saturating_sub(1) {
            assert!(limiter.try_acquire(&format!("random-user-{index}")).await);
        }

        let state = limiter.state.lock().await;
        assert_eq!(state.attempts.len(), MAX_ENTRIES);
        drop(state);

        for _ in 1..MAX_FAILURES {
            assert!(limiter.try_acquire(active_key).await);
        }
        assert!(!limiter.try_acquire(active_key).await);
        // Tracked keys keep their counters, but a saturated table must never lock out an
        // account that has no entry yet: unknown keys fail open instead.
        assert!(limiter.try_acquire("another-new-user").await);
        // A reservation that was never tracked must roll back without side effects.
        limiter.release_attempt("another-new-user").await;
    }

    #[tokio::test]
    async fn concurrent_attempts_never_exceed_the_failure_budget()
    -> Result<(), Box<dyn std::error::Error>> {
        const CONCURRENT: usize = 20;
        let limiter = LoginRateLimiter::default();
        let barrier = Arc::new(Barrier::new(CONCURRENT));
        let mut tasks = Vec::with_capacity(CONCURRENT);
        for _ in 0..CONCURRENT {
            let limiter = limiter.clone();
            let barrier = Arc::clone(&barrier);
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                limiter.try_acquire("shared-account").await
            }));
        }

        let mut allowed = 0;
        for task in tasks {
            if task.await? {
                allowed += 1;
            }
        }
        assert_eq!(allowed, usize::from(MAX_FAILURES));
        assert!(!limiter.try_acquire("shared-account").await);
        Ok(())
    }

    #[tokio::test]
    async fn released_attempts_do_not_consume_the_failure_budget() {
        let limiter = LoginRateLimiter::default();
        // A storage or access-rule fault gives the reservation back, so a run of server errors
        // must not lock the account out.
        for _ in 0..usize::from(MAX_FAILURES) * 3 {
            assert!(limiter.try_acquire("infra-key").await);
            limiter.release_attempt("infra-key").await;
        }
        for _ in 0..MAX_FAILURES {
            assert!(limiter.try_acquire("infra-key").await);
        }
        assert!(!limiter.try_acquire("infra-key").await);

        // Releasing frees exactly one reservation and never underflows below zero.
        limiter.release_attempt("infra-key").await;
        assert!(limiter.try_acquire("infra-key").await);
        limiter.record_success("infra-key").await;
        for _ in 0..10 {
            limiter.release_attempt("infra-key").await;
        }
        for _ in 0..MAX_FAILURES {
            assert!(limiter.try_acquire("infra-key").await);
        }
        assert!(!limiter.try_acquire("infra-key").await);
    }

    #[tokio::test]
    async fn successful_login_clears_the_failure_budget() {
        let limiter = LoginRateLimiter::default();
        for _ in 0..MAX_FAILURES {
            assert!(limiter.try_acquire("viewer").await);
        }
        assert!(!limiter.try_acquire("viewer").await);
        limiter.record_success("viewer").await;
        for _ in 0..MAX_FAILURES {
            assert!(limiter.try_acquire("viewer").await);
        }
        assert!(!limiter.try_acquire("viewer").await);
    }

    #[tokio::test]
    async fn an_expired_window_restores_attempts() {
        let limiter = LoginRateLimiter::default();
        let now = Instant::now();
        limiter.state.lock().await.attempts.insert(
            key_digest("expired-user"),
            LoginAttempt {
                started_at: now - WINDOW - Duration::from_secs(1),
                failures: MAX_FAILURES,
            },
        );

        // The stale window is thrown away, so the new attempt starts a fresh budget.
        assert!(limiter.try_acquire("expired-user").await);
        for _ in 1..MAX_FAILURES {
            assert!(limiter.try_acquire("expired-user").await);
        }
        assert!(!limiter.try_acquire("expired-user").await);
    }

    #[tokio::test]
    async fn periodic_sweep_removes_expired_entries_without_key_access() {
        let limiter = LoginRateLimiter::default();
        let now = Instant::now();
        let expired_at = now - WINDOW - Duration::from_secs(1);
        let mut state = limiter.state.lock().await;
        state.attempts.insert(
            key_digest("expired-one"),
            LoginAttempt {
                started_at: expired_at,
                failures: 1,
            },
        );
        state.attempts.insert(
            key_digest("expired-two"),
            LoginAttempt {
                started_at: expired_at,
                failures: 1,
            },
        );
        state.last_cleanup = now - CLEANUP_INTERVAL - Duration::from_secs(1);
        drop(state);

        assert!(limiter.try_acquire("fresh-user").await);
        limiter.release_attempt("fresh-user").await;
        assert!(limiter.state.lock().await.attempts.is_empty());
    }

    #[tokio::test]
    async fn device_pairing_rate_limiter_applies_per_key_limits() {
        let limiter = DevicePairingRateLimiter::default();
        assert!(limiter.try_acquire("client-a", 2).await);
        assert!(limiter.try_acquire("client-a", 2).await);
        assert!(!limiter.try_acquire("client-a", 2).await);
        assert!(limiter.try_acquire("client-b", 2).await);
    }

    #[tokio::test]
    async fn request_limiter_counts_per_key_within_the_window() {
        let limiter = RequestRateLimiter::new(2);
        assert!(limiter.allow("peer-a").await);
        assert!(limiter.allow("peer-a").await);
        assert!(!limiter.allow("peer-a").await);
        assert!(limiter.allow("peer-b").await);
    }

    #[tokio::test]
    async fn danmaku_search_limiter_allows_two_searches_per_session_per_second() {
        let limiter = DanmakuSearchRateLimiter::default();
        assert!(limiter.allow("session-a").await);
        assert!(limiter.allow("session-a").await);
        assert!(!limiter.allow("session-a").await);
        // Another administrator session keeps its own budget.
        assert!(limiter.allow("session-b").await);
    }

    #[tokio::test]
    async fn danmaku_search_limiter_reopens_the_budget_after_the_window() {
        let limiter = DanmakuSearchRateLimiter::new(1, Duration::from_millis(20));
        assert!(limiter.allow("session-a").await);
        assert!(!limiter.allow("session-a").await);
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert!(limiter.allow("session-a").await);
    }
}
