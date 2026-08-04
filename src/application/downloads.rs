use std::{fmt, io, path::PathBuf};

use tokio::fs;
use uuid::Uuid;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use crate::storage::{Database, StorageError};

#[derive(Clone)]
pub struct DownloadService {
    database: Database,
    temporary_directory: PathBuf,
}

#[derive(Debug)]
pub enum DownloadArtifact {
    File {
        path: PathBuf,
        file_name: String,
    },
    Archive {
        path: PathBuf,
        file_name: String,
        size: u64,
    },
}

impl DownloadArtifact {
    pub fn path(&self) -> &PathBuf {
        match self {
            Self::File { path, .. } | Self::Archive { path, .. } => path,
        }
    }

    pub fn file_name(&self) -> &str {
        match self {
            Self::File { file_name, .. } | Self::Archive { file_name, .. } => file_name,
        }
    }

    pub fn size(&self) -> Option<u64> {
        match self {
            Self::File { .. } => None,
            Self::Archive { size, .. } => Some(*size),
        }
    }
}

impl DownloadService {
    pub fn new(database: Database, temporary_directory: PathBuf) -> Self {
        Self {
            database,
            temporary_directory,
        }
    }

    pub async fn prepare(
        &self,
        item_id: &str,
        source_id: Option<&str>,
    ) -> Result<DownloadArtifact, DownloadError> {
        let source = match source_id {
            Some(source_id) => {
                self.database
                    .find_media_source_path_by_id(item_id, source_id)
                    .await?
            }
            None => self.database.find_media_source_path(item_id).await?,
        }
        .ok_or(DownloadError::ItemNotFound)?;
        let root = fs::canonicalize(&source.root_path).await?;
        let media_path = fs::canonicalize(root.join(&source.relative_path)).await?;
        if !media_path.starts_with(&root) || media_path == root {
            return Err(DownloadError::PathOutsideRoot(media_path));
        }
        let metadata = fs::metadata(&media_path).await?;
        if !metadata.is_file() {
            return Err(DownloadError::ItemNotFound);
        }
        let file_name = media_path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| DownloadError::InvalidFileName(media_path.clone()))?
            .to_owned();
        let parent = media_path
            .parent()
            .ok_or_else(|| DownloadError::PathOutsideRoot(media_path.clone()))?;
        let mut files = vec![media_path.clone()];
        let mut entries = fs::read_dir(parent).await?;
        while let Some(entry) = entries.next_entry().await? {
            let candidate = entry.path();
            if candidate == media_path
                || !is_matching_sidecar(&file_name, entry.file_name().to_string_lossy().as_ref())
            {
                continue;
            }
            let file_type = entry.file_type().await?;
            if !file_type.is_file() {
                continue;
            }
            let canonical = fs::canonicalize(&candidate).await?;
            if canonical.starts_with(&root) && canonical != root {
                files.push(canonical);
            }
        }
        files.sort_by(|left, right| left.to_string_lossy().cmp(&right.to_string_lossy()));
        if files.len() == 1 {
            return Ok(DownloadArtifact::File {
                path: media_path,
                file_name,
            });
        }

        let temporary_directory = self.temporary_directory.clone();
        let archive_path =
            temporary_directory.join(format!(".lux-download-{}.zip", Uuid::now_v7()));
        let archive_name = format!(
            "{}.zip",
            media_path
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("media")
        );
        let entries = files
            .into_iter()
            .filter_map(|path| {
                let name = path.file_name()?.to_str()?.to_owned();
                Some((path, name))
            })
            .collect::<Vec<_>>();
        let path_for_worker = archive_path.clone();
        tokio::task::spawn_blocking(move || create_archive(&path_for_worker, entries))
            .await
            .map_err(|error| DownloadError::Archive(error.to_string()))??;
        let size = fs::metadata(&archive_path).await?.len();
        Ok(DownloadArtifact::Archive {
            path: archive_path,
            file_name: archive_name,
            size,
        })
    }
}

fn create_archive(path: &PathBuf, entries: Vec<(PathBuf, String)>) -> Result<(), DownloadError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(path)?;
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (source, name) in entries {
        writer
            .start_file(name, options)
            .map_err(|error| DownloadError::Archive(error.to_string()))?;
        let mut input = std::fs::File::open(source)?;
        io::copy(&mut input, &mut writer)?;
    }
    writer
        .finish()
        .map_err(|error| DownloadError::Archive(error.to_string()))?;
    Ok(())
}

pub(crate) fn is_matching_sidecar(selected_name: &str, candidate_name: &str) -> bool {
    if selected_name.eq_ignore_ascii_case(candidate_name) {
        return true;
    }
    let Some(selected_stem) = selected_name.rsplit_once('.').map(|(stem, _)| stem) else {
        return false;
    };
    let Some((candidate_stem, extension)) = candidate_name.rsplit_once('.') else {
        return false;
    };
    if !is_sidecar_extension(extension) {
        return false;
    }
    candidate_stem.eq_ignore_ascii_case(selected_stem)
        || candidate_stem
            .to_ascii_lowercase()
            .starts_with(&format!("{}.", selected_stem.to_ascii_lowercase()))
}

fn is_sidecar_extension(extension: &str) -> bool {
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "srt"
            | "ass"
            | "ssa"
            | "vtt"
            | "sub"
            | "sup"
            | "idx"
            | "nfo"
            | "jpg"
            | "jpeg"
            | "png"
            | "webp"
    )
}

#[derive(Debug)]
pub enum DownloadError {
    ItemNotFound,
    InvalidFileName(PathBuf),
    PathOutsideRoot(PathBuf),
    Archive(String),
    Io(std::io::Error),
    Storage(StorageError),
}

impl fmt::Display for DownloadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ItemNotFound => formatter.write_str("download item not found"),
            Self::InvalidFileName(path) => {
                write!(formatter, "invalid media filename '{}'", path.display())
            }
            Self::PathOutsideRoot(path) => write!(
                formatter,
                "download path is outside root: {}",
                path.display()
            ),
            Self::Archive(error) => write!(formatter, "download archive failed: {error}"),
            Self::Io(error) => write!(formatter, "download file operation failed: {error}"),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for DownloadError {}

impl From<std::io::Error> for DownloadError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<StorageError> for DownloadError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

#[cfg(test)]
mod tests {
    use super::is_matching_sidecar;

    #[test]
    fn only_selected_source_sidecars_are_downloaded() {
        assert!(is_matching_sidecar(
            "二毛 (2019) - 2160p - H.265 - AAC - test.mkv",
            "二毛 (2019) - 2160p - H.265 - AAC - test.zh.ass",
        ));
        assert!(!is_matching_sidecar(
            "二毛 (2019) - 2160p - H.265 - AAC - test.mkv",
            "二毛 (2019) - 1080p - H.264 - AAC.mkv",
        ));
    }
}
