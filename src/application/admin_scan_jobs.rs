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
