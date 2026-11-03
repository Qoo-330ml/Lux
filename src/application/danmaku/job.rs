//! Danmaku match job lifecycle: creation, execution, cancellation, retry and
//! the bounded worker pool that turns a stored source into a sidecar track.

use std::{
    fmt,
    path::{Path, PathBuf},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::Serialize;
use tokio::{fs, task::JoinSet};
use uuid::Uuid;

use super::{
    DanmakuService, danmaku_sidecar_path,
    matching::{danmaku_plugin_error_code, match_file_name_candidates},
    path::safe_danmaku_source_paths,
    sidecar::{atomic_write_danmaku_xml, fingerprint, read_danmaku_file},
    validate_danmaku_xml,
};
use crate::{
    application::{
        plugin_protocol::DanmakuMatchStatus,
        plugins::{DanmakuSettings, PluginService, PluginServiceError},
    },
    domain::ids::LibraryId,
    storage::{
        Database, NewDanmakuMatchJob, NewDanmakuSingleItemJob, NewDanmakuTrack, StorageError,
        StoredDanmakuMatchItem, StoredDanmakuMatchJob, StoredDanmakuSource,
    },
};

pub const DEFAULT_DANMAKU_CONCURRENCY: i64 = 2;
pub(super) const MAX_CONCURRENCY: i64 = 64;
const MAX_EFFECTIVE_CONCURRENCY: usize = 4;
const WORK_PAGE_SIZE: i64 = 100;
const DANMAKU_CANCEL_CHECK_INTERVAL: usize = 8;

impl DanmakuService {
    pub async fn create_job(
        &self,
        library_id: LibraryId,
        concurrency: i64,
        overwrite: bool,
    ) -> Result<DanmakuMatchJob, DanmakuServiceError> {
        if !(0..=MAX_CONCURRENCY).contains(&concurrency) {
            return Err(DanmakuServiceError::InvalidConcurrency);
        }
        let Some(plugins) = &self.plugins else {
            return Err(DanmakuServiceError::ProviderNotConfigured);
        };
        let settings = plugins
            .danmaku_settings()
            .await
            .map_err(|_| DanmakuServiceError::ProviderNotConfigured)?;
        let mut available = None;
        self.create_job_with_settings(
            library_id,
            concurrency,
            overwrite,
            &settings,
            plugins,
            &mut available,
        )
        .await
    }

    async fn create_job_with_settings(
        &self,
        library_id: LibraryId,
        concurrency: i64,
        overwrite: bool,
        settings: &DanmakuSettings,
        plugins: &PluginService,
        available: &mut Option<bool>,
    ) -> Result<DanmakuMatchJob, DanmakuServiceError> {
        let library_id = library_id.to_string();
        self.database
            .find_library(&library_id)
            .await?
            .ok_or(DanmakuServiceError::LibraryNotFound)?;
        if self
            .database
            .has_active_danmaku_match_jobs(&library_id)
            .await?
        {
            return Err(DanmakuServiceError::AlreadyActive);
        }
        if !settings.library_ids.iter().any(|id| id == &library_id) {
            return Err(DanmakuServiceError::LibraryNotSelected);
        }
        let is_available = match *available {
            Some(value) => value,
            None => {
                let value = plugins
                    .has_available_danmaku()
                    .await
                    .map_err(|_| DanmakuServiceError::ProviderNotConfigured)?;
                *available = Some(value);
                value
            }
        };
        if !is_available {
            return Err(DanmakuServiceError::ProviderNotConfigured);
        }
        let id = Uuid::now_v7().to_string();
        self.database
            .create_danmaku_match_job(NewDanmakuMatchJob {
                id: &id,
                library_id: &library_id,
                overwrite,
                concurrency,
            })
            .await?;
        self.get(&id).await
    }

    pub async fn create_configured_jobs(
        &self,
    ) -> Result<Vec<DanmakuMatchJob>, DanmakuServiceError> {
        let Some(plugins) = &self.plugins else {
            return Err(DanmakuServiceError::ProviderNotConfigured);
        };
        let settings = plugins
            .danmaku_settings()
            .await
            .map_err(|_| DanmakuServiceError::ProviderNotConfigured)?;
        let concurrency = settings.concurrency;
        let overwrite = settings.overwrite;
        let mut jobs = Vec::with_capacity(settings.library_ids.len());
        let mut available = None;
        for library_id in &settings.library_ids {
            let library_id = library_id
                .parse::<LibraryId>()
                .map_err(|_| DanmakuServiceError::LibraryNotFound)?;
            match self
                .create_job_with_settings(
                    library_id,
                    concurrency,
                    overwrite,
                    &settings,
                    plugins,
                    &mut available,
                )
                .await
            {
                Ok(job) => jobs.push(job),
                Err(DanmakuServiceError::AlreadyActive) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(jobs)
    }

    pub async fn run(&self, job_id: &str) -> Result<(), DanmakuServiceError> {
        let job = self
            .database
            .find_danmaku_match_job(job_id)
            .await?
            .ok_or(DanmakuServiceError::JobNotFound)?;
        if job.status == "PENDING" && !self.database.claim_danmaku_match_job(job_id).await? {
            return Ok(());
        }
        if !matches!(job.status.as_str(), "PENDING" | "RUNNING") {
            return Ok(());
        }
        self.run_claimed(job).await
    }

    pub async fn active_job_ids(&self) -> Result<Vec<String>, DanmakuServiceError> {
        Ok(self.database.list_active_danmaku_match_job_ids().await?)
    }

    pub async fn get(&self, job_id: &str) -> Result<DanmakuMatchJob, DanmakuServiceError> {
        self.database
            .find_danmaku_match_job(job_id)
            .await?
            .map(danmaku_match_job)
            .ok_or(DanmakuServiceError::JobNotFound)
    }

    pub async fn list(
        &self,
        status: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<DanmakuMatchJob>, DanmakuServiceError> {
        Ok(self
            .database
            .list_danmaku_match_jobs(status, offset, limit)
            .await?
            .into_iter()
            .map(danmaku_match_job)
            .collect())
    }

    pub async fn cancel(&self, job_id: &str) -> Result<(), DanmakuServiceError> {
        if self
            .database
            .find_danmaku_match_job(job_id)
            .await?
            .is_none()
        {
            return Err(DanmakuServiceError::JobNotFound);
        }
        self.database
            .request_danmaku_match_job_cancel(job_id)
            .await?;
        Ok(())
    }

    pub async fn retry(&self, job_id: &str) -> Result<DanmakuMatchJob, DanmakuServiceError> {
        let job = self
            .database
            .find_danmaku_match_job(job_id)
            .await?
            .ok_or(DanmakuServiceError::JobNotFound)?;
        if !matches!(job.status.as_str(), "FAILED" | "CANCELLED") {
            return Err(DanmakuServiceError::NotRetryable);
        }
        let library_id = job
            .library_id
            .parse::<LibraryId>()
            .map_err(|_| DanmakuServiceError::LibraryNotFound)?;
        // A job that queued exactly one target is retried as that same target: re-running a manual
        // match as a library sweep would silently widen it, and retrying a one-item library job one
        // item at a time is the same work. Every other job keeps the library-wide retry.
        let items = self.database.list_danmaku_match_items(job_id).await?;
        if job.total_count == 1 && items.len() == 1 {
            return self.retry_single_item(&job, &items[0]).await;
        }
        self.create_job(library_id, job.concurrency, job.overwrite)
            .await
    }

    /// Queues a new one-target job for the single item of a failed/cancelled job.
    ///
    /// The prefilled provider identifiers of the old item are copied, so a manual match keeps
    /// fetching the same episode instead of falling back to `danmaku.match`; a one-item library job
    /// has none and keeps matching. Nothing in `danmaku_tracks` is rewritten here, so a retry can
    /// never relabel a user-provided sidecar as Lux-written.
    async fn retry_single_item(
        &self,
        job: &StoredDanmakuMatchJob,
        item: &StoredDanmakuMatchItem,
    ) -> Result<DanmakuMatchJob, DanmakuServiceError> {
        let Some(relative_path) = item.relative_path.as_deref() else {
            return Err(DanmakuServiceError::SourceNotFound);
        };
        let relative_sidecar_path = danmaku_sidecar_path(Path::new(relative_path))
            .map_err(|_| DanmakuServiceError::SourceNotFound)?
            .to_string_lossy()
            .into_owned();
        if self
            .database
            .has_active_danmaku_match_jobs(&job.library_id)
            .await?
        {
            return Err(DanmakuServiceError::AlreadyActive);
        }
        let provider = self
            .database
            .find_danmaku_track_provider(&item.media_source_id)
            .await?;
        let job_id = Uuid::now_v7().to_string();
        let item_row_id = format!("{job_id}:{}", item.media_source_id);
        let track_id = Uuid::now_v7().to_string();
        self.database
            .create_danmaku_single_item_job(
                NewDanmakuMatchJob {
                    id: &job_id,
                    library_id: &job.library_id,
                    overwrite: job.overwrite,
                    concurrency: job.concurrency,
                },
                NewDanmakuSingleItemJob {
                    id: &item_row_id,
                    track_id: &track_id,
                    media_source_id: &item.media_source_id,
                    provider_anime_id: item.provider_anime_id.as_deref(),
                    provider_episode_id: item.provider_episode_id.as_deref(),
                    relative_sidecar_path: &relative_sidecar_path,
                    provider: provider.as_deref(),
                },
            )
            .await?;
        self.get(&job_id).await
    }

    async fn run_claimed(&self, job: StoredDanmakuMatchJob) -> Result<(), DanmakuServiceError> {
        let Some(plugins) = self.plugins.clone() else {
            self.database
                .finish_danmaku_match_job(&job.id, "FAILED", Some("PLUGIN_NOT_CONFIGURED"))
                .await?;
            return Ok(());
        };
        let settings = match plugins.danmaku_settings().await {
            Ok(settings) => settings,
            Err(_) => {
                self.database
                    .finish_danmaku_match_job(&job.id, "FAILED", Some("PLUGIN_NOT_CONFIGURED"))
                    .await?;
                return Ok(());
            }
        };
        // A library sweep may only run for a library the administrator selected in the plugin
        // configuration. A manual match names one item explicitly and is exempt (ADR-052 P5,
        // `docs/LUX-DEVELOPMENT.md` §3.7), which is exactly a job whose every item carries the
        // administrator's episode.
        let manual_only = self
            .database
            .danmaku_match_job_is_manual_only(&job.id)
            .await?;
        if !manual_only && !settings.library_ids.contains(&job.library_id) {
            self.database
                .finish_danmaku_match_job(&job.id, "FAILED", Some("LIBRARY_NOT_SELECTED"))
                .await?;
            return Ok(());
        }
        self.database
            .reset_running_danmaku_match_items(&job.id)
            .await?;
        let concurrency = self
            .resources
            .background_concurrency(effective_danmaku_concurrency(job.concurrency))
            .await;
        let mut workers: JoinSet<Result<Option<WorkerResult>, StorageError>> = JoinSet::new();
        let mut cancelled = false;
        let mut items_since_cancel_check = DANMAKU_CANCEL_CHECK_INTERVAL;

        loop {
            let items = self
                .database
                .list_pending_danmaku_match_items(&job.id, WORK_PAGE_SIZE)
                .await?;
            if items.is_empty() {
                break;
            }
            for item in items {
                if danmaku_cancel_check_due(items_since_cancel_check) {
                    if self
                        .database
                        .danmaku_match_job_cancel_requested(&job.id)
                        .await?
                    {
                        cancelled = true;
                        break;
                    }
                    items_since_cancel_check = 0;
                }
                while workers.len() >= concurrency {
                    self.finish_worker(&job.id, workers.join_next().await)
                        .await?;
                }
                let source = match (item.root_path, item.relative_path) {
                    (Some(root_path), Some(relative_path)) => Some(StoredDanmakuSource {
                        source_id: item.media_source_id,
                        root_path,
                        relative_path,
                        item_type: item.item_type.clone(),
                        season_number: item.season_number,
                        episode_number: item.episode_number,
                        title: item.title.clone(),
                        original_title: item.original_title.clone(),
                        series_title: item.series_title.clone(),
                        series_original_title: item.series_original_title.clone(),
                    }),
                    _ => None,
                };
                // A job item that already carries an episode is an administrator's manual match:
                // the worker fetches that episode instead of asking the plugin to match a title.
                let manual = item
                    .provider_episode_id
                    .clone()
                    .map(|episode_id| ManualItemTarget {
                        provider_anime_id: item.provider_anime_id.clone(),
                        episode_id,
                    });
                let database = self.database.clone();
                let plugins = plugins.clone();
                let settings = settings.clone();
                let overwrite = job.overwrite;
                workers.spawn(async move {
                    if !database.claim_danmaku_match_item(&item.id).await? {
                        return Ok::<_, StorageError>(None);
                    }
                    let Some(source) = source else {
                        return Ok(Some(WorkerResult::failed(
                            item.id,
                            "SOURCE_NOT_FOUND",
                            "media source is no longer available",
                        )));
                    };
                    let result = match manual {
                        Some(manual) => {
                            process_manual_danmaku_source(
                                source, plugins, database, manual, overwrite, item.id,
                            )
                            .await
                        }
                        None => {
                            process_danmaku_source_with_plugin(
                                source, plugins, settings, overwrite, item.id,
                            )
                            .await
                        }
                    };
                    Ok(Some(result))
                });
                items_since_cancel_check = items_since_cancel_check.saturating_add(1);
            }
            while !workers.is_empty() {
                self.finish_worker(&job.id, workers.join_next().await)
                    .await?;
            }
            if cancelled {
                break;
            }
        }
        if cancelled
            || self
                .database
                .danmaku_match_job_cancel_requested(&job.id)
                .await?
        {
            self.database
                .cancel_pending_danmaku_match_items(&job.id)
                .await?;
            self.database
                .finish_danmaku_match_job(&job.id, "CANCELLED", None)
                .await?;
        } else {
            let completed = self
                .database
                .find_danmaku_match_job(&job.id)
                .await?
                .ok_or(DanmakuServiceError::JobNotFound)?;
            let status = if completed.failed_count > 0 {
                "FAILED"
            } else {
                "COMPLETED"
            };
            let error = (status == "FAILED").then_some("ONE_OR_MORE_ITEMS_FAILED");
            self.database
                .finish_danmaku_match_job(&job.id, status, error)
                .await?;
        }
        Ok(())
    }

    async fn finish_worker(
        &self,
        job_id: &str,
        result: Option<Result<Result<Option<WorkerResult>, StorageError>, tokio::task::JoinError>>,
    ) -> Result<(), DanmakuServiceError> {
        let Some(result) = result else {
            return Ok(());
        };
        let result = match result {
            Ok(Ok(Some(result))) => result,
            Ok(Ok(None)) => return Ok(()),
            Ok(Err(error)) => return Err(DanmakuServiceError::Storage(error)),
            Err(_) => {
                return Err(DanmakuServiceError::WorkerFailed);
            }
        };
        if let Some(track) = result.track.as_ref() {
            self.database
                .upsert_danmaku_track(NewDanmakuTrack {
                    id: &track.id,
                    media_source_id: &track.media_source_id,
                    relative_path: &track.relative_path,
                    provider: track.provider.as_deref(),
                    provider_anime_id: track.provider_anime_id.as_deref(),
                    provider_episode_id: track.provider_episode_id.as_deref(),
                    fingerprint: Some(&track.fingerprint),
                    status: "READY",
                    error_code: None,
                })
                .await
                .map_err(DanmakuServiceError::Storage)?;
        }
        self.database
            .finish_danmaku_match_item(
                &result.item_id,
                result.status,
                result.provider_anime_id.as_deref(),
                result.provider_episode_id.as_deref(),
                result.error_code,
                result.error_message,
            )
            .await?;
        self.database
            .increment_danmaku_match_progress(
                job_id,
                result.success,
                result.skipped,
                !result.success && !result.skipped,
            )
            .await?;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DanmakuMatchJob {
    pub id: String,
    pub library_id: String,
    pub status: String,
    pub overwrite: bool,
    pub concurrency: i64,
    pub total_count: i64,
    pub processed_count: i64,
    pub success_count: i64,
    pub skipped_count: i64,
    pub failed_count: i64,
    pub cancel_requested: bool,
    pub error: Option<String>,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
}

fn danmaku_match_job(job: StoredDanmakuMatchJob) -> DanmakuMatchJob {
    DanmakuMatchJob {
        id: job.id,
        library_id: job.library_id,
        status: job.status,
        overwrite: job.overwrite,
        concurrency: job.concurrency,
        total_count: job.total_count,
        processed_count: job.processed_count,
        success_count: job.success_count,
        skipped_count: job.skipped_count,
        failed_count: job.failed_count,
        cancel_requested: job.cancel_requested,
        error: job.error,
        created_at: job.created_at,
        started_at: job.started_at,
        finished_at: job.finished_at,
    }
}

#[derive(Debug)]
pub enum DanmakuServiceError {
    InvalidConcurrency,
    LibraryNotFound,
    LibraryNotSelected,
    SourceNotFound,
    ProviderNotConfigured,
    AlreadyActive,
    JobNotFound,
    NotRetryable,
    WorkerFailed,
    /// The administrator's search query or page bound is outside the documented limits.
    InvalidManualSearch,
    /// The item exists but is not a movie/episode backed by a matchable media source.
    UnsupportedItemType,
    /// No enabled, configured danmaku plugin can serve a manual match.
    ManualMatchUnavailable,
    /// The enabled danmaku plugin only declares `danmaku.match` (ADR-052 P4/P5).
    ManualMatchUnsupported,
    /// The selected provider/anime/episode identifiers are missing or malformed.
    InvalidManualTarget,
    /// No `danmaku_tracks` row is registered for the requested media source.
    SidecarNotFound,
    /// A track without a provider was written next to the media file by the user.
    UserSidecarConfirmationRequired,
    /// The sidecar path leaves the media root or is not a regular file.
    InvalidSidecarPath,
    /// Removing the sidecar file failed for an IO reason.
    SidecarDeleteFailed,
    /// A plugin call for a manual match failed; the handler maps it without echoing its text.
    Plugin(PluginServiceError),
    Storage(StorageError),
}

impl fmt::Display for DanmakuServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConcurrency => formatter.write_str("invalid danmaku match concurrency"),
            Self::LibraryNotFound => formatter.write_str("danmaku library was not found"),
            Self::LibraryNotSelected => {
                formatter.write_str("danmaku library is not selected in plugin configuration")
            }
            Self::SourceNotFound => formatter.write_str("danmaku media source was not found"),
            Self::ProviderNotConfigured => {
                formatter.write_str("danmaku provider is not configured")
            }
            Self::AlreadyActive => formatter.write_str("a danmaku match job is already active"),
            Self::JobNotFound => formatter.write_str("danmaku match job was not found"),
            Self::NotRetryable => formatter.write_str("danmaku match job cannot be retried"),
            Self::WorkerFailed => formatter.write_str("danmaku match worker failed"),
            Self::InvalidManualSearch => formatter.write_str("danmaku search request is invalid"),
            Self::UnsupportedItemType => {
                formatter.write_str("item is not a matchable local danmaku video")
            }
            Self::ManualMatchUnavailable => {
                formatter.write_str("danmaku manual match is not available")
            }
            Self::ManualMatchUnsupported => {
                formatter.write_str("danmaku plugin does not support manual matching")
            }
            Self::InvalidManualTarget => {
                formatter.write_str("danmaku manual match target is invalid")
            }
            Self::SidecarNotFound => formatter.write_str("danmaku sidecar is not registered"),
            Self::UserSidecarConfirmationRequired => {
                formatter.write_str("user-provided danmaku sidecar needs explicit confirmation")
            }
            Self::InvalidSidecarPath => formatter.write_str("danmaku sidecar path is invalid"),
            Self::SidecarDeleteFailed => {
                formatter.write_str("danmaku sidecar could not be removed")
            }
            Self::Plugin(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for DanmakuServiceError {}

impl From<StorageError> for DanmakuServiceError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

fn effective_danmaku_concurrency(configured: i64) -> usize {
    if configured == 0 {
        return MAX_EFFECTIVE_CONCURRENCY;
    }
    usize::try_from(configured)
        .unwrap_or(1)
        .clamp(1, MAX_EFFECTIVE_CONCURRENCY)
}

fn danmaku_cancel_check_due(items_since_last_check: usize) -> bool {
    items_since_last_check >= DANMAKU_CANCEL_CHECK_INTERVAL
}

#[cfg(test)]
fn danmaku_cancel_check_count(item_count: usize) -> usize {
    item_count.div_ceil(DANMAKU_CANCEL_CHECK_INTERVAL)
}

struct TrackWrite {
    id: String,
    media_source_id: String,
    relative_path: String,
    provider: Option<String>,
    provider_anime_id: Option<String>,
    provider_episode_id: Option<String>,
    fingerprint: Vec<u8>,
}

struct WorkerResult {
    item_id: String,
    status: &'static str,
    success: bool,
    skipped: bool,
    provider_anime_id: Option<String>,
    provider_episode_id: Option<String>,
    error_code: Option<&'static str>,
    error_message: Option<&'static str>,
    track: Option<TrackWrite>,
}

impl WorkerResult {
    fn failed(item_id: String, code: &'static str, message: &'static str) -> Self {
        Self {
            item_id,
            status: "FAILED",
            success: false,
            skipped: false,
            provider_anime_id: None,
            provider_episode_id: None,
            error_code: Some(code),
            error_message: Some(message),
            track: None,
        }
    }
}

/// Paths and sidecar name a worker needs before it can decide what to do with one source.
struct PreparedSource {
    media_path: PathBuf,
    target_path: PathBuf,
    relative_sidecar_path: String,
}

/// Provider identity a worker records on the track row it writes.
///
/// The automatic path fills it from the `danmaku.match` answer; a manual match already knows the
/// administrator's choice, so it fills the same fields from the job item and the track row.
struct TrackIdentity<'a> {
    provider: Option<&'a str>,
    provider_anime_id: Option<&'a str>,
    provider_episode_id: Option<&'a str>,
}

/// The episode an administrator selected for one media source (ADR-052 P5).
struct ManualItemTarget {
    provider_anime_id: Option<String>,
    episode_id: String,
}

async fn prepare_worker_source(
    source: &StoredDanmakuSource,
    item_id: &str,
) -> Result<PreparedSource, WorkerResult> {
    let invalid_path = || {
        WorkerResult::failed(
            item_id.to_owned(),
            "INVALID_SOURCE_PATH",
            "media source path is invalid",
        )
    };
    let (media_path, target_path) =
        safe_danmaku_source_paths(&source.root_path, &source.relative_path)
            .await
            .map_err(|_| invalid_path())?;
    let relative_sidecar_path = danmaku_sidecar_path(Path::new(&source.relative_path))
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|_| invalid_path())?;
    Ok(PreparedSource {
        media_path,
        target_path,
        relative_sidecar_path,
    })
}

/// Applies the "an existing sidecar wins unless the caller asked to overwrite" rule.
///
/// `None` means the worker may fetch and write; `Some(result)` is the final answer for the item, so
/// both the automatic and the manual path share the skip and invalid-sidecar behaviour.
async fn existing_sidecar_result(
    prepared: &PreparedSource,
    source: &StoredDanmakuSource,
    identity: &TrackIdentity<'_>,
    overwrite: bool,
    item_id: &str,
) -> Option<WorkerResult> {
    let metadata = fs::symlink_metadata(&prepared.target_path).await.ok()?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        return Some(WorkerResult::failed(
            item_id.to_owned(),
            "INVALID_SIDECAR",
            "danmaku sidecar is not a regular file",
        ));
    }
    match read_danmaku_file(&prepared.target_path).await {
        Ok(bytes) if !overwrite && validate_danmaku_xml(&bytes).is_ok() => Some(WorkerResult {
            item_id: item_id.to_owned(),
            status: "SKIPPED",
            success: false,
            skipped: true,
            provider_anime_id: identity.provider_anime_id.map(str::to_owned),
            provider_episode_id: identity.provider_episode_id.map(str::to_owned),
            error_code: None,
            error_message: None,
            track: Some(TrackWrite {
                id: Uuid::now_v7().to_string(),
                media_source_id: source.source_id.clone(),
                relative_path: prepared.relative_sidecar_path.clone(),
                provider: identity.provider.map(str::to_owned),
                provider_anime_id: identity.provider_anime_id.map(str::to_owned),
                provider_episode_id: identity.provider_episode_id.map(str::to_owned),
                fingerprint: fingerprint(&bytes),
            }),
        }),
        Ok(_) if !overwrite => Some(WorkerResult::failed(
            item_id.to_owned(),
            "INVALID_SIDECAR",
            "existing danmaku sidecar is invalid",
        )),
        Ok(_) | Err(_) => None,
    }
}

async fn write_worker_sidecar(
    source: &StoredDanmakuSource,
    prepared: &PreparedSource,
    identity: &TrackIdentity<'_>,
    xml: &[u8],
    overwrite: bool,
    item_id: String,
) -> WorkerResult {
    if atomic_write_danmaku_xml(&prepared.target_path, xml, overwrite)
        .await
        .is_err()
    {
        return WorkerResult::failed(
            item_id,
            "WRITE_FAILED",
            "danmaku sidecar could not be written",
        );
    }
    WorkerResult {
        item_id,
        status: "WRITTEN",
        success: true,
        skipped: false,
        provider_anime_id: identity.provider_anime_id.map(str::to_owned),
        provider_episode_id: identity.provider_episode_id.map(str::to_owned),
        error_code: None,
        error_message: None,
        track: Some(TrackWrite {
            id: Uuid::now_v7().to_string(),
            media_source_id: source.source_id.clone(),
            relative_path: prepared.relative_sidecar_path.clone(),
            provider: identity.provider.map(str::to_owned),
            provider_anime_id: identity.provider_anime_id.map(str::to_owned),
            provider_episode_id: identity.provider_episode_id.map(str::to_owned),
            fingerprint: fingerprint(xml),
        }),
    }
}

async fn process_danmaku_source_with_plugin(
    source: StoredDanmakuSource,
    plugins: PluginService,
    settings: DanmakuSettings,
    overwrite: bool,
    item_id: String,
) -> WorkerResult {
    let identity = TrackIdentity {
        provider: None,
        provider_anime_id: None,
        provider_episode_id: None,
    };
    let prepared = match prepare_worker_source(&source, &item_id).await {
        Ok(prepared) => prepared,
        Err(result) => return result,
    };
    if let Some(result) =
        existing_sidecar_result(&prepared, &source, &identity, overwrite, &item_id).await
    {
        return result;
    }
    let Some(file_name) = prepared
        .media_path
        .file_name()
        .and_then(|name| name.to_str())
    else {
        return WorkerResult::failed(item_id, "INVALID_SOURCE_PATH", "media filename is invalid");
    };
    let candidates = match_file_name_candidates(file_name, &source, &settings);
    let Some((primary, alternate)) = candidates.split_first() else {
        return WorkerResult::failed(item_id, "NO_MATCH", "no danmaku match title candidate");
    };
    let matched = match plugins
        .match_danmaku_with_candidates(primary, alternate)
        .await
    {
        Ok(value) => value,
        Err(error) => {
            return WorkerResult::failed(
                item_id,
                danmaku_plugin_error_code(&error),
                "danmaku plugin request failed",
            );
        }
    };
    if matched.status == DanmakuMatchStatus::NoMatch {
        return WorkerResult::failed(item_id, "NO_MATCH", "danmaku plugin returned no match");
    }
    let Some(episode_id) = matched.episode_id.as_deref() else {
        return WorkerResult::failed(
            item_id,
            "PLUGIN_INVALID_RESPONSE",
            "danmaku plugin response has no episode",
        );
    };
    let Some(xml_base64) = matched.xml_base64.as_deref() else {
        return WorkerResult::failed(
            item_id,
            "PLUGIN_INVALID_RESPONSE",
            "danmaku plugin response has no XML",
        );
    };
    let xml = match BASE64.decode(xml_base64) {
        Ok(bytes) => bytes,
        Err(_) => {
            return WorkerResult::failed(
                item_id,
                "PLUGIN_INVALID_RESPONSE",
                "danmaku plugin XML is not valid base64",
            );
        }
    };
    if validate_danmaku_xml(&xml).is_err() {
        return WorkerResult::failed(
            item_id,
            "PLUGIN_INVALID_RESPONSE",
            "danmaku plugin XML is invalid",
        );
    }
    let identity = TrackIdentity {
        provider: matched.provider.as_deref(),
        provider_anime_id: matched.anime_id.as_deref(),
        provider_episode_id: Some(episode_id),
    };
    write_worker_sidecar(&source, &prepared, &identity, &xml, overwrite, item_id).await
}

/// Runs one administrator-selected episode: no `danmaku.match`, only `danmaku.fetch`.
///
/// The chosen provider travels in the track row the job creation wrote, so a queued job keeps its
/// target across restarts without a new column. The XML still goes through the same host decode,
/// `validate_danmaku_xml` and atomic write as the automatic path.
async fn process_manual_danmaku_source(
    source: StoredDanmakuSource,
    plugins: PluginService,
    database: Database,
    manual: ManualItemTarget,
    overwrite: bool,
    item_id: String,
) -> WorkerResult {
    let provider = database
        .find_danmaku_track_provider(&source.source_id)
        .await
        .ok()
        .flatten();
    let identity = TrackIdentity {
        provider: provider.as_deref(),
        provider_anime_id: manual.provider_anime_id.as_deref(),
        provider_episode_id: Some(manual.episode_id.as_str()),
    };
    let prepared = match prepare_worker_source(&source, &item_id).await {
        Ok(prepared) => prepared,
        Err(result) => return result,
    };
    if let Some(result) =
        existing_sidecar_result(&prepared, &source, &identity, overwrite, &item_id).await
    {
        return result;
    }
    let fetched = match plugins
        .fetch_danmaku(identity.provider, &manual.episode_id)
        .await
    {
        Ok(value) => value,
        Err(error) => {
            return WorkerResult::failed(
                item_id,
                danmaku_plugin_error_code(&error),
                "danmaku plugin request failed",
            );
        }
    };
    let xml = match fetched.decode_xml() {
        Ok(bytes) => bytes,
        Err(_) => {
            return WorkerResult::failed(
                item_id,
                "PLUGIN_INVALID_RESPONSE",
                "danmaku plugin XML is invalid",
            );
        }
    };
    write_worker_sidecar(&source, &prepared, &identity, &xml, overwrite, item_id).await
}

#[cfg(test)]
mod tests {
    use super::{danmaku_cancel_check_count, effective_danmaku_concurrency};
    use crate::application::{libraries::LibraryService, plugins::DANMAKU_PLUGIN_ID};
    use crate::{config::Config, library::LibraryKind, storage::Database};

    #[test]
    fn worker_concurrency_has_a_memory_safe_ceiling() {
        assert_eq!(effective_danmaku_concurrency(0), 4);
        assert_eq!(effective_danmaku_concurrency(1), 1);
        assert_eq!(effective_danmaku_concurrency(2), 2);
        assert_eq!(effective_danmaku_concurrency(64), 4);
    }

    #[test]
    fn cancellation_checks_start_immediately_and_repeat_every_eight_items() {
        assert_eq!(danmaku_cancel_check_count(0), 0);
        assert_eq!(danmaku_cancel_check_count(1), 1);
        assert_eq!(danmaku_cancel_check_count(8), 1);
        assert_eq!(danmaku_cancel_check_count(9), 2);
        assert_eq!(danmaku_cancel_check_count(100), 13);
    }

    #[tokio::test]
    async fn configured_jobs_reuse_plugin_settings_across_libraries()
    -> Result<(), Box<dyn std::error::Error>> {
        const PLUGIN_ID: &str = DANMAKU_PLUGIN_ID;
        let temp_dir = tempfile::tempdir()?;
        let config_dir = temp_dir.path().join("config");
        let plugin_dir = config_dir.join(format!("plugins/{PLUGIN_ID}/binaries"));
        tokio::fs::create_dir_all(&plugin_dir).await?;
        tokio::fs::write(plugin_dir.join("plugin"), b"placeholder").await?;
        tokio::fs::write(
            config_dir.join(format!("plugins/{PLUGIN_ID}/manifest.json")),
            serde_json::to_vec_pretty(&serde_json::json!({
                "formatVersion": 1,
                "id": PLUGIN_ID,
                "name": "Test danmaku",
                "version": "1.0.0",
                "apiVersion": 1,
                "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
                "type": "danmaku",
                "category": "MEDIA",
                "capabilities": ["danmaku.match"],
                "configFields": [
                    {"key": "providerBaseUrl", "label": "Provider", "type": "text", "required": true},
                    {"key": "libraryIds", "label": "Libraries", "type": "select", "multiple": true, "optionsSource": "media-libraries"},
                    {"key": "matchOriginalFilename", "label": "Original", "type": "toggle", "defaultValue": true},
                    {"key": "matchSimplifiedTraditionalTitles", "label": "CJK", "type": "toggle", "defaultValue": true},
                    {"key": "matchEnglishTitle", "label": "English", "type": "toggle", "defaultValue": false},
                    {"key": "concurrency", "label": "Concurrency", "type": "number", "defaultValue": 2, "minimum": 0, "maximum": 64},
                    {"key": "overwrite", "label": "Overwrite", "type": "toggle", "defaultValue": false},
                    {"key": "schedule", "label": "Schedule", "type": "text", "required": true, "defaultValue": "0 6 * * *"}
                ],
                "permissions": {"network": [], "filesystem": []},
                "files": []
            }))?,
        )
        .await?;

        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: config_dir.clone(),
        };
        let database = Database::connect(&config).await?;
        let libraries = LibraryService::new(database.clone());
        let first = libraries
            .create_library("Danmaku one", LibraryKind::Series, false)
            .await?;
        let second = libraries
            .create_library("Danmaku two", LibraryKind::Series, false)
            .await?;
        tokio::fs::create_dir_all(config_dir.join("plugin-config")).await?;
        tokio::fs::write(
            config_dir.join(format!("plugin-config/{PLUGIN_ID}.json")),
            serde_json::to_vec(&serde_json::json!({
                "providerBaseUrl": "https://danmu.example",
                "libraryIds": [first.id.to_string(), second.id.to_string()],
                "matchOriginalFilename": true,
                "matchSimplifiedTraditionalTitles": true,
                "matchEnglishTitle": false,
                "concurrency": 2,
                "overwrite": false,
                "schedule": "0 6 * * *"
            }))?,
        )
        .await?;
        let plugins = crate::application::plugins::PluginService::new_without_login_background_worker_for_test(database.clone(), config_dir);
        plugins.install(PLUGIN_ID).await?;

        let service = super::DanmakuService::new(database.clone()).with_plugins(plugins);
        database.reset_query_count();
        let jobs = service.create_configured_jobs().await?;
        assert_eq!(jobs.len(), 2);
        assert_eq!(database.query_count(), 19);
        Ok(())
    }
}
