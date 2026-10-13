use crate::{
    application::{
        chapter_detector::{
            ChapterDetectionError, ChapterDetectionJob, ChapterDetectionOptions,
            ChapterDetectionService,
        },
        danmaku::{DanmakuMatchJob, DanmakuService, DanmakuServiceError},
        strm_probe::{StrmProbeError, StrmProbeJob, StrmProbeService},
    },
    domain::ids::LibraryId,
};
use std::sync::Arc;

#[derive(Clone, Copy)]
enum StrmProbeLog {
    Admin,
    Configured,
    Retried,
}

pub(crate) fn dispatch_emby_migration_job(
    service: Arc<crate::application::emby_migration_service::EmbyMigrationService>,
    job_id: String,
) {
    service.spawn(job_id);
}

pub(crate) async fn start_admin_strm_probe_jobs(
    service: &StrmProbeService,
) -> Result<Vec<StrmProbeJob>, StrmProbeError> {
    let jobs = service.create_configured_jobs().await?;
    spawn_strm_probe_jobs(service, &jobs, StrmProbeLog::Admin);
    Ok(jobs)
}

pub(crate) async fn start_configured_strm_probe_jobs(
    service: &StrmProbeService,
) -> Result<Vec<StrmProbeJob>, StrmProbeError> {
    let jobs = service.create_configured_jobs().await?;
    spawn_strm_probe_jobs(service, &jobs, StrmProbeLog::Configured);
    Ok(jobs)
}

pub(crate) async fn retry_strm_probe_job(
    service: &StrmProbeService,
    job_id: &str,
) -> Result<StrmProbeJob, StrmProbeError> {
    let job = service.retry(job_id).await?;
    spawn_strm_probe_jobs(service, std::slice::from_ref(&job), StrmProbeLog::Retried);
    Ok(job)
}

fn spawn_strm_probe_jobs(service: &StrmProbeService, jobs: &[StrmProbeJob], log: StrmProbeLog) {
    for job in jobs {
        let worker = service.clone();
        let job_id = job.id.clone();
        tokio::spawn(async move {
            if let Err(error) = worker.run(&job_id).await {
                match log {
                    StrmProbeLog::Admin => {
                        tracing::error!(job_id = %job_id, %error, "STRM probe job stopped");
                    }
                    StrmProbeLog::Configured => {
                        tracing::error!(job_id = %job_id, %error, "configured STRM probe job stopped");
                    }
                    StrmProbeLog::Retried => {
                        tracing::error!(job_id = %job_id, %error, "retried STRM probe job stopped");
                    }
                }
            }
        });
    }
}

pub(crate) async fn start_chapter_detection_job(
    service: &ChapterDetectionService,
    library_id: LibraryId,
    plugin_id: &str,
    options: ChapterDetectionOptions,
) -> Result<ChapterDetectionJob, ChapterDetectionError> {
    let job = service
        .create_library_job(library_id, plugin_id, options)
        .await?;
    spawn_chapter_detection_job(service, &job, false);
    Ok(job)
}

pub(crate) async fn retry_chapter_detection_job(
    service: &ChapterDetectionService,
    job_id: &str,
) -> Result<ChapterDetectionJob, ChapterDetectionError> {
    let job = service.retry(job_id).await?;
    spawn_chapter_detection_job(service, &job, true);
    Ok(job)
}

fn spawn_chapter_detection_job(
    service: &ChapterDetectionService,
    job: &ChapterDetectionJob,
    retried: bool,
) {
    let worker = service.clone();
    let job_id = job.id.clone();
    tokio::spawn(async move {
        if let Err(error) = worker.run(&job_id).await {
            if retried {
                tracing::error!(job_id = %job_id, %error, "retried chapter detection job stopped");
            } else {
                tracing::error!(job_id = %job_id, %error, "chapter detection job stopped");
            }
        }
    });
}

pub(crate) async fn start_danmaku_match_job(
    service: &DanmakuService,
    library_id: LibraryId,
    concurrency: i64,
    overwrite: bool,
) -> Result<DanmakuMatchJob, DanmakuServiceError> {
    let job = service
        .create_job(library_id, concurrency, overwrite)
        .await?;
    spawn_danmaku_match_job(service, &job, false);
    Ok(job)
}

pub(crate) async fn retry_danmaku_match_job(
    service: &DanmakuService,
    job_id: &str,
) -> Result<DanmakuMatchJob, DanmakuServiceError> {
    let job = service.retry(job_id).await?;
    spawn_danmaku_match_job(service, &job, true);
    Ok(job)
}

fn spawn_danmaku_match_job(service: &DanmakuService, job: &DanmakuMatchJob, retried: bool) {
    let worker = service.clone();
    let job_id = job.id.clone();
    tokio::spawn(async move {
        if let Err(error) = worker.run(&job_id).await {
            if retried {
                tracing::error!(job_id = %job_id, %error, "retried danmaku match job stopped");
            } else {
                tracing::error!(job_id = %job_id, %error, "danmaku match job stopped");
            }
        }
    });
}
