use std::{sync::Arc, time::Duration};

use tokio::{sync::watch, task::JoinHandle};

use crate::{
    application::nfo::{LocalNfoDetails, local_nfo_details_from_cache_json},
    domain::content_restrictions::project_content_restriction_index,
    storage::{CONTENT_RESTRICTION_INDEX_BATCH_LIMIT, Database, StorageError},
};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ContentRestrictionIndexBatchReport {
    pub(crate) candidates: usize,
    pub(crate) indexed: usize,
    pub(crate) invalid_cache: usize,
    pub(crate) invalid_projection: usize,
    pub(crate) stale_snapshots: usize,
    pub(crate) last_item_id: Option<String>,
}

#[derive(Clone)]
pub(crate) struct ContentRestrictionIndexService {
    database: Database,
}

impl ContentRestrictionIndexService {
    pub(crate) fn new(database: Database) -> Self {
        Self { database }
    }

    pub(crate) async fn index_batch(
        &self,
        after_item_id: Option<&str>,
        batch_size: usize,
    ) -> Result<ContentRestrictionIndexBatchReport, StorageError> {
        let candidates = self
            .database
            .media_content_restriction_index_candidates(
                after_item_id,
                batch_size.min(CONTENT_RESTRICTION_INDEX_BATCH_LIMIT),
            )
            .await?;
        let mut report = ContentRestrictionIndexBatchReport {
            candidates: candidates.len(),
            last_item_id: candidates.last().map(|candidate| candidate.item_id.clone()),
            ..ContentRestrictionIndexBatchReport::default()
        };

        for candidate in candidates {
            let details = match candidate.nfo_metadata_json.as_deref() {
                Some(json) => match local_nfo_details_from_cache_json(json) {
                    Some(details) => details,
                    None => {
                        report.invalid_cache += 1;
                        continue;
                    }
                },
                None => LocalNfoDetails::default(),
            };
            let projection = match project_content_restriction_index(
                details.certification.as_deref(),
                &details.genres,
            ) {
                Ok(projection) => projection,
                Err(_) => {
                    report.invalid_projection += 1;
                    continue;
                }
            };

            if self
                .database
                .replace_media_content_restriction_index_if_nfo_json_matches(
                    &candidate.item_id,
                    candidate.nfo_metadata_json.as_deref(),
                    &projection,
                )
                .await?
            {
                report.indexed += 1;
            } else {
                report.stale_snapshots += 1;
            }
        }

        Ok(report)
    }
}

const CONTENT_RESTRICTION_INDEX_IDLE_RETRY: Duration = Duration::from_secs(10);
const CONTENT_RESTRICTION_INDEX_ERROR_RETRY: Duration = Duration::from_secs(1);
const CONTENT_RESTRICTION_INDEX_BATCH_YIELD: Duration = Duration::from_millis(10);

#[derive(Clone)]
pub(crate) struct ContentRestrictionIndexWorker {
    shutdown_sender: watch::Sender<bool>,
    task: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
}

impl ContentRestrictionIndexWorker {
    pub(crate) fn start(database: Database) -> Self {
        let (shutdown_sender, shutdown_receiver) = watch::channel(false);
        let task = tokio::spawn(run_content_restriction_index_worker(
            database,
            shutdown_receiver,
        ));
        Self {
            shutdown_sender,
            task: Arc::new(tokio::sync::Mutex::new(Some(task))),
        }
    }

    pub(crate) async fn shutdown(&self) {
        self.shutdown_sender.send_replace(true);
        if let Some(task) = self.task.lock().await.take()
            && let Err(error) = task.await
        {
            tracing::warn!(%error, "content restriction index worker stopped unexpectedly");
        }
    }
}

async fn run_content_restriction_index_worker(
    database: Database,
    mut shutdown_receiver: watch::Receiver<bool>,
) {
    let indexer = ContentRestrictionIndexService::new(database);
    let mut cursor: Option<String> = None;
    loop {
        if *shutdown_receiver.borrow() {
            return;
        }

        match indexer
            .index_batch(cursor.as_deref(), CONTENT_RESTRICTION_INDEX_BATCH_LIMIT)
            .await
        {
            Ok(report) if report.candidates > 0 => {
                if report.invalid_cache > 0 || report.invalid_projection > 0 {
                    tracing::warn!(
                        indexed = report.indexed,
                        invalid_cache = report.invalid_cache,
                        invalid_projection = report.invalid_projection,
                        stale_snapshots = report.stale_snapshots,
                        "content restriction index batch left some items unready"
                    );
                } else {
                    tracing::debug!(
                        indexed = report.indexed,
                        stale_snapshots = report.stale_snapshots,
                        "content restriction index batch completed"
                    );
                }
                cursor = report.last_item_id;
                if wait_for_content_restriction_index_worker(
                    &mut shutdown_receiver,
                    CONTENT_RESTRICTION_INDEX_BATCH_YIELD,
                )
                .await
                {
                    return;
                }
            }
            Ok(_) => {
                cursor = None;
                if wait_for_content_restriction_index_worker(
                    &mut shutdown_receiver,
                    CONTENT_RESTRICTION_INDEX_IDLE_RETRY,
                )
                .await
                {
                    return;
                }
            }
            Err(error) => {
                tracing::warn!(%error, "content restriction indexing failed; will retry");
                if wait_for_content_restriction_index_worker(
                    &mut shutdown_receiver,
                    CONTENT_RESTRICTION_INDEX_ERROR_RETRY,
                )
                .await
                {
                    return;
                }
            }
        }
    }
}

async fn wait_for_content_restriction_index_worker(
    shutdown_receiver: &mut watch::Receiver<bool>,
    delay: Duration,
) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(delay) => *shutdown_receiver.borrow(),
        changed = shutdown_receiver.changed() => {
            changed.is_err() || *shutdown_receiver.borrow()
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        application::{
            content_restriction_index::{
                ContentRestrictionIndexService, ContentRestrictionIndexWorker,
            },
            libraries::LibraryService,
        },
        config::Config,
        library::LibraryKind,
        storage::Database,
    };

    #[tokio::test]
    async fn index_batches_use_cached_nfo_and_advance_past_invalid_entries()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        })
        .await?;
        let library = LibraryService::new(database.clone())
            .create_library("Content Index", LibraryKind::Movie, false)
            .await?;
        let library_id = library.id.to_string();
        for (item_id, nfo_json) in [
            ("indexer-a-invalid", Some("not-json")),
            (
                "indexer-b-valid",
                Some(r#"{"certification":"PG-13","genres":[" Family ","FAMILY"]}"#),
            ),
            ("indexer-c-unrated", None),
        ] {
            sqlx::query(
                "INSERT INTO media_items (
                     id, library_id, item_type, title, sort_title, identification_status,
                     nfo_metadata_json
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', ?)",
            )
            .bind(item_id)
            .bind(&library_id)
            .bind(item_id)
            .bind(item_id)
            .bind(nfo_json)
            .execute(database.pool())
            .await?;
        }
        let excessive_genres = (0..=100)
            .map(|index| format!("genre-{index}"))
            .collect::<Vec<_>>();
        let excessive_genres_json = serde_json::to_string(&serde_json::json!({
            "genres": excessive_genres,
        }))?;
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status,
                 nfo_metadata_json
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED', ?)",
        )
        .bind("indexer-d-invalid-tags")
        .bind(&library_id)
        .bind("indexer-d-invalid-tags")
        .bind("indexer-d-invalid-tags")
        .bind(excessive_genres_json)
        .execute(database.pool())
        .await?;

        let indexer = ContentRestrictionIndexService::new(database.clone());
        let first = indexer.index_batch(None, 1).await?;
        assert_eq!(first.indexed, 0);
        assert_eq!(first.invalid_cache, 1);
        assert_eq!(first.last_item_id.as_deref(), Some("indexer-a-invalid"));

        let second = indexer
            .index_batch(first.last_item_id.as_deref(), 1)
            .await?;
        assert_eq!(second.indexed, 1);
        assert_eq!(second.last_item_id.as_deref(), Some("indexer-b-valid"));
        let indexed: (Option<String>,) = sqlx::query_as(
            "SELECT certification_code FROM media_content_restriction_index
             WHERE item_id = 'indexer-b-valid'",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(indexed.0.as_deref(), Some("PG-13"));
        let tags: Vec<String> = sqlx::query_scalar(
            "SELECT tag FROM media_content_restriction_tags
             WHERE item_id = 'indexer-b-valid' ORDER BY tag",
        )
        .fetch_all(database.pool())
        .await?;
        assert_eq!(tags, ["family"]);

        let third = indexer
            .index_batch(second.last_item_id.as_deref(), 1)
            .await?;
        assert_eq!(third.indexed, 1);
        assert_eq!(third.last_item_id.as_deref(), Some("indexer-c-unrated"));
        let fourth = indexer
            .index_batch(third.last_item_id.as_deref(), 1)
            .await?;
        assert_eq!(fourth.indexed, 0);
        assert_eq!(fourth.invalid_projection, 1);
        assert_eq!(
            fourth.last_item_id.as_deref(),
            Some("indexer-d-invalid-tags")
        );
        let fifth = indexer
            .index_batch(fourth.last_item_id.as_deref(), 1)
            .await?;
        assert_eq!(fifth.indexed, 0);
        assert_eq!(fifth.last_item_id, None);

        let ready_unrated: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM media_content_restriction_index
             WHERE item_id = 'indexer-c-unrated' AND certification_code IS NULL",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(ready_unrated, 1);
        let invalid_ready: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM media_content_restriction_index
             WHERE item_id IN ('indexer-a-invalid', 'indexer-d-invalid-tags')",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(invalid_ready, 0);
        Ok(())
    }

    #[tokio::test]
    async fn background_worker_indexes_cached_metadata_and_shuts_down()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::time::Duration;

        let temp_dir = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        })
        .await?;
        let library = LibraryService::new(database.clone())
            .create_library("Content Index Worker", LibraryKind::Movie, false)
            .await?;
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status,
                 nfo_metadata_json
             ) VALUES (?, ?, 'MOVIE', 'Worker Item', 'worker item', 'LOCAL_CONFIRMED', ?)",
        )
        .bind("index-worker-item")
        .bind(library.id.to_string())
        .bind(r#"{"certification":"PG","genres":["Family"]}"#)
        .execute(database.pool())
        .await?;

        let worker = ContentRestrictionIndexWorker::start(database.clone());
        let result = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let indexed: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM media_content_restriction_index
                     WHERE item_id = 'index-worker-item' AND certification_code = 'PG'",
                )
                .fetch_one(database.pool())
                .await?;
                if indexed == 1 {
                    break Ok::<i64, sqlx::Error>(indexed);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        worker.shutdown().await;
        assert_eq!(result??, 1);
        Ok(())
    }
}
