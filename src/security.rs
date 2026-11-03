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
    pub async fn is_allowed(&self, key: &str) -> bool {
        let mut state = self.state.lock().await;
        let now = Instant::now();
        maybe_cleanup(&mut state, now);
        let key = key_digest(key);
        let Some(attempt) = state.attempts.get(&key) else {
            // A saturated table must never deny an account that has no entry yet: otherwise a
            // flood of unique usernames would lock every legitimate sign-in out. Unknown keys
            // simply stay untracked until the periodic sweep frees capacity.
            return true;
        };
        if now.saturating_duration_since(attempt.started_at) >= WINDOW {
            state.attempts.remove(&key);
            return true;
        }
        attempt.failures < MAX_FAILURES
    }

    pub async fn record_failure(&self, key: &str) {
        let mut state = self.state.lock().await;
        let now = Instant::now();
        maybe_cleanup(&mut state, now);
        let key = key_digest(key);
        if let Some(attempt) = state.attempts.get_mut(&key) {
            if now.saturating_duration_since(attempt.started_at) >= WINDOW {
                attempt.started_at = now;
                attempt.failures = 0;
            }
            attempt.failures = attempt.failures.saturating_add(1);
            return;
        }
        if state.attempts.len() >= MAX_ENTRIES {
            return;
        }
        state.attempts.insert(
            key,
            LoginAttempt {
                started_at: now,
                failures: 1,
            },
        );
    }

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
    state: Arc<Mutex<RequestRateLimiterState>>,
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
            state: Arc::new(Mutex::new(RequestRateLimiterState {
                entries: HashMap::new(),
                last_cleanup: Instant::now(),
            })),
            limit: limit.max(1),
            window,
        }
    }

    /// Counts one request for `key`; returns false once the window budget is exhausted.
    pub async fn allow(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut state = self.state.lock().await;
        if now.saturating_duration_since(state.last_cleanup) >= self.window {
            state
                .entries
                .retain(|_, entry| now.saturating_duration_since(entry.started_at) < self.window);
            state.last_cleanup = now;
        }
        let key = key_digest(key);
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
    entries: HashMap<[u8; 32], RequestWindow>,
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
        LoginRateLimiter, MAX_ENTRIES, RequestRateLimiter, WINDOW, key_digest,
    };
    use std::time::{Duration, Instant};

    #[tokio::test]
    async fn unique_failure_keys_are_bounded_and_a_full_table_does_not_deny_new_keys() {
        let limiter = LoginRateLimiter::default();
        let active_key = "active-user";
        limiter.record_failure(active_key).await;
        for index in 0..MAX_ENTRIES.saturating_sub(1) {
            limiter
                .record_failure(&format!("random-user-{index}"))
                .await;
        }

        let state = limiter.state.lock().await;
        assert_eq!(state.attempts.len(), MAX_ENTRIES);
        drop(state);

        assert!(limiter.is_allowed(active_key).await);
        limiter.record_failure(active_key).await;
        assert!(limiter.is_allowed(active_key).await);
        limiter.record_failure(active_key).await;
        assert!(limiter.is_allowed(active_key).await);
        limiter.record_failure(active_key).await;
        assert!(limiter.is_allowed(active_key).await);
        limiter.record_failure(active_key).await;
        assert!(!limiter.is_allowed(active_key).await);
        // Tracked keys keep their counters, but a saturated table must never lock out an
        // account that has no entry yet: unknown keys fail open instead.
        assert!(limiter.is_allowed("another-new-user").await);
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

        assert!(limiter.is_allowed("fresh-user").await);
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
