use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{
    sync::{Mutex as AsyncMutex, Notify, OnceCell, futures::OwnedNotified},
    time::{Duration, sleep},
};

use crate::observability::resources::ResourceMetrics;

const MAX_ENTRY_BYTES: usize = 4 * 1024 * 1024;
const MAX_CACHE_ENTRIES: usize = 4096;
const CACHE_PERSIST_DEBOUNCE: Duration = Duration::from_millis(250);

#[derive(Clone)]
pub(crate) struct ProviderResponseCache {
    state: Arc<Mutex<CacheState>>,
    load_once: Arc<OnceCell<()>>,
    persist_lock: Arc<AsyncMutex<()>>,
    persist_notify: Arc<Notify>,
    persist_task: Arc<OnceCell<()>>,
    persist_dirty: Arc<AtomicBool>,
    resources: Arc<Mutex<Option<ResourceMetrics>>>,
    path: Option<PathBuf>,
}

struct CacheState {
    entries: HashMap<String, CacheEntry>,
    inflight: HashMap<String, Arc<Notify>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CacheEntry {
    value: Value,
    expires_at: i64,
    negative: bool,
}

pub(crate) enum CacheLookup {
    Hit(Value),
    Negative,
    Wait(Pin<Box<OwnedNotified>>),
    Owner(CacheOwner),
}

pub(crate) struct CacheOwner {
    state: Arc<Mutex<CacheState>>,
    key: String,
    notify: Arc<Notify>,
    released: bool,
}

impl CacheOwner {
    pub(crate) fn finish(mut self) {
        self.release();
    }

    fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        let notify = {
            let mut state = lock_state(&self.state);
            let owned = state
                .inflight
                .get(&self.key)
                .is_some_and(|current| Arc::ptr_eq(current, &self.notify));
            if owned {
                state.inflight.remove(&self.key)
            } else {
                None
            }
        };
        if let Some(notify) = notify {
            notify.notify_waiters();
        }
    }
}

impl Drop for CacheOwner {
    fn drop(&mut self) {
        self.release();
    }
}

impl ProviderResponseCache {
    pub(crate) fn new(path: Option<PathBuf>) -> Self {
        Self {
            state: Arc::new(Mutex::new(CacheState {
                entries: HashMap::new(),
                inflight: HashMap::new(),
            })),
            load_once: Arc::new(OnceCell::new()),
            persist_lock: Arc::new(AsyncMutex::new(())),
            persist_notify: Arc::new(Notify::new()),
            persist_task: Arc::new(OnceCell::new()),
            persist_dirty: Arc::new(AtomicBool::new(false)),
            resources: Arc::new(Mutex::new(None)),
            path,
        }
    }

    pub(crate) fn with_resource_metrics(&self, resources: ResourceMetrics) {
        if let Ok(mut current) = self.resources.lock() {
            *current = Some(resources);
        }
    }

    pub(crate) async fn begin(&self, key: &str) -> CacheLookup {
        self.ensure_loaded().await;
        let now = unix_now();
        let mut state = lock_state(&self.state);
        if let Some(entry) = state.entries.get(key) {
            if entry.expires_at > now {
                return if entry.negative {
                    CacheLookup::Negative
                } else {
                    CacheLookup::Hit(entry.value.clone())
                };
            }
        }
        state.entries.retain(|_, entry| entry.expires_at > now);
        if let Some(notify) = state.inflight.get(key) {
            let mut waiter = Box::pin(notify.clone().notified_owned());
            waiter.as_mut().enable();
            return CacheLookup::Wait(waiter);
        }
        let notify = Arc::new(Notify::new());
        state.inflight.insert(key.to_owned(), notify.clone());
        CacheLookup::Owner(CacheOwner {
            state: self.state.clone(),
            key: key.to_owned(),
            notify,
            released: false,
        })
    }

    pub(crate) async fn store(&self, key: &str, value: &Value, ttl_seconds: i64) {
        if ttl_seconds <= 0 || serialized_size(value) > MAX_ENTRY_BYTES {
            return;
        }
        let Some(expires_at) = unix_now().checked_add(ttl_seconds) else {
            return;
        };
        {
            let mut state = lock_state(&self.state);
            if state.entries.len() >= MAX_CACHE_ENTRIES {
                evict_oldest(&mut state.entries);
            }
            state.entries.insert(
                key.to_owned(),
                CacheEntry {
                    value: value.clone(),
                    expires_at,
                    negative: false,
                },
            );
        }
        self.schedule_persist().await;
    }

    pub(crate) async fn store_negative(&self, key: &str, ttl_seconds: i64) {
        let Some(expires_at) = unix_now().checked_add(ttl_seconds) else {
            return;
        };
        {
            let mut state = lock_state(&self.state);
            if state.entries.len() >= MAX_CACHE_ENTRIES {
                evict_oldest(&mut state.entries);
            }
            state.entries.insert(
                key.to_owned(),
                CacheEntry {
                    value: Value::Null,
                    expires_at,
                    negative: true,
                },
            );
        }
        self.schedule_persist().await;
    }

    pub(crate) async fn clear(&self) {
        self.ensure_loaded().await;
        lock_state(&self.state).entries.clear();
        self.schedule_persist().await;
    }

    #[cfg(test)]
    pub(crate) async fn flush(&self) {
        if self.path.is_none() {
            return;
        }
        self.ensure_loaded().await;
        self.persist_dirty.store(false, Ordering::Release);
        self.persist_now().await;
        if self.persist_dirty.load(Ordering::Acquire) {
            self.persist_notify.notify_one();
        }
    }

    async fn ensure_loaded(&self) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let _ = self
            .load_once
            .get_or_init(|| async {
                let Ok(bytes) = tokio::fs::read(&path).await else {
                    return;
                };
                let Ok(entries) = serde_json::from_slice::<HashMap<String, CacheEntry>>(&bytes)
                else {
                    tracing::debug!(path = %path.display(), "provider response cache is invalid");
                    return;
                };
                let now = unix_now();
                let mut state = lock_state(&self.state);
                state.entries = entries
                    .into_iter()
                    .filter(|(_, entry)| entry.expires_at > now)
                    .take(MAX_CACHE_ENTRIES)
                    .collect();
            })
            .await;
    }

    async fn schedule_persist(&self) {
        if self.path.is_none() {
            return;
        }
        self.persist_dirty.store(true, Ordering::Release);
        self.persist_notify.notify_one();
        let cache = self.clone();
        let _ = self
            .persist_task
            .get_or_init(|| async move {
                tokio::spawn(async move { cache.persist_loop().await });
            })
            .await;
    }

    async fn persist_loop(&self) {
        loop {
            self.persist_notify.notified().await;
            sleep(CACHE_PERSIST_DEBOUNCE).await;
            if !self.persist_dirty.swap(false, Ordering::AcqRel) {
                continue;
            }
            self.persist_now().await;
            if self.persist_dirty.load(Ordering::Acquire) {
                self.persist_notify.notify_one();
            }
        }
    }

    async fn persist_now(&self) -> bool {
        let Some(path) = self.path.as_ref() else {
            return false;
        };
        let started = std::time::Instant::now();
        let success = self.persist_now_to_path(path).await;
        if let Ok(resources) = self.resources.lock()
            && let Some(resources) = resources.as_ref()
        {
            resources.record_metadata_cache_persist(started.elapsed(), success);
        }
        success
    }

    async fn persist_now_to_path(&self, path: &Path) -> bool {
        let _guard = self.persist_lock.lock().await;
        let entries = {
            let state = lock_state(&self.state);
            state.entries.clone()
        };
        let Ok(bytes) = serde_json::to_vec(&entries) else {
            return false;
        };
        let Some(parent) = path.parent() else {
            return false;
        };
        if tokio::fs::create_dir_all(parent).await.is_err() {
            return false;
        }
        let temporary = path.with_extension("json.tmp");
        if tokio::fs::write(&temporary, bytes).await.is_err() {
            let _ = tokio::fs::remove_file(&temporary).await;
            return false;
        }
        if tokio::fs::rename(&temporary, path).await.is_err() {
            let _ = tokio::fs::remove_file(&temporary).await;
            return false;
        }
        true
    }
}

fn lock_state(state: &Mutex<CacheState>) -> MutexGuard<'_, CacheState> {
    match state.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            tracing::error!("provider response cache lock was poisoned");
            poisoned.into_inner()
        }
    }
}

pub(crate) fn cache_key(provider: &str, method: &str, params: &Value) -> Option<String> {
    let params = serde_json::to_string(params).ok()?;
    Some(format!("{provider}\n{method}\n{params}"))
}

pub(crate) fn ttl_for_method(method: &str) -> i64 {
    match method {
        "metadata.search" => 5 * 60,
        "metadata.get" | "metadata.images" | "metadata.credits" => 24 * 60 * 60,
        "metadata.externalIds" | "metadata.trailers" => 7 * 24 * 60 * 60,
        _ => 60 * 60,
    }
}

fn evict_oldest(entries: &mut HashMap<String, CacheEntry>) {
    if let Some((key, _)) = entries
        .iter()
        .min_by_key(|(_, entry)| entry.expires_at)
        .map(|(key, entry)| (key.clone(), entry.clone()))
    {
        entries.remove(&key);
    }
}

fn serialized_size(value: &Value) -> usize {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX)
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(0)
}

#[allow(dead_code)]
fn _cache_path_is_safe(path: &Path) -> bool {
    path.components().next().is_some()
}

#[cfg(test)]
mod tests {
    use super::{CacheLookup, ProviderResponseCache};
    use serde_json::json;
    use std::sync::Arc;
    use tokio::time::{Duration, timeout};

    #[tokio::test]
    async fn cache_reuses_values_and_negative_results() {
        let cache = ProviderResponseCache::new(None);
        let CacheLookup::Owner(owner) = cache.begin("value").await else {
            panic!("first request should own value lookup");
        };
        cache.store("value", &json!({"id": 7}), 60).await;
        owner.finish();
        assert!(matches!(cache.begin("value").await, CacheLookup::Hit(value) if value["id"] == 7));

        let CacheLookup::Owner(owner) = cache.begin("missing").await else {
            panic!("first request should own missing lookup");
        };
        cache.store_negative("missing", 60).await;
        owner.finish();
        assert!(matches!(
            cache.begin("missing").await,
            CacheLookup::Negative
        ));
    }

    #[tokio::test]
    async fn concurrent_lookup_waits_for_the_single_owner() {
        let cache = Arc::new(ProviderResponseCache::new(None));
        let CacheLookup::Owner(owner) = cache.begin("same").await else {
            panic!("first request should own concurrent lookup");
        };
        let waiting_cache = cache.clone();
        let waiting = tokio::spawn(async move {
            match waiting_cache.begin("same").await {
                CacheLookup::Wait(waiter) => {
                    waiter.await;
                    matches!(waiting_cache.begin("same").await, CacheLookup::Hit(value) if value["ok"] == true)
                }
                _ => false,
            }
        });
        tokio::task::yield_now().await;
        cache.store("same", &json!({"ok": true}), 60).await;
        owner.finish();
        let result = timeout(Duration::from_secs(1), waiting)
            .await
            .expect("singleflight waiter timed out")
            .expect("singleflight waiter panicked");
        assert!(result);
    }

    #[tokio::test]
    async fn cancelled_owner_releases_waiters_for_a_new_request() {
        let cache = Arc::new(ProviderResponseCache::new(None));
        let owner_cache = cache.clone();
        let (owner_claimed, owner_claimed_rx) = tokio::sync::oneshot::channel();
        let owner = tokio::spawn(async move {
            let CacheLookup::Owner(_lease) = owner_cache.begin("cancelled").await else {
                return;
            };
            let _ = owner_claimed.send(());
            std::future::pending::<()>().await;
        });
        owner_claimed_rx
            .await
            .expect("owner task stopped before claiming the request");

        let waiting_cache = cache.clone();
        let (waiter_registered, waiter_registered_rx) = tokio::sync::oneshot::channel();
        let waiting = tokio::spawn(async move {
            let CacheLookup::Wait(waiter) = waiting_cache.begin("cancelled").await else {
                return false;
            };
            let _ = waiter_registered.send(());
            waiter.await;
            match waiting_cache.begin("cancelled").await {
                CacheLookup::Owner(_lease) => true,
                CacheLookup::Hit(_) | CacheLookup::Negative | CacheLookup::Wait(_) => false,
            }
        });
        waiter_registered_rx
            .await
            .expect("waiter task stopped before observing the owner");
        owner.abort();

        let continued = timeout(Duration::from_secs(1), waiting)
            .await
            .expect("new request remained blocked after owner cancellation")
            .expect("waiter task panicked");
        assert!(continued, "cancelled owner must not populate the cache");
    }

    #[tokio::test]
    async fn cache_survives_recreation_when_backed_by_a_file() {
        let directory = tempfile::tempdir().expect("temporary cache directory");
        let path = directory.path().join("provider-cache.json");
        let cache = ProviderResponseCache::new(Some(path.clone()));
        let CacheLookup::Owner(owner) = cache.begin("persisted").await else {
            panic!("first request should own persisted lookup");
        };
        cache.store("persisted", &json!({"id": 9}), 60).await;
        owner.finish();
        cache.flush().await;

        let restored = ProviderResponseCache::new(Some(path));
        assert!(
            matches!(restored.begin("persisted").await, CacheLookup::Hit(value) if value["id"] == 9)
        );
    }

    #[tokio::test]
    async fn cache_persistence_records_a_bounded_duration_metric() {
        let directory = tempfile::tempdir().expect("temporary cache directory");
        let path = directory.path().join("provider-cache.json");
        let resources = crate::observability::resources::ResourceMetrics::new();
        let cache = ProviderResponseCache::new(Some(path));
        cache.with_resource_metrics(resources.clone());

        let CacheLookup::Owner(owner) = cache.begin("persisted").await else {
            panic!("first cache lookup must own the key");
        };
        cache.store("persisted", &json!({"id": 9}), 60).await;
        owner.finish();
        cache.flush().await;

        let snapshot = resources.snapshot().await;
        assert_eq!(snapshot.metadata.counters["cache.persist.success.count"], 1);
        assert!(snapshot.metadata.stage_p95_ms.contains_key("cache_persist"));
    }
}
