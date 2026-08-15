use std::{
    collections::{HashMap, HashSet},
    fmt,
    sync::Arc,
    time::Duration,
};

use time::OffsetDateTime;
use tokio::{sync::Mutex, time::interval};

use crate::{
    application::{
        chapter_detector::{
            ChapterDetectionError, ChapterDetectionJob, ChapterDetectionOptions,
            ChapterDetectionService,
        },
        plugins::PluginService,
        probe::MediaProbeService,
        reidentify::{
            MetadataRefreshMode, MetadataReidentifyError, MetadataReidentifyJob,
            MetadataReidentifyService,
        },
        scanner::{BACKGROUND_SCAN_BATCH_SIZE, ScanJob, ScanJobError, ScanJobService},
        schedule::{
            CHAPTER_DETECTION_TASK_TYPE, CronSchedule, STRM_MEDIA_INFO_TASK_TYPE, parse_cron,
        },
        strm_probe::{StrmProbeError, StrmProbeJob, StrmProbeService},
        thumbnails::ThumbnailService,
    },
    domain::ids::LibraryId,
    storage::{Database, StorageError, StoredScheduledTaskConfig},
};

pub const RECONCILIATION_TASK_TYPE: &str = "RECONCILIATION_SCAN";
pub const METADATA_TASK_TYPE: &str = "METADATA_PARSE";

const POLL_INTERVAL: Duration = Duration::from_secs(15);
const SCHEDULER_PAGE_SIZE: i64 = 100;

#[derive(Clone)]
pub struct ScheduledTaskService {
    database: Database,
    plugins: PluginService,
    strm_probe: StrmProbeService,
    chapter_detection: Option<ChapterDetectionService>,
    scan_jobs: Option<ScanJobService>,
    metadata_reidentify: Option<MetadataReidentifyService>,
    probe: Option<MediaProbeService>,
    thumbnails: Option<ThumbnailService>,
    cursors: Arc<Mutex<HashMap<String, TaskCursor>>>,
}

struct TaskCursor {
    schedule: String,
    last_run_minute: i64,
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
    ChapterDetection {
        job: ChapterDetectionJob,
    },
}

impl ScheduledTaskRun {
    pub const fn task_type(&self) -> &'static str {
        match self {
            Self::Reconciliation { .. } => RECONCILIATION_TASK_TYPE,
            Self::Metadata { .. } => METADATA_TASK_TYPE,
            Self::StrmMediaInfo { .. } => STRM_MEDIA_INFO_TASK_TYPE,
            Self::ChapterDetection { .. } => CHAPTER_DETECTION_TASK_TYPE,
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
    Chapter(ChapterDetectionError),
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
            Self::Chapter(error) => error.fmt(formatter),
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
    pub fn new(
        database: Database,
        plugins: PluginService,
        strm_probe: StrmProbeService,
        chapter_detection: Option<ChapterDetectionService>,
    ) -> Self {
        Self {
            database,
            plugins,
            strm_probe,
            chapter_detection,
            scan_jobs: None,
            metadata_reidentify: None,
            probe: None,
            thumbnails: None,
            cursors: Arc::new(Mutex::new(HashMap::new())),
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

    pub fn spawn(&self) {
        let worker = self.clone();
        tokio::spawn(async move {
            let mut ticker = interval(POLL_INTERVAL);
            loop {
                ticker.tick().await;
                worker.run_once().await;
            }
        });
    }

    pub async fn run_once(&self) {
        let now = OffsetDateTime::now_utc();
        let current_minute = now.unix_timestamp().div_euclid(60);
        let mut offset = 0;
        let mut active_keys = HashSet::new();
        loop {
            let (tasks, total) = match self
                .database
                .list_scheduled_task_configs(offset, SCHEDULER_PAGE_SIZE)
                .await
            {
                Ok(result) => result,
                Err(error) => {
                    tracing::error!(%error, "failed to read scheduled tasks");
                    return;
                }
            };
            for task in tasks {
                let Some(schedule_text) = task.cron_or_interval.as_deref() else {
                    continue;
                };
                let Some(schedule) = scheduler_schedule(&task) else {
                    continue;
                };
                let key = task_key(&task);
                active_keys.insert(key.clone());
                if !schedule.matches(now)
                    || !self.claim_minute(&key, schedule_text, current_minute).await
                {
                    continue;
                }
                if let Err(error) = self
                    .run_task(&task.owner_type, &task.owner_id, &task.task_type)
                    .await
                {
                    match error {
                        ScheduledTaskError::Scan(ScanJobError::AlreadyActive(_))
                        | ScheduledTaskError::Strm(StrmProbeError::AlreadyActive) => {}
                        error => {
                            tracing::warn!(task = %key, %error, "scheduled task did not start")
                        }
                    }
                }
            }
            offset += SCHEDULER_PAGE_SIZE;
            if offset >= total || total == 0 {
                break;
            }
        }
        self.cursors
            .lock()
            .await
            .retain(|key, _| active_keys.contains(key));
    }

    async fn claim_minute(&self, key: &str, schedule_text: &str, minute: i64) -> bool {
        let mut cursors = self.cursors.lock().await;
        let due = cursors.get(key).is_none_or(|cursor| {
            cursor.schedule != schedule_text || cursor.last_run_minute != minute
        });
        if due {
            cursors.insert(
                key.to_owned(),
                TaskCursor {
                    schedule: schedule_text.to_owned(),
                    last_run_minute: minute,
                },
            );
        }
        due
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
                RECONCILIATION_TASK_TYPE | METADATA_TASK_TYPE | CHAPTER_DETECTION_TASK_TYPE
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
            ("LIBRARY", CHAPTER_DETECTION_TASK_TYPE) => {
                self.run_chapter_detection(owner_id, task.plugin_id.as_deref())
                    .await
            }
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
                    &job_id,
                    BACKGROUND_SCAN_BATCH_SIZE,
                    probe,
                    metadata,
                    thumbnails,
                )
                .await
            {
                tracing::error!(job_id = %job_id, %error, "scheduled reconciliation task stopped");
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
                    tracing::error!(job_id = %job_id, %error, "scheduled STRM task stopped");
                }
            });
        }
        let operation_id = jobs
            .first()
            .map(|job| job.operation_id.clone())
            .unwrap_or_default();
        Ok(ScheduledTaskRun::StrmMediaInfo { operation_id, jobs })
    }

    async fn run_chapter_detection(
        &self,
        library_id: &str,
        plugin_id: Option<&str>,
    ) -> Result<ScheduledTaskRun, ScheduledTaskError> {
        let service = self
            .chapter_detection
            .as_ref()
            .ok_or(ScheduledTaskError::ServiceUnavailable)?;
        let plugin_id = plugin_id.ok_or(ScheduledTaskError::ServiceUnavailable)?;
        let settings = self
            .plugins
            .enabled_chapter_detector_settings(plugin_id)
            .await
            .map_err(|error| ScheduledTaskError::Chapter(ChapterDetectionError::from(error)))?
            .ok_or(ScheduledTaskError::Disabled)?;
        let library_id = library_id
            .parse::<LibraryId>()
            .map_err(|_| ScheduledTaskError::InvalidOwner)?;
        let library = self
            .database
            .find_library(&library_id.to_string())
            .await?
            .ok_or(ScheduledTaskError::InvalidOwner)?;
        if library.chapter_source_id.as_deref() != Some(plugin_id) {
            return Err(ScheduledTaskError::Disabled);
        }
        let job = service
            .create_library_job(
                library_id,
                plugin_id,
                ChapterDetectionOptions {
                    concurrency: settings.concurrency,
                    intro_window_seconds: settings.intro_window_seconds,
                    credits_window_seconds: settings.credits_window_seconds,
                    match_threshold: settings.match_threshold,
                    force_refresh: false,
                },
            )
            .await
            .map_err(ScheduledTaskError::Chapter)?;
        let worker = service.clone();
        let job_id = job.id.clone();
        tokio::spawn(async move {
            if let Err(error) = worker.run(&job_id).await {
                tracing::error!(job_id = %job_id, %error, "scheduled chapter detection task stopped");
            }
        });
        Ok(ScheduledTaskRun::ChapterDetection { job })
    }
}

fn scheduler_schedule(task: &StoredScheduledTaskConfig) -> Option<CronSchedule> {
    if !task.is_enabled
        || !matches!(
            task.task_type.as_str(),
            RECONCILIATION_TASK_TYPE
                | METADATA_TASK_TYPE
                | STRM_MEDIA_INFO_TASK_TYPE
                | CHAPTER_DETECTION_TASK_TYPE
        )
    {
        return None;
    }
    let schedule = task.cron_or_interval.as_deref()?;
    match parse_cron(schedule) {
        Ok(schedule) => Some(schedule),
        Err(error) => {
            tracing::warn!(task_type = %task.task_type, ?error, "ignoring invalid scheduled task cron expression");
            None
        }
    }
}

fn task_key(task: &StoredScheduledTaskConfig) -> String {
    format!("{}:{}:{}", task.owner_type, task.owner_id, task.task_type)
}

#[cfg(test)]
mod tests {
    use super::task_key;
    use crate::storage::StoredScheduledTaskConfig;

    #[test]
    fn task_key_contains_owner_and_type() {
        let task = StoredScheduledTaskConfig {
            owner_type: "LIBRARY".to_owned(),
            owner_id: "library".to_owned(),
            task_type: "RECONCILIATION_SCAN".to_owned(),
            task_name: String::new(),
            task_description: String::new(),
            source_type: String::new(),
            plugin_id: None,
            cron_or_interval: None,
            is_enabled: false,
            resource_limit_json: String::new(),
            created_at: 0,
            updated_at: 0,
            library_name: None,
        };
        assert_eq!(task_key(&task), "LIBRARY:library:RECONCILIATION_SCAN");
    }
}
