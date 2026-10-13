use std::{
    future::Future,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use serde::Serialize;
use tokio::{fs, io::AsyncWriteExt, process::Command, sync::Mutex as AsyncMutex};

use crate::{
    application::libraries::LibraryService,
    config::DatabaseBackend,
    observability::resources::{ResourceMetrics, ResourceSnapshot},
    storage::{Database, DatabasePoolSnapshot},
};

const ADMIN_HEALTH_PROBE_CACHE_TTL: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AdminHealthProbeSnapshot {
    database_writable: bool,
    config_available: bool,
    config_writable: bool,
    ffprobe_available: bool,
}

#[derive(Clone, Default)]
pub struct AdminHealthProbeCache {
    cached: Arc<AsyncMutex<Option<CachedAdminHealthProbes>>>,
}

struct CachedAdminHealthProbes {
    expires_at: Instant,
    snapshot: AdminHealthProbeSnapshot,
}

impl AdminHealthProbeCache {
    async fn get_or_probe<F, Fut>(&self, probe: F) -> AdminHealthProbeSnapshot
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = AdminHealthProbeSnapshot>,
    {
        self.get_or_probe_with_clock(probe, Instant::now).await
    }

    #[cfg(test)]
    async fn get_or_probe_at<F, Fut>(&self, now: Instant, probe: F) -> AdminHealthProbeSnapshot
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = AdminHealthProbeSnapshot>,
    {
        self.get_or_probe_with_clock(probe, || now).await
    }

    async fn get_or_probe_with_clock<F, Fut, C>(&self, probe: F, now: C) -> AdminHealthProbeSnapshot
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = AdminHealthProbeSnapshot>,
        C: Fn() -> Instant,
    {
        let mut cached = self.cached.lock().await;
        if let Some(cached) = cached.as_ref().filter(|cached| now() < cached.expires_at) {
            return cached.snapshot;
        }

        let snapshot = probe().await;
        *cached = Some(CachedAdminHealthProbes {
            expires_at: now() + ADMIN_HEALTH_PROBE_CACHE_TTL,
            snapshot,
        });
        snapshot
    }
}

#[derive(Debug)]
pub enum AdminHealthError {
    DatabaseUnavailable,
    LibrariesUnavailable,
    ScanJobsUnavailable,
    MetadataJobsUnavailable,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminHealthReport {
    status: &'static str,
    schema_version: i64,
    runtime: AdminHealthRuntime,
    resources: ResourceSnapshot,
    database: AdminHealthDatabase,
    config: AdminHealthConfig,
    ffprobe: AdminHealthAvailability,
    jobs: AdminHealthJobs,
    libraries: Vec<AdminHealthLibrary>,
}

impl AdminHealthReport {
    pub fn schema_version(&self) -> i64 {
        self.schema_version
    }
}

#[derive(Debug, Serialize)]
struct AdminHealthRuntime {
    seconds: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminHealthDatabase {
    status: &'static str,
    backend: &'static str,
    journal_mode: &'static str,
    writable: bool,
    pool: AdminHealthDatabasePool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminHealthDatabasePool {
    max_connections: u32,
    size: u32,
    idle: u32,
    in_use: u32,
    saturated: bool,
}

#[derive(Debug, Serialize)]
struct AdminHealthConfig {
    available: bool,
    writable: bool,
}

#[derive(Debug, Serialize)]
struct AdminHealthAvailability {
    available: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminHealthJobs {
    scan_running: i64,
    scan_failed: i64,
    metadata_reidentify_running: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminHealthLibrary {
    id: String,
    name: String,
    is_enabled: bool,
    root_count: usize,
    available_root_count: usize,
    writable_root_count: usize,
}

pub async fn admin_health_report(
    database: &Database,
    config_dir: Option<&Path>,
    libraries: Option<&LibraryService>,
    resources: &ResourceMetrics,
    probe_cache: &AdminHealthProbeCache,
) -> Result<AdminHealthReport, AdminHealthError> {
    let schema_version = database
        .schema_version()
        .await
        .map_err(|_| AdminHealthError::DatabaseUnavailable)?;
    let resources = resources.snapshot().await;
    let database_pool = database.pool_snapshot();
    let probes = probe_cache
        .get_or_probe(|| async {
            let database_writable = database.probe_write().await.is_ok();
            let config_available = match config_dir {
                Some(path) => fs::metadata(path)
                    .await
                    .map(|metadata| metadata.is_dir())
                    .unwrap_or(false),
                None => false,
            };
            let config_writable = match config_dir {
                Some(path) if config_available => probe_directory_writable(path).await,
                _ => false,
            };
            let ffprobe_available = Command::new("ffprobe")
                .arg("-version")
                .output()
                .await
                .is_ok_and(|output| output.status.success());
            AdminHealthProbeSnapshot {
                database_writable,
                config_available,
                config_writable,
                ffprobe_available,
            }
        })
        .await;

    let library_status = match libraries {
        Some(libraries) => libraries
            .list_libraries()
            .await
            .map_err(|_| AdminHealthError::LibrariesUnavailable)?
            .iter()
            .map(|view| AdminHealthLibrary {
                id: view.library.id.to_string(),
                name: view.library.name.clone(),
                is_enabled: view.library.is_enabled,
                root_count: view.roots.len(),
                available_root_count: view.roots.iter().filter(|root| root.is_available).count(),
                writable_root_count: view.roots.iter().filter(|root| root.is_writable).count(),
            })
            .collect(),
        None => Vec::new(),
    };
    let scan_jobs = database
        .count_scan_jobs_by_status()
        .await
        .map_err(|_| AdminHealthError::ScanJobsUnavailable)?;
    let metadata_reidentify_running = database
        .list_active_metadata_reidentify_job_ids()
        .await
        .map_err(|_| AdminHealthError::MetadataJobsUnavailable)?
        .len();
    let (database_backend, journal_mode) = match database.backend() {
        DatabaseBackend::Sqlite => ("SQLITE", "wal"),
        DatabaseBackend::Postgres => ("POSTGRESQL", ""),
    };
    let status = if probes.database_writable
        && probes.config_available
        && probes.config_writable
        && probes.ffprobe_available
    {
        "ok"
    } else {
        "degraded"
    };

    Ok(AdminHealthReport {
        status,
        schema_version,
        runtime: AdminHealthRuntime {
            seconds: resources.runtime_seconds,
        },
        resources,
        database: AdminHealthDatabase {
            status: if probes.database_writable {
                "ok"
            } else {
                "degraded"
            },
            backend: database_backend,
            journal_mode,
            writable: probes.database_writable,
            pool: database_pool.into(),
        },
        config: AdminHealthConfig {
            available: probes.config_available,
            writable: probes.config_writable,
        },
        ffprobe: AdminHealthAvailability {
            available: probes.ffprobe_available,
        },
        jobs: AdminHealthJobs {
            scan_running: scan_jobs.running,
            scan_failed: scan_jobs.failed,
            metadata_reidentify_running,
        },
        libraries: library_status,
    })
}

impl From<DatabasePoolSnapshot> for AdminHealthDatabasePool {
    fn from(pool: DatabasePoolSnapshot) -> Self {
        Self {
            max_connections: pool.max_connections,
            size: pool.size,
            idle: pool.idle,
            in_use: pool.in_use,
            saturated: pool.saturated,
        }
    }
}

async fn probe_directory_writable(path: &Path) -> bool {
    let probe_path = path.join(format!(".lux-health-probe-{}", uuid::Uuid::now_v7()));
    let payload = [0_u8; 4096];
    let result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe_path)
            .await?;
        file.write_all(&payload).await?;
        file.sync_all().await
    }
    .await;
    let _ = fs::remove_file(probe_path).await;
    result.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn admin_health_probe_cache_reuses_results_and_coalesces_expired_refreshes() {
        let cache = AdminHealthProbeCache::default();
        let now = Instant::now();
        let probes = Arc::new(AtomicUsize::new(0));
        let first = {
            let probes = Arc::clone(&probes);
            cache
                .get_or_probe_at(now, move || async move {
                    probes.fetch_add(1, Ordering::Relaxed);
                    AdminHealthProbeSnapshot {
                        database_writable: true,
                        config_available: true,
                        config_writable: true,
                        ffprobe_available: true,
                    }
                })
                .await
        };
        let cached = cache
            .get_or_probe_at(now + Duration::from_secs(29), || async {
                AdminHealthProbeSnapshot::default()
            })
            .await;
        assert_eq!(cached, first);
        assert_eq!(probes.load(Ordering::Relaxed), 1);

        let refresh_at = now + Duration::from_secs(31);
        let first_cache = cache.clone();
        let second_cache = cache.clone();
        let first_probes = Arc::clone(&probes);
        let second_probes = Arc::clone(&probes);
        let (first_refresh, second_refresh) = tokio::join!(
            first_cache.get_or_probe_at(refresh_at, move || async move {
                first_probes.fetch_add(1, Ordering::Relaxed);
                tokio::task::yield_now().await;
                AdminHealthProbeSnapshot::default()
            }),
            second_cache.get_or_probe_at(refresh_at, move || async move {
                second_probes.fetch_add(1, Ordering::Relaxed);
                AdminHealthProbeSnapshot::default()
            }),
        );
        assert_eq!(first_refresh, second_refresh);
        assert_eq!(probes.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn admin_health_probe_cache_rechecks_expiry_after_waiting_for_refresh_lock() {
        let cache = AdminHealthProbeCache::default();
        {
            let mut cached = cache.cached.lock().await;
            *cached = Some(CachedAdminHealthProbes {
                expires_at: Instant::now() + Duration::from_millis(100),
                snapshot: AdminHealthProbeSnapshot::default(),
            });
        }

        let guard = cache.cached.lock().await;
        let probes = Arc::new(AtomicUsize::new(0));
        let request_cache = cache.clone();
        let request_probes = Arc::clone(&probes);
        let mut request = tokio::spawn(async move {
            request_cache
                .get_or_probe(move || async move {
                    request_probes.fetch_add(1, Ordering::Relaxed);
                    AdminHealthProbeSnapshot {
                        database_writable: true,
                        ..Default::default()
                    }
                })
                .await
        });

        let while_locked = tokio::time::timeout(Duration::from_millis(10), &mut request).await;
        assert!(
            while_locked.is_err(),
            "request should wait for the cache lock"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(guard);

        let refreshed = request.await.expect("health probe task should finish");
        assert!(refreshed.database_writable);
        assert_eq!(probes.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn admin_health_probe_cache_ttl_starts_after_probe_finishes() {
        let cache = AdminHealthProbeCache::default();
        let start = Instant::now();
        let elapsed = Arc::new(AtomicUsize::new(0));
        let probes = Arc::new(AtomicUsize::new(0));
        let probe_elapsed = Arc::clone(&elapsed);
        let probe_count = Arc::clone(&probes);
        let clock_elapsed = Arc::clone(&elapsed);
        cache
            .get_or_probe_with_clock(
                move || async move {
                    probe_count.fetch_add(1, Ordering::Relaxed);
                    probe_elapsed.store(40, Ordering::Relaxed);
                    AdminHealthProbeSnapshot {
                        database_writable: true,
                        ..Default::default()
                    }
                },
                move || start + Duration::from_secs(clock_elapsed.load(Ordering::Relaxed) as u64),
            )
            .await;

        let cached = cache
            .get_or_probe_at(start + Duration::from_secs(69), || async {
                AdminHealthProbeSnapshot::default()
            })
            .await;
        assert!(cached.database_writable);
        assert_eq!(probes.load(Ordering::Relaxed), 1);
    }
}
