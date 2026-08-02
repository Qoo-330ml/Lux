use std::{
    fmt,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use tokio::fs;

use crate::{
    application::access::{AccessError, AccessPrincipal, MediaAccessService},
    storage::{Database, StorageError},
};

const MAX_IMAGE_BYTES: u64 = 50 * 1024 * 1024;

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
