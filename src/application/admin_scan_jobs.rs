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

/// Whether any root of the library is usable right now, and why not when none is.
///
/// The stored `library_roots.is_available` flag is not refreshed at startup (only the scan path
/// writes it), so a mount that dropped during a scan stays marked unavailable even after it is back.
/// Deciding on the column alone would skip such a library on every startup, which is why the
/// recovery probes the paths instead. A stale flag is corrected to `true` on a successful probe,
/// because a queued scan discards roots the column calls unavailable.
async fn first_scan_root_state(
    database: &Database,
    library_id: &str,
) -> Result<FirstScanRootState, StorageError> {
    let roots = database.list_library_roots(library_id).await?;
    if roots.is_empty() {
        return Ok(FirstScanRootState::Unavailable("the library has no root"));
    }
    let mut reason = "every library root is unavailable";
    let mut any_available = false;
    for root in roots {
        let path = std::path::PathBuf::from(&root.canonical_path);
        let probe = first_scan_probe_within(
            FIRST_SCAN_ROOT_PROBE_TIMEOUT,
            crate::library::inspect_root_path(&path),
        )
        .await;
        match probe {
            Some(Ok(_)) => {
                // One usable root is enough to queue the scan, and every further probe could cost
                // another full timeout on a lost mount.
                any_available = true;
                if !root.is_available
                    && let Err(error) = database
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
                break;
            }
            Some(Err(_)) => {}
            None => {
                reason = "probing a library root timed out";
                tracing::warn!(
                    %library_id,
                    root_id = %root.id,
                    "probing a library root timed out; treating the root as unavailable"
                );
            }
        }
    }
    if any_available {
        Ok(FirstScanRootState::Available)
    } else {
        Ok(FirstScanRootState::Unavailable(reason))
    }
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
            match first_scan_root_state(database, &library_id).await {
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

    #[tokio::test]
    async fn the_root_probe_gives_up_instead_of_hanging_the_startup() {
        // A hung network mount is modelled by a probe that never resolves: the budget has to end the
        // wait and report "unprobed" instead of blocking the recovery.
        let started = std::time::Instant::now();
        let probed = first_scan_probe_within(
            std::time::Duration::from_millis(20),
            std::future::pending::<()>(),
        )
        .await;

        assert!(
            probed.is_none(),
            "an exhausted budget must report the root as unprobed"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "the wait has to stay bounded, took {:?}",
            started.elapsed()
        );
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
