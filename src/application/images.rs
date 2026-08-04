use std::{
    fmt,
    path::{Path, PathBuf},
    time::{Duration, UNIX_EPOCH},
};

use reqwest::{Client, Url, header::CONTENT_TYPE};
use sha2::{Digest, Sha256};
use tokio::{
    fs::{self, OpenOptions},
    io::AsyncWriteExt,
};
use uuid::Uuid;

use crate::{
    application::access::{AccessError, AccessPrincipal, MediaAccessService},
    application::tmdb::{TmdbClient, TmdbError, TmdbImageReference, TmdbImagesResponse},
    storage::{Database, StorageError},
};

const MAX_IMAGE_BYTES: u64 = 50 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct ImageDownloadConfig {
    pub timeout: Duration,
    pub max_bytes: u64,
}

impl Default for ImageDownloadConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
            max_bytes: MAX_IMAGE_BYTES,
        }
    }
}

#[derive(Clone)]
pub struct ImageWriteService {
    database: Database,
    http: Client,
    max_bytes: u64,
}

impl ImageWriteService {
    pub fn new(database: Database) -> Result<Self, ImageWriteError> {
        Self::with_config(database, ImageDownloadConfig::default())
    }

    pub fn with_config(
        database: Database,
        config: ImageDownloadConfig,
    ) -> Result<Self, ImageWriteError> {
        if config.max_bytes == 0 {
            return Err(ImageWriteError::InvalidConfiguration(
                "image maximum size must be positive".to_owned(),
            ));
        }
        let http = Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|error| ImageWriteError::ClientBuild(error.to_string()))?;
        Ok(Self {
            database,
            http,
            max_bytes: config.max_bytes,
        })
    }

    pub async fn download_item_image_if_missing(
        &self,
        item_id: &str,
        image_type: &str,
        image_url: &str,
    ) -> Result<Option<ImageWriteReport>, ImageWriteError> {
        if self.local_image_exists(item_id, image_type).await? {
            return Ok(None);
        }
        self.download_item_image(item_id, image_type, image_url)
            .await
            .map(Some)
    }

    pub(crate) async fn list_item_images(
        &self,
        item_id: &str,
    ) -> Result<Vec<crate::storage::StoredItemImage>, ImageWriteError> {
        Ok(self.database.list_item_images(item_id).await?)
    }

    pub(crate) async fn delete_item_image(
        &self,
        item_id: &str,
        image_id: &str,
    ) -> Result<(), ImageWriteError> {
        let image = self
            .database
            .find_item_image(item_id, image_id)
            .await?
            .ok_or(ImageWriteError::ItemNotFound)?;
        let Some(root_path) = image.root_path.as_deref() else {
            return Err(ImageWriteError::PathOutsideRoot(PathBuf::from(
                &image.local_path,
            )));
        };
        let canonical_root =
            fs::canonicalize(root_path)
                .await
                .map_err(|source| ImageWriteError::Io {
                    path: PathBuf::from(root_path),
                    source,
                })?;
        let path = PathBuf::from(&image.local_path);
        if let Ok(metadata) = fs::symlink_metadata(&path).await {
            if metadata.file_type().is_symlink() {
                return Err(ImageWriteError::SymlinkTarget(path));
            }
            let canonical_path =
                fs::canonicalize(&path)
                    .await
                    .map_err(|source| ImageWriteError::Io {
                        path: path.clone(),
                        source,
                    })?;
            if !canonical_path.starts_with(&canonical_root) || canonical_path == canonical_root {
                return Err(ImageWriteError::PathOutsideRoot(canonical_path));
            }
            fs::remove_file(&canonical_path)
                .await
                .map_err(|source| ImageWriteError::Io {
                    path: canonical_path,
                    source,
                })?;
        }
        if !self.database.delete_item_image(item_id, image_id).await? {
            return Err(ImageWriteError::ItemNotFound);
        }
        Ok(())
    }

    async fn local_image_exists(
        &self,
        item_id: &str,
        image_type: &str,
    ) -> Result<bool, ImageWriteError> {
        let image_type = normalize_image_type(image_type)
            .ok_or_else(|| ImageWriteError::InvalidImageType(image_type.to_owned()))?;
        let directory = self.media_directory(item_id).await?;
        let Some(path) = find_any_image_path(&directory, image_type).await? else {
            return Ok(false);
        };
        image_file_stamp(&path).await.map(|_| true)
    }

    pub async fn download_item_image(
        &self,
        item_id: &str,
        image_type: &str,
        image_url: &str,
    ) -> Result<ImageWriteReport, ImageWriteError> {
        let image_type = normalize_image_type(image_type)
            .ok_or_else(|| ImageWriteError::InvalidImageType(image_type.to_owned()))?;
        let url = Url::parse(image_url)
            .map_err(|error| ImageWriteError::InvalidUrl(error.to_string()))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(ImageWriteError::InvalidUrl(
                "image URL must be an http(s) URL without credentials".to_owned(),
            ));
        }

        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|error| ImageWriteError::Download(error.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(ImageWriteError::UpstreamStatus {
                status: status.as_u16(),
            });
        }
        if response
            .content_length()
            .is_some_and(|size| size > self.max_bytes)
        {
            return Err(ImageWriteError::TooLarge {
                size: response.content_length().unwrap_or_default(),
                max: self.max_bytes,
            });
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| ImageWriteError::UnsupportedContentType {
                content_type: "missing".to_owned(),
            })?;
        let format = ImageFormat::from_content_type(content_type).ok_or_else(|| {
            ImageWriteError::UnsupportedContentType {
                content_type: content_type.to_owned(),
            }
        })?;
        let body = response
            .bytes()
            .await
            .map_err(|error| ImageWriteError::Download(error.to_string()))?;
        let size = u64::try_from(body.len()).unwrap_or(u64::MAX);
        if size > self.max_bytes {
            return Err(ImageWriteError::TooLarge {
                size,
                max: self.max_bytes,
            });
        }
        validate_image_payload(format, &body)?;

        let directory = self.media_directory(item_id).await?;
        let root = self
            .database
            .find_media_source_path(item_id)
            .await?
            .ok_or(ImageWriteError::ItemNotFound)?;
        let root = fs::canonicalize(&root.root_path)
            .await
            .map_err(|error| image_io_error(Path::new(&root.root_path), error))?;
        let target = image_target(&directory, image_type, format).await?;
        if !target.starts_with(&root) {
            return Err(ImageWriteError::PathOutsideRoot(target));
        }
        write_image_atomically(&target, &body).await?;

        let file_size = i64::try_from(body.len()).map_err(|_| ImageWriteError::TooLarge {
            size,
            max: i64::MAX as u64,
        })?;
        let content_tag = content_tag(&body);
        let id = self
            .database
            .upsert_item_image(
                item_id,
                image_type,
                &target,
                file_size,
                &content_tag,
                "TMDB",
            )
            .await?;
        Ok(ImageWriteReport {
            id,
            image_type: image_type.to_owned(),
            path: target,
            content_type: format.content_type(),
            file_size,
            content_tag,
        })
    }

    pub async fn download_item_image_from_tmdb_candidate(
        &self,
        item_id: &str,
        image_type: &str,
        image_url: &str,
    ) -> Result<ImageWriteReport, ImageWriteError> {
        if !is_allowed_tmdb_image_url(image_url) {
            return Err(ImageWriteError::InvalidUrl(
                "selected image URL must be an HTTPS TMDb image path".to_owned(),
            ));
        }
        self.download_item_image(item_id, image_type, image_url)
            .await
    }

    async fn media_directory(&self, item_id: &str) -> Result<PathBuf, ImageWriteError> {
        let source = self
            .database
            .find_media_source_path(item_id)
            .await?
            .ok_or(ImageWriteError::ItemNotFound)?;
        let root = fs::canonicalize(&source.root_path)
            .await
            .map_err(|error| image_io_error(Path::new(&source.root_path), error))?;
        let media_path = root.join(&source.relative_path);
        let media_path = fs::canonicalize(&media_path)
            .await
            .map_err(|error| image_io_error(&media_path, error))?;
        if !media_path.starts_with(&root) {
            return Err(ImageWriteError::PathOutsideRoot(media_path));
        }
        let directory = media_path
            .parent()
            .ok_or_else(|| ImageWriteError::PathOutsideRoot(media_path.clone()))?;
        let directory = fs::canonicalize(directory)
            .await
            .map_err(|error| image_io_error(directory, error))?;
        if !directory.starts_with(&root) {
            return Err(ImageWriteError::PathOutsideRoot(directory));
        }
        Ok(directory)
    }
}

#[derive(Clone)]
pub struct ImageCandidateService {
    database: Database,
    tmdb: TmdbClient,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImageCandidate {
    pub id: String,
    pub image_type: String,
    pub image_index: i64,
    pub language: Option<String>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub source: String,
    pub url: String,
}

impl ImageCandidateService {
    pub fn new(database: Database, tmdb: TmdbClient) -> Self {
        Self { database, tmdb }
    }

    pub async fn search(
        &self,
        item_id: &str,
        image_type: &str,
        language: Option<&str>,
        source: Option<&str>,
    ) -> Result<Vec<ImageCandidate>, ImageCandidateError> {
        let image_type = normalize_image_type(image_type)
            .ok_or_else(|| ImageCandidateError::InvalidImageType(image_type.to_owned()))?;
        let source = source.unwrap_or("TMDB").trim();
        if !source.is_empty() && !source.eq_ignore_ascii_case("TMDB") {
            return Err(ImageCandidateError::InvalidSource);
        }
        let language = language.unwrap_or_default().trim();
        if language.len() > 32 {
            return Err(ImageCandidateError::InvalidLanguage);
        }
        let identity = self
            .database
            .find_media_item_image_identity(item_id)
            .await?
            .ok_or(ImageCandidateError::ItemNotFound)?;
        let provider_id = identity
            .provider_id
            .as_deref()
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|value| *value > 0)
            .ok_or(ImageCandidateError::ItemNotIdentified)?;
        let request_language = if language.is_empty() {
            "en-US"
        } else {
            language
        };
        let images = match identity.item_type.as_str() {
            "MOVIE" => {
                self.tmdb
                    .movie_images(provider_id, request_language)
                    .await?
            }
            "SERIES" => self.tmdb.tv_images(provider_id, request_language).await?,
            "SEASON" => {
                self.tmdb
                    .season_images(
                        provider_id,
                        i32::try_from(identity.season_number.unwrap_or_default())
                            .map_err(|_| ImageCandidateError::InvalidItem)?,
                        request_language,
                    )
                    .await?
            }
            "EPISODE" => {
                self.tmdb
                    .episode_images(
                        provider_id,
                        i32::try_from(identity.season_number.unwrap_or_default())
                            .map_err(|_| ImageCandidateError::InvalidItem)?,
                        i32::try_from(identity.episode_number.unwrap_or_default())
                            .map_err(|_| ImageCandidateError::InvalidItem)?,
                        request_language,
                    )
                    .await?
            }
            _ => return Ok(Vec::new()),
        };
        let references = references_for_type(&images, image_type);
        let requested_language = language.split('-').next().filter(|value| !value.is_empty());
        Ok(references
            .into_iter()
            .enumerate()
            .filter(|(_, image)| {
                requested_language.is_none()
                    || image
                        .iso_639_1
                        .as_deref()
                        .is_some_and(|value| Some(value) == requested_language)
            })
            .filter_map(|(index, image)| {
                let path = image.file_path.as_deref()?.trim();
                (!path.is_empty()).then(|| ImageCandidate {
                    id: format!("tmdb-{image_type}-{index}-{path}"),
                    image_type: image_type.to_owned(),
                    image_index: i64::try_from(index).unwrap_or_default(),
                    language: image.iso_639_1.clone(),
                    width: image.width,
                    height: image.height,
                    source: "TMDB".to_owned(),
                    url: format!("https://image.tmdb.org/t/p/w780{path}"),
                })
            })
            .take(50)
            .collect())
    }
}

fn references_for_type(images: &TmdbImagesResponse, image_type: &str) -> Vec<TmdbImageReference> {
    match image_type {
        "POSTER" | "DISC" => images.posters.clone(),
        "LOGO" => images.logos.clone(),
        _ => images.backdrops.clone(),
    }
}

#[derive(Debug)]
pub enum ImageCandidateError {
    ItemNotFound,
    ItemNotIdentified,
    InvalidItem,
    InvalidImageType(String),
    InvalidLanguage,
    InvalidSource,
    Tmdb(TmdbError),
    Storage(StorageError),
}

impl fmt::Display for ImageCandidateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ItemNotFound => formatter.write_str("media item not found"),
            Self::ItemNotIdentified => formatter.write_str("media item has no provider identity"),
            Self::InvalidItem => formatter.write_str("media item image identity is invalid"),
            Self::InvalidImageType(_) => formatter.write_str("unsupported image type"),
            Self::InvalidLanguage => formatter.write_str("image language is invalid"),
            Self::InvalidSource => formatter.write_str("unsupported image source"),
            Self::Tmdb(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ImageCandidateError {}

impl From<TmdbError> for ImageCandidateError {
    fn from(error: TmdbError) -> Self {
        Self::Tmdb(error)
    }
}

impl From<StorageError> for ImageCandidateError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImageWriteReport {
    pub id: String,
    pub image_type: String,
    pub path: PathBuf,
    pub content_type: &'static str,
    pub file_size: i64,
    pub content_tag: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ImageFormat {
    Jpeg,
    Png,
    Webp,
}

impl ImageFormat {
    fn from_content_type(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "image/jpeg" | "image/jpg" => Some(Self::Jpeg),
            "image/png" => Some(Self::Png),
            "image/webp" => Some(Self::Webp),
            _ => None,
        }
    }

    const fn content_type(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Webp => "image/webp",
        }
    }

    const fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            Self::Png => "png",
            Self::Webp => "webp",
        }
    }

    fn matches_path(self, path: &Path) -> bool {
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| match self {
                Self::Jpeg => matches!(extension.to_ascii_lowercase().as_str(), "jpg" | "jpeg"),
                Self::Png => extension.eq_ignore_ascii_case("png"),
                Self::Webp => extension.eq_ignore_ascii_case("webp"),
            })
    }
}

fn validate_image_payload(format: ImageFormat, body: &[u8]) -> Result<(), ImageWriteError> {
    let valid = match format {
        ImageFormat::Jpeg => {
            body.len() >= 4
                && body.starts_with(&[0xff, 0xd8, 0xff])
                && body.ends_with(&[0xff, 0xd9])
        }
        ImageFormat::Png => {
            body.len() >= 24
                && body.starts_with(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])
                && body.windows(4).any(|chunk| chunk == b"IEND")
        }
        ImageFormat::Webp => {
            body.len() >= 12 && body.starts_with(b"RIFF") && &body[8..12] == b"WEBP"
        }
    };
    if valid {
        Ok(())
    } else {
        Err(ImageWriteError::InvalidContent {
            content_type: format.content_type(),
        })
    }
}

async fn image_target(
    directory: &Path,
    image_type: &str,
    format: ImageFormat,
) -> Result<PathBuf, ImageWriteError> {
    let stem = match image_type {
        "POSTER" => "poster",
        "FANART" => "fanart",
        "LOGO" => "logo",
        "THUMB" => "thumb",
        "BANNER" => "banner",
        "DISC" => "disc",
        "ART" => "art",
        "WALLPAPER" => "wallpaper",
        _ => return Err(ImageWriteError::InvalidImageType(image_type.to_owned())),
    };
    let mut candidates = Vec::new();
    let mut entries = fs::read_dir(directory)
        .await
        .map_err(|source| image_io_error(directory, source))?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|source| image_io_error(directory, source))?
    {
        let path = entry.path();
        let matches_stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case(stem));
        let known_extension = path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| {
                matches!(
                    value.to_ascii_lowercase().as_str(),
                    "jpg" | "jpeg" | "png" | "webp"
                )
            });
        if matches_stem && known_extension {
            candidates.push(path);
        }
    }
    candidates.sort_by(|left, right| left.to_string_lossy().cmp(&right.to_string_lossy()));
    if let Some(existing) = candidates
        .into_iter()
        .find(|path| format.matches_path(path))
    {
        return Ok(existing);
    }
    Ok(directory.join(format!("{stem}.{}", format.extension())))
}

async fn find_any_image_path(
    directory: &Path,
    image_type: &str,
) -> Result<Option<PathBuf>, ImageWriteError> {
    let stem = match image_type {
        "POSTER" => "poster",
        "FANART" => "fanart",
        "LOGO" => "logo",
        "THUMB" => "thumb",
        "BANNER" => "banner",
        "DISC" => "disc",
        "ART" => "art",
        "WALLPAPER" => "wallpaper",
        _ => return Err(ImageWriteError::InvalidImageType(image_type.to_owned())),
    };
    let mut candidates = Vec::new();
    let mut entries = fs::read_dir(directory)
        .await
        .map_err(|source| image_io_error(directory, source))?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|source| image_io_error(directory, source))?
    {
        let path = entry.path();
        let matches_stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case(stem));
        let known_extension = path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| {
                matches!(
                    value.to_ascii_lowercase().as_str(),
                    "jpg" | "jpeg" | "png" | "webp"
                )
            });
        if matches_stem && known_extension {
            candidates.push(path);
        }
    }
    candidates.sort_by(|left, right| left.to_string_lossy().cmp(&right.to_string_lossy()));
    Ok(candidates.into_iter().next())
}

pub async fn write_image_atomically(target: &Path, bytes: &[u8]) -> Result<(), ImageWriteError> {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let before = image_file_stamp(target).await?;
    let original = match fs::read(target).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(source) => return Err(image_io_error(target, source)),
    };
    let temporary = parent.join(format!(".lux-{}.image.tmp", Uuid::now_v7()));
    let result = async {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .await
            .map_err(|source| image_io_error(&temporary, source))?;
        file.write_all(bytes)
            .await
            .map_err(|source| image_io_error(&temporary, source))?;
        file.sync_all()
            .await
            .map_err(|source| image_io_error(&temporary, source))?;
        drop(file);
        let current_stamp = image_file_stamp(target).await?;
        let current = match fs::read(target).await {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(source) => return Err(image_io_error(target, source)),
        };
        let unchanged = match (&before, current.as_ref()) {
            (None, None) => true,
            (Some(before), Some(current)) => current == &original && current_stamp == Some(*before),
            _ => false,
        };
        if !unchanged {
            return Err(ImageWriteError::ConcurrentModification(target.to_owned()));
        }
        fs::rename(&temporary, target)
            .await
            .map_err(|source| image_io_error(target, source))?;
        let directory = fs::File::open(parent)
            .await
            .map_err(|source| image_io_error(parent, source))?;
        directory
            .sync_all()
            .await
            .map_err(|source| image_io_error(parent, source))?;
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_file(&temporary).await;
    }
    result
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ImageFileStamp {
    size: u64,
    modified: Option<(u64, u32)>,
}

async fn image_file_stamp(path: &Path) -> Result<Option<ImageFileStamp>, ImageWriteError> {
    let metadata = match fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(image_io_error(path, source)),
    };
    if metadata.file_type().is_symlink() {
        return Err(ImageWriteError::SymlinkTarget(path.to_owned()));
    }
    let modified = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| (value.as_secs(), value.subsec_nanos()));
    Ok(Some(ImageFileStamp {
        size: metadata.len(),
        modified,
    }))
}

fn content_tag(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn image_io_error(path: &Path, source: std::io::Error) -> ImageWriteError {
    ImageWriteError::Io {
        path: path.to_owned(),
        source,
    }
}

#[derive(Clone)]
pub struct ImageService {
    database: Database,
    access: MediaAccessService,
}

impl ImageService {
    pub fn new(database: Database, access: MediaAccessService) -> Self {
        Self { database, access }
    }

    pub async fn resolve(
        &self,
        principal: AccessPrincipal,
        item_id: &str,
        image_type: &str,
        image_index: i64,
    ) -> Result<Option<ResolvedImage>, ImageError> {
        if !self.access.can_view_item(principal, item_id).await? {
            return Ok(None);
        }
        let candidates = self
            .database
            .list_item_image_candidates(item_id, image_type, image_index)
            .await?;
        if candidates.is_empty() {
            return Ok(None);
        }

        let mut saw_outside_root = false;
        for candidate in candidates {
            let path = PathBuf::from(&candidate.local_path);
            let Ok(canonical_path) = fs::canonicalize(&path).await else {
                continue;
            };
            let Ok(canonical_root) = fs::canonicalize(&candidate.root_path).await else {
                continue;
            };
            if !canonical_path.starts_with(&canonical_root) || canonical_path == canonical_root {
                saw_outside_root = true;
                continue;
            }
            let metadata =
                fs::metadata(&canonical_path)
                    .await
                    .map_err(|source| ImageError::Io {
                        path: canonical_path.clone(),
                        source,
                    })?;
            if !metadata.is_file() {
                return Ok(None);
            }
            if metadata.len() > MAX_IMAGE_BYTES {
                return Err(ImageError::TooLarge {
                    path: canonical_path,
                    size: metadata.len(),
                });
            }
            let Some(content_type) = content_type(&canonical_path) else {
                return Ok(None);
            };
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok());
            let etag = format!(
                "\"{}-{}-{}\"",
                metadata.len(),
                modified.map(|value| value.as_secs()).unwrap_or_default(),
                modified
                    .map(|value| value.subsec_nanos())
                    .unwrap_or_default()
            );
            return Ok(Some(ResolvedImage {
                id: candidate.id,
                path: canonical_path,
                content_type,
                content_length: metadata.len(),
                etag,
            }));
        }

        if saw_outside_root {
            return Err(ImageError::Forbidden);
        }
        Ok(None)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedImage {
    pub id: String,
    pub path: PathBuf,
    pub content_type: &'static str,
    pub content_length: u64,
    pub etag: String,
}

pub fn normalize_image_type(value: &str) -> Option<&'static str> {
    match value.to_ascii_lowercase().as_str() {
        "poster" | "primary" => Some("POSTER"),
        "fanart" | "fan-art" | "backdrop" => Some("FANART"),
        "logo" | "clearlogo" => Some("LOGO"),
        "thumb" | "thumbnail" => Some("THUMB"),
        "banner" => Some("BANNER"),
        "disc" | "discart" => Some("DISC"),
        "art" | "artwork" => Some("ART"),
        "wallpaper" => Some("WALLPAPER"),
        _ => None,
    }
}

fn content_type(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => Some("image/jpeg"),
        "png" => Some("image/png"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

#[derive(Debug)]
pub enum ImageError {
    Forbidden,
    TooLarge {
        path: PathBuf,
        size: u64,
    },
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Storage(StorageError),
}

impl fmt::Display for ImageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Forbidden => formatter.write_str("image path is outside the media root"),
            Self::TooLarge { path, size } => {
                write!(
                    formatter,
                    "image '{}' is too large: {size} bytes",
                    path.display()
                )
            }
            Self::Io { path, source } => {
                write!(formatter, "image path '{}': {source}", path.display())
            }
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ImageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Storage(error) => Some(error),
            Self::Forbidden | Self::TooLarge { .. } => None,
        }
    }
}

impl From<StorageError> for ImageError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<AccessError> for ImageError {
    fn from(error: AccessError) -> Self {
        match error {
            AccessError::Storage(error) => Self::Storage(error),
        }
    }
}

#[derive(Debug)]
pub enum ImageWriteError {
    InvalidConfiguration(String),
    InvalidImageType(String),
    InvalidUrl(String),
    ClientBuild(String),
    Download(String),
    UpstreamStatus {
        status: u16,
    },
    UnsupportedContentType {
        content_type: String,
    },
    InvalidContent {
        content_type: &'static str,
    },
    TooLarge {
        size: u64,
        max: u64,
    },
    ItemNotFound,
    PathOutsideRoot(PathBuf),
    SymlinkTarget(PathBuf),
    ConcurrentModification(PathBuf),
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Storage(StorageError),
}

impl fmt::Display for ImageWriteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfiguration(message) => formatter.write_str(message),
            Self::InvalidImageType(value) => write!(formatter, "unsupported image type: {value}"),
            Self::InvalidUrl(message) => write!(formatter, "invalid image URL: {message}"),
            Self::ClientBuild(message) => write!(formatter, "image HTTP client: {message}"),
            Self::Download(message) => write!(formatter, "image download failed: {message}"),
            Self::UpstreamStatus { status } => {
                write!(formatter, "image service returned HTTP {status}")
            }
            Self::UnsupportedContentType { content_type } => {
                write!(formatter, "unsupported image content type: {content_type}")
            }
            Self::InvalidContent { content_type } => {
                write!(formatter, "image content is invalid for {content_type}")
            }
            Self::TooLarge { size, max } => {
                write!(
                    formatter,
                    "image is too large: {size} bytes, maximum is {max}"
                )
            }
            Self::ItemNotFound => formatter.write_str("media item has no local source"),
            Self::PathOutsideRoot(path) => {
                write!(
                    formatter,
                    "image path '{}' is outside the media root",
                    path.display()
                )
            }
            Self::SymlinkTarget(path) => {
                write!(
                    formatter,
                    "refusing to replace image symlink '{}'",
                    path.display()
                )
            }
            Self::ConcurrentModification(path) => {
                write!(
                    formatter,
                    "image changed while writing '{}'",
                    path.display()
                )
            }
            Self::Io { path, source } => {
                write!(formatter, "image path '{}': {source}", path.display())
            }
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ImageWriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Storage(error) => Some(error),
            Self::InvalidConfiguration(_)
            | Self::InvalidImageType(_)
            | Self::InvalidUrl(_)
            | Self::ClientBuild(_)
            | Self::Download(_)
            | Self::UpstreamStatus { .. }
            | Self::UnsupportedContentType { .. }
            | Self::InvalidContent { .. }
            | Self::TooLarge { .. }
            | Self::ItemNotFound
            | Self::PathOutsideRoot(_)
            | Self::SymlinkTarget(_)
            | Self::ConcurrentModification(_) => None,
        }
    }
}

impl From<StorageError> for ImageWriteError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

fn is_allowed_tmdb_image_url(value: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    url.scheme().eq_ignore_ascii_case("https")
        && url
            .host_str()
            .is_some_and(|host| host.eq_ignore_ascii_case("image.tmdb.org"))
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path().starts_with("/t/p/")
        && url.query().is_none()
        && url.fragment().is_none()
}

#[cfg(test)]
mod tests {
    use super::is_allowed_tmdb_image_url;

    #[test]
    fn selected_image_urls_are_limited_to_tmdb_image_paths() {
        assert!(is_allowed_tmdb_image_url(
            "https://image.tmdb.org/t/p/w780/poster.jpg"
        ));
        assert!(!is_allowed_tmdb_image_url(
            "http://image.tmdb.org/t/p/w780/poster.jpg"
        ));
        assert!(!is_allowed_tmdb_image_url(
            "https://image.tmdb.org/t/p/w780/poster.jpg?redirect=http://127.0.0.1"
        ));
        assert!(!is_allowed_tmdb_image_url(
            "https://example.com/t/p/w780/poster.jpg"
        ));
    }
}
