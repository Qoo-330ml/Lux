use std::{
    collections::{HashMap, HashSet},
    fmt,
    path::PathBuf,
    sync::Arc,
};

use serde::Serialize;
use tokio::{
    sync::{Mutex, Semaphore},
    task::JoinSet,
};
use uuid::Uuid;

use crate::{
    application::{
        plugin_runtime::PluginRuntimeError,
        plugins::{PluginService, PluginServiceError},
        probe::{MediaProbeResult, safe_media_path, write_media_info_sidecar},
        strm_probe_policy::validate_remote_media_url,
    },
    domain::ids::LibraryId,
    observability::resources::ResourceMetrics,
    storage::{
        Database, MediaProbeUpdate, MediaStreamUpdate, StorageError, StoredStrmMediaSource,
        StoredStrmProbeJob,
    },
};

const MAX_LIBRARY_COUNT: usize = 64;
const MAX_CONCURRENCY: i64 = 64;
const SOURCE_PAGE_SIZE: i64 = 500;
const JOB_ERROR: &str = "one or more STRM media sources failed";

#[derive(Clone)]
pub struct StrmProbeService {
    database: Database,
    plugins: PluginService,
    operations: Arc<Mutex<HashMap<String, Arc<Semaphore>>>>,
    resources: ResourceMetrics,
}

impl StrmProbeService {
    pub fn new(database: Database, plugins: PluginService) -> Self {
        Self {
            database,
            plugins,
            operations: Arc::new(Mutex::new(HashMap::new())),
            resources: ResourceMetrics::new(),
        }
    }

    pub fn with_resource_metrics(mut self, resources: ResourceMetrics) -> Self {
        self.resources = resources;
        self
    }

    pub async fn create_jobs(
        &self,
        library_ids: &[LibraryId],
        concurrency: i64,
        include_ready: bool,
        write_sidecars: bool,
    ) -> Result<Vec<StrmProbeJob>, StrmProbeError> {
        if library_ids.is_empty() || library_ids.len() > MAX_LIBRARY_COUNT {
            return Err(StrmProbeError::InvalidLibraryCount);
        }
        if !(1..=MAX_CONCURRENCY).contains(&concurrency) {
            return Err(StrmProbeError::InvalidConcurrency);
        }
        if self.database.has_active_strm_probe_jobs().await? {
            return Err(StrmProbeError::AlreadyActive);
        }
        let mut unique_ids = HashSet::new();
        let mut libraries = Vec::with_capacity(library_ids.len());
        for library_id in library_ids {
            let library_id = library_id.to_string();
            if !unique_ids.insert(library_id.clone()) {
                continue;
            }
            let library = self
                .database
                .find_library(&library_id)
                .await?
                .ok_or(StrmProbeError::LibraryNotFound)?;
            let total_count = self
                .database
                .count_strm_media_sources_for_library(&library_id)
                .await?;
            libraries.push((library.id, total_count));
        }
        if libraries.is_empty() {
            return Err(StrmProbeError::InvalidLibraryCount);
        }
        let operation_id = Uuid::now_v7().to_string();
        let mut jobs = Vec::with_capacity(libraries.len());
        for (library_id, total_count) in libraries {
            let id = Uuid::now_v7().to_string();
            self.database
                .create_strm_probe_job(crate::storage::NewStrmProbeJob {
                    id: &id,
                    operation_id: &operation_id,
                    library_id: &library_id,
                    concurrency,
                    include_ready,
                    write_sidecars,
                    total_count,
                })
                .await?;
            let job = self
                .database
                .find_strm_probe_job(&id)
                .await?
                .ok_or(StrmProbeError::JobNotFound)?;
            jobs.push(strm_probe_job(job));
        }
        Ok(jobs)
    }

    pub async fn create_configured_jobs(&self) -> Result<Vec<StrmProbeJob>, StrmProbeError> {
        let settings = self.plugins.media_info_settings().await?;
        self.create_jobs(
            &settings.library_ids,
            settings.concurrency,
            settings.include_ready,
            settings.write_sidecars,
        )
        .await
    }

    pub async fn run(&self, job_id: &str) -> Result<(), StrmProbeError> {
        let job = self
            .database
            .find_strm_probe_job(job_id)
            .await?
            .ok_or(StrmProbeError::JobNotFound)?;
        if job.status == "PENDING" && !self.database.claim_strm_probe_job(job_id).await? {
            return Ok(());
        }
        if !matches!(job.status.as_str(), "PENDING" | "RUNNING") {
            self.cleanup_operation(&job.operation_id).await?;
            return Ok(());
        }
        let operation_id = job.operation_id.clone();
        let result = self.run_claimed(job).await;
        let cleanup_result = self.cleanup_operation(&operation_id).await;
        match (result, cleanup_result) {
            (Err(error), _) => Err(error),
            (Ok(()), Err(error)) => Err(error),
            (Ok(()), Ok(())) => Ok(()),
        }
    }

    pub async fn active_job_ids(&self) -> Result<Vec<String>, StrmProbeError> {
        Ok(self.database.list_active_strm_probe_job_ids().await?)
    }

    pub async fn get(&self, job_id: &str) -> Result<StrmProbeJob, StrmProbeError> {
        self.database
            .find_strm_probe_job(job_id)
            .await?
            .map(strm_probe_job)
            .ok_or(StrmProbeError::JobNotFound)
    }

    pub async fn list(
        &self,
        status: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StrmProbeJob>, StrmProbeError> {
        Ok(self
            .database
            .list_strm_probe_jobs(status, offset, limit)
            .await?
            .into_iter()
            .map(strm_probe_job)
            .collect())
    }

    pub async fn cancel(&self, job_id: &str) -> Result<(), StrmProbeError> {
        if self.database.find_strm_probe_job(job_id).await?.is_none() {
            return Err(StrmProbeError::JobNotFound);
        }
        self.database.request_strm_probe_job_cancel(job_id).await?;
        Ok(())
    }

    pub async fn retry(&self, job_id: &str) -> Result<StrmProbeJob, StrmProbeError> {
        let job = self
            .database
            .find_strm_probe_job(job_id)
            .await?
            .ok_or(StrmProbeError::JobNotFound)?;
        if !matches!(job.status.as_str(), "FAILED" | "CANCELLED") {
            return Err(StrmProbeError::NotRetryable);
        }
        let library_id = job
            .library_id
            .parse::<LibraryId>()
            .map_err(|_| StrmProbeError::LibraryNotFound)?;
        self.create_jobs(
            &[library_id],
            job.concurrency,
            job.include_ready,
            job.write_sidecars,
        )
        .await?
        .into_iter()
        .next()
        .ok_or(StrmProbeError::JobNotFound)
    }

    async fn run_claimed(&self, job: StoredStrmProbeJob) -> Result<(), StrmProbeError> {
        let library = self
            .database
            .find_library(&job.library_id)
            .await?
            .ok_or(StrmProbeError::LibraryNotFound)?;
        let per_library = match usize::try_from(library.probe_concurrency) {
            Ok(value) => value.clamp(1, MAX_CONCURRENCY as usize),
            Err(_) => 1,
        };
        let requested = match usize::try_from(job.concurrency) {
            Ok(value) => value.clamp(1, MAX_CONCURRENCY as usize),
            Err(_) => 1,
        };
        let concurrency = self
            .resources
            .background_concurrency(per_library.min(requested).max(1))
            .await;
        let operation_semaphore = self
            .operation_semaphore(&job.operation_id, concurrency)
            .await;
        let mut pending: JoinSet<SourceOutcome> = JoinSet::new();
        // A restarted RUNNING job re-enumerates sources. READY sources can be
        // skipped by probe_source, while failed/incomplete sources are retried;
        // restarting the counter keeps progress bounded by total_count.
        let mut processed = 0_i64;
        let mut failed = 0_usize;
        let mut cancelled = false;
        let mut after_source_id = None::<String>;

        loop {
            let sources = self
                .database
                .list_strm_media_sources_for_library_page(
                    &job.library_id,
                    after_source_id.as_deref(),
                    SOURCE_PAGE_SIZE,
                )
                .await?;
            let Some(last_source_id) = sources.last().map(|source| source.source_id.clone()) else {
                break;
            };
            after_source_id = Some(last_source_id);
            for source in sources {
                if self
                    .database
                    .strm_probe_job_cancel_requested(&job.id)
                    .await?
                {
                    cancelled = true;
                    break;
                }
                while pending.len() >= concurrency {
                    if let Some(result) = pending.join_next().await {
                        let outcome = result.map_err(|_| StrmProbeError::WorkerFailed)?;
                        let next_cursor = outcome.source_id.clone();
                        failed += self.finish_source(&job, outcome).await?;
                        processed += 1;
                        self.database
                            .update_strm_probe_job_progress(
                                &job.id,
                                Some(next_cursor.as_str()),
                                processed,
                            )
                            .await?;
                    }
                }
                let service = self.clone();
                let semaphore = operation_semaphore.clone();
                let include_ready = job.include_ready;
                pending.spawn(async move {
                    service.probe_source(source, semaphore, include_ready).await
                });
            }
            if cancelled {
                break;
            }
        }
        while let Some(result) = pending.join_next().await {
            let outcome = result.map_err(|_| StrmProbeError::WorkerFailed)?;
            let next_cursor = outcome.source_id.clone();
            failed += self.finish_source(&job, outcome).await?;
            processed += 1;
            self.database
                .update_strm_probe_job_progress(&job.id, Some(&next_cursor), processed)
                .await?;
        }
        if self
            .database
            .strm_probe_job_cancel_requested(&job.id)
            .await?
        {
            cancelled = true;
        }
        let (status, error) = if cancelled {
            ("CANCELLED", None)
        } else if failed > 0 {
            ("FAILED", Some(JOB_ERROR))
        } else {
            ("COMPLETED", None)
        };
        self.database
            .finish_strm_probe_job(&job.id, status, error)
            .await?;
        Ok(())
    }

    async fn operation_semaphore(
        &self,
        operation_id: &str,
        effective_concurrency: usize,
    ) -> Arc<Semaphore> {
        let mut operations = self.operations.lock().await;
        operations
            .entry(operation_id.to_owned())
            .or_insert_with(|| Arc::new(Semaphore::new(effective_concurrency.max(1))))
            .clone()
    }

    async fn cleanup_operation(&self, operation_id: &str) -> Result<(), StrmProbeError> {
        if self
            .database
            .has_active_strm_probe_jobs_for_operation(operation_id)
            .await?
        {
            return Ok(());
        }
        self.operations.lock().await.remove(operation_id);
        Ok(())
    }

    async fn probe_source(
        &self,
        source: StoredStrmMediaSource,
        semaphore: Arc<Semaphore>,
        include_ready: bool,
    ) -> SourceOutcome {
        let path = match safe_media_path(&source.root_path, &source.relative_path) {
            Ok(path) => path,
            Err(error) => {
                return SourceOutcome::failed(source.source_id, "FAILED", error.to_string());
            }
        };
        if source.probe_status == "READY" && !include_ready {
            return SourceOutcome::skipped(source.source_id);
        }
        let Some(url) = source.external_url else {
            return SourceOutcome::failed(
                source.source_id,
                "FAILED",
                "STRM source has no external URL".to_owned(),
            );
        };
        if !validate_remote_media_url(&url) {
            return SourceOutcome::failed(
                source.source_id,
                "FAILED",
                "STRM source URL is not allowed".to_owned(),
            );
        }
        let permit = match semaphore.acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => {
                return SourceOutcome::failed(
                    source.source_id,
                    "FAILED",
                    "STRM probe concurrency is unavailable".to_owned(),
                );
            }
        };
        let result = self.plugins.probe_media(&url).await;
        drop(permit);
        match result {
            Ok(result) => SourceOutcome::ready(source.source_id, path, result),
            Err(error) => {
                SourceOutcome::failed(source.source_id, failure_status(&error), error.to_string())
            }
        }
    }

    async fn finish_source(
        &self,
        job: &StoredStrmProbeJob,
        outcome: SourceOutcome,
    ) -> Result<usize, StrmProbeError> {
        if outcome.skipped {
            return Ok(0);
        }
        let Some(result) = outcome.result else {
            self.database
                .mark_media_probe_failed(
                    &outcome.source_id,
                    &outcome.failure_status,
                    &outcome.error,
                )
                .await?;
            return Ok(1);
        };
        let details = result
            .streams
            .iter()
            .map(|stream| {
                if stream.details.is_empty() {
                    None
                } else {
                    serde_json::to_string(&stream.details).ok()
                }
            })
            .collect::<Vec<_>>();
        let streams = result
            .streams
            .iter()
            .zip(details.iter())
            .map(|(stream, details)| MediaStreamUpdate {
                stream_index: stream.stream_index,
                stream_type: stream.stream_type.as_str(),
                codec: stream.codec.as_deref(),
                language: stream.language.as_deref(),
                title: stream.title.as_deref(),
                details_json: details.as_deref(),
                external_path: None,
                is_external: false,
                is_default: stream.is_default,
                is_forced: stream.is_forced,
            })
            .collect::<Vec<_>>();
        self.database
            .save_media_probe(MediaProbeUpdate {
                source_id: &outcome.source_id,
                container: result.container.as_deref(),
                source_size: result.source_size,
                duration_ticks: result.duration_ticks,
                bitrate: result.bitrate,
                streams: &streams,
            })
            .await?;
        if job.write_sidecars
            && write_media_info_sidecar(&outcome.path, &result)
                .await
                .is_err()
        {
            self.database
                .mark_media_probe_failed(
                    &outcome.source_id,
                    "FAILED",
                    "media info sidecar write failed",
                )
                .await?;
            return Ok(1);
        }
        Ok(0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StrmProbeJob {
    pub id: String,
    pub operation_id: String,
    pub library_id: String,
    pub status: String,
    pub concurrency: i64,
    pub include_ready: bool,
    pub write_sidecars: bool,
    pub cursor: Option<String>,
    pub processed_count: i64,
    pub total_count: i64,
    pub cancel_requested: bool,
    pub error: Option<String>,
}

fn strm_probe_job(job: StoredStrmProbeJob) -> StrmProbeJob {
    StrmProbeJob {
        id: job.id,
        operation_id: job.operation_id,
        library_id: job.library_id,
        status: job.status,
        concurrency: job.concurrency,
        include_ready: job.include_ready,
        write_sidecars: job.write_sidecars,
        cursor: job.cursor,
        processed_count: job.processed_count,
        total_count: job.total_count,
        cancel_requested: job.cancel_requested,
        error: job.error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[tokio::test]
    async fn completed_operation_is_removed_from_registry() {
        let temp_dir = tempfile::tempdir().expect("temporary directory");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("test address"),
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await.expect("database");
        let plugins = PluginService::new(database.clone(), config.config_dir.clone());
        let service = StrmProbeService::new(database.clone(), plugins);
        service
            .operations
            .lock()
            .await
            .insert("operation".to_owned(), Arc::new(Semaphore::new(1)));

        service
            .cleanup_operation("operation")
            .await
            .expect("cleanup operation");

        assert!(service.operations.lock().await.is_empty());
        database.close().await;
    }

    #[tokio::test]
    async fn operation_semaphore_uses_the_effective_worker_limit() {
        let temp_dir = tempfile::tempdir().expect("temporary directory");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("test address"),
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await.expect("database");
        let plugins = PluginService::new(database.clone(), config.config_dir.clone());
        let service = StrmProbeService::new(database.clone(), plugins);
        let effective_limit: usize = 2;

        let semaphore = service
            .operation_semaphore("operation", effective_limit)
            .await;

        assert_eq!(semaphore.available_permits(), effective_limit);
        database.close().await;
    }
}

struct SourceOutcome {
    source_id: String,
    path: PathBuf,
    result: Option<MediaProbeResult>,
    skipped: bool,
    failure_status: String,
    error: String,
}

impl SourceOutcome {
    fn ready(source_id: String, path: PathBuf, result: MediaProbeResult) -> Self {
        Self {
            source_id,
            path,
            result: Some(result),
            skipped: false,
            failure_status: String::new(),
            error: String::new(),
        }
    }

    fn skipped(source_id: String) -> Self {
        Self {
            source_id,
            path: PathBuf::new(),
            result: None,
            skipped: true,
            failure_status: String::new(),
            error: String::new(),
        }
    }

    fn failed(source_id: String, status: &str, error: String) -> Self {
        Self {
            source_id,
            path: PathBuf::new(),
            result: None,
            skipped: false,
            failure_status: status.to_owned(),
            error,
        }
    }
}

fn failure_status(error: &PluginServiceError) -> &'static str {
    match error {
        PluginServiceError::Runtime(PluginRuntimeError::Timeout) => "TIMEOUT",
        PluginServiceError::Runtime(PluginRuntimeError::Plugin { code, .. })
            if code == "MEDIA_PROBE_TIMEOUT" =>
        {
            "TIMEOUT"
        }
        _ => "FAILED",
    }
}

#[derive(Debug)]
pub enum StrmProbeError {
    InvalidLibraryCount,
    InvalidConcurrency,
    AlreadyActive,
    LibraryNotFound,
    JobNotFound,
    NotRetryable,
    WorkerFailed,
    Plugin(PluginServiceError),
    Storage(StorageError),
}

impl fmt::Display for StrmProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLibraryCount => formatter.write_str("invalid STRM library selection"),
            Self::InvalidConcurrency => formatter.write_str("invalid STRM probe concurrency"),
            Self::AlreadyActive => formatter.write_str("a STRM probe operation is already active"),
            Self::LibraryNotFound => formatter.write_str("library not found"),
            Self::JobNotFound => formatter.write_str("STRM probe job not found"),
            Self::NotRetryable => formatter.write_str("STRM probe job is not retryable"),
            Self::WorkerFailed => formatter.write_str("STRM probe worker failed"),
            Self::Plugin(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for StrmProbeError {}

impl From<StorageError> for StrmProbeError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<PluginServiceError> for StrmProbeError {
    fn from(error: PluginServiceError) -> Self {
        Self::Plugin(error)
    }
}
