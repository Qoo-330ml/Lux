use std::{fmt, path::PathBuf};

use tokio::fs;

use crate::{
    application::downloads::is_matching_sidecar,
    storage::{Database, StorageError},
};

#[derive(Clone)]
pub struct MediaDeleteService {
    database: Database,
}

impl MediaDeleteService {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub async fn delete(
        &self,
        item_id: &str,
        source_id: Option<&str>,
    ) -> Result<MediaDeleteReport, MediaDeleteError> {
        let source = match source_id {
            Some(source_id) => {
                self.database
                    .find_media_source_path_by_id(item_id, source_id)
                    .await?
            }
            None => self.database.find_media_source_path(item_id).await?,
        }
        .ok_or(MediaDeleteError::ItemNotFound)?;
        let root = fs::canonicalize(&source.root_path).await?;
        let media_path = fs::canonicalize(root.join(&source.relative_path)).await?;
        if !media_path.starts_with(&root) || media_path == root {
            return Err(MediaDeleteError::PathOutsideRoot(media_path));
        }
        let metadata = fs::metadata(&media_path).await?;
        if !metadata.is_file() {
            return Err(MediaDeleteError::ItemNotFound);
        }
        let file_name = media_path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| MediaDeleteError::InvalidFileName(media_path.clone()))?
            .to_owned();
        let parent = media_path
            .parent()
            .ok_or_else(|| MediaDeleteError::PathOutsideRoot(media_path.clone()))?;
        let mut paths = vec![media_path.clone()];
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
                paths.push(canonical);
            }
        }
        for path in &paths {
            fs::remove_file(path).await?;
        }
        if !self
            .database
            .delete_media_source(item_id, &source.source_id)
            .await?
        {
            return Err(MediaDeleteError::ItemNotFound);
        }
        Ok(MediaDeleteReport {
            item_id: item_id.to_owned(),
            source_id: source.source_id,
            deleted_file_count: paths.len(),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaDeleteReport {
    pub item_id: String,
    pub source_id: String,
    pub deleted_file_count: usize,
}

#[derive(Debug)]
pub enum MediaDeleteError {
    ItemNotFound,
    InvalidFileName(PathBuf),
    PathOutsideRoot(PathBuf),
    Io(std::io::Error),
    Storage(StorageError),
}

impl fmt::Display for MediaDeleteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ItemNotFound => formatter.write_str("media item not found"),
            Self::InvalidFileName(path) => {
                write!(formatter, "invalid media filename '{}'", path.display())
            }
            Self::PathOutsideRoot(path) => {
                write!(formatter, "media path is outside root: {}", path.display())
            }
            Self::Io(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for MediaDeleteError {}

impl From<std::io::Error> for MediaDeleteError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<StorageError> for MediaDeleteError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}
