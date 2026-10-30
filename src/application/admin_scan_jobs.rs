use std::path::Path;

use crate::{
    application::{
        probe::MediaProbeService,
        reidentify::MetadataReidentifyService,
        scanner::{BACKGROUND_SCAN_BATCH_SIZE, ScanJob, ScanJobError, ScanJobService},
        thumbnails::ThumbnailService,
    },
    domain::ids::LibraryId,
    observability::logs::{LogStore, ScanJobLogEvent},
    storage::{Database, StorageError, StoredScanJob},
};

pub(crate) struct AdminScanJob {
    pub(crate) job: StoredScanJob,
    pub(crate) has_postprocessing_issues: bool,
    pub(crate) can_retry: bool,
}

pub(crate) async fn list_admin_scan_jobs(
    database: &Database,
    status: Option<&str>,
    has_postprocessing_issues: Option<bool>,
    offset: i64,
    limit: i64,
) -> Result<Vec<AdminScanJob>, StorageError> {
    let jobs = database
        .list_admin_scan_jobs(status, has_postprocessing_issues, offset, limit)
        .await?;
    attach_retry_capabilities(database, jobs).await
}

pub(crate) async fn find_admin_scan_job(
    database: &Database,
    job_id: &str,
) -> Result<Option<AdminScanJob>, StorageError> {
    let Some(job) = database.find_scan_job(job_id).await? else {
        return Ok(None);
    };
    Ok(attach_retry_capabilities(database, vec![job]).await?.pop())
}

async fn attach_retry_capabilities(
    database: &Database,
    jobs: Vec<StoredScanJob>,
) -> Result<Vec<AdminScanJob>, StorageError> {
    let ids = jobs.iter().map(|job| job.id.clone()).collect::<Vec<_>>();
    let states = database.scan_job_retry_states(&ids).await?;
    Ok(jobs
        .into_iter()
        .map(|job| {
            let recovery = states.get(&job.id);
            AdminScanJob {
                has_postprocessing_issues: recovery
                    .is_some_and(|state| state.has_postprocessing_issues),
                can_retry: recovery
                    .is_some_and(|state| ScanJobService::can_retry_stored_job(&job, state)),
                job,
            }
        })
        .collect())
}

pub(crate) enum AdminScanJobEventsError {
    NotFound,
    Unavailable,
}

pub(crate) async fn list_admin_scan_job_events(
    database: Option<&Database>,
    config_dir: Option<&Path>,
    job_id: &str,
    level: Option<&str>,
    event_code: Option<&str>,
    offset: i64,
    limit: i64,
) -> Result<(i64, Vec<ScanJobLogEvent>), AdminScanJobEventsError> {
    let database = database.ok_or(AdminScanJobEventsError::Unavailable)?;
    match database.find_scan_job(job_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return Err(AdminScanJobEventsError::NotFound),
        Err(_) => return Err(AdminScanJobEventsError::Unavailable),
    }
    let config_dir = config_dir.ok_or(AdminScanJobEventsError::Unavailable)?;
    LogStore::new(config_dir)
        .list_scan_job_events(job_id, level, event_code, offset, limit)
        .await
        .map_err(|_| AdminScanJobEventsError::Unavailable)
}

#[derive(Clone, Default)]
pub(crate) struct ScanWorkerServices {
    pub(crate) probe: Option<MediaProbeService>,
    pub(crate) metadata: Option<MetadataReidentifyService>,
    pub(crate) thumbnails: Option<ThumbnailService>,
}

pub(crate) async fn start_movie_scan_job(
    scan_jobs: &ScanJobService,
    library_id: LibraryId,
    workers: ScanWorkerServices,
) -> Result<ScanJob, ScanJobError> {
    let job = scan_jobs.create_movie_scan_job(library_id).await?;
    spawn_scan_job_worker(scan_jobs, &job.id, workers);
    Ok(job)
}

pub(crate) async fn start_path_scan_job(
    scan_jobs: &ScanJobService,
    library_id: LibraryId,
    root_id: Option<&str>,
    relative_path: &str,
    workers: ScanWorkerServices,
) -> Result<ScanJob, ScanJobError> {
    let job = scan_jobs
        .create_path_scan_job(library_id, root_id, relative_path)
        .await?;
    spawn_scan_job_worker(scan_jobs, &job.id, workers);
    Ok(job)
}

pub(crate) async fn start_item_folder_scan_job(
    scan_jobs: &ScanJobService,
    item_id: &str,
    workers: ScanWorkerServices,
) -> Result<ScanJob, ScanJobError> {
    let job = scan_jobs.create_item_folder_scan_job(item_id).await?;
    spawn_scan_job_worker(scan_jobs, &job.id, workers);
    Ok(job)
}

pub(crate) async fn retry_scan_job(
    scan_jobs: &ScanJobService,
    job_id: &str,
    workers: ScanWorkerServices,
) -> Result<ScanJob, ScanJobError> {
    let job = scan_jobs.retry(job_id).await?;
    spawn_scan_job_worker(scan_jobs, &job.id, workers);
    Ok(job)
}

pub(crate) async fn start_library_scan(
    scan_jobs: &ScanJobService,
    library_id: LibraryId,
    workers: ScanWorkerServices,
) -> Result<Option<ScanJob>, ScanJobError> {
    let job = match scan_jobs
        .create_movie_scan_job_with_metadata(library_id, false)
        .await
    {
        Ok(job) => job,
        Err(ScanJobError::AlreadyActive(_)) => return Ok(None),
        Err(error) => return Err(error),
    };
    spawn_scan_job_worker(scan_jobs, &job.id, workers);
    Ok(Some(job))
}

/// How many libraries one page of the first-scan recovery reads from storage.
pub(crate) const FIRST_SCAN_RESUME_PAGE_SIZE: i64 = 32;

/// Upper bound of candidates one startup examines and queues.
///
/// The recovery pages through every eligible library, but a pathological database must not turn one
/// startup into an unbounded scan storm (the scan queue serializes the work anyway). Every examined
/// candidate costs a filesystem probe, so the bound covers both the queued and the skipped ones.
/// When the bound trips, the remaining libraries stay eligible for the next startup and are counted
/// (up to [`FIRST_SCAN_RESUME_REMAINDER_PROBE_LIMIT`]) for the log.
pub(crate) const FIRST_SCAN_RESUME_MAX_LIBRARIES: usize = 512;

/// How many still-eligible libraries the "reached the limit" log counts.
///
/// The count is only for the log; when it hits this bound the log says "at least" instead of
/// claiming an exact number.
const FIRST_SCAN_RESUME_REMAINDER_PROBE_LIMIT: i64 = 10_000;

/// How many shutdown interruptions of the same first scan one startup tolerates automatically.
///
/// A first scan that keeps killing the process (OOM, `SIGKILL`) never records `FAILED`: each
/// restart cancels it with `SERVER_SHUTDOWN` and the recovery queues it again, which would loop
/// without any backoff. After this many interruptions the library is left to the administrator.
pub(crate) const FIRST_SCAN_RESUME_MAX_INTERRUPTIONS: i64 = 3;

/// How long the probe of a single root may take before the root counts as unavailable.
///
/// A hung network mount must not hold up startup. The probe itself is `crate::library::
/// inspect_root_path`, which uses `tokio::fs` (the blocking pool), so this only bounds how long the
/// recovery waits for an answer.
pub(crate) const FIRST_SCAN_ROOT_PROBE_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(5);

/// How many root probes may run at the same time.
///
/// A probe of a lost mount keeps its blocking thread until the filesystem call answers, no matter
/// how soon the recovery stops waiting. Bounding the concurrency keeps that from growing with the
/// number of candidate libraries and starving every other `tokio::fs` user in the process.
pub(crate) const FIRST_SCAN_ROOT_PROBE_CONCURRENCY: usize = 4;

/// Outcome of one startup's first-scan recovery.
pub(crate) struct FirstScanRecovery {
    /// How many libraries were queued for a new first scan.
    pub(crate) resumed: usize,
    /// How many still-eligible libraries the per-startup bound left for the next startup.
    pub(crate) skipped_by_limit: usize,
    /// Whether `skipped_by_limit` is only a lower bound because the count was truncated.
    pub(crate) skipped_by_limit_truncated: bool,
}

/// Requeues the first scans that a shutdown cancelled mid-way.
///
/// The intent of a first scan is already persisted (the `scan_jobs` row setup created before it
/// returned `201`), so the recovery needs no new marker: a `CANCELLED` `RECONCILE_LIBRARY` job with
/// `error = 'SERVER_SHUTDOWN'` for a library that was never scanned is the evidence. Every library
/// gets a *new* job, which keeps the history readable (cancelled job, then the replacement) and
/// avoids resuming a manifest that was only half written.
///
/// Pages through the eligible libraries instead of taking a single batch, so a shutdown that
/// interrupted more libraries than one page holds still recovers all of them.
pub(crate) async fn resume_shutdown_interrupted_first_scans(
    database: &Database,
    scan_jobs: &ScanJobService,
    workers: ScanWorkerServices,
) -> Result<FirstScanRecovery, StorageError> {
    resume_shutdown_interrupted_first_scans_bounded(
        database,
        scan_jobs,
        workers,
        FIRST_SCAN_RESUME_MAX_LIBRARIES,
        FIRST_SCAN_RESUME_REMAINDER_PROBE_LIMIT,
    )
    .await
}

/// Outcome of probing the filesystem that hosts a library root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MountProbe {
    Available,
    Unavailable,
    TimedOut,
}

/// The future a root probe returns; `true` means the path is usable.
type RootProbeFuture = std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>;

/// A root probe, injectable so tests can model a mount that never answers.
type RootProbeFn = std::sync::Arc<dyn Fn(std::path::PathBuf) -> RootProbeFuture + Send + Sync>;

/// Probes the roots of the first-scan candidates without letting one lost mount dominate startup.
///
/// The stored `library_roots.is_available` flag is not refreshed at startup (only the scan path
/// writes it), so a mount that dropped during a scan stays marked unavailable even after it is back.
/// Deciding on the column alone would skip such a library on every startup, which is why the
/// recovery probes the paths instead. A stale flag is corrected to `true` on a successful probe,
/// because a queued scan discards roots the column calls unavailable.
///
/// Three properties keep a missing NAS share affordable:
/// - roots are grouped by their mount prefix, so every root of one share is probed once per startup
///   instead of once per library (a lost share then costs one timeout, not one per candidate);
/// - at most [`FIRST_SCAN_ROOT_PROBE_CONCURRENCY`] probes run at a time;
/// - a probe that exhausts its budget is logged with the library id and the path, so the abandoned
///   work is visible in the log.
struct FirstScanProber<'a> {
    database: &'a Database,
    probe: RootProbeFn,
    timeout: std::time::Duration,
    permits: tokio::sync::Semaphore,
    mount_outcomes: tokio::sync::Mutex<std::collections::HashMap<String, MountProbe>>,
    probes_started: std::sync::atomic::AtomicUsize,
    probes_timed_out: std::sync::atomic::AtomicUsize,
    probes_running: std::sync::atomic::AtomicUsize,
    probes_running_peak: std::sync::atomic::AtomicUsize,
}

impl<'a> FirstScanProber<'a> {
    fn new(database: &'a Database) -> Self {
        Self::with_probe(
            database,
            FIRST_SCAN_ROOT_PROBE_TIMEOUT,
            std::sync::Arc::new(|path| {
                Box::pin(async move { crate::library::inspect_root_path(&path).await.is_ok() })
            }),
        )
    }

    fn with_probe(
        database: &'a Database,
        timeout: std::time::Duration,
        probe: RootProbeFn,
    ) -> Self {
        Self {
            database,
            probe,
            timeout,
            permits: tokio::sync::Semaphore::new(FIRST_SCAN_ROOT_PROBE_CONCURRENCY),
            mount_outcomes: tokio::sync::Mutex::new(std::collections::HashMap::new()),
            probes_started: std::sync::atomic::AtomicUsize::new(0),
            probes_timed_out: std::sync::atomic::AtomicUsize::new(0),
            probes_running: std::sync::atomic::AtomicUsize::new(0),
            probes_running_peak: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Probes one mount, or reuses what this startup already learned about it.
    async fn probe_mount(&self, library_id: &str, root_path: &std::path::Path) -> MountProbe {
        let key = first_scan_mount_key(root_path);
        if let Some(outcome) = self.known_mount(&key).await {
            return outcome;
        }
        // The permit is what bounds the number of blocking threads a lost mount can occupy.
        let Ok(_permit) = self.permits.acquire().await else {
            return MountProbe::Unavailable;
        };
        // Another candidate may have finished the same mount while this one waited for a permit.
        if let Some(outcome) = self.known_mount(&key).await {
            return outcome;
        }
        self.probes_started
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let running = self
            .probes_running
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        self.probes_running_peak
            .fetch_max(running, std::sync::atomic::Ordering::Relaxed);
        let probe = (self.probe)(root_path.to_path_buf());
        let outcome = match first_scan_probe_within(self.timeout, probe).await {
            Some(true) => MountProbe::Available,
            Some(false) => MountProbe::Unavailable,
            None => {
                self.probes_timed_out
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                tracing::warn!(
                    %library_id,
                    path = %root_path.display(),
                    mount = %key,
                    "probing a library root timed out; the mount counts as unavailable for this                      startup and is not probed again"
                );
                MountProbe::TimedOut
            }
        };
        self.probes_running
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        self.mount_outcomes.lock().await.insert(key, outcome);
        outcome
    }

    async fn known_mount(&self, key: &str) -> Option<MountProbe> {
        self.mount_outcomes.lock().await.get(key).copied()
    }

    /// Whether any root of the library is usable right now, and why not when none is.
    async fn root_state(&self, library_id: &str) -> Result<FirstScanRootState, StorageError> {
        let roots = self.database.list_library_roots(library_id).await?;
        if roots.is_empty() {
            return Ok(FirstScanRootState::Unavailable("the library has no root"));
        }
        let mut reason = "every library root is unavailable";
        for root in roots {
            let path = std::path::PathBuf::from(&root.canonical_path);
            match self.probe_mount(library_id, &path).await {
                // One usable root is enough to queue the scan, and every further probe could cost
                // another full timeout on a lost mount.
                MountProbe::Available => {
                    if !root.is_available
                        && let Err(error) = self
                            .database
                            .update_library_root_availability(&root.id, true)
                            .await
                    {
                        tracing::warn!(
                            %library_id,
                            root_id = %root.id,
                            %error,
                            "could not refresh the availability of a library root"
                        );
                    }
                    return Ok(FirstScanRootState::Available);
                }
                MountProbe::Unavailable => {}
                MountProbe::TimedOut => {
                    reason = "probing a library root timed out";
                }
            }
        }
        Ok(FirstScanRootState::Unavailable(reason))
    }
}

/// Groups the roots of one share under a key.
///
/// `library_roots.canonical_path` is already canonical (it is stored after `canonicalize` when the
/// root is added), so no filesystem call is needed here: the first two components (`/Volumes/NAS`)
/// identify the mount that all of them live on.
fn first_scan_mount_key(root_path: &std::path::Path) -> String {
    let mut components = root_path.components();
    let mut key = std::path::PathBuf::new();
    if let Some(root) = components.next() {
        key.push(root);
    }
    if let Some(first) = components.next() {
        key.push(first);
    }
    if key.as_os_str().is_empty() {
        return root_path.to_string_lossy().into_owned();
    }
    key.to_string_lossy().into_owned()
}

enum FirstScanRootState {
    Available,
    Unavailable(&'static str),
}

/// Runs one root probe under a time budget; `None` means the budget was exhausted.
async fn first_scan_probe_within<F, T>(budget: std::time::Duration, probe: F) -> Option<T>
where
    F: std::future::Future<Output = T>,
{
    tokio::time::timeout(budget, probe).await.ok()
}

/// [`resume_shutdown_interrupted_first_scans`] with an explicit per-startup bound.
///
/// The bound and the remainder-count limit are parameters so the accounting can be tested without
/// creating 512 libraries or 10,000 rows.
async fn resume_shutdown_interrupted_first_scans_bounded(
    database: &Database,
    scan_jobs: &ScanJobService,
    workers: ScanWorkerServices,
    max_libraries: usize,
    remainder_limit: i64,
) -> Result<FirstScanRecovery, StorageError> {
    let prober = FirstScanProber::new(database);
    let mut cursor: Option<String> = None;
    // The last library the loop actually handled. The limit accounting counts what comes after it,
    // which also covers the libraries that sit behind the limit inside the same page.
    let mut last_processed: Option<String> = None;
    let mut examined = 0_usize;
    let mut resumed = 0_usize;
    loop {
        let library_ids = database
            .list_shutdown_interrupted_first_scan_library_ids(
                cursor.as_deref(),
                FIRST_SCAN_RESUME_PAGE_SIZE,
            )
            .await?;
        if library_ids.is_empty() {
            break;
        }
        let last_page = (library_ids.len() as i64) < FIRST_SCAN_RESUME_PAGE_SIZE;
        cursor = library_ids.last().cloned();
        for library_id in library_ids {
            if examined >= max_libraries {
                let remaining = database
                    .list_shutdown_interrupted_first_scan_library_ids(
                        last_processed.as_deref(),
                        remainder_limit,
                    )
                    .await?;
                let recovery = FirstScanRecovery {
                    resumed,
                    skipped_by_limit: remaining.len(),
                    // The remainder query is capped, so hitting the cap only proves a lower bound.
                    skipped_by_limit_truncated: (remaining.len() as i64) >= remainder_limit,
                };
                if recovery.skipped_by_limit_truncated {
                    tracing::warn!(
                        max = max_libraries,
                        queued = recovery.resumed,
                        remaining_at_least = recovery.skipped_by_limit,
                        "first-scan recovery examined its per-startup limit; at least that many more \
                         libraries stay eligible for the next startup (the remaining count was \
                         truncated)"
                    );
                } else {
                    tracing::warn!(
                        max = max_libraries,
                        queued = recovery.resumed,
                        remaining = recovery.skipped_by_limit,
                        "first-scan recovery examined its per-startup limit; the remaining \
                         libraries stay eligible for the next startup"
                    );
                }
                return Ok(recovery);
            }
            examined += 1;
            last_processed = Some(library_id.clone());
            let Ok(parsed_library_id) = library_id.parse::<LibraryId>() else {
                tracing::warn!(
                    %library_id,
                    "ignoring an unparsable library id from the first-scan recovery query"
                );
                continue;
            };
            match database
                .count_shutdown_interrupted_first_scans(&library_id)
                .await
            {
                Ok(interruptions) if interruptions >= FIRST_SCAN_RESUME_MAX_INTERRUPTIONS => {
                    tracing::warn!(
                        %library_id,
                        interruptions,
                        "the first scan was interrupted by a shutdown that many times; it is not \
                         re-queued automatically any more — start a scan by hand and check memory \
                         usage and the logs"
                    );
                    continue;
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(
                        %library_id,
                        %error,
                        "could not count the shutdown interruptions of a first scan"
                    );
                }
            }
            match prober.root_state(&library_id).await {
                // A scan against a missing mount reports success with zero items and records
                // `last_scan_at`, which would consume the first-scan intent. Probing now keeps the
                // library eligible: the next startup probes again.
                Ok(FirstScanRootState::Unavailable(reason)) => {
                    tracing::warn!(
                        %library_id,
                        reason,
                        "first scan not re-queued: no media root is usable right now; the next \
                         startup probes again"
                    );
                    continue;
                }
                Ok(FirstScanRootState::Available) => {}
                Err(error) => {
                    tracing::warn!(
                        %library_id,
                        %error,
                        "could not probe the media roots of an interrupted first scan"
                    );
                    continue;
                }
            }
            match start_library_scan(scan_jobs, parsed_library_id, workers.clone()).await {
                Ok(Some(job)) => {
                    tracing::info!(
                        %library_id,
                        job_id = %job.id,
                        "restarted a first scan that a shutdown interrupted"
                    );
                    resumed += 1;
                }
                // `None` means a scan appeared between the query and now (a manual trigger won the
                // race), which is exactly the work the recovery would have queued.
                Ok(None) => {}
                // The library was disabled or deleted in between: nothing to recover.
                Err(ScanJobError::LibraryNotFound) => {}
                Err(error) => {
                    tracing::warn!(
                        %library_id,
                        %error,
                        "an interrupted first scan could not be restarted"
                    );
                }
            }
        }
        if last_page {
            break;
        }
    }
    tracing::debug!(
        probes = prober
            .probes_started
            .load(std::sync::atomic::Ordering::Relaxed),
        timed_out = prober
            .probes_timed_out
            .load(std::sync::atomic::Ordering::Relaxed),
        peak_concurrency = prober
            .probes_running_peak
            .load(std::sync::atomic::Ordering::Relaxed),
        "first-scan recovery finished probing the media roots"
    );
    Ok(FirstScanRecovery {
        resumed,
        skipped_by_limit: 0,
        skipped_by_limit_truncated: false,
    })
}

pub(crate) fn spawn_scan_job_worker(
    scan_jobs: &ScanJobService,
    job_id: &str,
    workers: ScanWorkerServices,
) {
    let worker = scan_jobs.clone();
    let job_id = job_id.to_owned();
    tokio::spawn(async move {
        let _ = worker
            .run_to_completion_with_metadata_and_thumbnails(
                &job_id,
                BACKGROUND_SCAN_BATCH_SIZE,
                workers.probe,
                workers.metadata,
                workers.thumbnails,
            )
            .await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{application::libraries::LibraryService, config::Config, library::LibraryKind};

    /// Creates `count` libraries, each with an available root and a first scan that a shutdown
    /// cancelled, i.e. exactly the state the recovery is meant to pick up.
    async fn cancelled_first_scans(
        count: usize,
    ) -> Result<(tempfile::TempDir, Database, ScanJobService), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        })
        .await?;
        let libraries = LibraryService::new(database.clone());
        let scan_jobs = ScanJobService::new(database.clone());
        for index in 0..count {
            let library = libraries
                .create_library(&format!("Movies {index:02}"), LibraryKind::Movie, false)
                .await?;
            let root_path = temp_dir.path().join(format!("movies-{index:02}"));
            tokio::fs::create_dir_all(&root_path).await?;
            libraries
                .add_root(library.id, root_path.to_str().ok_or("non-UTF-8 root path")?)
                .await?;
            scan_jobs.create_movie_scan_job(library.id).await?;
        }
        database.cancel_incomplete_jobs_for_shutdown().await?;
        Ok((temp_dir, database, scan_jobs))
    }

    #[tokio::test]
    async fn the_limit_log_reports_a_lower_bound_when_the_remainder_is_truncated()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database, scan_jobs) = cancelled_first_scans(6).await?;
        let recovery = resume_shutdown_interrupted_first_scans_bounded(
            &database,
            &scan_jobs,
            ScanWorkerServices::default(),
            2,
            // A remainder budget that cannot cover the four libraries left behind.
            2,
        )
        .await?;

        assert_eq!(recovery.resumed, 2);
        assert_eq!(recovery.skipped_by_limit, 2);
        assert!(
            recovery.skipped_by_limit_truncated,
            "a truncated remainder has to be reported as \"at least\" instead of an exact number"
        );
        Ok(())
    }

    /// A probe that never resolves models a share that stopped answering.
    fn hanging_probe() -> RootProbeFn {
        std::sync::Arc::new(|_path: std::path::PathBuf| {
            Box::pin(std::future::pending::<bool>()) as RootProbeFuture
        })
    }

    #[tokio::test]
    async fn the_root_probe_gives_up_instead_of_hanging_the_startup()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database, _scan_jobs) = cancelled_first_scans(0).await?;
        let prober = FirstScanProber::with_probe(
            &database,
            std::time::Duration::from_millis(20),
            hanging_probe(),
        );

        let started = std::time::Instant::now();
        let outcome = prober
            .probe_mount("library", std::path::Path::new("/mnt/hung/movies"))
            .await;

        assert_eq!(outcome, MountProbe::TimedOut);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "the wait has to stay bounded, took {:?}",
            started.elapsed()
        );
        assert_eq!(
            prober
                .probes_timed_out
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn every_candidate_on_one_lost_mount_costs_a_single_probe()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database, _scan_jobs) = cancelled_first_scans(0).await?;
        let timeout = std::time::Duration::from_millis(20);
        let prober = FirstScanProber::with_probe(&database, timeout, hanging_probe());
        let mount = std::path::Path::new("/Volumes/lost-nas/movies");

        let started = std::time::Instant::now();
        for index in 0..512 {
            assert_eq!(
                prober
                    .probe_mount(&format!("library-{index:03}"), mount)
                    .await,
                MountProbe::TimedOut
            );
        }

        assert_eq!(
            prober
                .probes_started
                .load(std::sync::atomic::Ordering::Relaxed),
            1,
            "all 512 candidates share one mount, so it is probed once"
        );
        assert_eq!(
            prober
                .probes_timed_out
                .load(std::sync::atomic::Ordering::Relaxed),
            1
        );
        assert!(
            started.elapsed() < timeout * 2,
            "512 candidates must cost one timeout, took {:?}",
            started.elapsed()
        );
        Ok(())
    }

    #[tokio::test]
    async fn the_probes_never_exceed_the_concurrency_limit()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database, _scan_jobs) = cancelled_first_scans(0).await?;
        let prober = FirstScanProber::with_probe(
            &database,
            std::time::Duration::from_secs(5),
            std::sync::Arc::new(|_path: std::path::PathBuf| {
                Box::pin(async {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    true
                }) as RootProbeFuture
            }),
        );
        let paths = (0..8)
            .map(|index| std::path::PathBuf::from(format!("/mnt/share{index}/movies")))
            .collect::<Vec<_>>();
        let probe = |index: usize| prober.probe_mount("library", &paths[index]);

        let outcomes = tokio::join!(
            probe(0),
            probe(1),
            probe(2),
            probe(3),
            probe(4),
            probe(5),
            probe(6),
            probe(7)
        );
        for outcome in [
            outcomes.0, outcomes.1, outcomes.2, outcomes.3, outcomes.4, outcomes.5, outcomes.6,
            outcomes.7,
        ] {
            assert_eq!(outcome, MountProbe::Available);
        }

        let peak = prober
            .probes_running_peak
            .load(std::sync::atomic::Ordering::Relaxed);
        assert!(
            peak <= FIRST_SCAN_ROOT_PROBE_CONCURRENCY,
            "peak concurrency {peak} exceeded the limit {FIRST_SCAN_ROOT_PROBE_CONCURRENCY}"
        );
        assert!(
            peak >= 2,
            "the probes are supposed to overlap, peak was only {peak}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn the_limit_accounting_counts_every_library_left_for_the_next_startup()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database, scan_jobs) = cancelled_first_scans(5).await?;
        let recovery = resume_shutdown_interrupted_first_scans_bounded(
            &database,
            &scan_jobs,
            ScanWorkerServices::default(),
            2,
            FIRST_SCAN_RESUME_REMAINDER_PROBE_LIMIT,
        )
        .await?;

        assert_eq!(recovery.resumed, 2);
        // The three libraries that did not fit are all still eligible, including the ones that sat
        // behind the limit inside the same page. Under-counting them would hide work from the log.
        assert_eq!(recovery.skipped_by_limit, 3);
        assert!(
            !recovery.skipped_by_limit_truncated,
            "a remainder below the count budget is exact"
        );
        Ok(())
    }
}
