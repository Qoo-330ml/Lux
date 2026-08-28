use std::{
    collections::HashSet,
    fmt,
    path::{Component, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::json;
use tokio::fs;

use crate::{
    application::{
        downloads::is_matching_sidecar,
        webhooks::{WebhookEventType, WebhookService},
    },
    storage::{Database, StorageError},
};

#[derive(Clone)]
pub struct MediaDeleteService {
    database: Database,
    webhooks: Option<WebhookService>,
}

impl MediaDeleteService {
    pub fn new(database: Database) -> Self {
        Self {
            database,
            webhooks: None,
        }
    }

    pub fn with_webhooks(mut self, webhooks: WebhookService) -> Self {
        self.webhooks = Some(webhooks);
        self
    }

    pub async fn delete(
        &self,
        item_id: &str,
        source_id: Option<&str>,
    ) -> Result<MediaDeleteReport, MediaDeleteError> {
        let sources = match source_id {
            Some(source_id) => self
                .database
                .find_deletable_media_source_path_by_id(item_id, source_id)
                .await?
                .into_iter()
                .collect(),
            None => {
                self.database
                    .find_deletable_media_source_paths(item_id)
                    .await?
            }
        }
        .into_iter()
        .collect::<Vec<_>>();
        if sources.is_empty() {
            return Err(MediaDeleteError::ItemNotFound);
        }

        let mut paths = Vec::new();
        let mut seen_paths = HashSet::new();
        for source in &sources {
            let root = fs::canonicalize(&source.root_path).await?;
            let relative_path = PathBuf::from(&source.relative_path);
            if relative_path.is_absolute()
                || relative_path
                    .components()
                    .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
            {
                return Err(MediaDeleteError::PathOutsideRoot(root.join(relative_path)));
            }

            let media_path = match fs::canonicalize(root.join(&source.relative_path)).await {
                Ok(media_path) => {
                    if !media_path.starts_with(&root) || media_path == root {
                        return Err(MediaDeleteError::PathOutsideRoot(media_path));
                    }
                    let metadata = fs::metadata(&media_path).await?;
                    if !metadata.is_file() {
                        return Err(MediaDeleteError::ItemNotFound);
                    }
                    Some(media_path)
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(error.into()),
            };
            let Some(media_path) = media_path else {
                continue;
            };
            let file_name = media_path
                .file_name()
                .and_then(|value| value.to_str())
                .ok_or_else(|| MediaDeleteError::InvalidFileName(media_path.clone()))?
                .to_owned();
            let parent = media_path
                .parent()
                .ok_or_else(|| MediaDeleteError::PathOutsideRoot(media_path.clone()))?;
            if seen_paths.insert(media_path.clone()) {
                paths.push(media_path.clone());
            }
            let mut entries = fs::read_dir(parent).await?;
            while let Some(entry) = entries.next_entry().await? {
                let candidate = entry.path();
                if candidate == media_path
                    || !is_matching_sidecar(
                        &file_name,
                        entry.file_name().to_string_lossy().as_ref(),
                    )
                {
                    continue;
                }
                let file_type = entry.file_type().await?;
                if !file_type.is_file() {
                    continue;
                }
                let canonical = fs::canonicalize(&candidate).await?;
                if canonical.starts_with(&root)
                    && canonical != root
                    && seen_paths.insert(canonical.clone())
                {
                    paths.push(canonical);
                }
            }
        }
        for path in &paths {
            fs::remove_file(path).await?;
        }
        for source in &sources {
            if !self
                .database
                .delete_media_source(&source.item_id, &source.source_id)
                .await?
            {
                return Err(MediaDeleteError::ItemNotFound);
            }
        }
        let report = MediaDeleteReport {
            item_id: item_id.to_owned(),
            source_ids: sources
                .iter()
                .map(|source| source.source_id.clone())
                .collect(),
            deleted_file_count: paths.len(),
        };
        if let Some(webhooks) = self.webhooks.as_ref() {
            for source in &sources {
                let dedupe_key = format!("media-removed:{}:{}", source.item_id, source.source_id);
                if let Err(_error) = webhooks
                    .publish(
                        WebhookEventType::MediaRemoved,
                        &dedupe_key,
                        unix_now(),
                        json!({
                            "itemId": source.item_id.as_str(),
                            "sourceId": source.source_id.as_str(),
                            "deletedFileCount": report.deleted_file_count,
                        }),
                    )
                    .await
                {
                    tracing::warn!(
                        item_id = %source.item_id,
                        event_type = WebhookEventType::MediaRemoved.as_str(),
                        "failed to enqueue webhook event"
                    );
                }
            }
        }
        Ok(report)
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(0)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaDeleteReport {
    pub item_id: String,
    pub source_ids: Vec<String>,
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
