use std::{
    collections::{BTreeSet, HashMap},
    fmt,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use reqwest::{Client, Url, header::CONTENT_TYPE};
use sha2::{Digest, Sha256};
use tokio::{
    fs::{self, OpenOptions},
    io::AsyncWriteExt,
    sync::Semaphore,
    time::sleep,
};
use uuid::Uuid;

use crate::{
    application::access::{AccessError, AccessPrincipal, MediaAccessService},
    application::{
        metadata::series_directory,
        metadata_paths::{library_item_directory, metadata_root},
        metadata_writeback::item_metadata_writeback_enabled,
        remote_body::{LimitedBodyError, read_response_body_limited},
        scraper::{
            ScraperError, ScraperImage, ScraperImageRequest, ScraperItemType, ScraperProvider,
            ScraperResolver,
        },
    },
    network::client_builder_from_env_or,
    observability::resources::ResourceMetrics,
    storage::{
        Database, ItemImageMetadata, MetadataImageAttemptUpdate, StorageError, StoredItemImage,
    },
};

const MAX_IMAGE_BYTES: u64 = 50 * 1024 * 1024;
const IMAGE_DOWNLOAD_MAX_RETRIES: u32 = 2;
const IMAGE_RETRY_BASE_DELAY: Duration = Duration::from_millis(250);
const IMAGE_RETRY_MAX_DELAY: Duration = Duration::from_secs(4);
const IMAGE_ATTEMPT_LEASE: Duration = Duration::from_secs(5 * 60);
const IMAGE_RETRY_BASE_SECONDS: i64 = 60;
const IMAGE_RETRY_MAX_SECONDS: i64 = 6 * 60 * 60;
const IMAGE_GLOBAL_CONCURRENCY: usize = 16;
pub(crate) const MAX_IMAGE_VARIANTS: usize = 4;

static IMAGE_GLOBAL_DOWNLOAD_PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();
static IMAGE_GLOBAL_WRITE_PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();

fn global_image_download_permits() -> Arc<Semaphore> {
    IMAGE_GLOBAL_DOWNLOAD_PERMITS
        .get_or_init(|| Arc::new(Semaphore::new(IMAGE_GLOBAL_CONCURRENCY)))
        .clone()
}

fn global_image_write_permits() -> Arc<Semaphore> {
    IMAGE_GLOBAL_WRITE_PERMITS
        .get_or_init(|| Arc::new(Semaphore::new(IMAGE_GLOBAL_CONCURRENCY)))
        .clone()
}

pub(crate) async fn read_image_dimensions(path: &Path) -> Option<(i32, i32)> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        image::image_dimensions(path)
            .ok()
            .and_then(|(width, height)| {
                Some((i32::try_from(width).ok()?, i32::try_from(height).ok()?))
            })
    })
    .await
    .ok()
    .flatten()
}

pub(crate) async fn read_image_dimensions_from_bytes(bytes: &[u8]) -> Option<(i32, i32)> {
    let bytes = bytes.to_owned();
    tokio::task::spawn_blocking(move || {
        image::load_from_memory(&bytes).ok().and_then(|image| {
            Some((
                i32::try_from(image.width()).ok()?,
                i32::try_from(image.height()).ok()?,
            ))
        })
    })
    .await
    .ok()
    .flatten()
}

pub(crate) async fn image_content_tag_and_dimensions_from_bytes(
    bytes: Vec<u8>,
) -> Result<(String, Option<(i32, i32)>), std::io::Error> {
    tokio::task::spawn_blocking(move || {
        let content_tag = format!("{:x}", Sha256::digest(&bytes));
        let dimensions = image::load_from_memory(&bytes).ok().and_then(|image| {
            Some((
                i32::try_from(image.width()).ok()?,
                i32::try_from(image.height()).ok()?,
            ))
        });
        Ok((content_tag, dimensions))
    })
    .await
    .map_err(|error| std::io::Error::other(format!("image metadata worker failed: {error}")))?
}

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
    config_dir: Option<PathBuf>,
    download_permits: Arc<Semaphore>,
    write_permits: Arc<Semaphore>,
    resources: Option<ResourceMetrics>,
}

impl ImageWriteService {
    pub fn new(database: Database) -> Result<Self, ImageWriteError> {
        Self::with_proxy_config(database, ImageDownloadConfig::default(), None, None)
    }

    pub fn new_with_proxy(
        database: Database,
        proxy_url: Option<String>,
    ) -> Result<Self, ImageWriteError> {
        Self::with_proxy_config(database, ImageDownloadConfig::default(), proxy_url, None)
    }

    pub fn new_with_config_dir(
        database: Database,
        config_dir: PathBuf,
    ) -> Result<Self, ImageWriteError> {
        Self::with_proxy_config(
            database,
            ImageDownloadConfig::default(),
            None,
            Some(config_dir),
        )
    }

    pub fn new_with_proxy_and_config_dir(
        database: Database,
        config_dir: PathBuf,
        proxy_url: Option<String>,
    ) -> Result<Self, ImageWriteError> {
        Self::with_proxy_config(
            database,
            ImageDownloadConfig::default(),
            proxy_url,
            Some(config_dir),
        )
    }

    pub fn with_config(
        database: Database,
        config: ImageDownloadConfig,
    ) -> Result<Self, ImageWriteError> {
        Self::with_proxy_config(database, config, None, None)
    }

    pub fn with_config_and_concurrency(
        database: Database,
        config: ImageDownloadConfig,
        concurrency: usize,
    ) -> Result<Self, ImageWriteError> {
        Self::with_proxy_config_and_concurrency(database, config, None, None, concurrency)
    }

    fn with_proxy_config(
        database: Database,
        config: ImageDownloadConfig,
        proxy_url: Option<String>,
        config_dir: Option<PathBuf>,
    ) -> Result<Self, ImageWriteError> {
        Self::with_proxy_config_and_permits(
            database,
            config,
            proxy_url,
            config_dir,
            global_image_download_permits(),
            global_image_write_permits(),
        )
    }

    fn with_proxy_config_and_concurrency(
        database: Database,
        config: ImageDownloadConfig,
        proxy_url: Option<String>,
        config_dir: Option<PathBuf>,
        concurrency: usize,
    ) -> Result<Self, ImageWriteError> {
        if concurrency == 0 {
            return Err(ImageWriteError::InvalidConfiguration(
                "image concurrency must be positive".to_owned(),
            ));
        }
        Self::with_proxy_config_and_permits(
            database,
            config,
            proxy_url,
            config_dir,
            Arc::new(Semaphore::new(concurrency)),
            Arc::new(Semaphore::new(concurrency)),
        )
    }

    fn with_proxy_config_and_permits(
        database: Database,
        config: ImageDownloadConfig,
        proxy_url: Option<String>,
        config_dir: Option<PathBuf>,
        download_permits: Arc<Semaphore>,
        write_permits: Arc<Semaphore>,
    ) -> Result<Self, ImageWriteError> {
        if config.max_bytes == 0 {
            return Err(ImageWriteError::InvalidConfiguration(
                "image maximum size must be positive".to_owned(),
            ));
        }
        let http = client_builder_from_env_or(proxy_url.as_deref())
            .map_err(|error| ImageWriteError::ClientBuild(error.to_string()))?
            .timeout(config.timeout)
            .build()
            .map_err(|error| ImageWriteError::ClientBuild(error.to_string()))?;
        Ok(Self {
            database,
            http,
            max_bytes: config.max_bytes,
            config_dir,
            download_permits,
            write_permits,
            resources: None,
        })
    }

    pub(crate) fn with_resource_metrics(mut self, resources: ResourceMetrics) -> Self {
        self.resources = Some(resources);
        self
    }

    pub async fn download_item_image_if_missing(
        &self,
        item_id: &str,
        image_type: &str,
        image_url: &str,
    ) -> Result<Option<ImageWriteReport>, ImageWriteError> {
        self.download_item_image_if_missing_impl(item_id, image_type, image_url, "SCRAPER")
            .await
    }

    pub(crate) async fn try_download_item_image_if_missing_from_scraper_at_index(
        &self,
        item_id: &str,
        image_type: &str,
        image_url: &str,
        source: &str,
        image_index: i64,
    ) -> Result<Option<ImageWriteReport>, ImageWriteError> {
        let normalized_type = normalize_image_type(image_type)
            .ok_or_else(|| ImageWriteError::InvalidImageType(image_type.to_owned()))?;
        if self
            .local_image_exists_at_index(item_id, normalized_type, image_index)
            .await?
        {
            if image_index == 0
                && normalized_type == "THUMB"
                && self
                    .database
                    .find_item_image_source(item_id, "THUMB")
                    .await?
                    .is_some_and(|value| value.eq_ignore_ascii_case("STRM_FFMPEG"))
            {
                return self
                    .download_item_image_attempt_at_index(
                        item_id,
                        normalized_type,
                        image_url,
                        source,
                        image_index,
                        true,
                    )
                    .await;
            }
            return Ok(None);
        }
        self.download_item_image_attempt_at_index(
            item_id,
            normalized_type,
            image_url,
            source,
            image_index,
            false,
        )
        .await
    }

    async fn download_item_image_if_missing_impl(
        &self,
        item_id: &str,
        image_type: &str,
        image_url: &str,
        source: &str,
    ) -> Result<Option<ImageWriteReport>, ImageWriteError> {
        self.try_download_item_image_if_missing_from_scraper_at_index(
            item_id, image_type, image_url, source, 0,
        )
        .await
    }

    async fn download_item_image_attempt(
        &self,
        item_id: &str,
        image_type: &str,
        image_url: &str,
        source: &str,
        force: bool,
    ) -> Result<Option<ImageWriteReport>, ImageWriteError> {
        self.download_item_image_attempt_at_index(item_id, image_type, image_url, source, 0, force)
            .await
    }

    async fn download_item_image_attempt_at_index(
        &self,
        item_id: &str,
        image_type: &str,
        image_url: &str,
        source: &str,
        image_index: i64,
        force: bool,
    ) -> Result<Option<ImageWriteReport>, ImageWriteError> {
        let normalized_type = normalize_image_type(image_type)
            .ok_or_else(|| ImageWriteError::InvalidImageType(image_type.to_owned()))?;
        let now = current_unix_timestamp();
        let claimed_until = now.saturating_add(IMAGE_ATTEMPT_LEASE.as_secs() as i64);
        let candidate_key = image_candidate_key(source, normalized_type, image_url);
        if !self
            .database
            .claim_metadata_image_attempt(
                item_id,
                normalized_type,
                &candidate_key,
                now,
                claimed_until,
                force,
            )
            .await?
        {
            return Ok(None);
        }

        let result = self
            .download_item_image_impl(
                item_id,
                normalized_type,
                image_url,
                source,
                image_index,
                true,
            )
            .await;
        match result {
            Ok(report) => {
                self.database
                    .finish_metadata_image_attempt(MetadataImageAttemptUpdate {
                        item_id,
                        image_type: normalized_type,
                        candidate_key: &candidate_key,
                        status: "AVAILABLE",
                        next_retry_at: None,
                        error_code: None,
                        now: current_unix_timestamp(),
                    })
                    .await?;
                Ok(Some(report))
            }
            Err(error) => {
                self.record_image_attempt_failure(item_id, normalized_type, &candidate_key, &error)
                    .await?;
                Err(error)
            }
        }
    }

    async fn record_image_attempt_failure(
        &self,
        item_id: &str,
        image_type: &str,
        candidate_key: &str,
        error: &ImageWriteError,
    ) -> Result<(), ImageWriteError> {
        let now = current_unix_timestamp();
        let (status, next_retry_at, error_code) = image_attempt_failure(
            error,
            now,
            self.database
                .metadata_image_attempt_count(item_id, image_type, candidate_key)
                .await?,
        );
        self.database
            .finish_metadata_image_attempt(MetadataImageAttemptUpdate {
                item_id,
                image_type,
                candidate_key,
                status,
                next_retry_at,
                error_code: Some(error_code),
                now,
            })
            .await?;
        Ok(())
    }

    pub(crate) async fn image_source_url_exists(
        &self,
        item_id: &str,
        image_type: &str,
        source_url: &str,
    ) -> Result<bool, ImageWriteError> {
        let image_type = normalize_image_type(image_type)
            .ok_or_else(|| ImageWriteError::InvalidImageType(image_type.to_owned()))?;
        self.database
            .item_image_source_url_exists(item_id, image_type, source_url)
            .await
            .map_err(ImageWriteError::Storage)
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
            let in_metadata = if let Some(config_dir) = self.config_dir.as_ref() {
                fs::canonicalize(metadata_root(config_dir))
                    .await
                    .map(|root| canonical_path.starts_with(&root) && canonical_path != root)
                    .unwrap_or(false)
            } else {
                false
            };
            let in_media_root = if let Some(root_path) = image.root_path.as_deref() {
                fs::canonicalize(root_path)
                    .await
                    .map(|root| canonical_path.starts_with(&root) && canonical_path != root)
                    .unwrap_or(false)
            } else {
                false
            };
            if !in_metadata && !in_media_root {
                return Err(ImageWriteError::PathOutsideRoot(canonical_path));
            }
            if !self
                .database
                .item_image_path_is_shared(&image.local_path, &image.id)
                .await?
            {
                fs::remove_file(&canonical_path)
                    .await
                    .map_err(|source| ImageWriteError::Io {
                        path: canonical_path,
                        source,
                    })?;
            }
        }
        self.delete_metadata_image_copy(&image).await?;
        if !self.database.delete_item_image(item_id, image_id).await? {
            return Err(ImageWriteError::ItemNotFound);
        }
        Ok(())
    }

    async fn delete_metadata_image_copy(
        &self,
        image: &StoredItemImage,
    ) -> Result<(), ImageWriteError> {
        let Some(config_dir) = self.config_dir.as_deref() else {
            return Ok(());
        };
        let metadata_root_path = metadata_root(config_dir);
        let metadata_directory = library_item_directory(config_dir, &image.item_id)
            .map_err(|error| ImageWriteError::InvalidConfiguration(error.to_string()))?;
        reject_metadata_symlinks(&metadata_root_path).await?;
        let directory_metadata = match fs::symlink_metadata(&metadata_directory).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => return Err(image_io_error(&metadata_directory, source)),
        };
        if !directory_metadata.is_dir() {
            return Err(ImageWriteError::Io {
                path: metadata_directory,
                source: std::io::Error::new(
                    std::io::ErrorKind::NotADirectory,
                    "metadata item path is not a directory",
                ),
            });
        }
        reject_metadata_symlinks(&metadata_directory).await?;
        let canonical_root = fs::canonicalize(&metadata_root_path)
            .await
            .map_err(|source| image_io_error(&metadata_root_path, source))?;
        let canonical_directory = fs::canonicalize(&metadata_directory)
            .await
            .map_err(|source| image_io_error(&metadata_directory, source))?;
        if !canonical_directory.starts_with(&canonical_root) {
            return Err(ImageWriteError::PathOutsideRoot(canonical_directory));
        }
        let stems = image_lookup_stems(&image.image_type, None, None, image.image_index)?;
        let Some(path) = find_existing_image_path(&canonical_directory, &stems, None).await? else {
            return Ok(());
        };
        let metadata = fs::symlink_metadata(&path)
            .await
            .map_err(|source| image_io_error(&path, source))?;
        if metadata.file_type().is_symlink() {
            return Err(ImageWriteError::SymlinkTarget(path));
        }
        let canonical_path = fs::canonicalize(&path)
            .await
            .map_err(|source| image_io_error(&path, source))?;
        if !canonical_path.starts_with(&canonical_root) {
            return Err(ImageWriteError::PathOutsideRoot(canonical_path));
        }
        fs::remove_file(&canonical_path)
            .await
            .map_err(|source| image_io_error(&canonical_path, source))?;
        Ok(())
    }

    async fn local_image_exists(
        &self,
        item_id: &str,
        image_type: &str,
    ) -> Result<bool, ImageWriteError> {
        let image_type = normalize_image_type(image_type)
            .ok_or_else(|| ImageWriteError::InvalidImageType(image_type.to_owned()))?;
        self.local_image_exists_at_index(item_id, image_type, 0)
            .await
    }

    async fn local_image_exists_at_index(
        &self,
        item_id: &str,
        image_type: &str,
        image_index: i64,
    ) -> Result<bool, ImageWriteError> {
        if let Some(config_dir) = self.config_dir.as_ref()
            && self
                .metadata_image_exists_at_index(config_dir, item_id, image_type, image_index)
                .await?
        {
            return Ok(true);
        }
        let (_, directory, movie_stem, episode_stem) = self.writeback_paths(item_id).await?;
        let Some(path) = find_image_path_at_index(
            &directory,
            image_type,
            movie_stem.as_deref(),
            episode_stem.as_deref(),
            image_index,
        )
        .await?
        else {
            return Ok(false);
        };
        image_file_stamp(&path).await.map(|_| true)
    }

    async fn metadata_image_exists_at_index(
        &self,
        config_dir: &Path,
        item_id: &str,
        image_type: &str,
        image_index: i64,
    ) -> Result<bool, ImageWriteError> {
        let directory = library_item_directory(config_dir, item_id)
            .map_err(|error| ImageWriteError::InvalidConfiguration(error.to_string()))?;
        reject_metadata_symlinks(&directory).await?;
        let metadata = match fs::symlink_metadata(&directory).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(source) => return Err(image_io_error(&directory, source)),
        };
        if !metadata.is_dir() {
            return Err(ImageWriteError::Io {
                path: directory,
                source: std::io::Error::new(
                    std::io::ErrorKind::NotADirectory,
                    "metadata item path is not a directory",
                ),
            });
        }
        let Some(path) =
            find_image_path_at_index(&directory, image_type, None, None, image_index).await?
        else {
            return Ok(false);
        };
        image_file_stamp(&path).await.map(|stamp| stamp.is_some())
    }

    pub(crate) async fn has_local_image(
        &self,
        item_id: &str,
        image_type: &str,
    ) -> Result<bool, ImageWriteError> {
        self.local_image_exists(item_id, image_type).await
    }

    pub(crate) async fn local_image_types(
        &self,
        item_id: &str,
        image_types: &[&str],
    ) -> Result<BTreeSet<String>, ImageWriteError> {
        let image_types = image_types
            .iter()
            .map(|image_type| {
                normalize_image_type(image_type)
                    .ok_or_else(|| ImageWriteError::InvalidImageType((*image_type).to_owned()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut found = BTreeSet::new();

        if let Some(config_dir) = self.config_dir.as_ref() {
            let root = metadata_root(config_dir);
            let directory = library_item_directory(config_dir, item_id)
                .map_err(|error| ImageWriteError::InvalidConfiguration(error.to_string()))?;
            reject_metadata_symlinks(&root).await?;
            reject_metadata_symlinks(&directory).await?;
            let paths = read_image_directory_entries(&directory).await?;
            for image_type in &image_types {
                for image_index in 0..MAX_IMAGE_VARIANTS {
                    let stems = image_lookup_stems(image_type, None, None, image_index as i64)?;
                    if let Some(path) = find_existing_image_path_in_paths(&paths, &stems)
                        && image_file_stamp(&path).await?.is_some()
                    {
                        found.insert((*image_type).to_owned());
                        break;
                    }
                }
            }
        }

        let (_, directory, movie_stem, episode_stem) = self.writeback_paths(item_id).await?;
        let paths = read_image_directory_entries(&directory).await?;
        for image_type in &image_types {
            if found.contains(*image_type) {
                continue;
            }
            for image_index in 0..MAX_IMAGE_VARIANTS {
                let stems = image_lookup_stems(
                    image_type,
                    movie_stem.as_deref(),
                    episode_stem.as_deref(),
                    image_index as i64,
                )?;
                if let Some(path) = find_existing_image_path_in_paths(&paths, &stems)
                    && image_file_stamp(&path).await?.is_some()
                {
                    found.insert((*image_type).to_owned());
                    break;
                }
            }
        }
        Ok(found)
    }

    pub async fn download_item_image(
        &self,
        item_id: &str,
        image_type: &str,
        image_url: &str,
    ) -> Result<ImageWriteReport, ImageWriteError> {
        self.download_item_image_with_source(item_id, image_type, image_url, "SCRAPER")
            .await
    }

    async fn download_item_image_impl(
        &self,
        item_id: &str,
        image_type: &str,
        image_url: &str,
        source: &str,
        image_index: i64,
        reuse_existing_path: bool,
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

        let _download_permit = self
            .download_permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| {
                ImageWriteError::InvalidConfiguration("image semaphore closed".to_owned())
            })?;
        let download_started = std::time::Instant::now();
        let response = self.fetch_image(&url).await;
        if let Some(resources) = &self.resources {
            resources.record_metadata_stage("image_download", download_started.elapsed());
        }
        let response = response?;
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
        let body = read_response_body_limited(response, self.max_bytes)
            .await
            .map_err(|error| match error {
                LimitedBodyError::Download(error) => ImageWriteError::Download(error),
                LimitedBodyError::TooLarge { observed, max } => ImageWriteError::TooLarge {
                    size: observed,
                    max,
                },
            })?;
        let size = u64::try_from(body.len()).unwrap_or(u64::MAX);
        if size > self.max_bytes {
            return Err(ImageWriteError::TooLarge {
                size,
                max: self.max_bytes,
            });
        }
        validate_image_payload(format, &body)?;
        drop(_download_permit);

        let (root, directory, movie_stem, episode_stem) = self.writeback_paths(item_id).await?;
        let target = image_target(
            &directory,
            image_type,
            format,
            movie_stem.as_deref(),
            episode_stem.as_deref(),
            image_index,
            reuse_existing_path,
        )
        .await?;
        if !target.starts_with(&root) {
            return Err(ImageWriteError::PathOutsideRoot(target));
        }
        let metadata_target = if self.config_dir.is_some()
            && item_metadata_writeback_enabled(&self.database, item_id).await?
        {
            let config_dir = self.config_dir.as_ref().ok_or_else(|| {
                ImageWriteError::InvalidConfiguration("missing config directory".to_owned())
            })?;
            let metadata_root_path = metadata_root(config_dir);
            let metadata_directory = self.metadata_image_directory(config_dir, item_id).await?;
            let canonical_metadata_root = fs::canonicalize(&metadata_root_path)
                .await
                .map_err(|source| image_io_error(&metadata_root_path, source))?;
            let metadata_target = image_target(
                &metadata_directory,
                image_type,
                format,
                None,
                None,
                image_index,
                true,
            )
            .await?;
            if !metadata_target.starts_with(&canonical_metadata_root) {
                return Err(ImageWriteError::PathOutsideRoot(metadata_target));
            }
            Some(metadata_target)
        } else {
            None
        };
        let _write_permit = self
            .write_permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| {
                ImageWriteError::InvalidConfiguration("image write semaphore closed".to_owned())
            })?;
        let write_started = std::time::Instant::now();
        write_image_atomically(&target, &body).await?;
        if let Some(metadata_target) = metadata_target.as_deref() {
            write_image_atomically(metadata_target, &body).await?;
        }

        let file_size = i64::try_from(body.len()).map_err(|_| ImageWriteError::TooLarge {
            size,
            max: i64::MAX as u64,
        })?;
        let content_tag = content_tag(&body);
        let dimensions = read_image_dimensions(&target).await;
        let id = self
            .database
            .upsert_item_image_at_index(
                item_id,
                image_type,
                image_index,
                &target,
                ItemImageMetadata {
                    file_size,
                    width: dimensions.map(|(width, _)| width),
                    height: dimensions.map(|(_, height)| height),
                    content_tag: &content_tag,
                    source,
                    source_url: Some(image_url),
                },
            )
            .await?;
        if let Some(resources) = &self.resources {
            resources.record_metadata_stage("image_write", write_started.elapsed());
            resources.record_metadata_image_bytes(size);
        }
        Ok(ImageWriteReport {
            id,
            image_type: image_type.to_owned(),
            path: target,
            content_type: format.content_type(),
            file_size,
            content_tag,
        })
    }

    async fn fetch_image(&self, url: &Url) -> Result<reqwest::Response, ImageWriteError> {
        let mut retry_count = 0;
        loop {
            let response = self.http.get(url.clone()).send().await;
            match response {
                Ok(response)
                    if retry_count < IMAGE_DOWNLOAD_MAX_RETRIES
                        && retryable_image_status(response.status().as_u16()) =>
                {
                    retry_count += 1;
                    self.record_image_retry();
                    sleep(image_download_retry_delay(retry_count)).await;
                }
                Ok(response) => return Ok(response),
                Err(error)
                    if retry_count < IMAGE_DOWNLOAD_MAX_RETRIES
                        && (error.is_timeout() || error.is_connect()) =>
                {
                    retry_count += 1;
                    self.record_image_retry();
                    sleep(image_download_retry_delay(retry_count)).await;
                }
                Err(error) => return Err(ImageWriteError::Download(error.to_string())),
            }
        }
    }

    fn record_image_retry(&self) {
        if let Some(resources) = &self.resources {
            resources.record_metadata_image_retry();
        }
    }

    async fn metadata_image_directory(
        &self,
        config_dir: &Path,
        item_id: &str,
    ) -> Result<PathBuf, ImageWriteError> {
        let root = metadata_root(config_dir);
        let directory = library_item_directory(config_dir, item_id)
            .map_err(|error| ImageWriteError::InvalidConfiguration(error.to_string()))?;
        reject_metadata_symlinks(&root).await?;
        fs::create_dir_all(&directory)
            .await
            .map_err(|source| image_io_error(&directory, source))?;
        reject_metadata_symlinks(&directory).await?;
        let canonical_root = fs::canonicalize(&root)
            .await
            .map_err(|source| image_io_error(&root, source))?;
        let canonical_directory = fs::canonicalize(&directory)
            .await
            .map_err(|source| image_io_error(&directory, source))?;
        if !canonical_directory.starts_with(&canonical_root) {
            return Err(ImageWriteError::PathOutsideRoot(canonical_directory));
        }
        Ok(canonical_directory)
    }

    pub async fn download_item_image_from_scraper_candidate(
        &self,
        item_id: &str,
        image_type: &str,
        image_url: &str,
    ) -> Result<ImageWriteReport, ImageWriteError> {
        if !is_allowed_scraper_image_url(image_url) {
            return Err(ImageWriteError::InvalidUrl(
                "selected image URL must be a valid HTTPS scraper image URL".to_owned(),
            ));
        }
        let source = self
            .database
            .find_media_item_image_identity(item_id)
            .await?
            .and_then(|identity| identity.provider_name)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "SCRAPER".to_owned());
        self.download_item_image_attempt(item_id, image_type, image_url, &source, true)
            .await?
            .ok_or(ImageWriteError::AttemptInProgress)
    }

    pub(crate) async fn download_item_image_from_scraper(
        &self,
        item_id: &str,
        image_type: &str,
        image_url: &str,
        source: &str,
    ) -> Result<Option<ImageWriteReport>, ImageWriteError> {
        if !is_allowed_scraper_image_url(image_url) {
            return Err(ImageWriteError::InvalidUrl(
                "scraper image URL must be a valid HTTPS URL".to_owned(),
            ));
        }
        self.download_item_image_attempt(item_id, image_type, image_url, source, true)
            .await
    }

    pub(crate) async fn download_item_image_from_scraper_at_index(
        &self,
        item_id: &str,
        image_type: &str,
        image_url: &str,
        source: &str,
        image_index: i64,
    ) -> Result<Option<ImageWriteReport>, ImageWriteError> {
        if !is_allowed_scraper_image_url(image_url) {
            return Err(ImageWriteError::InvalidUrl(
                "scraper image URL must be a valid HTTPS URL".to_owned(),
            ));
        }
        self.download_item_image_attempt_at_index(
            item_id,
            image_type,
            image_url,
            source,
            image_index,
            true,
        )
        .await
    }

    pub(crate) async fn next_image_index(
        &self,
        item_id: &str,
        image_type: &str,
    ) -> Result<i64, ImageWriteError> {
        let image_type = normalize_image_type(image_type)
            .ok_or_else(|| ImageWriteError::InvalidImageType(image_type.to_owned()))?;
        if image_type != "FANART" {
            return Ok(0);
        }
        let images = self.database.list_item_images(item_id).await?;
        Ok(images
            .iter()
            .filter(|image| image.image_type.eq_ignore_ascii_case(image_type))
            .map(|image| image.image_index)
            .max()
            .unwrap_or(-1)
            .saturating_add(1))
    }

    async fn download_item_image_with_source(
        &self,
        item_id: &str,
        image_type: &str,
        image_url: &str,
        source: &str,
    ) -> Result<ImageWriteReport, ImageWriteError> {
        self.download_item_image_impl(item_id, image_type, image_url, source, 0, false)
            .await
    }

    async fn writeback_paths(
        &self,
        item_id: &str,
    ) -> Result<(PathBuf, PathBuf, Option<String>, Option<String>), ImageWriteError> {
        let kind = self
            .database
            .find_media_item_kind(item_id)
            .await?
            .ok_or(ImageWriteError::ItemNotFound)?;
        let item_type = kind.item_type;
        let source = match item_type.as_str() {
            "SERIES" | "SEASON" => {
                self.database
                    .find_first_episode_source_path(item_id)
                    .await?
            }
            _ => {
                self.database
                    .find_metadata_writeback_source_path(item_id)
                    .await?
            }
        }
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
        let directory = if item_type == "SERIES" {
            series_directory(&root, &source.relative_path)
                .ok_or_else(|| ImageWriteError::PathOutsideRoot(media_path.clone()))?
        } else {
            media_path
                .parent()
                .ok_or_else(|| ImageWriteError::PathOutsideRoot(media_path.clone()))?
                .to_owned()
        };
        let directory = fs::canonicalize(&directory)
            .await
            .map_err(|error| image_io_error(&directory, error))?;
        if !directory.starts_with(&root) {
            return Err(ImageWriteError::PathOutsideRoot(directory));
        }
        let episode_stem = (item_type == "EPISODE").then(|| {
            media_path
                .file_stem()
                .and_then(|value| value.to_str())
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("episode-{item_id}"))
        });
        let movie_stem = (item_type == "MOVIE")
            .then(|| {
                media_path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
            })
            .flatten();
        Ok((root, directory, movie_stem, episode_stem))
    }
}

#[derive(Clone)]
pub struct ImageCandidateService {
    database: Database,
    scraper: ScraperProvider,
    resolver: Option<ScraperResolver>,
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
    pub fn new<T>(database: Database, scraper: T) -> Self
    where
        T: Into<ScraperProvider>,
    {
        Self {
            database,
            scraper: scraper.into(),
            resolver: None,
        }
    }

    pub fn with_resolver<T>(database: Database, scraper: T, resolver: ScraperResolver) -> Self
    where
        T: Into<ScraperProvider>,
    {
        Self {
            database,
            scraper: scraper.into(),
            resolver: Some(resolver),
        }
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
        let source = source.map(str::trim).filter(|value| !value.is_empty());
        if source.is_some_and(|value| value.chars().count() > 64) {
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
            .clone()
            .filter(|value| !value.trim().is_empty())
            .ok_or(ImageCandidateError::ItemNotIdentified)?;
        let item_type = match identity.item_type.as_str() {
            "MOVIE" => ScraperItemType::Movie,
            "SERIES" => ScraperItemType::Series,
            "SEASON" => ScraperItemType::Season,
            "EPISODE" => ScraperItemType::Episode,
            _ => return Ok(Vec::new()),
        };
        let scraper = self.provider_for_item(item_id).await?;
        let mut image_request = ScraperImageRequest::new(item_type, provider_id, language);
        image_request.season_number = identity
            .season_number
            .map(|value| i32::try_from(value).map_err(|_| ImageCandidateError::InvalidItem))
            .transpose()?;
        image_request.episode_number = identity
            .episode_number
            .map(|value| i32::try_from(value).map_err(|_| ImageCandidateError::InvalidItem))
            .transpose()?;
        let images = scraper
            .images_generic(image_request)
            .await
            .map_err(ImageCandidateError::Scraper)?
            .images;
        let requested_language = language.split('-').next().filter(|value| !value.is_empty());
        Ok(images
            .into_iter()
            .enumerate()
            .filter(|(_, image)| matches_image_type(image, image_type))
            .filter(|(_, image)| {
                source.is_none_or(|source| {
                    image
                        .provider_name
                        .as_deref()
                        .is_some_and(|value| value.eq_ignore_ascii_case(source))
                })
            })
            .filter(|(_, image)| {
                requested_language.is_none()
                    || image
                        .language
                        .as_deref()
                        .is_some_and(|value| Some(value) == requested_language)
            })
            .map(|(index, image)| {
                let provider_name = image
                    .provider_name
                    .clone()
                    .or_else(|| identity.provider_name.clone())
                    .unwrap_or_else(|| "SCRAPER".to_owned());
                ImageCandidate {
                    id: format!(
                        "{}-{image_type}-{index}",
                        provider_name.to_ascii_lowercase()
                    ),
                    image_type: image_type.to_owned(),
                    image_index: i64::try_from(index).unwrap_or_default(),
                    language: image.language,
                    width: image.width,
                    height: image.height,
                    source: provider_name,
                    url: image.url,
                }
            })
            .take(50)
            .collect())
    }

    async fn provider_for_item(
        &self,
        item_id: &str,
    ) -> Result<ScraperProvider, ImageCandidateError> {
        let Some(resolver) = &self.resolver else {
            return Ok(self.scraper.clone());
        };
        resolver
            .for_item(item_id)
            .await
            .map_err(ImageCandidateError::Scraper)
            .map(|client| {
                client
                    .map(ScraperProvider::from_scraper)
                    .unwrap_or_else(|| self.scraper.clone())
            })
    }
}

fn matches_image_type(image: &ScraperImage, image_type: &str) -> bool {
    match image_type {
        "POSTER" | "DISC" => matches!(image.image_type.as_str(), "Primary" | "Poster" | "POSTER"),
        "LOGO" => matches!(image.image_type.as_str(), "Logo" | "LOGO"),
        "FANART" | "THUMB" | "BANNER" | "ART" | "WALLPAPER" => {
            matches!(image.image_type.as_str(), "Backdrop" | "Fanart" | "FANART")
        }
        _ => false,
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
    Scraper(ScraperError),
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
            Self::Scraper(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ImageCandidateError {}

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
    movie_stem: Option<&str>,
    episode_stem: Option<&str>,
    image_index: i64,
    reuse_existing_path: bool,
) -> Result<PathBuf, ImageWriteError> {
    let stems = image_lookup_stems(image_type, movie_stem, episode_stem, image_index)?;
    if reuse_existing_path {
        if let Some(existing) = find_existing_image_path(directory, &stems, None).await? {
            return Ok(existing);
        }
    }
    if let Some(existing) = find_existing_image_path(directory, &stems, Some(format)).await? {
        return Ok(existing);
    }
    let (prefixed_stem, generic_stem) =
        canonical_image_stems(image_type, movie_stem, episode_stem, image_index)?;
    let target_stem = if let Some(prefixed_stem) = prefixed_stem {
        let prefixed_exists =
            find_existing_image_path(directory, std::slice::from_ref(&prefixed_stem), None)
                .await?
                .is_some();
        if prefixed_exists || directory_has_multiple_media_files(directory).await? {
            prefixed_stem
        } else {
            generic_stem
        }
    } else {
        generic_stem
    };
    Ok(directory.join(format!("{target_stem}.{}", format.extension())))
}

async fn find_image_path_at_index(
    directory: &Path,
    image_type: &str,
    movie_stem: Option<&str>,
    episode_stem: Option<&str>,
    image_index: i64,
) -> Result<Option<PathBuf>, ImageWriteError> {
    let stems = image_lookup_stems(image_type, movie_stem, episode_stem, image_index)?;
    find_existing_image_path(directory, &stems, None).await
}

fn find_existing_image_path_in_paths(paths: &[PathBuf], stems: &[String]) -> Option<PathBuf> {
    let mut candidates = paths
        .iter()
        .filter(|path| {
            let matches_stem = path
                .file_stem()
                .and_then(|value| value.to_str())
                .is_some_and(|value| stems.iter().any(|stem| value.eq_ignore_ascii_case(stem)));
            let known_extension = path
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| {
                    matches!(
                        value.to_ascii_lowercase().as_str(),
                        "jpg" | "jpeg" | "png" | "webp"
                    )
                });
            matches_stem && known_extension
        })
        .cloned()
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| left.to_string_lossy().cmp(&right.to_string_lossy()));
    stems.iter().find_map(|stem| {
        candidates.iter().find_map(|path| {
            path.file_stem()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case(stem))
                .then(|| path.clone())
        })
    })
}

async fn read_image_directory_entries(directory: &Path) -> Result<Vec<PathBuf>, ImageWriteError> {
    let metadata = match fs::symlink_metadata(directory).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(image_io_error(directory, source)),
    };
    if !metadata.is_dir() {
        return Err(ImageWriteError::Io {
            path: directory.to_owned(),
            source: std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                "image path is not a directory",
            ),
        });
    }
    let mut entries = fs::read_dir(directory)
        .await
        .map_err(|source| image_io_error(directory, source))?;
    let mut paths = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|source| image_io_error(directory, source))?
    {
        paths.push(entry.path());
    }
    Ok(paths)
}

fn image_lookup_stems(
    image_type: &str,
    movie_stem: Option<&str>,
    episode_stem: Option<&str>,
    image_index: i64,
) -> Result<Vec<String>, ImageWriteError> {
    let image_index = image_index.max(0);
    let (legacy_prefixed, legacy_generic) = image_file_stems(image_type, movie_stem, episode_stem)?;
    let (canonical_prefixed, canonical_generic) =
        canonical_image_stems(image_type, movie_stem, episode_stem, image_index)?;
    let mut stems = Vec::with_capacity(6);
    if let Some(stem) = canonical_prefixed {
        stems.push(stem);
    }
    if let Some(stem) = legacy_prefixed {
        stems.push(indexed_legacy_stem(&stem, image_index));
    }
    stems.push(canonical_generic.clone());
    stems.push(indexed_legacy_stem(&legacy_generic, image_index));
    if image_index > 0 && image_type.eq_ignore_ascii_case("FANART") {
        stems.push(format!("{canonical_generic}-{image_index}"));
    }
    Ok(stems)
}

fn canonical_image_stems(
    image_type: &str,
    movie_stem: Option<&str>,
    episode_stem: Option<&str>,
    image_index: i64,
) -> Result<(Option<String>, String), ImageWriteError> {
    let (prefixed, generic) = image_file_stems(image_type, movie_stem, episode_stem)?;
    let canonical = |stem: String| {
        let stem = if image_type.eq_ignore_ascii_case("FANART") && episode_stem.is_none() {
            stem.replace("-fanart", "-backdrop")
                .replace("fanart", "backdrop")
        } else {
            stem
        };
        if image_index == 0 {
            stem
        } else {
            format!("{stem}{image_index}")
        }
    };
    Ok((prefixed.map(canonical), canonical(generic)))
}

fn indexed_legacy_stem(stem: &str, image_index: i64) -> String {
    if image_index == 0 {
        stem.to_owned()
    } else {
        format!("{stem}-{image_index}")
    }
}

fn image_file_stems(
    image_type: &str,
    movie_stem: Option<&str>,
    episode_stem: Option<&str>,
) -> Result<(Option<String>, String), ImageWriteError> {
    let image_stem = match image_type {
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
    let image_stem = if episode_stem.is_some() && image_stem == "fanart" {
        "thumb"
    } else {
        image_stem
    };
    let generic_stem = episode_stem
        .map(|episode_stem| format!("{episode_stem}-{image_stem}"))
        .unwrap_or_else(|| image_stem.to_owned());
    let prefixed_stem = movie_stem.map(|movie_stem| format!("{movie_stem}-{image_stem}"));
    Ok((prefixed_stem, generic_stem))
}

async fn find_existing_image_path(
    directory: &Path,
    stems: &[String],
    format: Option<ImageFormat>,
) -> Result<Option<PathBuf>, ImageWriteError> {
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
            .is_some_and(|value| stems.iter().any(|stem| value.eq_ignore_ascii_case(stem)));
        let known_extension = path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| {
                matches!(
                    value.to_ascii_lowercase().as_str(),
                    "jpg" | "jpeg" | "png" | "webp"
                )
            });
        let matching_format = format.is_none_or(|format| format.matches_path(&path));
        if matches_stem && known_extension && matching_format {
            candidates.push(path);
        }
    }
    candidates.sort_by(|left, right| left.to_string_lossy().cmp(&right.to_string_lossy()));
    for stem in stems {
        if let Some(path) = candidates.iter().find(|path| {
            path.file_stem()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case(stem))
        }) {
            return Ok(Some(path.clone()));
        }
    }
    Ok(None)
}

async fn directory_has_multiple_media_files(directory: &Path) -> Result<bool, ImageWriteError> {
    let mut entries = fs::read_dir(directory)
        .await
        .map_err(|source| image_io_error(directory, source))?;
    let mut media_count = 0;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|source| image_io_error(directory, source))?
    {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .await
            .map_err(|source| image_io_error(&path, source))?;
        if file_type.is_file()
            && path
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| {
                    matches!(value.to_ascii_lowercase().as_str(), "mkv" | "mp4" | "strm")
                })
        {
            media_count += 1;
            if media_count > 1 {
                return Ok(true);
            }
        }
    }
    Ok(false)
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

async fn reject_metadata_symlinks(path: &Path) -> Result<(), ImageWriteError> {
    let mut current = Some(path.to_owned());
    while let Some(candidate) = current {
        match fs::symlink_metadata(&candidate).await {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(ImageWriteError::SymlinkTarget(candidate));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(ImageWriteError::Io {
                    path: candidate,
                    source: std::io::Error::new(
                        std::io::ErrorKind::NotADirectory,
                        "metadata path component is not a directory",
                    ),
                });
            }
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                current = candidate.parent().map(Path::to_owned);
            }
            Err(source) => return Err(image_io_error(&candidate, source)),
        }
    }
    Ok(())
}

fn content_tag(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn current_unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default()
}

pub(crate) fn image_candidate_key(source: &str, image_type: &str, image_url: &str) -> String {
    let material = format!("{source}\0{image_type}\0{image_url}");
    content_tag(material.as_bytes())
}

pub(crate) fn image_no_candidate_key(source: &str, image_type: &str, provider_id: &str) -> String {
    image_candidate_key(source, image_type, &format!("__NO_IMAGE__\0{provider_id}"))
}

fn image_retry_delay_seconds(attempt_count: u32) -> i64 {
    let exponent = attempt_count.saturating_sub(1).min(8);
    let multiplier = 1_i64.checked_shl(exponent).unwrap_or(i64::MAX);
    IMAGE_RETRY_BASE_SECONDS
        .saturating_mul(multiplier)
        .min(IMAGE_RETRY_MAX_SECONDS)
}

fn image_attempt_failure(
    error: &ImageWriteError,
    now: i64,
    attempt_count: u32,
) -> (&'static str, Option<i64>, &'static str) {
    match error {
        ImageWriteError::UpstreamStatus { status } if retryable_image_status(*status) => (
            "FAILED",
            Some(now.saturating_add(image_retry_delay_seconds(attempt_count))),
            "TRANSIENT_FAILURE",
        ),
        ImageWriteError::UpstreamStatus { status: 404 | 410 } => {
            ("UNAVAILABLE", None, "UPSTREAM_NOT_FOUND")
        }
        ImageWriteError::UpstreamStatus { .. } => ("UNAVAILABLE", None, "UPSTREAM_PERMANENT"),
        ImageWriteError::InvalidUrl(_)
        | ImageWriteError::UnsupportedContentType { .. }
        | ImageWriteError::InvalidContent { .. }
        | ImageWriteError::TooLarge { .. } => ("UNAVAILABLE", None, "INVALID_IMAGE"),
        ImageWriteError::Download(_)
        | ImageWriteError::ConcurrentModification(_)
        | ImageWriteError::Io { .. } => (
            "FAILED",
            Some(now.saturating_add(image_retry_delay_seconds(attempt_count))),
            "TRANSIENT_FAILURE",
        ),
        ImageWriteError::InvalidConfiguration(_)
        | ImageWriteError::InvalidImageType(_)
        | ImageWriteError::ClientBuild(_)
        | ImageWriteError::ItemNotFound
        | ImageWriteError::PathOutsideRoot(_)
        | ImageWriteError::SymlinkTarget(_)
        | ImageWriteError::AttemptInProgress
        | ImageWriteError::Storage(_) => ("FAILED", None, "PERMANENT_FAILURE"),
    }
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
    config_dir: PathBuf,
}

impl ImageService {
    pub fn new(database: Database, access: MediaAccessService, config_dir: PathBuf) -> Self {
        Self {
            database,
            access,
            config_dir,
        }
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
        self.resolve_candidates(candidates).await
    }

    pub async fn resolve_tagged(
        &self,
        item_id: &str,
        image_type: &str,
        image_index: i64,
        tag: &str,
    ) -> Result<Option<ResolvedImage>, ImageError> {
        let candidates = self
            .database
            .list_item_image_candidates(item_id, image_type, image_index)
            .await?
            .into_iter()
            .filter(|candidate| candidate.id == tag)
            .collect();
        self.resolve_candidates(candidates).await
    }

    pub(crate) async fn resolve_filmly_compat(
        &self,
        item_id: &str,
        image_type: &str,
        image_index: i64,
    ) -> Result<Option<ResolvedImage>, ImageError> {
        let candidates = self
            .database
            .list_item_image_candidates(item_id, image_type, image_index)
            .await?;
        self.resolve_candidates(candidates).await
    }

    async fn resolve_candidates(
        &self,
        candidates: Vec<crate::storage::StoredItemImageCandidate>,
    ) -> Result<Option<ResolvedImage>, ImageError> {
        if candidates.is_empty() {
            return Ok(None);
        }

        let metadata_root = fs::canonicalize(metadata_root(&self.config_dir)).await.ok();
        let mut canonical_roots = HashMap::<PathBuf, Option<PathBuf>>::new();
        let mut canonical_paths = HashMap::<PathBuf, Option<PathBuf>>::new();
        let mut metadata_by_path = HashMap::<PathBuf, std::fs::Metadata>::new();
        let mut saw_outside_root = false;
        for candidate in candidates {
            let path = PathBuf::from(&candidate.local_path);
            let canonical_path = if let Some(canonical_path) = canonical_paths.get(&path) {
                canonical_path.clone()
            } else {
                let canonical_path = fs::canonicalize(&path).await.ok();
                canonical_paths.insert(path, canonical_path.clone());
                canonical_path
            };
            let Some(canonical_path) = canonical_path else {
                continue;
            };
            let root_path = PathBuf::from(&candidate.root_path);
            let canonical_root = if let Some(canonical_root) = canonical_roots.get(&root_path) {
                canonical_root.clone()
            } else {
                let canonical_root = fs::canonicalize(&root_path).await.ok();
                canonical_roots.insert(root_path, canonical_root.clone());
                canonical_root
            };
            let in_media_root = canonical_root
                .as_ref()
                .is_some_and(|root| canonical_path.starts_with(root) && canonical_path != *root);
            let in_metadata_root = metadata_root
                .as_ref()
                .is_some_and(|root| canonical_path.starts_with(root) && canonical_path != *root);
            if !in_media_root && !in_metadata_root {
                saw_outside_root = true;
                continue;
            }
            let metadata = if let Some(metadata) = metadata_by_path.get(&canonical_path) {
                metadata.clone()
            } else {
                let metadata =
                    fs::metadata(&canonical_path)
                        .await
                        .map_err(|source| ImageError::Io {
                            path: canonical_path.clone(),
                            source,
                        })?;
                metadata_by_path.insert(canonical_path.clone(), metadata.clone());
                metadata
            };
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
            Self::Forbidden => formatter.write_str("image path is outside the allowed image roots"),
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
    AttemptInProgress,
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
                    "image path '{}' is outside the allowed image roots",
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
            Self::AttemptInProgress => formatter.write_str("image download is already in progress"),
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
            | Self::ConcurrentModification(_)
            | Self::AttemptInProgress => None,
        }
    }
}

impl From<StorageError> for ImageWriteError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

fn is_allowed_scraper_image_url(value: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    let loopback_http = url.scheme().eq_ignore_ascii_case("http")
        && url.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<IpAddr>()
                    .is_ok_and(|address| address.is_loopback())
        });
    (url.scheme().eq_ignore_ascii_case("https") || loopback_http)
        && url.host_str().is_some_and(|host| !host.is_empty())
        && url.username().is_empty()
        && url.password().is_none()
        && (url.port().is_none() || loopback_http)
        && !url.path().is_empty()
        && url.query().is_none()
        && url.fragment().is_none()
}

fn retryable_image_status(status: u16) -> bool {
    status == 429 || (500..=599).contains(&status)
}

fn image_download_retry_delay(retry_count: u32) -> Duration {
    let exponent = retry_count.saturating_sub(1).min(31);
    let factor = 1_u32.checked_shl(exponent).unwrap_or(u32::MAX);
    IMAGE_RETRY_BASE_DELAY
        .checked_mul(factor)
        .unwrap_or(IMAGE_RETRY_MAX_DELAY)
        .min(IMAGE_RETRY_MAX_DELAY)
}

#[cfg(test)]
mod tests {
    use super::{
        IMAGE_GLOBAL_CONCURRENCY, IMAGE_RETRY_BASE_DELAY, IMAGE_RETRY_MAX_DELAY, ImageWriteError,
        canonical_image_stems, global_image_download_permits, global_image_write_permits,
        image_attempt_failure, image_content_tag_and_dimensions_from_bytes,
        image_download_retry_delay, image_lookup_stems, is_allowed_scraper_image_url,
        retryable_image_status,
    };

    #[test]
    fn image_retries_only_transient_upstream_statuses() {
        assert!(retryable_image_status(429));
        assert!(retryable_image_status(500));
        assert!(retryable_image_status(503));
        assert!(!retryable_image_status(200));
        assert!(!retryable_image_status(404));
    }

    #[test]
    fn image_retries_use_bounded_exponential_backoff() {
        assert_eq!(image_download_retry_delay(1), IMAGE_RETRY_BASE_DELAY);
        assert_eq!(image_download_retry_delay(2), IMAGE_RETRY_BASE_DELAY * 2);
        assert_eq!(image_download_retry_delay(99), IMAGE_RETRY_MAX_DELAY);
    }

    #[test]
    fn permanent_upstream_status_does_not_schedule_image_retry() {
        let (status, next_retry_at, error_code) =
            image_attempt_failure(&ImageWriteError::UpstreamStatus { status: 403 }, 100, 1);

        assert_eq!(status, "UNAVAILABLE");
        assert_eq!(next_retry_at, None);
        assert_eq!(error_code, "UPSTREAM_PERMANENT");
    }

    #[test]
    fn image_download_and_write_quotas_are_independent() {
        let first = global_image_download_permits();
        let second = global_image_download_permits();
        assert!(std::sync::Arc::ptr_eq(&first, &second));
        assert_eq!(first.available_permits(), IMAGE_GLOBAL_CONCURRENCY);

        let write = global_image_write_permits();
        assert!(!std::sync::Arc::ptr_eq(&first, &write));
        assert_eq!(write.available_permits(), IMAGE_GLOBAL_CONCURRENCY);
    }

    #[tokio::test]
    async fn combined_local_image_metadata_reads_bytes_once() {
        let image = image::RgbImage::from_pixel(3, 2, image::Rgb([12, 34, 56]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("png encoding");
        let (content_tag, dimensions) = image_content_tag_and_dimensions_from_bytes(bytes)
            .await
            .expect("metadata worker");
        assert_eq!(content_tag.len(), 64);
        assert_eq!(dimensions, Some((3, 2)));
    }

    #[test]
    fn scraper_image_urls_accept_any_safe_https_host() {
        assert!(is_allowed_scraper_image_url(
            "https://img.douban.example/poster.jpg"
        ));
        assert!(!is_allowed_scraper_image_url(
            "http://img.douban.example/poster.jpg"
        ));
        assert!(!is_allowed_scraper_image_url(
            "https://img.douban.example/poster.jpg?redirect=http://127.0.0.1"
        ));
        assert!(!is_allowed_scraper_image_url(
            "https://user:pass@img.douban.example/poster.jpg"
        ));
    }

    #[test]
    fn scraper_image_urls_allow_only_loopback_http_for_local_stubs() {
        assert!(is_allowed_scraper_image_url(
            "http://127.0.0.1:8099/poster.jpg"
        ));
        assert!(!is_allowed_scraper_image_url(
            "http://img.douban.example/poster.jpg"
        ));
    }

    #[test]
    fn fanart_uses_emby_backdrop_numbering_and_keeps_legacy_names_readable() {
        assert_eq!(
            canonical_image_stems("FANART", None, None, 0).expect("fanart type"),
            (None, "backdrop".to_owned())
        );
        assert_eq!(
            canonical_image_stems("FANART", None, None, 1).expect("fanart type"),
            (None, "backdrop1".to_owned())
        );
        let lookup = image_lookup_stems("FANART", None, None, 1).expect("fanart type");
        assert!(lookup.iter().any(|stem| stem == "backdrop1"));
        assert!(lookup.iter().any(|stem| stem == "fanart-1"));
    }
}
