use std::{fmt, path::PathBuf};

use tokio::fs;

use crate::storage::{Database, StorageError};

#[derive(Clone)]
pub struct DownloadService {
    database: Database,
}

#[derive(Debug)]
pub struct DownloadArtifact {
    path: PathBuf,
    file_name: String,
}

impl DownloadArtifact {
    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    pub fn file_name(&self) -> &str {
        &self.file_name
    }
}

impl DownloadService {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub async fn prepare(
        &self,
        item_id: &str,
        source_id: Option<&str>,
    ) -> Result<DownloadArtifact, DownloadError> {
        let source = match source_id {
            Some(source_id) => {
                self.database
                    .find_download_source_path_by_id(item_id, source_id)
                    .await?
            }
            None => self.database.find_download_source_path(item_id).await?,
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
        Ok(DownloadArtifact {
            path: media_path,
            file_name,
        })
    }
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
