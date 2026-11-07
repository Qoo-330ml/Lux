use std::{sync::Arc, time::Duration};

use tokio::{sync::watch, task::JoinHandle};

use crate::{
    application::nfo::{LocalNfoDetails, local_nfo_details_from_cache_json},
    domain::content_restrictions::project_content_restriction_index,
    storage::{
        CONTENT_RESTRICTION_INDEX_BATCH_LIMIT, ContentRestrictionIndexWrite, Database,
        MediaFacetKind, MediaItemFacetWrite, StorageError,
    },
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
        _after_item_id: Option<&str>,
        batch_size: usize,
    ) -> Result<ContentRestrictionIndexBatchReport, StorageError> {
        let candidates = self
            .database
            .media_content_restriction_index_queued_candidates(
                batch_size.min(CONTENT_RESTRICTION_INDEX_BATCH_LIMIT),
            )
            .await?;
        let mut report = ContentRestrictionIndexBatchReport {
            candidates: candidates.len(),
            last_item_id: candidates.last().map(|candidate| candidate.item_id.clone()),
            ..ContentRestrictionIndexBatchReport::default()
        };
        let mut writes = Vec::with_capacity(candidates.len());
        let mut invalid_item_ids = Vec::new();

        for candidate in candidates {
            let details = match candidate.nfo_metadata_json.as_deref() {
                Some(json) => match local_nfo_details_from_cache_json(json) {
                    Some(details) => details,
                    None => {
                        report.invalid_cache += 1;
                        invalid_item_ids.push(candidate.item_id);
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
                    invalid_item_ids.push(candidate.item_id);
                    continue;
                }
            };
            writes.push(ContentRestrictionIndexWrite {
                item_id: candidate.item_id,
                expected_nfo_json: candidate.nfo_metadata_json,
                certification_code: projection.certification_code,
                tags: projection.tags,
                facets: facets_from_details(&details.genres, &details.studios, &details.tags),
            });
        }

        let outcome = self
            .database
            .apply_content_restriction_index_batch(&writes, &invalid_item_ids)
            .await?;
        report.indexed = outcome.indexed;
        report.stale_snapshots = outcome.stale;
        Ok(report)
    }
}

const MAX_FACET_NAME_CHARS: usize = 256;

/// Genre, studio and tag values worth indexing: trimmed, bounded, de-duplicated case-insensitively
/// (the first spelling wins as the display name). Tags are the NFO `<tag>` values and stay a
/// separate facet kind from genres, which the parental-control projection keeps using.
fn facets_from_details(
    genres: &[String],
    studios: &[String],
    tags: &[String],
) -> Vec<MediaItemFacetWrite> {
    let mut facets: Vec<MediaItemFacetWrite> = Vec::new();
    for (kind, values) in [
        (MediaFacetKind::Genre, genres),
        (MediaFacetKind::Studio, studios),
        (MediaFacetKind::Tag, tags),
    ] {
        for value in values {
            let name = value.trim();
            if name.is_empty() || name.chars().count() > MAX_FACET_NAME_CHARS {
                continue;
            }
            let name_key = name.to_lowercase();
            if facets
                .iter()
                .any(|facet| facet.kind == kind && facet.name_key == name_key)
            {
                continue;
            }
            facets.push(MediaItemFacetWrite {
                kind,
                name_key,
                name: name.to_owned(),
            });
        }
    }
    facets
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
                     id, library_id, item_type, title, sort_title, identification_status
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
            )
            .bind(item_id)
            .bind(&library_id)
            .bind(item_id)
            .bind(item_id)
            .execute(database.pool())
            .await?;
            if let Some(json) = nfo_json {
                sqlx::query(
                    "INSERT INTO media_item_nfo_cache (item_id, json, bytes)
                     VALUES (?, ?, length(CAST(? AS BLOB)))",
                )
                .bind(item_id)
                .bind(json)
                .bind(json)
                .execute(database.pool())
                .await?;
            }
        }
        let excessive_genres = (0..=100)
            .map(|index| format!("genre-{index}"))
            .collect::<Vec<_>>();
        let excessive_genres_json = serde_json::to_string(&serde_json::json!({
            "genres": excessive_genres,
        }))?;
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind("indexer-d-invalid-tags")
        .bind(&library_id)
        .bind("indexer-d-invalid-tags")
        .bind("indexer-d-invalid-tags")
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO media_item_nfo_cache (item_id, json, bytes)
             VALUES ('indexer-d-invalid-tags', ?, length(CAST(? AS BLOB)))",
        )
        .bind(&excessive_genres_json)
        .bind(&excessive_genres_json)
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
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', 'Worker Item', 'worker item', 'LOCAL_CONFIRMED')",
        )
        .bind("index-worker-item")
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO media_item_nfo_cache (item_id, json, bytes)
             VALUES ('index-worker-item', ?, length(CAST(? AS BLOB)))",
        )
        .bind(r#"{"certification":"PG","genres":["Family"]}"#)
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

    #[tokio::test]
    async fn index_batches_project_genres_and_studios_into_facets()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        })
        .await?;
        let library = LibraryService::new(database.clone())
            .create_library("Facet Index", LibraryKind::Movie, false)
            .await?;
        let library_id = library.id.to_string();
        let nfo_json = r#"{"genres":[" Sci-Fi ","sci-fi","Drama"],"studios":["A24"]}"#;
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('facet-a', ?, 'MOVIE', 'facet-a', 'facet-a', 'LOCAL_CONFIRMED')",
        )
        .bind(&library_id)
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO media_item_nfo_cache (item_id, json, bytes)
             VALUES ('facet-a', ?, length(CAST(? AS BLOB)))",
        )
        .bind(nfo_json)
        .bind(nfo_json)
        .execute(database.pool())
        .await?;

        let report = ContentRestrictionIndexService::new(database.clone())
            .index_batch(None, 10)
            .await?;
        assert_eq!(report.indexed, 1);
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT kind, name_key, name FROM media_item_facets
             WHERE item_id = 'facet-a' ORDER BY kind, name_key",
        )
        .fetch_all(database.pool())
        .await?;
        assert_eq!(
            rows,
            [
                ("GENRE".to_owned(), "drama".to_owned(), "Drama".to_owned()),
                ("GENRE".to_owned(), "sci-fi".to_owned(), "Sci-Fi".to_owned()),
                ("STUDIO".to_owned(), "a24".to_owned(), "A24".to_owned()),
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn index_batches_project_nfo_tags_into_tag_facets()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        })
        .await?;
        let library = LibraryService::new(database.clone())
            .create_library("Tag Facet Index", LibraryKind::Movie, false)
            .await?;
        let library_id = library.id.to_string();
        let nfo_json = r#"{"genres":["Drama"],"studios":["A24"],"tags":[" Sci-Fi ","sci-fi","Director's Cut"]}"#;
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('facet-tags-a', ?, 'MOVIE', 'facet-tags-a', 'facet-tags-a', 'LOCAL_CONFIRMED')",
        )
        .bind(&library_id)
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO media_item_nfo_cache (item_id, json, bytes)
             VALUES ('facet-tags-a', ?, length(CAST(? AS BLOB)))",
        )
        .bind(nfo_json)
        .bind(nfo_json)
        .execute(database.pool())
        .await?;

        let report = ContentRestrictionIndexService::new(database.clone())
            .index_batch(None, 10)
            .await?;
        assert_eq!(report.indexed, 1);
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT kind, name_key, name FROM media_item_facets
             WHERE item_id = 'facet-tags-a' ORDER BY kind, name_key",
        )
        .fetch_all(database.pool())
        .await?;
        assert_eq!(
            rows,
            [
                ("GENRE".to_owned(), "drama".to_owned(), "Drama".to_owned()),
                ("STUDIO".to_owned(), "a24".to_owned(), "A24".to_owned()),
                (
                    "TAG".to_owned(),
                    "director's cut".to_owned(),
                    "Director's Cut".to_owned()
                ),
                ("TAG".to_owned(), "sci-fi".to_owned(), "Sci-Fi".to_owned()),
            ]
        );

        // The parental-control projection stays on genres; NFO tags must not leak into it.
        let restriction_tags: Vec<String> = sqlx::query_scalar(
            "SELECT tag FROM media_content_restriction_tags
             WHERE item_id = 'facet-tags-a' ORDER BY tag",
        )
        .fetch_all(database.pool())
        .await?;
        assert_eq!(restriction_tags, ["drama"]);
        Ok(())
    }
}
