use std::{
    collections::{HashMap, HashSet},
    fmt,
    path::{Component, PathBuf},
    sync::{Arc, OnceLock, Weak},
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::json;
use tokio::{
    fs,
    sync::{Mutex, OwnedMutexGuard},
};

use crate::{
    application::{
        downloads::is_matching_sidecar,
        notification_template::bounded_display_text,
        webhooks::{WebhookEventType, WebhookService},
    },
    storage::{Database, StorageError},
};

static MEDIA_DELETE_LOCKS: OnceLock<Mutex<HashMap<String, Weak<Mutex<()>>>>> = OnceLock::new();

async fn acquire_media_delete_lock(item_id: &str) -> OwnedMutexGuard<()> {
    let locks = MEDIA_DELETE_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let lock = {
        let mut locks = locks.lock().await;
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(item_id).and_then(Weak::upgrade) {
            lock
        } else {
            let lock = Arc::new(Mutex::new(()));
            locks.insert(item_id.to_owned(), Arc::downgrade(&lock));
            lock
        }
    };
    lock.lock_owned().await
}

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
        let deletion_guard = acquire_media_delete_lock(item_id).await;
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
        let (item_title, library_name) = if self.webhooks.is_some() {
            let item_title = self
                .database
                .find_media_item_metadata(item_id)
                .await
                .ok()
                .flatten()
                .map(|item| bounded_display_text(&item.title));
            let library_name = match self
                .database
                .find_item_library_id(item_id)
                .await
                .ok()
                .flatten()
            {
                Some(library_id) => self
                    .database
                    .find_library(&library_id)
                    .await
                    .ok()
                    .flatten()
                    .map(|library| bounded_display_text(&library.name)),
                None => None,
            };
            (item_title, library_name)
        } else {
            (None, None)
        };

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
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let mut staged_paths = Vec::with_capacity(paths.len());
        for (index, path) in paths.iter().enumerate() {
            let parent = path
                .parent()
                .ok_or_else(|| MediaDeleteError::PathOutsideRoot(path.clone()))?;
            let staged = parent.join(format!(".lux-delete-{nonce}-{index}"));
            if let Err(error) = fs::rename(path, &staged).await {
                restore_staged_paths(&staged_paths).await;
                return Err(error.into());
            }
            staged_paths.push((path.clone(), staged));
        }
        let source_keys = sources
            .iter()
            .map(|source| (source.item_id.clone(), source.source_id.clone()))
            .collect::<Vec<_>>();
        match self
            .database
            .delete_media_sources_atomically(&source_keys)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                restore_staged_paths(&staged_paths).await;
                return Err(MediaDeleteError::ItemNotFound);
            }
            Err(error) => {
                restore_staged_paths(&staged_paths).await;
                return Err(error.into());
            }
        }
        for (_, staged) in &staged_paths {
            if let Err(error) = fs::remove_file(staged).await {
                tracing::warn!(path = %staged.display(), %error, "staged media deletion cleanup failed");
            }
        }
        drop(deletion_guard);
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
                            "itemTitle": item_title.as_deref(),
                            "libraryName": library_name.as_deref(),
                            "removedCount": 1,
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

async fn restore_staged_paths(staged_paths: &[(PathBuf, PathBuf)]) {
    for (original, staged) in staged_paths.iter().rev() {
        if let Err(error) = fs::rename(staged, original).await {
            tracing::error!(
                original_path = %original.display(),
                staged_path = %staged.display(),
                %error,
                "failed to restore staged media after deletion rollback"
            );
        }
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{MediaDeleteService, acquire_media_delete_lock};
    use crate::{
        application::{libraries::LibraryService, scanner::LibraryScanner},
        config::Config,
        library::LibraryKind,
        storage::Database,
    };
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn delete_waits_for_other_deletion_of_the_same_item()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let libraries = LibraryService::new(database.clone());
        let library = libraries
            .create_library("Movies", LibraryKind::Movie, false)
            .await?;
        let root = temp_dir.path().join("Movies");
        tokio::fs::create_dir_all(&root).await?;
        let media_file = root.join("Example.Movie.2024.mkv");
        tokio::fs::write(&media_file, b"fixture").await?;
        libraries
            .add_root(library.id, root.to_str().ok_or("non-UTF-8 path")?)
            .await?;
        LibraryScanner::new(database.clone())
            .scan_movie_library(library.id)
            .await?;

        let (item_id, source_id): (String, String) = sqlx::query_as(
            "SELECT mi.id, ms.id FROM media_items mi
             JOIN media_sources ms ON ms.item_id = mi.id
             WHERE mi.library_id = ? AND mi.item_type = 'MOVIE' LIMIT 1",
        )
        .bind(library.id.to_string())
        .fetch_one(database.pool())
        .await?;
        let deletion_guard = acquire_media_delete_lock(&item_id).await;
        let deletion = MediaDeleteService::new(database.clone());
        let deletion_item_id = item_id.clone();
        let (started_tx, started_rx) = oneshot::channel();
        let mut task = tokio::spawn(async move {
            let _ = started_tx.send(());
            deletion.delete(&deletion_item_id, Some(&source_id)).await
        });
        started_rx.await?;
        assert!(
            tokio::time::timeout(Duration::from_secs(1), &mut task)
                .await
                .is_err(),
            "deletion must wait while another request holds the item lock"
        );

        let remaining_sources: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM media_sources WHERE item_id = ?")
                .bind(&item_id)
                .fetch_one(database.pool())
                .await?;
        assert_eq!(remaining_sources, 1);
        assert!(media_file.exists());

        drop(deletion_guard);
        task.await??;
        assert!(!media_file.exists());
        Ok(())
    }
}
