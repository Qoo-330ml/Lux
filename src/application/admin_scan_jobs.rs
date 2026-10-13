use crate::{
    application::{
        probe::MediaProbeService,
        reidentify::MetadataReidentifyService,
        scanner::{BACKGROUND_SCAN_BATCH_SIZE, ScanJob, ScanJobError, ScanJobService},
        thumbnails::ThumbnailService,
    },
    domain::ids::LibraryId,
};

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
