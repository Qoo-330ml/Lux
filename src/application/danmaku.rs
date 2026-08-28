use std::{
    fmt,
    path::{Component, Path, PathBuf},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use quick_xml::{events::Event, reader::Reader};
use reqwest::Url;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::{
    fs::{self, OpenOptions},
    io::{AsyncReadExt, AsyncWriteExt},
    task::JoinSet,
};
use uuid::Uuid;

use crate::{
    application::{
        media_matching::{MediaKind, parse_media_name},
        plugin_protocol::DanmakuMatchStatus,
        plugins::{DanmakuSettings, PluginService, PluginServiceError},
    },
    domain::ids::LibraryId,
    observability::resources::ResourceMetrics,
    storage::{
        Database, NewDanmakuMatchJob, NewDanmakuTrack, StorageError, StoredDanmakuMatchJob,
        StoredDanmakuSource,
    },
};

pub const MAX_DANMAKU_XML_BYTES: usize = 50 * 1024 * 1024;
const MAX_PROVIDER_BASE_URL_CHARS: usize = 4096;
const MAX_CONCURRENCY: i64 = 64;
const MAX_EFFECTIVE_CONCURRENCY: usize = 4;
const WORK_PAGE_SIZE: i64 = 100;
pub const DEFAULT_DANMAKU_CONCURRENCY: i64 = 2;

#[derive(Clone, Eq, PartialEq)]
pub struct ProviderBaseUrl {
    normalized: String,
    redacted: String,
}

impl ProviderBaseUrl {
    pub fn normalized(&self) -> &str {
        &self.normalized
    }

    pub fn redacted(&self) -> &str {
        &self.redacted
    }
}

impl fmt::Debug for ProviderBaseUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderBaseUrl")
            .field("redacted", &self.redacted)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderUrlError {
    Invalid,
    TooLong,
}

impl fmt::Display for ProviderUrlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid => formatter.write_str("danmaku provider URL is invalid"),
            Self::TooLong => formatter.write_str("danmaku provider URL is too long"),
        }
    }
}

impl std::error::Error for ProviderUrlError {}

pub fn validate_provider_base_url(value: &str) -> Result<ProviderBaseUrl, ProviderUrlError> {
    let value = value.trim();
    if value.chars().count() > MAX_PROVIDER_BASE_URL_CHARS {
        return Err(ProviderUrlError::TooLong);
    }
    let mut url = Url::parse(value).map_err(|_| ProviderUrlError::Invalid)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ProviderUrlError::Invalid);
    }
    let path = url.path().trim_end_matches('/').to_owned();
    url.set_path(if path.is_empty() { "/" } else { &path });
    let normalized = url.to_string().trim_end_matches('/').to_owned();
    let authority = match url.port() {
        Some(port) => format!("{}:{port}", url.host_str().unwrap_or("configured")),
        None => url.host_str().unwrap_or("configured").to_owned(),
    };
    let redacted = format!("{}://{}/[redacted]", url.scheme(), authority);
    Ok(ProviderBaseUrl {
        normalized,
        redacted,
    })
}

pub fn danmaku_sidecar_path(video_path: &Path) -> Result<PathBuf, DanmakuPathError> {
    if video_path.is_absolute()
        || video_path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(DanmakuPathError::OutsideMediaRoot);
    }
    let file_name = video_path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .ok_or(DanmakuPathError::InvalidFileName)?;
    let stem = video_path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .ok_or(DanmakuPathError::InvalidFileName)?;
    if file_name == stem {
        return Err(DanmakuPathError::InvalidFileName);
    }
    let mut sidecar = video_path.to_path_buf();
    sidecar.set_file_name(format!("{stem}.xml"));
    Ok(sidecar)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DanmakuPathError {
    OutsideMediaRoot,
    InvalidFileName,
}

impl fmt::Display for DanmakuPathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutsideMediaRoot => formatter.write_str("danmaku path leaves the media root"),
            Self::InvalidFileName => formatter.write_str("danmaku media filename is invalid"),
        }
    }
}

impl std::error::Error for DanmakuPathError {}

pub fn validate_danmaku_xml(bytes: &[u8]) -> Result<(), DanmakuXmlError> {
    if bytes.is_empty() {
        return Err(DanmakuXmlError::Empty);
    }
    if bytes.len() > MAX_DANMAKU_XML_BYTES {
        return Err(DanmakuXmlError::TooLarge);
    }

    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().trim_text(false);
    let mut buffer = Vec::new();
    let mut depth = 0_usize;
    let mut saw_root = false;
    let mut saw_danmaku = false;
    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Eof) => break,
            Ok(Event::Start(event)) => {
                let event_name = event.name();
                let name = local_name(event_name.as_ref());
                if depth == 0 {
                    if saw_root || name != b"i" {
                        return Err(DanmakuXmlError::InvalidRoot);
                    }
                    saw_root = true;
                }
                if depth >= 1 && name == b"d" {
                    saw_danmaku = true;
                }
                depth = depth.saturating_add(1);
            }
            Ok(Event::Empty(event)) => {
                let event_name = event.name();
                let name = local_name(event_name.as_ref());
                if depth == 0 {
                    if saw_root || name != b"i" {
                        return Err(DanmakuXmlError::InvalidRoot);
                    }
                    saw_root = true;
                } else if name == b"d" {
                    saw_danmaku = true;
                }
            }
            Ok(Event::End(_)) => {
                if depth == 0 {
                    return Err(DanmakuXmlError::InvalidXml(
                        "unexpected closing element".to_owned(),
                    ));
                }
                depth -= 1;
            }
            Ok(Event::DocType(_)) => {
                return Err(DanmakuXmlError::InvalidXml(
                    "document type declarations are not allowed".to_owned(),
                ));
            }
            Ok(_) => {}
            Err(error) => return Err(DanmakuXmlError::InvalidXml(error.to_string())),
        }
        buffer.clear();
    }
    if !saw_root || depth != 0 {
        return Err(DanmakuXmlError::InvalidXml(
            "danmaku XML document is incomplete".to_owned(),
        ));
    }
    if !saw_danmaku {
        return Err(DanmakuXmlError::MissingDanmaku);
    }
    Ok(())
}

fn local_name(name: &[u8]) -> &[u8] {
    name.rsplit(|value| *value == b':').next().unwrap_or(name)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DanmakuXmlError {
    Empty,
    TooLarge,
    InvalidRoot,
    MissingDanmaku,
    InvalidXml(String),
}

impl fmt::Display for DanmakuXmlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("danmaku XML is empty"),
            Self::TooLarge => formatter.write_str("danmaku XML is too large"),
            Self::InvalidRoot => formatter.write_str("danmaku XML root must be <i>"),
            Self::MissingDanmaku => formatter.write_str("danmaku XML has no <d> entries"),
            Self::InvalidXml(message) => write!(formatter, "danmaku XML is invalid: {message}"),
        }
    }
}

impl std::error::Error for DanmakuXmlError {}

pub async fn atomic_write_danmaku_xml(
    target: &Path,
    bytes: &[u8],
    overwrite: bool,
) -> Result<(), DanmakuWriteError> {
    validate_danmaku_xml(bytes).map_err(DanmakuWriteError::InvalidXml)?;
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let target_type = fs::symlink_metadata(target).await;
    match target_type {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(DanmakuWriteError::SymlinkTarget);
        }
        Ok(_) if !overwrite => return Err(DanmakuWriteError::AlreadyExists),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(DanmakuWriteError::Io(error.to_string())),
        Ok(_) => {}
    }

    let temporary = parent.join(format!(".lux-{}.danmaku.xml.tmp", Uuid::now_v7()));
    let result = async {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .await
            .map_err(|error| DanmakuWriteError::Io(error.to_string()))?;
        file.write_all(bytes)
            .await
            .map_err(|error| DanmakuWriteError::Io(error.to_string()))?;
        file.sync_all()
            .await
            .map_err(|error| DanmakuWriteError::Io(error.to_string()))?;
        drop(file);

        if !overwrite {
            match fs::symlink_metadata(target).await {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(DanmakuWriteError::SymlinkTarget);
                }
                Ok(_) => return Err(DanmakuWriteError::AlreadyExists),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(DanmakuWriteError::Io(error.to_string())),
            }
        } else if fs::symlink_metadata(target)
            .await
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(DanmakuWriteError::SymlinkTarget);
        }

        fs::rename(&temporary, target)
            .await
            .map_err(|error| DanmakuWriteError::Io(error.to_string()))?;
        let directory = fs::File::open(parent)
            .await
            .map_err(|error| DanmakuWriteError::Io(error.to_string()))?;
        directory
            .sync_all()
            .await
            .map_err(|error| DanmakuWriteError::Io(error.to_string()))?;
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_file(&temporary).await;
    }
    result
}

#[derive(Debug, Eq, PartialEq)]
pub enum DanmakuWriteError {
    AlreadyExists,
    SymlinkTarget,
    InvalidXml(DanmakuXmlError),
    Io(String),
}

impl fmt::Display for DanmakuWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyExists => formatter.write_str("danmaku XML already exists"),
            Self::SymlinkTarget => formatter.write_str("danmaku XML target cannot be a symlink"),
            Self::InvalidXml(error) => error.fmt(formatter),
            Self::Io(message) => write!(formatter, "danmaku XML IO error: {message}"),
        }
    }
}

impl std::error::Error for DanmakuWriteError {}

#[derive(Clone)]
pub struct DanmakuService {
    database: Database,
    plugins: Option<PluginService>,
    resources: ResourceMetrics,
}

impl DanmakuService {
    pub fn new(database: Database) -> Self {
        Self {
            database,
            plugins: None,
            resources: ResourceMetrics::new(),
        }
    }

    pub fn with_plugins(mut self, plugins: PluginService) -> Self {
        self.plugins = Some(plugins);
        self
    }

    pub fn with_resource_metrics(mut self, resources: ResourceMetrics) -> Self {
        self.resources = resources;
        self
    }

    pub async fn create_job(
        &self,
        library_id: LibraryId,
        concurrency: i64,
        overwrite: bool,
    ) -> Result<DanmakuMatchJob, DanmakuServiceError> {
        if !(1..=MAX_CONCURRENCY).contains(&concurrency) {
            return Err(DanmakuServiceError::InvalidConcurrency);
        }
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
        let Some(plugins) = &self.plugins else {
            return Err(DanmakuServiceError::ProviderNotConfigured);
        };
        let settings = plugins
            .danmaku_settings()
            .await
            .map_err(|_| DanmakuServiceError::ProviderNotConfigured)?;
        if !settings.library_ids.iter().any(|id| id == &library_id) {
            return Err(DanmakuServiceError::LibraryNotSelected);
        }
        if !plugins
            .has_available_danmaku()
            .await
            .map_err(|_| DanmakuServiceError::ProviderNotConfigured)?
        {
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
        let mut jobs = Vec::with_capacity(settings.library_ids.len());
        for library_id in settings.library_ids {
            let library_id = library_id
                .parse::<LibraryId>()
                .map_err(|_| DanmakuServiceError::LibraryNotFound)?;
            match self
                .create_job(library_id, DEFAULT_DANMAKU_CONCURRENCY, false)
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

    pub async fn read_sidecar(
        &self,
        item_id: &str,
    ) -> Result<Option<Vec<u8>>, DanmakuServiceError> {
        self.read_sidecar_for_source(item_id, None).await
    }

    pub async fn read_sidecar_for_source(
        &self,
        item_id: &str,
        source_id: Option<&str>,
    ) -> Result<Option<Vec<u8>>, DanmakuServiceError> {
        let Some(source) = self
            .database
            .find_local_danmaku_source_for_item(item_id, source_id)
            .await?
        else {
            return Ok(None);
        };
        let Ok((_, target)) = safe_danmaku_source_paths(&source).await else {
            return Ok(None);
        };
        let Ok(metadata) = fs::symlink_metadata(&target).await else {
            return Ok(None);
        };
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return Ok(None);
        }
        let Ok(bytes) = read_danmaku_file(&target).await else {
            return Ok(None);
        };
        if validate_danmaku_xml(&bytes).is_err() {
            return Ok(None);
        }
        Ok(Some(bytes))
    }

    pub async fn read_registered_sidecar_for_source(
        &self,
        item_id: &str,
        source_id: Option<&str>,
    ) -> Result<Option<Vec<u8>>, DanmakuServiceError> {
        let Some(source) = self
            .database
            .find_registered_local_danmaku_source_for_item(item_id, source_id)
            .await?
        else {
            return Ok(None);
        };
        let Ok((_, target)) = safe_danmaku_source_paths(&source).await else {
            return Ok(None);
        };
        let Ok(metadata) = fs::symlink_metadata(&target).await else {
            return Ok(None);
        };
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return Ok(None);
        }
        let Ok(bytes) = read_danmaku_file(&target).await else {
            return Ok(None);
        };
        if validate_danmaku_xml(&bytes).is_err() {
            return Ok(None);
        }
        Ok(Some(bytes))
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
        self.create_job(library_id, job.concurrency, job.overwrite)
            .await
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
        if !settings.library_ids.contains(&job.library_id) {
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

        loop {
            let items = self
                .database
                .list_pending_danmaku_match_items(&job.id, WORK_PAGE_SIZE)
                .await?;
            if items.is_empty() {
                break;
            }
            for item in items {
                if self
                    .database
                    .danmaku_match_job_cancel_requested(&job.id)
                    .await?
                {
                    cancelled = true;
                    break;
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
                        title: item.title.clone(),
                        original_title: item.original_title.clone(),
                        series_title: item.series_title.clone(),
                        series_original_title: item.series_original_title.clone(),
                    }),
                    _ => None,
                };
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
                    let result = process_danmaku_source_with_plugin(
                        source, plugins, settings, overwrite, item.id,
                    )
                    .await;
                    Ok(Some(result))
                });
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
    usize::try_from(configured)
        .unwrap_or(1)
        .clamp(1, MAX_EFFECTIVE_CONCURRENCY)
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

async fn process_danmaku_source_with_plugin(
    source: StoredDanmakuSource,
    plugins: PluginService,
    settings: DanmakuSettings,
    overwrite: bool,
    item_id: String,
) -> WorkerResult {
    let (media_path, target_path) = match safe_danmaku_source_paths(&source).await {
        Ok(paths) => paths,
        Err(_) => {
            return WorkerResult::failed(
                item_id,
                "INVALID_SOURCE_PATH",
                "media source path is invalid",
            );
        }
    };
    let relative_path = match danmaku_sidecar_path(Path::new(&source.relative_path)) {
        Ok(path) => path.to_string_lossy().into_owned(),
        Err(_) => {
            return WorkerResult::failed(
                item_id,
                "INVALID_SOURCE_PATH",
                "media source path is invalid",
            );
        }
    };
    if let Ok(metadata) = fs::symlink_metadata(&target_path).await {
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return WorkerResult::failed(
                item_id,
                "INVALID_SIDECAR",
                "danmaku sidecar is not a regular file",
            );
        }
        match read_danmaku_file(&target_path).await {
            Ok(bytes) if !overwrite && validate_danmaku_xml(&bytes).is_ok() => {
                return WorkerResult {
                    item_id,
                    status: "SKIPPED",
                    success: false,
                    skipped: true,
                    provider_anime_id: None,
                    provider_episode_id: None,
                    error_code: None,
                    error_message: None,
                    track: Some(TrackWrite {
                        id: Uuid::now_v7().to_string(),
                        media_source_id: source.source_id,
                        relative_path,
                        provider: None,
                        provider_anime_id: None,
                        provider_episode_id: None,
                        fingerprint: fingerprint(&bytes),
                    }),
                };
            }
            Ok(_) if !overwrite => {
                return WorkerResult::failed(
                    item_id,
                    "INVALID_SIDECAR",
                    "existing danmaku sidecar is invalid",
                );
            }
            Ok(_) | Err(_) => {}
        }
    }
    let Some(file_name) = media_path.file_name().and_then(|name| name.to_str()) else {
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
    let Some(episode_id) = matched.episode_id.clone() else {
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
    if atomic_write_danmaku_xml(&target_path, &xml, overwrite)
        .await
        .is_err()
    {
        return WorkerResult::failed(
            item_id,
            "WRITE_FAILED",
            "danmaku sidecar could not be written",
        );
    }
    let fingerprint = fingerprint(&xml);
    WorkerResult {
        item_id,
        status: "WRITTEN",
        success: true,
        skipped: false,
        provider_anime_id: matched.anime_id.clone(),
        provider_episode_id: Some(episode_id.clone()),
        error_code: None,
        error_message: None,
        track: Some(TrackWrite {
            id: Uuid::now_v7().to_string(),
            media_source_id: source.source_id,
            relative_path,
            provider: matched.provider,
            provider_anime_id: matched.anime_id,
            provider_episode_id: Some(episode_id),
            fingerprint,
        }),
    }
}

fn match_file_name_candidates(
    file_name: &str,
    source: &StoredDanmakuSource,
    settings: &DanmakuSettings,
) -> Vec<String> {
    let mut candidates = Vec::new();
    if settings.match_original_filename {
        candidates.push(file_name.to_owned());
    }
    let title = if source.item_type.as_deref() == Some("EPISODE") {
        source.series_title.as_deref().or(source.title.as_deref())
    } else {
        source.title.as_deref()
    };
    if settings.match_simplified_traditional_titles {
        if let Some(title) = title {
            candidates.push(title_with_episode_suffix(title, file_name));
        }
    }
    if settings.match_english_title {
        let title = if source.item_type.as_deref() == Some("EPISODE") {
            source
                .series_original_title
                .as_deref()
                .or(source.original_title.as_deref())
        } else {
            source.original_title.as_deref()
        };
        if let Some(title) = title {
            candidates.push(title_with_episode_suffix(title, file_name));
        }
    }
    candidates.retain(|candidate| !candidate.trim().is_empty());
    candidates.dedup();
    candidates
}

fn title_with_episode_suffix(title: &str, file_name: &str) -> String {
    let Some(parsed) = parse_media_name(file_name, MediaKind::Episode) else {
        return title.trim().to_owned();
    };
    let mut result = title.trim().to_owned();
    if let Some(season) = parsed.season {
        result.push_str(&format!(" S{season:02}"));
    }
    if let Some(episode) = parsed.episode {
        result.push_str(&format!("E{episode:02}"));
    }
    result
}

async fn safe_danmaku_source_paths(source: &StoredDanmakuSource) -> Result<(PathBuf, PathBuf), ()> {
    let relative = Path::new(&source.relative_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(());
    }
    let root = fs::canonicalize(&source.root_path).await.map_err(|_| ())?;
    let media_path = fs::canonicalize(root.join(relative))
        .await
        .map_err(|_| ())?;
    if !media_path.starts_with(&root) {
        return Err(());
    }
    let sidecar_relative = danmaku_sidecar_path(relative).map_err(|_| ())?;
    let parent = media_path.parent().ok_or(())?.to_path_buf();
    if !parent.starts_with(&root) {
        return Err(());
    }
    let sidecar_name = sidecar_relative.file_name().ok_or(())?.to_owned();
    let sidecar_path = parent.join(sidecar_name);
    if let Ok(canonical_sidecar) = fs::canonicalize(&sidecar_path).await {
        if !canonical_sidecar.starts_with(&root) {
            return Err(());
        }
    }
    Ok((media_path, sidecar_path))
}

async fn read_danmaku_file(path: &Path) -> Result<Vec<u8>, ()> {
    let metadata = fs::metadata(path).await.map_err(|_| ())?;
    if metadata.len() > MAX_DANMAKU_XML_BYTES as u64 {
        return Err(());
    }
    let file = fs::File::open(path).await.map_err(|_| ())?;
    let mut bytes = Vec::new();
    file.take((MAX_DANMAKU_XML_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| ())?;
    if bytes.len() > MAX_DANMAKU_XML_BYTES {
        return Err(());
    }
    Ok(bytes)
}

fn fingerprint(bytes: &[u8]) -> Vec<u8> {
    Sha256::digest(bytes).to_vec()
}

fn danmaku_plugin_error_code(error: &PluginServiceError) -> &'static str {
    match error {
        PluginServiceError::UnknownPlugin(_) | PluginServiceError::Unavailable(_) => {
            "PLUGIN_UNAVAILABLE"
        }
        PluginServiceError::InvalidConfig => "PLUGIN_NOT_CONFIGURED",
        PluginServiceError::InvalidResponse => "PLUGIN_INVALID_RESPONSE",
        PluginServiceError::Runtime(_) => "PLUGIN_RUNTIME_ERROR",
        PluginServiceError::ConfigIo(_) => "PLUGIN_CONFIG_ERROR",
        PluginServiceError::NoUpdate
        | PluginServiceError::Store(_)
        | PluginServiceError::Storage(_) => "PLUGIN_ERROR",
    }
}

#[cfg(test)]
mod tests {
    use super::{effective_danmaku_concurrency, match_file_name_candidates};
    use crate::application::{plugins::DanmakuSettings, schedule::DEFAULT_DANMAKU_MATCH_SCHEDULE};
    use crate::storage::StoredDanmakuSource;

    #[test]
    fn worker_concurrency_has_a_memory_safe_ceiling() {
        assert_eq!(effective_danmaku_concurrency(1), 1);
        assert_eq!(effective_danmaku_concurrency(2), 2);
        assert_eq!(effective_danmaku_concurrency(64), 4);
    }

    #[test]
    fn title_matching_candidates_follow_configured_order() {
        let source = StoredDanmakuSource {
            source_id: "source-1".to_owned(),
            root_path: "/media".to_owned(),
            relative_path: "Show/Episode 01.mkv".to_owned(),
            item_type: Some("EPISODE".to_owned()),
            title: Some("第 1 集".to_owned()),
            original_title: Some("Episode 1".to_owned()),
            series_title: Some("简体剧名".to_owned()),
            series_original_title: Some("English Show".to_owned()),
        };
        let settings = DanmakuSettings {
            library_ids: vec!["library-1".to_owned()],
            match_original_filename: true,
            match_simplified_traditional_titles: true,
            match_english_title: true,
            schedule: DEFAULT_DANMAKU_MATCH_SCHEDULE.to_owned(),
        };

        assert_eq!(
            match_file_name_candidates("Original.S01E01.mkv", &source, &settings),
            vec![
                "Original.S01E01.mkv",
                "简体剧名 S01E01",
                "English Show S01E01"
            ]
        );
    }
}
