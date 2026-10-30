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

/// Upper bound of first scans one startup queues.
///
/// The recovery pages through every eligible library, but a pathological database must not turn one
/// startup into an unbounded scan storm (the scan queue serializes the work anyway). When the bound
/// trips, the remaining libraries stay eligible for the next startup and the count is logged.
pub(crate) const FIRST_SCAN_RESUME_MAX_LIBRARIES: usize = 512;

/// How many still-eligible libraries the "reached the limit" log counts.
const FIRST_SCAN_RESUME_REMAINDER_PROBE_LIMIT: i64 = 10_000;

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
) -> Result<usize, StorageError> {
    let mut cursor: Option<String> = None;
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
            if resumed >= FIRST_SCAN_RESUME_MAX_LIBRARIES {
                let remaining = database
                    .list_shutdown_interrupted_first_scan_library_ids(
                        cursor.as_deref(),
                        FIRST_SCAN_RESUME_REMAINDER_PROBE_LIMIT,
                    )
                    .await?;
                tracing::warn!(
                    max = FIRST_SCAN_RESUME_MAX_LIBRARIES,
                    queued = resumed,
                    remaining = remaining.len(),
                    "first-scan recovery reached its per-startup limit; the remaining libraries stay \
                     eligible for the next startup"
                );
                return Ok(resumed);
            }
            let Ok(library_id) = library_id.parse::<LibraryId>() else {
                tracing::warn!(
                    %library_id,
                    "ignoring an unparsable library id from the first-scan recovery query"
                );
                continue;
            };
            match start_library_scan(scan_jobs, library_id, workers.clone()).await {
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
    Ok(resumed)
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
