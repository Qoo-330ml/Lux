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
            return state.attempts.len() < MAX_ENTRIES;
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

struct LoginRateLimiterState {
    attempts: HashMap<[u8; 32], LoginAttempt>,
    last_cleanup: Instant,
}

struct LoginAttempt {
    started_at: Instant,
    failures: u8,
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

fn key_digest(key: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::{
        CLEANUP_INTERVAL, LoginAttempt, LoginRateLimiter, MAX_ENTRIES, WINDOW, key_digest,
    };
    use std::time::{Duration, Instant};

    #[tokio::test]
    async fn unique_failure_keys_are_bounded_and_active_keys_are_not_evicted() {
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
        assert!(!limiter.is_allowed("another-new-user").await);
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
}
