use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use tokio::sync::Mutex;

const WINDOW: Duration = Duration::from_secs(60);
const MAX_FAILURES: u8 = 5;

#[derive(Clone, Default)]
pub struct LoginRateLimiter {
    attempts: Arc<Mutex<HashMap<String, LoginAttempt>>>,
}

impl LoginRateLimiter {
    pub async fn is_allowed(&self, key: &str) -> bool {
        let mut attempts = self.attempts.lock().await;
        let Some(attempt) = attempts.get_mut(key) else {
            return true;
        };
        if attempt.started_at.elapsed() >= WINDOW {
            attempts.remove(key);
            return true;
        }
        attempt.failures < MAX_FAILURES
    }

    pub async fn record_failure(&self, key: &str) {
        let mut attempts = self.attempts.lock().await;
        let attempt = attempts.entry(key.to_owned()).or_insert(LoginAttempt {
            started_at: Instant::now(),
            failures: 0,
        });
        if attempt.started_at.elapsed() >= WINDOW {
            attempt.started_at = Instant::now();
            attempt.failures = 0;
        }
        attempt.failures = attempt.failures.saturating_add(1);
    }

    pub async fn record_success(&self, key: &str) {
        self.attempts.lock().await.remove(key);
    }
}

struct LoginAttempt {
    started_at: Instant,
    failures: u8,
}
