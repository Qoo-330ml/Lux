use std::{fmt, sync::OnceLock, time::Instant};

use tokio::sync::Semaphore;

use crate::{
    application::{
        home::HomeService,
        plugins::PluginService,
        scanner::{ScanJobError, ScanJobService},
    },
    domain::ids::{LibraryDeletionJobId, LibraryId},
    storage::{Database, LibraryDeletionBatch, StorageError, StoredLibraryDeletionJob},
};

const MAX_CONCURRENT_LIBRARY_DELETIONS: usize = 1;

static LIBRARY_DELETION_WORKERS: OnceLock<Semaphore> = OnceLock::new();

#[derive(Clone)]
pub(crate) struct LibraryDeletionService {
    database: Database,
    scan_jobs: Option<ScanJobService>,
    plugins: Option<PluginService>,
    home: Option<HomeService>,
}

impl LibraryDeletionService {
    pub(crate) fn new(
        database: Database,
        scan_jobs: Option<ScanJobService>,
        plugins: Option<PluginService>,
        home: Option<HomeService>,
    ) -> Self {
        Self {
            database,
            scan_jobs,
            plugins,
            home,
        }
    }

    pub(crate) async fn request(
        &self,
        library_id: LibraryId,
    ) -> Result<StoredLibraryDeletionJob, LibraryDeletionError> {
        let job_id = LibraryDeletionJobId::new().to_string();
        let Some(job) = self
            .database
            .request_library_deletion(&library_id.to_string(), &job_id)
            .await?
        else {
            return Err(LibraryDeletionError::LibraryNotFound);
        };
        if matches!(job.status.as_str(), "PENDING" | "RUNNING") {
            Ok(job)
        } else {
            Err(LibraryDeletionError::RetryRequired)
        }
    }

    pub(crate) async fn list(
        &self,
        offset: i64,
        limit: i64,
        status: Option<&str>,
    ) -> Result<(Vec<StoredLibraryDeletionJob>, i64), LibraryDeletionError> {
        let jobs = self
            .database
            .list_library_deletion_jobs(offset, limit, status)
            .await?;
        let total = self.database.count_library_deletion_jobs(status).await?;
        Ok((jobs, total))
    }

    pub(crate) async fn retry(
        &self,
        job_id: &str,
    ) -> Result<StoredLibraryDeletionJob, LibraryDeletionError> {
        let Some(job) = self.database.retry_library_deletion_job(job_id).await? else {
            return Err(LibraryDeletionError::JobNotFound);
        };
        if job.status == "PENDING" {
            Ok(job)
        } else {
            Err(LibraryDeletionError::JobNotRetryable)
        }
    }

    pub(crate) fn spawn_worker(&self, job_id: &str) {
        let service = self.clone();
        let job_id = job_id.to_owned();
        let workers = LIBRARY_DELETION_WORKERS
            .get_or_init(|| Semaphore::new(MAX_CONCURRENT_LIBRARY_DELETIONS));
        tokio::spawn(async move {
            let Ok(_permit) = workers.acquire().await else {
                tracing::error!(job_id, "library deletion worker semaphore was closed");
                return;
            };
            service.run_worker(&job_id).await;
        });
    }

    async fn run_worker(&self, job_id: &str) {
        if !matches!(
            self.database.claim_library_deletion_job(job_id).await,
            Ok(true)
        ) {
            return;
        }
        let started = Instant::now();
        let Some(job) = (match self.database.get_library_deletion_job(job_id).await {
            Ok(job) => job,
            Err(error) => {
                tracing::error!(job_id, %error, "failed to load library deletion job");
                let _ = self
                    .database
                    .fail_library_deletion_job(job_id, "DATABASE_ERROR")
                    .await;
                return;
            }
        }) else {
            tracing::error!(job_id, "library deletion job disappeared after claim");
            return;
        };
        let Ok(library_id) = job.library_id.parse::<LibraryId>() else {
            self.fail(job_id, &job.library_id, "INVALID_LIBRARY_ID", started)
                .await;
            return;
        };

        let _deletion_guard = match &self.scan_jobs {
            Some(scan_jobs) => match scan_jobs.prepare_library_deletion(library_id).await {
                Ok(guard) => Some(guard),
                Err(error) => {
                    let error_code = match error {
                        ScanJobError::LibraryDeletionScanTimeout => "SCAN_CANCEL_TIMEOUT",
                        _ => "SCAN_SHUTDOWN_FAILED",
                    };
                    self.fail(job_id, &job.library_id, error_code, started)
                        .await;
                    return;
                }
            },
            None => None,
        };

        match self.delete_in_batches(job_id, &job.library_id).await {
            Ok(true) => {}
            Ok(false) => {
                tracing::info!(job_id, library_id = %job.library_id, "library deletion paused");
                return;
            }
            Err(error) => {
                tracing::error!(
                    job_id,
                    library_id = %job.library_id,
                    stage = %job.stage,
                    %error,
                    "library deletion worker failed"
                );
                self.fail(job_id, &job.library_id, "DATABASE_ERROR", started)
                    .await;
                return;
            }
        }

        match self
            .database
            .set_library_deletion_stage(job_id, "FINALIZING")
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                tracing::info!(job_id, library_id = %job.library_id, "library deletion paused");
                return;
            }
            Err(error) => {
                tracing::error!(job_id, library_id = %job.library_id, %error, "failed to finalize library deletion");
                self.fail(job_id, &job.library_id, "DATABASE_ERROR", started)
                    .await;
                return;
            }
        }
        if let Some(plugins) = &self.plugins {
            if let Err(error) = plugins.prune_media_info_library_ids().await {
                tracing::error!(job_id, library_id = %job.library_id, %error, "deleted library could not be pruned from STRM plugin settings");
                self.fail(job_id, &job.library_id, "PLUGIN_CLEANUP_FAILED", started)
                    .await;
                return;
            }
            if let Err(error) = plugins.prune_danmaku_library_ids().await {
                tracing::error!(job_id, library_id = %job.library_id, %error, "deleted library could not be pruned from danmaku plugin settings");
                self.fail(job_id, &job.library_id, "PLUGIN_CLEANUP_FAILED", started)
                    .await;
                return;
            }
        }
        match self
            .database
            .complete_library_deletion(job_id, &job.library_id)
            .await
        {
            Ok(Some(true)) => {
                if let Some(home) = &self.home {
                    home.invalidate();
                }
                tracing::info!(
                    job_id,
                    library_id = %job.library_id,
                    elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    outcome = "completed",
                    "library deletion completed"
                );
            }
            Ok(Some(false)) => {
                self.fail(job_id, &job.library_id, "LIBRARY_RECORD_MISSING", started)
                    .await;
            }
            Ok(None) => {
                tracing::info!(job_id, library_id = %job.library_id, "library deletion paused");
            }
            Err(error) => {
                tracing::error!(job_id, library_id = %job.library_id, %error, "library deletion final transaction failed");
                self.fail(job_id, &job.library_id, "DATABASE_ERROR", started)
                    .await;
            }
        }
    }

    async fn delete_in_batches(
        &self,
        job_id: &str,
        library_id: &str,
    ) -> Result<bool, StorageError> {
        if !self
            .delete_stage(
                job_id,
                library_id,
                "MEDIA_ITEMS",
                &[LibraryDeletionBatch::MediaItems],
            )
            .await?
        {
            return Ok(false);
        }
        if !self
            .delete_stage(
                job_id,
                library_id,
                "FILESYSTEM_ENTRIES",
                &[LibraryDeletionBatch::FilesystemEntries],
            )
            .await?
        {
            return Ok(false);
        }
        if !self
            .delete_stage(
                job_id,
                library_id,
                "STRM_PROBE_JOBS",
                &[LibraryDeletionBatch::StrmProbeJobs],
            )
            .await?
        {
            return Ok(false);
        }
        if !self
            .delete_stage(
                job_id,
                library_id,
                "SCAN_JOB_DATA",
                &[
                    LibraryDeletionBatch::ScanManifestDeltas,
                    LibraryDeletionBatch::ScanManifestEntries,
                    LibraryDeletionBatch::ScanManifestDirectories,
                    LibraryDeletionBatch::ScanManifestSeenPaths,
                    LibraryDeletionBatch::ScanManifestRoots,
                    LibraryDeletionBatch::ScanManifests,
                    LibraryDeletionBatch::ScanJobTargets,
                    LibraryDeletionBatch::ScanJobPaths,
                    LibraryDeletionBatch::ReconciliationEntries,
                    LibraryDeletionBatch::ScanJobFileChanges,
                    LibraryDeletionBatch::ScanJobProcessedMediaItems,
                    LibraryDeletionBatch::ScanJobEvents,
                    LibraryDeletionBatch::ScanLocalMetadataBatches,
                ],
            )
            .await?
        {
            return Ok(false);
        }
        if !self
            .delete_stage(
                job_id,
                library_id,
                "SCAN_JOBS",
                &[LibraryDeletionBatch::ScanJobs],
            )
            .await?
        {
            return Ok(false);
        }
        Ok(true)
    }

    async fn delete_stage(
        &self,
        job_id: &str,
        library_id: &str,
        stage: &str,
        batches: &[LibraryDeletionBatch],
    ) -> Result<bool, StorageError> {
        if !self
            .database
            .set_library_deletion_stage(job_id, stage)
            .await?
        {
            return Ok(false);
        }
        for batch in batches {
            let batch_size = self.database.library_deletion_batch_size(*batch);
            loop {
                let Some(deleted) = self
                    .database
                    .delete_library_deletion_batch(job_id, library_id, *batch, batch_size)
                    .await?
                else {
                    return Ok(false);
                };
                if deleted == 0 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        }
        // Trimming the WAL between stages keeps the file from carrying every dirty
        // page of a multi-hour deletion while still letting readers proceed.
        self.database.checkpoint_library_deletion_wal().await;
        Ok(true)
    }

    async fn fail(&self, job_id: &str, library_id: &str, error_code: &str, started: Instant) {
        if let Err(error) = self
            .database
            .fail_library_deletion_job(job_id, error_code)
            .await
        {
            tracing::error!(job_id, library_id, %error, "could not persist library deletion failure");
        }
        tracing::warn!(
            job_id,
            library_id,
            error_code,
            elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            outcome = "failed",
            "library deletion failed"
        );
    }
}

#[derive(Debug)]
pub(crate) enum LibraryDeletionError {
    Storage(StorageError),
    LibraryNotFound,
    JobNotFound,
    RetryRequired,
    JobNotRetryable,
}

impl fmt::Display for LibraryDeletionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(error) => write!(formatter, "{error}"),
            Self::LibraryNotFound => formatter.write_str("library not found"),
            Self::JobNotFound => formatter.write_str("library deletion job not found"),
            Self::RetryRequired => formatter.write_str("library deletion requires retry"),
            Self::JobNotRetryable => formatter.write_str("library deletion job cannot be retried"),
        }
    }
}

impl std::error::Error for LibraryDeletionError {}

impl From<StorageError> for LibraryDeletionError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}
