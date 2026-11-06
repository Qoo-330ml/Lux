//! The connection budget shared by the realtime endpoints.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;

use super::WebsocketLimits;

/// The connection budget: one global semaphore plus a per-account counter.
#[derive(Clone)]
pub(crate) struct WebsocketConnections {
    inner: Arc<WebsocketConnectionState>,
}

struct WebsocketConnectionState {
    limits: WebsocketLimits,
    total: Arc<Semaphore>,
    per_user: std::sync::Mutex<HashMap<String, usize>>,
}

impl Default for WebsocketConnections {
    fn default() -> Self {
        Self::new(WebsocketLimits::default())
    }
}

impl WebsocketConnections {
    pub(crate) fn new(limits: WebsocketLimits) -> Self {
        let limits = limits.normalized();
        let total = Arc::new(Semaphore::new(limits.max_connections));
        Self {
            inner: Arc::new(WebsocketConnectionState {
                limits,
                total,
                per_user: std::sync::Mutex::new(HashMap::new()),
            }),
        }
    }

    pub(crate) fn limits(&self) -> &WebsocketLimits {
        &self.inner.limits
    }

    /// Takes one connection slot or answers `None` when a limit is reached. Both limits are
    /// checked before the handshake completes, so a rejected client never sees a `101`.
    pub(crate) fn try_acquire(&self, user_key: &str) -> Option<WebsocketPermit> {
        let total = self.inner.total.clone().try_acquire_owned().ok()?;
        let mut per_user = self
            .inner
            .per_user
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let count = per_user.entry(user_key.to_owned()).or_insert(0);
        if *count >= self.inner.limits.max_connections_per_user {
            return None;
        }
        *count += 1;
        drop(per_user);
        Some(WebsocketPermit {
            _total: total,
            connections: self.clone(),
            user_key: user_key.to_owned(),
        })
    }

    /// Waits until every connection released its slot. The graceful shutdown uses it so the
    /// database is not closed under a connection that is still building a payload.
    pub(crate) async fn wait_until_idle(&self, timeout: Duration) -> bool {
        let Ok(capacity) = u32::try_from(self.inner.limits.max_connections) else {
            return false;
        };
        if capacity == 0 {
            return true;
        }
        tokio::time::timeout(timeout, self.inner.total.acquire_many(capacity))
            .await
            .is_ok()
    }
}

/// Holds one connection slot; the per-account count is decremented when it is dropped, which is
/// what keeps an abandoned connection from leaking capacity.
pub(crate) struct WebsocketPermit {
    _total: tokio::sync::OwnedSemaphorePermit,
    connections: WebsocketConnections,
    user_key: String,
}

impl Drop for WebsocketPermit {
    fn drop(&mut self) {
        let mut per_user = self
            .connections
            .inner
            .per_user
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(count) = per_user.get_mut(&self.user_key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                per_user.remove(&self.user_key);
            }
        }
    }
}
