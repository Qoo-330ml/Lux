use std::{
    fmt,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use quick_xml::{events::Event, reader::Reader};
use reqwest::{Client, StatusCode, Url};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::{
    fs::{self, OpenOptions},
    io::{AsyncReadExt, AsyncWriteExt},
    task::JoinSet,
};
use uuid::Uuid;

use crate::{
    application::{
        media_matching::{MediaKind, normalize_title, parse_media_name},
        settings::{read_danmaku_provider_url_async, read_network_proxy_url_async},
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
const MAX_PROVIDER_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const PROVIDER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const MAX_CONCURRENCY: i64 = 64;
const MAX_EFFECTIVE_CONCURRENCY: usize = 4;
const WORK_PAGE_SIZE: i64 = 100;
const DANMAKU_PROVIDER: &str = "dandanplay";

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

    pub(crate) fn url(&self) -> Result<Url, ProviderUrlError> {
        Url::parse(&self.normalized).map_err(|_| ProviderUrlError::Invalid)
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
pub struct DanmakuProviderClient {
    client: Client,
    base: ProviderBaseUrl,
}

impl DanmakuProviderClient {
    pub fn new(base_url: &str, proxy_url: Option<&str>) -> Result<Self, DanmakuProviderError> {
        let base = validate_provider_base_url(base_url)
            .map_err(DanmakuProviderError::InvalidProviderUrl)?;
        let builder = crate::network::client_builder_from_env_or(proxy_url)
            .map_err(|_| DanmakuProviderError::InvalidProxy)?;
        let client = builder
            .timeout(PROVIDER_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| DanmakuProviderError::Client)?;
        Ok(Self { client, base })
    }

    pub fn redacted_base_url(&self) -> &str {
        self.base.redacted()
    }

    pub async fn match_filename(
        &self,
        file_name: &str,
    ) -> Result<Option<DanmakuMatch>, DanmakuProviderError> {
        if file_name.trim().is_empty() || file_name.chars().count() > 1024 {
            return Err(DanmakuProviderError::InvalidRequest);
        }
        let response = self
            .client
            .post(self.api_url("match")?)
            .json(&serde_json::json!({ "fileName": file_name }))
            .send()
            .await
            .map_err(|_| DanmakuProviderError::Unavailable)?;
        if matches!(
            response.status(),
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
        ) {
            return self.fallback_match(file_name).await;
        }
        let body = self.read_response(response).await?;
        let value: Value =
            serde_json::from_slice(&body).map_err(|_| DanmakuProviderError::InvalidResponse)?;
        let Some(first) = value
            .get("matches")
            .and_then(Value::as_array)
            .and_then(|matches| matches.first())
        else {
            return Ok(None);
        };
        let episode_id = json_string(first.get("episodeId"))
            .filter(|value| !value.is_empty())
            .ok_or(DanmakuProviderError::InvalidResponse)?;
        Ok(Some(DanmakuMatch {
            anime_id: json_string(first.get("animeId")),
            episode_id,
            anime_title: json_string(first.get("animeTitle")),
            episode_title: json_string(first.get("episodeTitle")),
        }))
    }

    async fn fallback_match(
        &self,
        file_name: &str,
    ) -> Result<Option<DanmakuMatch>, DanmakuProviderError> {
        let Some(parsed) = parse_media_name(file_name, MediaKind::Episode) else {
            return Ok(None);
        };
        let Some(episode_number) = parsed.episode else {
            return Ok(None);
        };
        let mut url = self.api_url("search/episodes")?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("anime", &parsed.title);
            query.append_pair("episode", &episode_number.to_string());
            if let Some(season) = parsed.season {
                query.append_pair("season", &season.to_string());
            }
        }
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|_| DanmakuProviderError::Unavailable)?;
        let body = match self.read_response(response).await {
            Ok(body) => body,
            Err(DanmakuProviderError::Unsupported) => return Ok(None),
            Err(error) => return Err(error),
        };
        let value: Value =
            serde_json::from_slice(&body).map_err(|_| DanmakuProviderError::InvalidResponse)?;
        let normalized_title = normalize_title(&parsed.title);
        let Some((episode_id, anime_id, anime_title, episode_title)) =
            find_episode_match(&value, episode_number, &normalized_title, None)
        else {
            return Ok(None);
        };
        Ok(Some(DanmakuMatch {
            anime_id,
            episode_id,
            anime_title,
            episode_title,
        }))
    }

    pub async fn fetch_episode_xml(
        &self,
        episode_id: &str,
    ) -> Result<Vec<u8>, DanmakuProviderError> {
        if episode_id.trim().is_empty() || episode_id.chars().count() > 256 {
            return Err(DanmakuProviderError::InvalidRequest);
        }
        let mut url = self.api_url("comment")?;
        url.path_segments_mut()
            .map_err(|_| DanmakuProviderError::InvalidProviderUrl(ProviderUrlError::Invalid))?
            .push(episode_id);
        url.query_pairs_mut().append_pair("format", "xml");
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|_| DanmakuProviderError::Unavailable)?;
        let body = self.read_response(response).await?;
        validate_danmaku_xml(&body).map_err(DanmakuProviderError::InvalidXml)?;
        Ok(body)
    }

    fn api_url(&self, operation: &str) -> Result<Url, DanmakuProviderError> {
        let mut url = self
            .base
            .url()
            .map_err(DanmakuProviderError::InvalidProviderUrl)?;
        let base_path = url.path().trim_end_matches('/');
        let path = if base_path.ends_with("/api/v2") {
            format!("{base_path}/{operation}")
        } else {
            format!("{base_path}/api/v2/{operation}")
        };
        url.set_path(&path);
        url.set_query(None);
        Ok(url)
    }

    async fn read_response(
        &self,
        mut response: reqwest::Response,
    ) -> Result<Vec<u8>, DanmakuProviderError> {
        let status = response.status();
        if !status.is_success() {
            return Err(
                if status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED {
                    DanmakuProviderError::Unsupported
                } else {
                    DanmakuProviderError::HttpStatus(status.as_u16())
                },
            );
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_PROVIDER_RESPONSE_BYTES as u64)
        {
            return Err(DanmakuProviderError::ResponseTooLarge);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| DanmakuProviderError::Unavailable)?
        {
            if body.len().saturating_add(chunk.len()) > MAX_PROVIDER_RESPONSE_BYTES {
                return Err(DanmakuProviderError::ResponseTooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
}

fn json_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .or_else(|| value.as_i64().map(|value| value.to_string()))
        })
        .filter(|value| !value.trim().is_empty())
}

type EpisodeMatch = (String, Option<String>, Option<String>, Option<String>);

fn find_episode_match(
    value: &Value,
    expected_episode: u32,
    normalized_title: &str,
    inherited_anime_id: Option<String>,
) -> Option<EpisodeMatch> {
    match value {
        Value::Array(values) => values.iter().find_map(|value| {
            find_episode_match(
                value,
                expected_episode,
                normalized_title,
                inherited_anime_id.clone(),
            )
        }),
        Value::Object(object) => {
            let anime_id = json_string(object.get("animeId")).or(inherited_anime_id);
            let anime_title = json_string(object.get("animeTitle"));
            let title_matches = anime_title
                .as_deref()
                .map(normalize_title)
                .is_none_or(|title| {
                    normalized_title.is_empty()
                        || title.contains(normalized_title)
                        || normalized_title.contains(&title)
                });
            if title_matches
                && let Some(episode_id) = json_string(object.get("episodeId"))
                && object_episode_number(object).is_some_and(|number| number == expected_episode)
            {
                return Some((
                    episode_id,
                    anime_id,
                    anime_title,
                    json_string(object.get("episodeTitle")),
                ));
            }
            object.values().find_map(|value| {
                find_episode_match(value, expected_episode, normalized_title, anime_id.clone())
            })
        }
        _ => None,
    }
}

fn object_episode_number(object: &serde_json::Map<String, Value>) -> Option<u32> {
    object
        .get("episodeNumber")
        .or_else(|| object.get("episodeNo"))
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .or_else(|| {
            json_string(object.get("episodeTitle")).and_then(|title| {
                title
                    .split(|character: char| !character.is_ascii_digit())
                    .filter(|value| !value.is_empty())
                    .find_map(|value| value.parse::<u32>().ok())
            })
        })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DanmakuMatch {
    pub anime_id: Option<String>,
    pub episode_id: String,
    pub anime_title: Option<String>,
    pub episode_title: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DanmakuProviderError {
    InvalidProviderUrl(ProviderUrlError),
    InvalidProxy,
    InvalidRequest,
    Client,
    Unavailable,
    Unsupported,
    HttpStatus(u16),
    ResponseTooLarge,
    InvalidResponse,
    InvalidXml(DanmakuXmlError),
}

impl fmt::Display for DanmakuProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidProviderUrl(error) => error.fmt(formatter),
            Self::InvalidProxy => formatter.write_str("danmaku provider proxy is invalid"),
            Self::InvalidRequest => formatter.write_str("danmaku provider request is invalid"),
            Self::Client => formatter.write_str("danmaku provider client is unavailable"),
            Self::Unavailable => formatter.write_str("danmaku provider is unavailable"),
            Self::Unsupported => formatter.write_str("danmaku provider endpoint is unsupported"),
            Self::HttpStatus(status) => {
                write!(formatter, "danmaku provider returned HTTP {status}")
            }
            Self::ResponseTooLarge => formatter.write_str("danmaku provider response is too large"),
            Self::InvalidResponse => formatter.write_str("danmaku provider response is invalid"),
            Self::InvalidXml(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for DanmakuProviderError {}

#[derive(Clone)]
pub struct DanmakuService {
    database: Database,
    config_dir: PathBuf,
    proxy_url: Option<String>,
    resources: ResourceMetrics,
}

impl DanmakuService {
    pub fn new(database: Database, config_dir: PathBuf, proxy_url: Option<String>) -> Self {
        Self {
            database,
            config_dir,
            proxy_url,
            resources: ResourceMetrics::new(),
        }
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
        let provider_url = read_danmaku_provider_url_async(&self.config_dir)
            .await
            .ok_or(DanmakuServiceError::ProviderNotConfigured)?;
        validate_provider_base_url(&provider_url)
            .map_err(DanmakuServiceError::InvalidProviderUrl)?;
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
        let Some(source) = self
            .database
            .find_local_danmaku_source_for_item(item_id)
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
        let provider_url = match read_danmaku_provider_url_async(&self.config_dir).await {
            Some(value) => value,
            None => {
                self.database
                    .finish_danmaku_match_job(&job.id, "FAILED", Some("PROVIDER_NOT_CONFIGURED"))
                    .await?;
                return Ok(());
            }
        };
        let proxy_url = match self.proxy_url.as_deref() {
            Some(value) => Some(value.to_owned()),
            None => read_network_proxy_url_async(&self.config_dir).await,
        };
        let provider = match DanmakuProviderClient::new(&provider_url, proxy_url.as_deref()) {
            Ok(provider) => Arc::new(provider),
            Err(error) => {
                self.database
                    .finish_danmaku_match_job(&job.id, "FAILED", Some(provider_error_code(&error)))
                    .await?;
                return Ok(());
            }
        };
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
                    }),
                    _ => None,
                };
                let database = self.database.clone();
                let provider = provider.clone();
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
                    Ok(Some(
                        process_danmaku_source(source, provider, overwrite, item.id).await,
                    ))
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
    }
}

#[derive(Debug)]
pub enum DanmakuServiceError {
    InvalidConcurrency,
    LibraryNotFound,
    SourceNotFound,
    ProviderNotConfigured,
    InvalidProviderUrl(ProviderUrlError),
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
            Self::SourceNotFound => formatter.write_str("danmaku media source was not found"),
            Self::ProviderNotConfigured => {
                formatter.write_str("danmaku provider is not configured")
            }
            Self::InvalidProviderUrl(error) => error.fmt(formatter),
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

async fn process_danmaku_source(
    source: StoredDanmakuSource,
    provider: Arc<DanmakuProviderClient>,
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
            Ok(_) => {}
            Err(_) => {}
        }
    }
    let Some(file_name) = media_path.file_name().and_then(|name| name.to_str()) else {
        return WorkerResult::failed(item_id, "INVALID_SOURCE_PATH", "media filename is invalid");
    };
    let matched = match provider.match_filename(file_name).await {
        Ok(Some(value)) => value,
        Ok(None) => return WorkerResult::failed(item_id, "NO_MATCH", "provider returned no match"),
        Err(error) => {
            return WorkerResult::failed(
                item_id,
                provider_error_code(&error),
                "provider request failed",
            );
        }
    };
    let xml = match provider.fetch_episode_xml(&matched.episode_id).await {
        Ok(bytes) => bytes,
        Err(error) => {
            return WorkerResult::failed(
                item_id,
                provider_error_code(&error),
                "provider comment request failed",
            );
        }
    };
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
        provider_episode_id: Some(matched.episode_id.clone()),
        error_code: None,
        error_message: None,
        track: Some(TrackWrite {
            id: Uuid::now_v7().to_string(),
            media_source_id: source.source_id,
            relative_path,
            provider: Some(DANMAKU_PROVIDER.to_owned()),
            provider_anime_id: matched.anime_id,
            provider_episode_id: Some(matched.episode_id),
            fingerprint,
        }),
    }
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

fn provider_error_code(error: &DanmakuProviderError) -> &'static str {
    match error {
        DanmakuProviderError::InvalidProviderUrl(_) => "INVALID_PROVIDER_URL",
        DanmakuProviderError::InvalidProxy => "INVALID_PROXY",
        DanmakuProviderError::InvalidRequest => "INVALID_REQUEST",
        DanmakuProviderError::Client => "CLIENT_UNAVAILABLE",
        DanmakuProviderError::Unavailable => "PROVIDER_UNAVAILABLE",
        DanmakuProviderError::Unsupported => "PROVIDER_UNSUPPORTED",
        DanmakuProviderError::HttpStatus(_) => "PROVIDER_HTTP_ERROR",
        DanmakuProviderError::ResponseTooLarge => "PROVIDER_RESPONSE_TOO_LARGE",
        DanmakuProviderError::InvalidResponse => "PROVIDER_INVALID_RESPONSE",
        DanmakuProviderError::InvalidXml(_) => "PROVIDER_INVALID_XML",
    }
}

#[cfg(test)]
mod tests {
    use super::effective_danmaku_concurrency;

    #[test]
    fn worker_concurrency_has_a_memory_safe_ceiling() {
        assert_eq!(effective_danmaku_concurrency(1), 1);
        assert_eq!(effective_danmaku_concurrency(2), 2);
        assert_eq!(effective_danmaku_concurrency(64), 4);
    }
}
