use std::{
    fmt,
    path::{Path, PathBuf},
};

use serde::Serialize;
use tokio::{fs, fs::OpenOptions, io::AsyncWriteExt};
use uuid::Uuid;

use crate::application::metadata_paths::{
    MetadataObjectKind, MetadataPathError, metadata_object_directory, metadata_root,
};

const MAX_SNAPSHOT_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataObjectSnapshot {
    kind: MetadataObjectKind,
    display_name: String,
    provider: String,
    object_id: String,
    overview: Option<String>,
    member_count: Option<usize>,
}

impl MetadataObjectSnapshot {
    pub fn new(
        kind: MetadataObjectKind,
        display_name: &str,
        provider: &str,
        object_id: &str,
    ) -> Result<Self, MetadataObjectError> {
        let display_name = display_name.trim();
        if display_name.is_empty() || display_name.len() > 512 {
            return Err(MetadataObjectError::InvalidField("display name"));
        }
        metadata_object_directory(Path::new("."), kind, display_name, provider, object_id)?;
        Ok(Self {
            kind,
            display_name: display_name.to_owned(),
            provider: provider.trim().to_ascii_lowercase(),
            object_id: object_id.to_owned(),
            overview: None,
            member_count: None,
        })
    }

    pub fn with_overview(mut self, overview: impl Into<String>) -> Self {
        self.overview = Some(overview.into());
        self
    }

    pub fn with_member_count(mut self, member_count: usize) -> Self {
        self.member_count = Some(member_count);
        self
    }
}

#[derive(Clone)]
pub struct MetadataObjectStore {
    config_dir: PathBuf,
}

impl MetadataObjectStore {
    pub fn new(config_dir: PathBuf) -> Self {
        Self { config_dir }
    }

    pub async fn write_snapshot(
        &self,
        snapshot: MetadataObjectSnapshot,
    ) -> Result<MetadataObjectWriteReport, MetadataObjectError> {
        let directory = metadata_object_directory(
            &self.config_dir,
            snapshot.kind,
            &snapshot.display_name,
            &snapshot.provider,
            &snapshot.object_id,
        )?;
        let metadata_root = metadata_root(&self.config_dir);
        reject_symlink_ancestors(&metadata_root).await?;
        fs::create_dir_all(&directory)
            .await
            .map_err(|source| io_error(&directory, source))?;
        reject_symlink_ancestors(&directory).await?;
        let canonical_root = fs::canonicalize(&metadata_root)
            .await
            .map_err(|source| io_error(&metadata_root, source))?;
        let canonical_directory = fs::canonicalize(&directory)
            .await
            .map_err(|source| io_error(&directory, source))?;
        if !canonical_directory.starts_with(&canonical_root) {
            return Err(MetadataObjectError::PathOutsideRoot(canonical_directory));
        }

        let payload = StoredMetadataObject::from(&snapshot);
        let bytes = serde_json::to_vec_pretty(&payload)
            .map_err(|source| MetadataObjectError::Serialization(source.to_string()))?;
        if bytes.len() > MAX_SNAPSHOT_BYTES {
            return Err(MetadataObjectError::TooLarge);
        }
        let path = canonical_directory.join(snapshot.kind.file_name());
        write_atomically(&path, &bytes).await?;
        Ok(MetadataObjectWriteReport {
            path,
            byte_count: bytes.len(),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataObjectWriteReport {
    pub path: PathBuf,
    pub byte_count: usize,
}

#[derive(Debug)]
pub enum MetadataObjectError {
    InvalidField(&'static str),
    InvalidPath(MetadataPathError),
    Serialization(String),
    TooLarge,
    SymlinkTarget(PathBuf),
    PathOutsideRoot(PathBuf),
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

impl fmt::Display for MetadataObjectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidField(field) => write!(formatter, "metadata object {field} is invalid"),
            Self::InvalidPath(error) => error.fmt(formatter),
            Self::Serialization(error) => {
                write!(formatter, "metadata object serialization failed: {error}")
            }
            Self::TooLarge => formatter.write_str("metadata object snapshot is too large"),
            Self::SymlinkTarget(path) => {
                write!(
                    formatter,
                    "metadata object path is a symlink: {}",
                    path.display()
                )
            }
            Self::PathOutsideRoot(path) => {
                write!(
                    formatter,
                    "metadata object path is outside metadata: {}",
                    path.display()
                )
            }
            Self::Io { path, source } => {
                write!(
                    formatter,
                    "metadata object path '{}': {source}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for MetadataObjectError {}

impl From<MetadataPathError> for MetadataObjectError {
    fn from(error: MetadataPathError) -> Self {
        Self::InvalidPath(error)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StoredMetadataObject<'a> {
    kind: &'static str,
    display_name: &'a str,
    provider: &'a str,
    object_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    overview: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    member_count: Option<usize>,
}

impl<'a> From<&'a MetadataObjectSnapshot> for StoredMetadataObject<'a> {
    fn from(snapshot: &'a MetadataObjectSnapshot) -> Self {
        Self {
            kind: snapshot.kind.as_str(),
            display_name: &snapshot.display_name,
            provider: &snapshot.provider,
            object_id: &snapshot.object_id,
            overview: snapshot.overview.as_deref(),
            member_count: snapshot.member_count,
        }
    }
}

async fn reject_symlink_ancestors(path: &Path) -> Result<(), MetadataObjectError> {
    let mut current = Some(path.to_owned());
    while let Some(candidate) = current {
        match fs::symlink_metadata(&candidate).await {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(MetadataObjectError::SymlinkTarget(candidate));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(MetadataObjectError::Io {
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
            Err(source) => return Err(io_error(&candidate, source)),
        }
    }
    Ok(())
}

async fn write_atomically(target: &Path, bytes: &[u8]) -> Result<(), MetadataObjectError> {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    if let Ok(metadata) = fs::symlink_metadata(target).await
        && metadata.file_type().is_symlink()
    {
        return Err(MetadataObjectError::SymlinkTarget(target.to_owned()));
    }
    let temporary = parent.join(format!(".lux-{}.metadata.tmp", Uuid::now_v7()));
    let result = async {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .await
            .map_err(|source| io_error(&temporary, source))?;
        file.write_all(bytes)
            .await
            .map_err(|source| io_error(&temporary, source))?;
        file.sync_all()
            .await
            .map_err(|source| io_error(&temporary, source))?;
        drop(file);
        fs::rename(&temporary, target)
            .await
            .map_err(|source| io_error(target, source))?;
        let directory = fs::File::open(parent)
            .await
            .map_err(|source| io_error(parent, source))?;
        directory
            .sync_all()
            .await
            .map_err(|source| io_error(parent, source))?;
        Ok(())
    }
    .await;
    if result.is_err() {
        let _ = fs::remove_file(&temporary).await;
    }
    result
}

fn io_error(path: &Path, source: std::io::Error) -> MetadataObjectError {
    MetadataObjectError::Io {
        path: path.to_owned(),
        source,
    }
}
