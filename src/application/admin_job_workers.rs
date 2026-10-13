use crate::{
    application::{
        chapter_detector::{
            ChapterDetectionError, ChapterDetectionJob, ChapterDetectionOptions,
            ChapterDetectionService,
        },
        danmaku::{DanmakuMatchJob, DanmakuService, DanmakuServiceError},
        plugins::{PluginService, PluginServiceError},
        strm_probe::{StrmProbeError, StrmProbeJob, StrmProbeService},
    },
    domain::ids::LibraryId,
};
use std::sync::Arc;

#[derive(Clone, Copy, Default)]
pub(crate) struct ChapterDetectionOverrides {
    pub(crate) concurrency: Option<i64>,
    pub(crate) intro_window_seconds: Option<i64>,
    pub(crate) credits_window_seconds: Option<i64>,
    pub(crate) match_threshold: Option<u32>,
    pub(crate) force_refresh: bool,
}

pub(crate) enum AdminChapterDetectionError {
    ServiceUnavailable,
    Detection(ChapterDetectionError),
}

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

pub(crate) async fn start_admin_chapter_detection_job(
    service: Option<&ChapterDetectionService>,
    plugins: Option<&PluginService>,
    library_id: LibraryId,
    requested_plugin_id: Option<&str>,
    overrides: ChapterDetectionOverrides,
) -> Result<ChapterDetectionJob, AdminChapterDetectionError> {
    let service = service.ok_or(AdminChapterDetectionError::ServiceUnavailable)?;
    let plugin_id = requested_plugin_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(crate::application::chapter_detector::DEFAULT_CHAPTER_DETECTOR_PLUGIN_ID);
    let configured_options = match plugins {
        Some(plugins) => match plugins.chapter_detector_settings(plugin_id).await {
            Ok(settings) => Some(ChapterDetectionOptions {
                concurrency: settings.concurrency,
                intro_window_seconds: settings.intro_window_seconds,
                credits_window_seconds: settings.credits_window_seconds,
                match_threshold: settings.match_threshold,
                force_refresh: false,
            }),
            Err(
                PluginServiceError::InvalidConfig
                | PluginServiceError::UnknownPlugin(_)
                | PluginServiceError::Unavailable(_),
            ) => None,
            Err(error) => {
                return Err(AdminChapterDetectionError::Detection(
                    ChapterDetectionError::Plugin(error),
                ));
            }
        },
        None => None,
    }
    .unwrap_or_default();
    let options = ChapterDetectionOptions {
        concurrency: overrides
            .concurrency
            .unwrap_or(configured_options.concurrency),
        intro_window_seconds: overrides
            .intro_window_seconds
            .unwrap_or(configured_options.intro_window_seconds),
        credits_window_seconds: overrides
            .credits_window_seconds
            .unwrap_or(configured_options.credits_window_seconds),
        match_threshold: overrides
            .match_threshold
            .unwrap_or(configured_options.match_threshold),
        force_refresh: overrides.force_refresh,
    };
    start_chapter_detection_job(service, library_id, plugin_id, options)
        .await
        .map_err(AdminChapterDetectionError::Detection)
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
