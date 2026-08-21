use std::{
    collections::{HashMap, HashSet},
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::{
    fs,
    sync::{Mutex, Semaphore},
    task::JoinSet,
};
use uuid::Uuid;

use crate::{
    application::{
        images::{read_image_dimensions_from_bytes, write_image_atomically},
        plugin_runtime::PluginRuntimeError,
        plugins::{
            MAX_STRM_THUMBNAIL_POSITION_PERCENT, MIN_STRM_THUMBNAIL_POSITION_PERCENT,
            MediaProbeOutput, PluginService, PluginServiceError,
        },
        probe::{safe_media_path, write_media_info_sidecar},
        strm_probe_policy::validate_remote_media_url,
    },
    domain::ids::LibraryId,
    observability::resources::ResourceMetrics,
    storage::{
        Database, ItemImageMetadata, MediaProbeUpdate, MediaStreamUpdate, StorageError,
        StoredStrmMediaSource, StoredStrmProbeJob,
    },
};

const MAX_LIBRARY_COUNT: usize = 64;
const MAX_CONCURRENCY: i64 = 64;
const SOURCE_PAGE_SIZE: i64 = 500;
const JOB_ERROR: &str = "one or more STRM media sources failed";
const MAX_STRM_THUMBNAIL_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct StrmProbeOptions {
    pub concurrency: i64,
    pub include_ready: bool,
    pub write_sidecars: bool,
    pub media_info_enabled: bool,
    pub thumbnail_enabled: bool,
    pub thumbnail_position_percent: i64,
}

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
        options: StrmProbeOptions,
    ) -> Result<Vec<StrmProbeJob>, StrmProbeError> {
        if library_ids.is_empty() || library_ids.len() > MAX_LIBRARY_COUNT {
            return Err(StrmProbeError::InvalidLibraryCount);
        }
        if !(1..=MAX_CONCURRENCY).contains(&options.concurrency) {
            return Err(StrmProbeError::InvalidConcurrency);
        }
        if !(MIN_STRM_THUMBNAIL_POSITION_PERCENT..=MAX_STRM_THUMBNAIL_POSITION_PERCENT)
            .contains(&options.thumbnail_position_percent)
        {
            return Err(StrmProbeError::InvalidThumbnailPosition);
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
            jobs.push(
                self.create_job_record(&operation_id, &library_id, None, total_count, options)
                    .await?,
            );
        }
        Ok(jobs)
    }

    pub async fn create_configured_incremental_job(
        &self,
        scan_job_id: &str,
        library_id: LibraryId,
    ) -> Result<Option<StrmProbeJob>, StrmProbeError> {
        let Some(settings) = self.plugins.enabled_media_info_settings().await? else {
            return Ok(None);
        };
        if !settings.library_ids.contains(&library_id)
            || (!settings.media_info_enabled && !settings.thumbnail_enabled)
        {
            return Ok(None);
        }
        let library_id_text = library_id.to_string();
        let library = self
            .database
            .find_library(&library_id_text)
            .await?
            .ok_or(StrmProbeError::LibraryNotFound)?;
        if !library.is_enabled {
            return Ok(None);
        }
        let total_count = self
            .database
            .count_strm_media_sources_for_incremental_scan(scan_job_id)
            .await?;
        if total_count == 0 {
            return Ok(None);
        }
        if self.database.has_active_strm_probe_jobs().await? {
            return Err(StrmProbeError::AlreadyActive);
        }
        let operation_id = Uuid::now_v7().to_string();
        let job = self
            .create_job_record(
                &operation_id,
                &library_id_text,
                Some(scan_job_id),
                total_count,
                StrmProbeOptions {
                    concurrency: settings.concurrency,
                    include_ready: settings.include_ready,
                    write_sidecars: settings.write_sidecars,
                    media_info_enabled: settings.media_info_enabled,
                    thumbnail_enabled: settings.thumbnail_enabled,
                    thumbnail_position_percent: settings.thumbnail_position_percent,
                },
            )
            .await?;
        Ok(Some(job))
    }

    async fn create_job_record(
        &self,
        operation_id: &str,
        library_id: &str,
        target_scan_job_id: Option<&str>,
        total_count: i64,
        options: StrmProbeOptions,
    ) -> Result<StrmProbeJob, StrmProbeError> {
        let id = Uuid::now_v7().to_string();
        self.database
            .create_strm_probe_job(crate::storage::NewStrmProbeJob {
                id: &id,
                operation_id,
                library_id,
                concurrency: options.concurrency,
                include_ready: options.include_ready,
                write_sidecars: options.write_sidecars,
                media_info_enabled: options.media_info_enabled,
                thumbnail_enabled: options.thumbnail_enabled,
                thumbnail_position_percent: options.thumbnail_position_percent,
                target_scan_job_id,
                total_count,
            })
            .await?;
        let job = self
            .database
            .find_strm_probe_job(&id)
            .await?
            .ok_or(StrmProbeError::JobNotFound)?;
        Ok(strm_probe_job(job))
    }

    pub async fn create_configured_jobs(&self) -> Result<Vec<StrmProbeJob>, StrmProbeError> {
        let settings = self.plugins.media_info_settings().await?;
        self.create_jobs(
            &settings.library_ids,
            StrmProbeOptions {
                concurrency: settings.concurrency,
                include_ready: settings.include_ready,
                write_sidecars: settings.write_sidecars,
                media_info_enabled: settings.media_info_enabled,
                thumbnail_enabled: settings.thumbnail_enabled,
                thumbnail_position_percent: settings.thumbnail_position_percent,
            },
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
        let options = StrmProbeOptions {
            concurrency: job.concurrency,
            include_ready: job.include_ready,
            write_sidecars: job.write_sidecars,
            media_info_enabled: job.media_info_enabled,
            thumbnail_enabled: job.thumbnail_enabled,
            thumbnail_position_percent: job.thumbnail_position_percent,
        };
        if let Some(scan_job_id) = job.target_scan_job_id.as_deref() {
            if self.database.has_active_strm_probe_jobs().await? {
                return Err(StrmProbeError::AlreadyActive);
            }
            let total_count = self
                .database
                .count_strm_media_sources_for_incremental_scan(scan_job_id)
                .await?;
            let operation_id = Uuid::now_v7().to_string();
            let library_id_text = library_id.to_string();
            self.create_job_record(
                &operation_id,
                &library_id_text,
                Some(scan_job_id),
                total_count,
                options,
            )
            .await
        } else {
            self.create_jobs(&[library_id], options)
                .await?
                .into_iter()
                .next()
                .ok_or(StrmProbeError::JobNotFound)
        }
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
            let sources = if let Some(scan_job_id) = job.target_scan_job_id.as_deref() {
                self.database
                    .list_strm_media_sources_for_incremental_scan_page(
                        scan_job_id,
                        after_source_id.as_deref(),
                        SOURCE_PAGE_SIZE,
                    )
                    .await?
            } else {
                self.database
                    .list_strm_media_sources_for_library_page(
                        &job.library_id,
                        after_source_id.as_deref(),
                        SOURCE_PAGE_SIZE,
                    )
                    .await?
            };
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
                let media_info_enabled = job.media_info_enabled;
                let thumbnail_enabled = job.thumbnail_enabled;
                let thumbnail_position_percent = job.thumbnail_position_percent;
                pending.spawn(async move {
                    service
                        .probe_source(
                            source,
                            semaphore,
                            include_ready,
                            media_info_enabled,
                            thumbnail_enabled,
                            thumbnail_position_percent,
                        )
                        .await
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
        media_info_enabled: bool,
        thumbnail_enabled: bool,
        thumbnail_position_percent: i64,
    ) -> SourceOutcome {
        let path = match safe_media_path(&source.root_path, &source.relative_path) {
            Ok(path) => path,
            Err(error) => {
                return SourceOutcome::failed(&source.source_id, "FAILED", error.to_string());
            }
        };
        let media_info_needed = media_info_enabled && (include_ready || !source.has_media_info);
        if thumbnail_enabled
            && safe_strm_thumbnail_target(&path, &source.root_path)
                .await
                .is_none()
        {
            return SourceOutcome::failed(
                &source.source_id,
                "FAILED",
                "STRM thumbnail path is outside the library root".to_owned(),
            );
        }
        let thumbnail_needed = thumbnail_enabled
            && source.poster_fallback_required
            && usable_strm_thumbnail(&path, &source.root_path, source.thumbnail_path.as_deref())
                .await
                .is_none();
        if !media_info_needed && !thumbnail_needed {
            return SourceOutcome::skipped(source.source_id);
        }
        let Some(url) = source.external_url else {
            return SourceOutcome::failed(
                &source.source_id,
                "FAILED",
                "STRM source has no external URL".to_owned(),
            );
        };
        if !validate_remote_media_url(&url) {
            return SourceOutcome::failed(
                &source.source_id,
                "FAILED",
                "STRM source URL is not allowed".to_owned(),
            );
        }
        let permit = match semaphore.acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => {
                return SourceOutcome::failed(
                    &source.source_id,
                    "FAILED",
                    "STRM probe concurrency is unavailable".to_owned(),
                );
            }
        };
        let result = self
            .plugins
            .probe_media_with_options(
                &url,
                media_info_needed,
                thumbnail_needed,
                thumbnail_position_percent,
            )
            .await;
        drop(permit);
        match result {
            Ok(result) => SourceOutcome::ready(
                source.source_id,
                source.item_id,
                source.root_path,
                path,
                result,
                media_info_needed,
                thumbnail_needed,
            ),
            Err(error) => {
                SourceOutcome::failed(&source.source_id, failure_status(&error), error.to_string())
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
        if outcome.media_info_needed {
            let details = result
                .media
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
                .media
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
                    container: result.media.container.as_deref(),
                    source_size: result.media.source_size,
                    duration_ticks: result.media.duration_ticks,
                    bitrate: result.media.bitrate,
                    streams: &streams,
                })
                .await?;
            if job.write_sidecars
                && write_media_info_sidecar(&outcome.path, &result.media)
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
        }
        if outcome.thumbnail_needed {
            let Some(thumbnail) = result.thumbnail_jpeg.as_deref() else {
                self.database
                    .mark_media_probe_failed(
                        &outcome.source_id,
                        "FAILED",
                        "thumbnail output is missing",
                    )
                    .await?;
                return Ok(1);
            };
            if !is_valid_jpeg(thumbnail) {
                self.database
                    .mark_media_probe_failed(
                        &outcome.source_id,
                        "FAILED",
                        "thumbnail output is invalid",
                    )
                    .await?;
                return Ok(1);
            }
            let target = match safe_strm_thumbnail_target(&outcome.path, &outcome.root_path).await {
                Some(path) => path,
                None => {
                    self.database
                        .mark_media_probe_failed(
                            &outcome.source_id,
                            "FAILED",
                            "STRM path has no valid thumbnail name",
                        )
                        .await?;
                    return Ok(1);
                }
            };
            if write_image_atomically(&target, thumbnail).await.is_err() {
                self.database
                    .mark_media_probe_failed(&outcome.source_id, "FAILED", "thumbnail write failed")
                    .await?;
                return Ok(1);
            }
            let file_size =
                i64::try_from(thumbnail.len()).map_err(|_| StrmProbeError::WorkerFailed)?;
            let content_tag = hex_sha256(thumbnail);
            let dimensions = read_image_dimensions_from_bytes(thumbnail).await;
            for image_type in ["POSTER", "THUMB"] {
                if self
                    .database
                    .upsert_item_image(
                        &outcome.item_id,
                        image_type,
                        &target,
                        ItemImageMetadata {
                            file_size,
                            width: dimensions.map(|(width, _)| width),
                            height: dimensions.map(|(_, height)| height),
                            content_tag: &content_tag,
                            source: "STRM_FFMPEG",
                        },
                    )
                    .await
                    .is_err()
                {
                    self.database
                        .mark_media_probe_failed(
                            &outcome.source_id,
                            "FAILED",
                            "thumbnail registration failed",
                        )
                        .await?;
                    return Ok(1);
                }
            }
            self.database
                .set_poster_fallback_required(&outcome.item_id, false)
                .await
                .map_err(StrmProbeError::Storage)?;
        }
        Ok(0)
    }
}

fn is_valid_jpeg(bytes: &[u8]) -> bool {
    bytes.len() >= 4
        && bytes.len() <= MAX_STRM_THUMBNAIL_BYTES
        && bytes.starts_with(&[0xff, 0xd8])
        && bytes.ends_with(&[0xff, 0xd9])
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn strm_thumbnail_path(path: &Path) -> Option<PathBuf> {
    let stem = path.file_stem()?.to_str()?;
    Some(path.with_file_name(format!("{stem}-thumb.jpg")))
}

async fn safe_strm_thumbnail_target(media_path: &Path, root_path: &str) -> Option<PathBuf> {
    let target = strm_thumbnail_path(media_path)?;
    let root = fs::canonicalize(root_path).await.ok()?;
    let parent = target.parent()?;
    let canonical_parent = fs::canonicalize(parent).await.ok()?;
    canonical_parent.starts_with(root).then_some(target)
}

async fn usable_strm_thumbnail(
    media_path: &Path,
    root_path: &str,
    registered_path: Option<&str>,
) -> Option<PathBuf> {
    let root = fs::canonicalize(root_path).await.ok()?;
    let expected = strm_thumbnail_path(media_path)?;
    let mut candidates = Vec::with_capacity(2);
    if let Some(registered_path) = registered_path {
        candidates.push(PathBuf::from(registered_path));
    }
    candidates.push(expected);
    for candidate in candidates {
        let Ok(metadata) = fs::symlink_metadata(&candidate).await else {
            continue;
        };
        let file_type = metadata.file_type();
        if !file_type.is_file() || file_type.is_symlink() {
            continue;
        }
        let Ok(canonical) = fs::canonicalize(&candidate).await else {
            continue;
        };
        if !canonical.starts_with(&root) {
            continue;
        }
        let Ok(metadata) = fs::metadata(&canonical).await else {
            continue;
        };
        let Ok(size) = usize::try_from(metadata.len()) else {
            continue;
        };
        if size == 0 || size > MAX_STRM_THUMBNAIL_BYTES {
            continue;
        }
        let Ok(bytes) = fs::read(&canonical).await else {
            continue;
        };
        if is_valid_jpeg(&bytes) {
            return Some(canonical);
        }
    }
    None
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
    pub media_info_enabled: bool,
    pub thumbnail_enabled: bool,
    pub thumbnail_position_percent: i64,
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
        media_info_enabled: job.media_info_enabled,
        thumbnail_enabled: job.thumbnail_enabled,
        thumbnail_position_percent: job.thumbnail_position_percent,
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
    item_id: String,
    root_path: String,
    path: PathBuf,
    result: Option<MediaProbeOutput>,
    media_info_needed: bool,
    thumbnail_needed: bool,
    skipped: bool,
    failure_status: String,
    error: String,
}

impl SourceOutcome {
    fn ready(
        source_id: String,
        item_id: String,
        root_path: String,
        path: PathBuf,
        result: MediaProbeOutput,
        media_info_needed: bool,
        thumbnail_needed: bool,
    ) -> Self {
        Self {
            source_id,
            item_id,
            root_path,
            path,
            result: Some(result),
            skipped: false,
            media_info_needed,
            thumbnail_needed,
            failure_status: String::new(),
            error: String::new(),
        }
    }

    fn skipped(source_id: String) -> Self {
        Self {
            source_id,
            item_id: String::new(),
            root_path: String::new(),
            path: PathBuf::new(),
            result: None,
            skipped: true,
            media_info_needed: false,
            thumbnail_needed: false,
            failure_status: String::new(),
            error: String::new(),
        }
    }

    fn failed(source_id: &str, status: &str, error: String) -> Self {
        Self {
            source_id: source_id.to_owned(),
            item_id: String::new(),
            root_path: String::new(),
            path: PathBuf::new(),
            result: None,
            skipped: false,
            media_info_needed: false,
            thumbnail_needed: false,
            failure_status: status.to_owned(),
            error,
        }
    }
}

fn failure_status(error: &PluginServiceError) -> &'static str {
    match error {
        PluginServiceError::Runtime(PluginRuntimeError::Timeout) => "TIMEOUT",
        PluginServiceError::Runtime(PluginRuntimeError::Plugin { code, .. })
            if matches!(
                code.as_str(),
                "MEDIA_PROBE_TIMEOUT" | "MEDIA_THUMBNAIL_TIMEOUT"
            ) =>
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
    InvalidThumbnailPosition,
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
            Self::InvalidThumbnailPosition => {
                formatter.write_str("invalid STRM thumbnail position percent")
            }
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
