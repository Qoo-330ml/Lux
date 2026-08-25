use std::{
    collections::HashSet,
    fmt,
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use sha2::{Digest, Sha256};
use tokio::{fs, process::Command, time::timeout};
use uuid::Uuid;

use crate::{
    application::images::{read_image_dimensions_from_bytes, write_image_atomically},
    domain::ids::LibraryId,
    storage::{Database, ItemImageMetadata, StorageError, StoredThumbnailSource},
};

const DEFAULT_FRAME: &str = "00:03:01";
const MAX_THUMBNAIL_BYTES: u64 = 50 * 1024 * 1024;
const LIBRARY_SOURCE_PAGE_SIZE: usize = 500;

#[derive(Clone)]
pub struct ThumbnailService {
    database: Database,
    ffmpeg_binary: PathBuf,
    timeout: Duration,
}

impl ThumbnailService {
    pub fn new(database: Database) -> Self {
        Self::with_runner(database, PathBuf::from("ffmpeg"), Duration::from_secs(30))
    }

    pub fn with_runner(
        database: Database,
        ffmpeg_binary: impl Into<PathBuf>,
        timeout: Duration,
    ) -> Self {
        Self {
            database,
            ffmpeg_binary: ffmpeg_binary.into(),
            timeout,
        }
    }

    pub async fn generate_library(
        &self,
        library_id: LibraryId,
    ) -> Result<ThumbnailReport, ThumbnailError> {
        let mut seen_items = HashSet::new();
        let mut report = ThumbnailReport::default();
        let library_id = library_id.to_string();
        let mut offset = 0_i64;
        loop {
            let candidates = self
                .database
                .list_local_thumbnail_sources_for_library_page(
                    &library_id,
                    LIBRARY_SOURCE_PAGE_SIZE as i64,
                    offset,
                )
                .await?;
            let last_page = candidates.len() < LIBRARY_SOURCE_PAGE_SIZE;
            self.generate_sources(candidates, &mut seen_items, &mut report)
                .await;
            if last_page {
                break;
            }
            offset = offset.saturating_add(LIBRARY_SOURCE_PAGE_SIZE as i64);
        }
        Ok(report)
    }

    pub async fn generate_incremental_scan(
        &self,
        scan_job_id: &str,
    ) -> Result<ThumbnailReport, ThumbnailError> {
        let mut seen_items = HashSet::new();
        let mut report = ThumbnailReport::default();
        let mut offset = 0_i64;
        loop {
            let candidates = self
                .database
                .list_local_thumbnail_sources_for_incremental_scan_page(
                    scan_job_id,
                    LIBRARY_SOURCE_PAGE_SIZE as i64,
                    offset,
                )
                .await?;
            let last_page = candidates.len() < LIBRARY_SOURCE_PAGE_SIZE;
            self.generate_sources(candidates, &mut seen_items, &mut report)
                .await;
            if last_page {
                break;
            }
            offset = offset.saturating_add(LIBRARY_SOURCE_PAGE_SIZE as i64);
        }
        Ok(report)
    }

    pub async fn generate_scan_job(
        &self,
        scan_job_id: &str,
    ) -> Result<ThumbnailReport, ThumbnailError> {
        let mut report = ThumbnailReport::default();
        loop {
            let candidates = self
                .database
                .list_scan_job_thumbnail_sources_page(
                    scan_job_id,
                    LIBRARY_SOURCE_PAGE_SIZE as i64,
                    0,
                )
                .await?;
            if candidates.is_empty() {
                break;
            }
            let item_ids = candidates
                .iter()
                .map(|candidate| candidate.item_id.clone())
                .collect::<Vec<_>>();
            let mut seen_items = HashSet::new();
            self.generate_sources(candidates, &mut seen_items, &mut report)
                .await;
            self.database
                .mark_scan_job_target_stage(scan_job_id, "ITEM", &item_ids, "THUMBNAIL", "DONE")
                .await?;
        }
        Ok(report)
    }

    async fn generate_sources(
        &self,
        candidates: Vec<StoredThumbnailSource>,
        seen_items: &mut HashSet<String>,
        report: &mut ThumbnailReport,
    ) {
        for candidate in candidates {
            if !seen_items.insert(candidate.item_id.clone()) {
                continue;
            }
            if is_strm_path(&candidate.relative_path) {
                report.skipped_strm += 1;
                continue;
            }
            report.considered += 1;
            match self.generate_for_source(&candidate).await {
                Ok(ThumbnailOutcome::Generated) => report.generated += 1,
                Ok(ThumbnailOutcome::Reused) => report.reused += 1,
                Err(error) => {
                    report.failed += 1;
                    tracing::warn!(
                        item_id = %candidate.item_id,
                        error = %error,
                        "thumbnail generation failed"
                    );
                }
            }
        }
    }

    async fn generate_for_source(
        &self,
        source: &StoredThumbnailSource,
    ) -> Result<ThumbnailOutcome, ThumbnailFileError> {
        let (source_path, target_path, root_path) = resolve_media_paths(source).await?;

        if let Some(existing) = source.thumbnail_path.as_deref() {
            let existing_path = PathBuf::from(existing);
            if usable_image_path(&existing_path, &root_path).await? {
                return Ok(ThumbnailOutcome::Reused);
            }
        }

        if existing_target(&target_path).await? {
            self.register_image(&source.item_id, &target_path).await?;
            return Ok(ThumbnailOutcome::Reused);
        }

        let temporary = target_path
            .parent()
            .unwrap_or(Path::new("."))
            .join(format!(".lux-{}-thumbnail.tmp.jpg", Uuid::now_v7()));
        let result = async {
            self.run_ffmpeg(&source_path, &temporary).await?;
            let metadata = fs::metadata(&temporary)
                .await
                .map_err(|error| ThumbnailFileError::io(&temporary, error))?;
            if !metadata.is_file() || metadata.len() == 0 {
                return Err(ThumbnailFileError::InvalidOutput);
            }
            if metadata.len() > MAX_THUMBNAIL_BYTES {
                return Err(ThumbnailFileError::OutputTooLarge);
            }
            let bytes = fs::read(&temporary)
                .await
                .map_err(|error| ThumbnailFileError::io(&temporary, error))?;
            if !is_jpeg(&bytes) {
                return Err(ThumbnailFileError::InvalidOutput);
            }
            write_image_atomically(&target_path, &bytes)
                .await
                .map_err(|error| ThumbnailFileError::Write(error.to_string()))?;
            self.register_image(&source.item_id, &target_path).await
        }
        .await;
        let _ = fs::remove_file(&temporary).await;
        result.map(|()| ThumbnailOutcome::Generated)
    }

    async fn run_ffmpeg(
        &self,
        source_path: &Path,
        output_path: &Path,
    ) -> Result<(), ThumbnailFileError> {
        let mut child = Command::new(&self.ffmpeg_binary)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-y",
                "-ss",
                DEFAULT_FRAME,
                "-i",
            ])
            .arg(source_path)
            .args(["-frames:v", "1", "-an", "-f", "image2"])
            .arg(output_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(ThumbnailFileError::ProcessIo)?;
        let status = match timeout(self.timeout, child.wait()).await {
            Ok(result) => result.map_err(ThumbnailFileError::ProcessIo)?,
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(ThumbnailFileError::Timeout);
            }
        };
        if status.success() {
            Ok(())
        } else {
            Err(ThumbnailFileError::Exit(status.code()))
        }
    }

    async fn register_image(&self, item_id: &str, path: &Path) -> Result<(), ThumbnailFileError> {
        let bytes = fs::read(path)
            .await
            .map_err(|error| ThumbnailFileError::io(path, error))?;
        let file_size =
            i64::try_from(bytes.len()).map_err(|_| ThumbnailFileError::OutputTooLarge)?;
        let content_tag = Sha256::digest(&bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let dimensions = read_image_dimensions_from_bytes(&bytes).await;
        self.database
            .upsert_item_image(
                item_id,
                "THUMB",
                path,
                ItemImageMetadata {
                    file_size,
                    width: dimensions.map(|(width, _)| width),
                    height: dimensions.map(|(_, height)| height),
                    content_tag: &content_tag,
                    source: "FFMPEG",
                },
            )
            .await
            .map(|_| ())
            .map_err(ThumbnailFileError::Storage)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ThumbnailReport {
    pub considered: usize,
    pub generated: usize,
    pub reused: usize,
    pub failed: usize,
    pub skipped_strm: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ThumbnailOutcome {
    Generated,
    Reused,
}

async fn resolve_media_paths(
    source: &StoredThumbnailSource,
) -> Result<(PathBuf, PathBuf, PathBuf), ThumbnailFileError> {
    let root_path = fs::canonicalize(&source.root_path)
        .await
        .map_err(|error| ThumbnailFileError::io(Path::new(&source.root_path), error))?;
    let relative_path = Path::new(&source.relative_path);
    if !safe_relative_path(relative_path) {
        return Err(ThumbnailFileError::InvalidRelativePath);
    }
    let source_path = fs::canonicalize(root_path.join(relative_path))
        .await
        .map_err(|error| ThumbnailFileError::io(&root_path, error))?;
    if !source_path.starts_with(&root_path) {
        return Err(ThumbnailFileError::OutsideRoot);
    }
    let metadata = fs::metadata(&source_path)
        .await
        .map_err(|error| ThumbnailFileError::io(&source_path, error))?;
    if !metadata.is_file() {
        return Err(ThumbnailFileError::SourceNotFile);
    }
    let parent = source_path
        .parent()
        .ok_or(ThumbnailFileError::InvalidSourcePath)?;
    if !parent.starts_with(&root_path) {
        return Err(ThumbnailFileError::OutsideRoot);
    }
    let stem = source_path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .ok_or(ThumbnailFileError::InvalidSourcePath)?;
    let target_path = parent.join(format!("{stem}-thumb.jpg"));
    Ok((source_path, target_path, root_path))
}

fn safe_relative_path(path: &Path) -> bool {
    !path.is_absolute()
        && path.components().all(|component| {
            !matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
}

async fn usable_image_path(path: &Path, root_path: &Path) -> Result<bool, ThumbnailFileError> {
    let metadata = match fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(ThumbnailFileError::io(path, error)),
    };
    if metadata.file_type().is_symlink() {
        return Err(ThumbnailFileError::SymlinkTarget);
    }
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_THUMBNAIL_BYTES {
        return Ok(false);
    }
    let canonical = fs::canonicalize(path)
        .await
        .map_err(|error| ThumbnailFileError::io(path, error))?;
    Ok(canonical.starts_with(root_path))
}

async fn existing_target(path: &Path) -> Result<bool, ThumbnailFileError> {
    let metadata = match fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(ThumbnailFileError::io(path, error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ThumbnailFileError::TargetUnavailable);
    }
    if metadata.len() == 0 || metadata.len() > MAX_THUMBNAIL_BYTES {
        return Err(ThumbnailFileError::TargetUnavailable);
    }
    let bytes = fs::read(path)
        .await
        .map_err(|error| ThumbnailFileError::io(path, error))?;
    if !is_jpeg(&bytes) {
        return Err(ThumbnailFileError::TargetUnavailable);
    }
    Ok(true)
}

fn is_jpeg(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && bytes.starts_with(&[0xff, 0xd8]) && bytes.ends_with(&[0xff, 0xd9])
}

fn is_strm_path(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("strm"))
}

#[derive(Debug)]
enum ThumbnailFileError {
    Exit(Option<i32>),
    InvalidOutput,
    InvalidRelativePath,
    InvalidSourcePath,
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    OutsideRoot,
    OutputTooLarge,
    ProcessIo(std::io::Error),
    Storage(StorageError),
    SymlinkTarget,
    TargetUnavailable,
    Timeout,
    Write(String),
    SourceNotFile,
}

impl ThumbnailFileError {
    fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.to_owned(),
            source,
        }
    }
}

impl fmt::Display for ThumbnailFileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exit(code) => write!(formatter, "ffmpeg exited with code {code:?}"),
            Self::InvalidOutput => formatter.write_str("ffmpeg did not produce a JPEG image"),
            Self::InvalidRelativePath => formatter.write_str("media relative path is invalid"),
            Self::InvalidSourcePath => formatter.write_str("media source path is invalid"),
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::OutsideRoot => formatter.write_str("media path is outside its library root"),
            Self::OutputTooLarge => formatter.write_str("thumbnail output is too large"),
            Self::ProcessIo(error) => write!(formatter, "ffmpeg process: {error}"),
            Self::Storage(error) => write!(formatter, "thumbnail storage: {error}"),
            Self::SymlinkTarget => formatter.write_str("thumbnail path is a symlink"),
            Self::TargetUnavailable => {
                formatter.write_str("thumbnail target is not a regular file")
            }
            Self::Timeout => formatter.write_str("ffmpeg timed out"),
            Self::Write(error) => write!(formatter, "thumbnail write: {error}"),
            Self::SourceNotFile => formatter.write_str("media source is not a regular file"),
        }
    }
}

#[derive(Debug)]
pub enum ThumbnailError {
    Storage(StorageError),
}

impl fmt::Display for ThumbnailError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ThumbnailError {}

impl From<StorageError> for ThumbnailError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}
