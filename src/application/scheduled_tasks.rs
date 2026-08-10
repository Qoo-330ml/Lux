use std::fmt;

use crate::{
    application::{
        plugins::PluginService,
        probe::MediaProbeService,
        reidentify::{
            MetadataRefreshMode, MetadataReidentifyError, MetadataReidentifyJob,
            MetadataReidentifyService,
        },
        scanner::{ScanJob, ScanJobError, ScanJobService},
        strm_probe::{StrmProbeError, StrmProbeJob, StrmProbeService},
        thumbnails::ThumbnailService,
    },
    domain::ids::LibraryId,
    storage::{Database, StorageError},
};

pub const RECONCILIATION_TASK_TYPE: &str = "RECONCILIATION_SCAN";
pub const METADATA_TASK_TYPE: &str = "METADATA_PARSE";
pub const STRM_MEDIA_INFO_TASK_TYPE: &str = "STRM_MEDIA_INFO";

#[derive(Clone)]
pub struct ScheduledTaskService {
    database: Database,
    strm_probe: StrmProbeService,
    scan_jobs: Option<ScanJobService>,
    metadata_reidentify: Option<MetadataReidentifyService>,
    probe: Option<MediaProbeService>,
    thumbnails: Option<ThumbnailService>,
}

#[derive(Debug)]
pub enum ScheduledTaskRun {
    Reconciliation {
        job: ScanJob,
    },
    Metadata {
        job: MetadataReidentifyJob,
    },
    StrmMediaInfo {
        operation_id: String,
        jobs: Vec<StrmProbeJob>,
    },
}

impl ScheduledTaskRun {
    pub const fn task_type(&self) -> &'static str {
        match self {
            Self::Reconciliation { .. } => RECONCILIATION_TASK_TYPE,
            Self::Metadata { .. } => METADATA_TASK_TYPE,
            Self::StrmMediaInfo { .. } => STRM_MEDIA_INFO_TASK_TYPE,
        }
    }
}

#[derive(Debug)]
pub enum ScheduledTaskError {
    InvalidOwner,
    UnsupportedTask,
    NotRegistered,
    Disabled,
    ServiceUnavailable,
    Scan(ScanJobError),
    Metadata(MetadataReidentifyError),
    Strm(StrmProbeError),
    Storage(StorageError),
}

impl fmt::Display for ScheduledTaskError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOwner => formatter.write_str("scheduled task owner is invalid"),
            Self::UnsupportedTask => formatter.write_str("scheduled task is unsupported"),
            Self::NotRegistered => formatter.write_str("scheduled task is not registered"),
            Self::Disabled => formatter.write_str("scheduled task is disabled"),
            Self::ServiceUnavailable => {
                formatter.write_str("scheduled task service is unavailable")
            }
            Self::Scan(error) => error.fmt(formatter),
            Self::Metadata(error) => error.fmt(formatter),
            Self::Strm(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ScheduledTaskError {}

impl From<StorageError> for ScheduledTaskError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl ScheduledTaskService {
    pub fn new(database: Database, _plugins: PluginService, strm_probe: StrmProbeService) -> Self {
        Self {
            database,
            strm_probe,
            scan_jobs: None,
            metadata_reidentify: None,
            probe: None,
            thumbnails: None,
        }
    }

    pub fn with_library_services(
        mut self,
        scan_jobs: ScanJobService,
        metadata_reidentify: Option<MetadataReidentifyService>,
        probe: Option<MediaProbeService>,
        thumbnails: Option<ThumbnailService>,
    ) -> Self {
        self.scan_jobs = Some(scan_jobs);
        self.metadata_reidentify = metadata_reidentify;
        self.probe = probe;
        self.thumbnails = thumbnails;
        self
    }

    pub async fn run_task(
        &self,
        owner_type: &str,
        owner_id: &str,
        task_type: &str,
    ) -> Result<ScheduledTaskRun, ScheduledTaskError> {
        let owner_type = owner_type.trim().to_ascii_uppercase();
        let owner_id = owner_id.trim();
        let task_type = task_type.trim().to_ascii_uppercase();
        if !matches!(owner_type.as_str(), "GLOBAL" | "LIBRARY") || owner_id.is_empty() {
            return Err(ScheduledTaskError::InvalidOwner);
        }
        if owner_type == "GLOBAL" && owner_id != "global" {
            return Err(ScheduledTaskError::InvalidOwner);
        }
        if owner_type == "LIBRARY" && owner_id.parse::<LibraryId>().is_err() {
            return Err(ScheduledTaskError::InvalidOwner);
        }
        if owner_type == "GLOBAL" && task_type != STRM_MEDIA_INFO_TASK_TYPE {
            return Err(ScheduledTaskError::UnsupportedTask);
        }
        if owner_type == "LIBRARY"
            && !matches!(
                task_type.as_str(),
                RECONCILIATION_TASK_TYPE | METADATA_TASK_TYPE
            )
        {
            return Err(ScheduledTaskError::UnsupportedTask);
        }
        let task = self
            .database
            .find_scheduled_task_config(&owner_type, owner_id, &task_type)
            .await?
            .ok_or(ScheduledTaskError::NotRegistered)?;
        if !task.is_enabled {
            return Err(ScheduledTaskError::Disabled);
        }

        match (owner_type.as_str(), task_type.as_str()) {
            ("LIBRARY", RECONCILIATION_TASK_TYPE) => self.run_reconciliation(owner_id).await,
            ("LIBRARY", METADATA_TASK_TYPE) => self.run_metadata(owner_id).await,
            ("GLOBAL", STRM_MEDIA_INFO_TASK_TYPE) => self.run_strm_media_info().await,
            _ => Err(ScheduledTaskError::UnsupportedTask),
        }
    }

    async fn run_reconciliation(
        &self,
        library_id: &str,
    ) -> Result<ScheduledTaskRun, ScheduledTaskError> {
        let library_id = library_id
            .parse::<LibraryId>()
            .map_err(|_| ScheduledTaskError::InvalidOwner)?;
        let scan_jobs = self
            .scan_jobs
            .as_ref()
            .ok_or(ScheduledTaskError::ServiceUnavailable)?;
        let job = scan_jobs
            .create_movie_scan_job(library_id)
            .await
            .map_err(ScheduledTaskError::Scan)?;
        let worker = scan_jobs.clone();
        let job_id = job.id.clone();
        let probe = self.probe.clone();
        let metadata = self.metadata_reidentify.clone();
        let thumbnails = self.thumbnails.clone();
        tokio::spawn(async move {
            if let Err(error) = worker
                .run_to_completion_with_metadata_and_thumbnails(
                    &job_id, 100, probe, metadata, thumbnails,
                )
                .await
            {
                tracing::error!(job_id = %job_id, %error, "cron reconciliation task stopped");
            }
        });
        Ok(ScheduledTaskRun::Reconciliation { job })
    }

    async fn run_metadata(&self, library_id: &str) -> Result<ScheduledTaskRun, ScheduledTaskError> {
        let service = self
            .metadata_reidentify
            .as_ref()
            .ok_or(ScheduledTaskError::ServiceUnavailable)?;
        let job = service
            .create_library_refresh_job(library_id, MetadataRefreshMode::FillMissing)
            .await
            .map_err(ScheduledTaskError::Metadata)?;
        let worker = service.clone();
        let job_id = job.id.clone();
        tokio::spawn(async move {
            worker.run(&job_id).await;
        });
        Ok(ScheduledTaskRun::Metadata { job })
    }

    async fn run_strm_media_info(&self) -> Result<ScheduledTaskRun, ScheduledTaskError> {
        let jobs = self
            .strm_probe
            .create_configured_jobs()
            .await
            .map_err(ScheduledTaskError::Strm)?;
        for job in &jobs {
            let worker = self.strm_probe.clone();
            let job_id = job.id.clone();
            tokio::spawn(async move {
                if let Err(error) = worker.run(&job_id).await {
                    tracing::error!(job_id = %job_id, %error, "cron STRM task stopped");
                }
            });
        }
        let operation_id = jobs
            .first()
            .map(|job| job.operation_id.clone())
            .unwrap_or_default();
        Ok(ScheduledTaskRun::StrmMediaInfo { operation_id, jobs })
    }
}
