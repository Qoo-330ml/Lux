use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex, Notify, OnceCell};

const MAX_ENTRY_BYTES: usize = 4 * 1024 * 1024;
const MAX_CACHE_ENTRIES: usize = 4096;

#[derive(Clone)]
pub(crate) struct ProviderResponseCache {
    state: Arc<Mutex<CacheState>>,
    load_once: Arc<OnceCell<()>>,
    persist_lock: Arc<Mutex<()>>,
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
    Wait(Arc<Notify>),
    Owner,
}

impl ProviderResponseCache {
    pub(crate) fn new(path: Option<PathBuf>) -> Self {
        Self {
            state: Arc::new(Mutex::new(CacheState {
                entries: HashMap::new(),
                inflight: HashMap::new(),
            })),
            load_once: Arc::new(OnceCell::new()),
            persist_lock: Arc::new(Mutex::new(())),
            path,
        }
    }

    pub(crate) async fn begin(&self, key: &str) -> CacheLookup {
        self.ensure_loaded().await;
        let now = unix_now();
        let mut state = self.state.lock().await;
        if let Some(entry) = state.entries.get(key) {
            if entry.expires_at > now {
                return if entry.negative {
                    CacheLookup::Hit(Value::Null)
                } else {
                    CacheLookup::Hit(entry.value.clone())
                };
            }
        }
        state.entries.retain(|_, entry| entry.expires_at > now);
        if let Some(notify) = state.inflight.get(key) {
            return CacheLookup::Wait(notify.clone());
        }
        state
            .inflight
            .insert(key.to_owned(), Arc::new(Notify::new()));
        CacheLookup::Owner
    }

    pub(crate) async fn store(&self, key: &str, value: &Value, ttl_seconds: i64) {
        if ttl_seconds <= 0 || serialized_size(value) > MAX_ENTRY_BYTES {
            return;
        }
        let Some(expires_at) = unix_now().checked_add(ttl_seconds) else {
            return;
        };
        {
            let mut state = self.state.lock().await;
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
        self.persist().await;
    }

    pub(crate) async fn store_negative(&self, key: &str, ttl_seconds: i64) {
        let Some(expires_at) = unix_now().checked_add(ttl_seconds) else {
            return;
        };
        {
            let mut state = self.state.lock().await;
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
        self.persist().await;
    }

    pub(crate) async fn finish(&self, key: &str) {
        let notify = self.state.lock().await.inflight.remove(key);
        if let Some(notify) = notify {
            notify.notify_waiters();
        }
    }

    pub(crate) async fn clear(&self) {
        self.ensure_loaded().await;
        self.state.lock().await.entries.clear();
        self.persist().await;
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
                let mut state = self.state.lock().await;
                state.entries = entries
                    .into_iter()
                    .filter(|(_, entry)| entry.expires_at > now)
                    .take(MAX_CACHE_ENTRIES)
                    .collect();
            })
            .await;
    }

    async fn persist(&self) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        let _guard = self.persist_lock.lock().await;
        let entries = {
            let state = self.state.lock().await;
            state.entries.clone()
        };
        let Ok(bytes) = serde_json::to_vec(&entries) else {
            return;
        };
        let Some(parent) = path.parent() else {
            return;
        };
        if tokio::fs::create_dir_all(parent).await.is_err() {
            return;
        }
        let temporary = path.with_extension("json.tmp");
        if tokio::fs::write(&temporary, bytes).await.is_err() {
            return;
        }
        if tokio::fs::rename(&temporary, path).await.is_err() {
            let _ = tokio::fs::remove_file(&temporary).await;
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

pub(crate) fn tmdb_ttl_for_endpoint(endpoint: &str) -> i64 {
    if endpoint.contains("search/") {
        5 * 60
    } else if endpoint.contains("/images") || endpoint.contains("/credits") {
        24 * 60 * 60
    } else if endpoint.contains("/external_ids") || endpoint.contains("/videos") {
        7 * 24 * 60 * 60
    } else if endpoint.starts_with("3/person/") {
        7 * 24 * 60 * 60
    } else {
        24 * 60 * 60
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
        assert!(matches!(cache.begin("value").await, CacheLookup::Owner));
        cache.store("value", &json!({"id": 7}), 60).await;
        cache.finish("value").await;
        assert!(matches!(cache.begin("value").await, CacheLookup::Hit(value) if value["id"] == 7));

        assert!(matches!(cache.begin("missing").await, CacheLookup::Owner));
        cache.store_negative("missing", 60).await;
        cache.finish("missing").await;
        assert!(matches!(cache.begin("missing").await, CacheLookup::Hit(value) if value.is_null()));
    }

    #[tokio::test]
    async fn concurrent_lookup_waits_for_the_single_owner() {
        let cache = Arc::new(ProviderResponseCache::new(None));
        assert!(matches!(cache.begin("same").await, CacheLookup::Owner));
        let waiting_cache = cache.clone();
        let waiting = tokio::spawn(async move {
            match waiting_cache.begin("same").await {
                CacheLookup::Wait(notify) => {
                    notify.notified().await;
                    matches!(waiting_cache.begin("same").await, CacheLookup::Hit(value) if value["ok"] == true)
                }
                _ => false,
            }
        });
        tokio::task::yield_now().await;
        cache.store("same", &json!({"ok": true}), 60).await;
        cache.finish("same").await;
        let result = timeout(Duration::from_secs(1), waiting)
            .await
            .expect("singleflight waiter timed out")
            .expect("singleflight waiter panicked");
        assert!(result);
    }

    #[tokio::test]
    async fn cache_survives_recreation_when_backed_by_a_file() {
        let directory = tempfile::tempdir().expect("temporary cache directory");
        let path = directory.path().join("provider-cache.json");
        let cache = ProviderResponseCache::new(Some(path.clone()));
        assert!(matches!(cache.begin("persisted").await, CacheLookup::Owner));
        cache.store("persisted", &json!({"id": 9}), 60).await;
        cache.finish("persisted").await;

        let restored = ProviderResponseCache::new(Some(path));
        assert!(
            matches!(restored.begin("persisted").await, CacheLookup::Hit(value) if value["id"] == 9)
        );
    }
}
