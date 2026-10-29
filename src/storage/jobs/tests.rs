use super::{
    Database, MAX_MEDIA_SOURCE_DELETE_BATCH_SIZE, NewScanManifest, NewScanManifestDelta,
    NewScanManifestDiscoveryChunk, NewScanManifestEntry, NewScanManifestRoot,
    NewScanManifestSeenFilesystemEntry, prune_sidecar_directories, sidecar_target_query,
};
use crate::{
    config::{Config, DatabaseConfiguration, PostgresConnection},
    storage::{ScanManifestRemovalCandidateQuery, StoredScanManifestDirectory},
};
use sqlx::Row;

#[tokio::test]
async fn shutdown_cancels_legacy_scan_jobs_in_one_bounded_update()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    database
        .query(
            "WITH RECURSIVE numbers(n) AS (
                 SELECT 1 UNION ALL SELECT n + 1 FROM numbers WHERE n < 205
             )
             INSERT INTO libraries (id, name, kind)
             SELECT 'shutdown-lib-' || n, 'Library ' || n, 'MOVIE' FROM numbers",
        )
        .execute(database.pool())
        .await?;
    database
        .query(
            "WITH RECURSIVE numbers(n) AS (
                 SELECT 1 UNION ALL SELECT n + 1 FROM numbers WHERE n < 205
             )
             INSERT INTO scan_jobs (
                 id, library_id, job_type, status, generation, scan_phase
             )
             SELECT 'legacy-' || n, 'shutdown-lib-' || n, 'RECONCILE_LIBRARY', 'RUNNING',
                    'generation', 'DISCOVERY'
             FROM numbers",
        )
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO libraries (id, name, kind)
             VALUES ('shutdown-failed-lib', 'Failed library', 'MOVIE')",
        )
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO scan_jobs (
                 id, library_id, job_type, status, generation, scan_phase, cancel_requested
             ) VALUES (
                 'legacy-failed-with-stale-cancel', 'shutdown-failed-lib',
                 'RECONCILE_LIBRARY', 'FAILED', 'generation', 'IDLE', 1
             )",
        )
        .execute(database.pool())
        .await?;

    database.reset_query_count();
    assert_eq!(database.cancel_incomplete_jobs_for_shutdown().await?, 205);
    assert_eq!(database.query_count(), 10);
    let cancelled: i64 = database
        .query_scalar("SELECT COUNT(*) FROM scan_jobs WHERE status = 'CANCELLED'")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(cancelled, 205);
    let failed_status: String = database
        .query_scalar("SELECT status FROM scan_jobs WHERE id = 'legacy-failed-with-stale-cancel'")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(failed_status, "FAILED");
    Ok(())
}

#[tokio::test]
async fn manifest_local_metadata_outbox_inserts_three_batches_with_one_query()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    assert_manifest_local_metadata_outbox_batches(&database).await?;
    database.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a disposable local PostgreSQL database"]
async fn manifest_local_metadata_outbox_conflicts_are_validated_on_postgres()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let connection = DatabaseConfiguration::Postgres(PostgresConnection {
        host: std::env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
        port: std::env::var("POSTGRES_TEST_PORT")
            .unwrap_or_else(|_| "55432".into())
            .parse()?,
        database: std::env::var("POSTGRES_TEST_DATABASE")?,
        username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".into()),
        password: std::env::var("POSTGRES_TEST_PASSWORD")?,
        ssl_mode: "disable".into(),
    });
    let database = Database::connect_with_configuration(&config, &connection).await?;
    assert_manifest_local_metadata_outbox_batches(&database).await?;
    database.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a disposable local PostgreSQL database"]
async fn scan_job_file_change_queue_is_revision_safe_on_postgres()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let connection = DatabaseConfiguration::Postgres(PostgresConnection {
        host: std::env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
        port: std::env::var("POSTGRES_TEST_PORT")
            .unwrap_or_else(|_| "55432".into())
            .parse()?,
        database: std::env::var("POSTGRES_TEST_DATABASE")?,
        username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".into()),
        password: std::env::var("POSTGRES_TEST_PASSWORD")?,
        ssl_mode: "disable".into(),
    });
    let database = Database::connect_with_configuration(&config, &connection).await?;
    let suffix = uuid::Uuid::now_v7().simple().to_string();
    let library_id = format!("file-change-library-{suffix}");
    let root_id = format!("file-change-root-{suffix}");
    let job_id = format!("file-change-job-{suffix}");
    let relative_path = "Movie (2025)/Movie.en.srt";

    database
        .query("INSERT INTO libraries (id, name, kind) VALUES (?, ?, 'MOVIE')")
        .bind(&library_id)
        .bind(&library_id)
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO library_roots (
                 id, library_id, canonical_path, display_path, is_available, is_writable
             ) VALUES (?, ?, '/media/file-change-test', '/media/file-change-test', 1, 0)",
        )
        .bind(&root_id)
        .bind(&library_id)
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO scan_jobs (id, library_id, job_type, status, generation)
             VALUES (?, ?, 'INCREMENTAL_SCAN', 'RUNNING', 'file-change-test')",
        )
        .bind(&job_id)
        .bind(&library_id)
        .execute(database.pool())
        .await?;

    assert!(
        database
            .record_scan_job_file_change(&job_id, &root_id, relative_path, "ADD")
            .await?
    );
    let first = database
        .list_pending_scan_job_file_changes(&job_id, None, 10)
        .await?
        .into_iter()
        .next()
        .ok_or("initial file change was not queued")?;
    let first_revision = first.file_change_revision.ok_or("missing revision")?;
    assert!(
        database
            .set_scan_job_file_change_state(
                &job_id,
                &root_id,
                relative_path,
                first_revision,
                "RUNNING",
                None,
            )
            .await?
    );
    assert!(
        database
            .record_scan_job_file_change(&job_id, &root_id, relative_path, "CHANGE")
            .await?
    );
    let updated = database
        .list_pending_scan_job_file_changes(&job_id, None, 10)
        .await?
        .into_iter()
        .next()
        .ok_or("updated file change was not requeued")?;
    let updated_revision = updated.file_change_revision.ok_or("missing revision")?;
    assert!(updated_revision > first_revision);
    assert!(
        !database
            .set_scan_job_file_change_state(
                &job_id,
                &root_id,
                relative_path,
                first_revision,
                "DONE",
                None,
            )
            .await?
    );
    assert!(
        database
            .set_scan_job_file_change_state(
                &job_id,
                &root_id,
                relative_path,
                updated_revision,
                "RUNNING",
                None,
            )
            .await?
    );
    database
        .requeue_running_scan_job_file_changes(&job_id)
        .await?;
    let resumed = database
        .list_pending_scan_job_file_changes(&job_id, None, 10)
        .await?
        .into_iter()
        .next()
        .ok_or("running file change was not recovered")?;
    assert!(
        database
            .set_scan_job_file_change_state(
                &job_id,
                &root_id,
                relative_path,
                resumed.file_change_revision.ok_or("missing revision")?,
                "DONE",
                None,
            )
            .await?
    );

    database
        .query("DELETE FROM libraries WHERE id = ?")
        .bind(&library_id)
        .execute(database.pool())
        .await?;
    database.close().await;
    Ok(())
}

async fn assert_manifest_local_metadata_outbox_batches(
    database: &Database,
) -> Result<(), Box<dyn std::error::Error>> {
    database
        .query("INSERT INTO libraries (id, name, kind) VALUES ('lib', 'Library', 'MOVIE')")
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO library_roots (
                 id, library_id, canonical_path, display_path, is_available, is_writable
             ) VALUES ('root', 'lib', '/media', '/media', 1, 0)",
        )
        .execute(database.pool())
        .await?;
    let source_refs = (0..513)
        .map(|index| (format!("item-{index}"), format!("source-{index}")))
        .collect::<Vec<_>>();
    let source_refs = source_refs
        .iter()
        .map(|(item_id, source_id)| (item_id.as_str(), source_id.as_str()))
        .collect::<Vec<_>>();

    database.reset_query_count();
    let mut transaction = database.begin_metadata_write_transaction().await?;
    database
        .enqueue_manifest_local_metadata_refs_in_transaction(
            &mut transaction,
            "job",
            "root",
            &source_refs,
            0,
        )
        .await?;
    transaction.commit().await?;
    assert_eq!(
        database.query_count(),
        1,
        "three durable 256-source batches should share one INSERT query"
    );

    let batches = database
        .list_scan_local_metadata_batches(None, None, 10)
        .await?;
    assert_eq!(batches.len(), 3);
    assert_eq!(
        batches
            .iter()
            .map(|batch| batch.source_count)
            .collect::<Vec<_>>(),
        vec![256, 256, 1]
    );

    database.reset_query_count();
    let mut transaction = database.begin_metadata_write_transaction().await?;
    database
        .enqueue_manifest_local_metadata_refs_in_transaction(
            &mut transaction,
            "job",
            "root",
            &source_refs,
            0,
        )
        .await?;
    transaction.commit().await?;
    assert_eq!(
        database.query_count(),
        2,
        "an idempotent replay should batch insert and validate conflicts"
    );

    let changed_source_refs = source_refs
        .iter()
        .map(|(item_id, source_id)| {
            if *source_id == "source-0" {
                (*item_id, "changed-source")
            } else {
                (*item_id, *source_id)
            }
        })
        .collect::<Vec<_>>();
    let additional_sources = (513..769)
        .map(|index| (format!("item-{index}"), format!("source-{index}")))
        .collect::<Vec<_>>();
    let mut changed_source_refs = changed_source_refs;
    changed_source_refs.extend(
        additional_sources
            .iter()
            .map(|(item_id, source_id)| (item_id.as_str(), source_id.as_str())),
    );
    database.reset_query_count();
    let mut transaction = database.begin_metadata_write_transaction().await?;
    let enqueue_result = database
        .enqueue_manifest_local_metadata_refs_in_transaction(
            &mut transaction,
            "job",
            "root",
            &changed_source_refs,
            0,
        )
        .await;
    assert!(
        enqueue_result.is_err(),
        "changed batch content must conflict"
    );
    transaction.rollback().await?;
    assert_eq!(database.query_count(), 2);

    database
        .query(
            "UPDATE scan_local_metadata_batches SET id = 'different-id'
             WHERE job_id = 'job' AND library_root_id = 'root' AND batch_sequence = 0",
        )
        .execute(database.pool())
        .await?;
    database.reset_query_count();
    let mut transaction = database.begin_metadata_write_transaction().await?;
    let enqueue_result = database
        .enqueue_manifest_local_metadata_refs_in_transaction(
            &mut transaction,
            "job",
            "root",
            &source_refs,
            0,
        )
        .await;
    assert!(enqueue_result.is_err(), "changed batch ID must conflict");
    transaction.rollback().await?;
    assert_eq!(database.query_count(), 2);

    database
        .query(
            "UPDATE scan_local_metadata_batches SET id = 'job:root:0', source_count = 255
             WHERE job_id = 'job' AND library_root_id = 'root' AND batch_sequence = 0",
        )
        .execute(database.pool())
        .await?;
    database.reset_query_count();
    let mut transaction = database.begin_metadata_write_transaction().await?;
    let enqueue_result = database
        .enqueue_manifest_local_metadata_refs_in_transaction(
            &mut transaction,
            "job",
            "root",
            &source_refs,
            0,
        )
        .await;
    assert!(
        enqueue_result.is_err(),
        "changed source count must conflict"
    );
    transaction.rollback().await?;
    assert_eq!(database.query_count(), 2);

    let existing_count: i64 = database
        .query_scalar(
            "SELECT COUNT(*) FROM scan_local_metadata_batches
             WHERE job_id = 'job' AND library_root_id = 'root'",
        )
        .fetch_one(database.pool())
        .await?;
    assert_eq!(
        existing_count, 3,
        "failed replay must not alter stored batches"
    );
    Ok(())
}

#[tokio::test]
async fn finishing_a_cancelled_strm_job_cannot_restore_terminal_status()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    database
        .query("INSERT INTO libraries (id, name, kind) VALUES ('lib', 'Library', 'MOVIE')")
        .execute(database.pool())
        .await?;
    database
        .query("INSERT INTO strm_probe_jobs (id, operation_id, library_id, status, concurrency) VALUES ('job', 'op', 'lib', 'RUNNING', 1)")
        .execute(database.pool())
        .await?;
    database.request_strm_probe_job_cancel("job").await?;
    database
        .finish_strm_probe_job("job", "COMPLETED", None)
        .await?;
    let status: String = database
        .query_scalar("SELECT status FROM strm_probe_jobs WHERE id = 'job'")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(status, "CANCELLED");
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn finishing_a_cancelled_scan_job_cannot_restore_terminal_status()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    database
        .query("INSERT INTO libraries (id, name, kind) VALUES ('lib', 'Library', 'MOVIE')")
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO scan_jobs (id, library_id, job_type, status, generation)
             VALUES ('job', 'lib', 'RECONCILE_LIBRARY', 'RUNNING', 'generation')",
        )
        .execute(database.pool())
        .await?;
    database.request_scan_job_cancel("job").await?;
    database
        .finish_scan_job("job", "COMPLETED", Some("late worker completion"))
        .await?;
    let (status, error): (String, Option<String>) = database
        .query_as("SELECT status, error FROM scan_jobs WHERE id = 'job'")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(status, "CANCELLED");
    assert_eq!(error, None);
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn local_metadata_completeness_reuses_current_source_identities_and_expands_episode_hierarchy()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    database
        .query("INSERT INTO libraries (id, name, kind) VALUES ('lib', 'Library', 'SERIES')")
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO library_roots (
                 id, library_id, canonical_path, display_path, is_available, is_writable
             ) VALUES ('root', 'lib', '/media', '/media', 1, 0)",
        )
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO media_items (
                 id, library_id, item_type, parent_id, series_id, season_number,
                 episode_number, title, sort_title, production_year, identification_status
             ) VALUES
                ('series', 'lib', 'SERIES', NULL, NULL, NULL, NULL,
                 'Show', 'show', 2024, 'LOCAL_CONFIRMED'),
                ('season', 'lib', 'SEASON', 'series', 'series', 1, NULL,
                 'Season 01', 'season 01', NULL, 'LOCAL_CONFIRMED'),
                ('item', 'lib', 'EPISODE', 'season', 'series', 1, 1,
                 'Episode', 'episode', NULL, 'LOCAL_CONFIRMED')",
        )
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO filesystem_entries (
                 id, library_root_id, relative_path, entry_kind, size, modified_at,
                 last_seen_generation
             ) VALUES
                ('entry-old', 'root', 'Movie/movie.mkv', 'FILE', 10, 1, 'generation'),
                ('entry-new', 'root', 'Movie/movie-alt.mkv', 'FILE', 10, 1, 'generation')",
        )
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO media_sources (
                 id, item_id, source_kind, filesystem_entry_id, is_default, probe_status
             ) VALUES
                ('source-old', 'item', 'LOCAL_FILE', 'entry-old', 1, 'READY'),
                ('source-new', 'item', 'LOCAL_FILE', 'entry-new', 0, 'READY')",
        )
        .execute(database.pool())
        .await?;

    let source_ids = vec!["entry-old".to_owned()];
    database.reset_query_count();
    let sources = database
        .list_scan_local_metadata_sources(&source_ids)
        .await?;
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].source_id, "source-old");
    let metadata = database
        .list_active_media_item_metadata_with_libraries(&["item".to_owned()])
        .await?;
    assert_eq!(metadata.len(), 1);
    assert_eq!(
        database.query_count(),
        3,
        "the old path expands the directory again"
    );

    let identities = vec![("item".to_owned(), "source-old".to_owned())];
    database.reset_query_count();
    assert_eq!(
        database
            .list_current_scan_local_metadata_item_ids(&identities)
            .await?,
        vec!["item".to_owned(), "season".to_owned(), "series".to_owned()]
    );
    let metadata = database
        .list_active_media_item_metadata_with_libraries(&["item".to_owned()])
        .await?;
    assert_eq!(metadata.len(), 1);
    assert_eq!(database.query_count(), 2);

    database
        .query("UPDATE media_sources SET is_default = 0 WHERE id = 'source-old'")
        .execute(database.pool())
        .await?;
    database
        .query("UPDATE media_sources SET is_default = 1 WHERE id = 'source-new'")
        .execute(database.pool())
        .await?;
    database.reset_query_count();
    assert!(
        database
            .list_current_scan_local_metadata_item_ids(&identities)
            .await?
            .is_empty(),
        "a preferred source changed after NFO processing, so the old identity is stale"
    );
    assert_eq!(
        database
            .list_current_scan_local_metadata_item_ids(&[(
                "item".to_owned(),
                "source-new".to_owned(),
            )])
            .await?,
        vec!["item".to_owned(), "season".to_owned(), "series".to_owned()]
    );
    database
        .query("UPDATE media_items SET removed_at = unixepoch() WHERE id = 'item'")
        .execute(database.pool())
        .await?;
    assert!(
        database
            .list_current_scan_local_metadata_item_ids(&[(
                "item".to_owned(),
                "source-new".to_owned(),
            )])
            .await?
            .is_empty(),
        "a removed item cannot pass freshness validation"
    );
    database
        .query("UPDATE media_items SET removed_at = NULL WHERE id = 'item'")
        .execute(database.pool())
        .await?;
    database
        .query("UPDATE filesystem_entries SET is_missing = 1 WHERE id = 'entry-new'")
        .execute(database.pool())
        .await?;
    assert!(
        database
            .list_current_scan_local_metadata_item_ids(&[(
                "item".to_owned(),
                "source-new".to_owned(),
            )])
            .await?
            .is_empty(),
        "a preferred source with a missing entry is not current"
    );
    database
        .query("UPDATE filesystem_entries SET is_missing = 0 WHERE id = 'entry-new'")
        .execute(database.pool())
        .await?;
    database
        .query("DELETE FROM media_sources WHERE id = 'source-new'")
        .execute(database.pool())
        .await?;
    assert!(
        database
            .list_current_scan_local_metadata_item_ids(&[(
                "item".to_owned(),
                "source-new".to_owned(),
            )])
            .await?
            .is_empty(),
        "a source deleted after NFO processing cannot pass freshness validation"
    );
    Ok(())
}

#[tokio::test]
async fn scan_job_status_counts_use_covering_status_indexes()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    database
        .query("INSERT INTO libraries (id, name, kind) VALUES ('lib', 'Library', 'MOVIE')")
        .execute(database.pool())
        .await?;
    for (id, job_type, status) in [
        ("pending", "RECONCILE_LIBRARY", "PENDING"),
        ("running", "INCREMENTAL_SCAN", "RUNNING"),
        ("failed", "RECONCILE_LIBRARY", "FAILED"),
        ("completed", "INCREMENTAL_SCAN", "COMPLETED"),
    ] {
        database
            .query(
                "INSERT INTO scan_jobs (id, library_id, job_type, status, generation)
                 VALUES (?, 'lib', ?, ?, 'generation')",
            )
            .bind(id)
            .bind(job_type)
            .bind(status)
            .execute(database.pool())
            .await?;
    }

    let counts = database.count_scan_jobs_by_status().await?;
    assert_eq!(counts.running, 2);
    assert_eq!(counts.failed, 1);

    let plan = database
        .query(
            "EXPLAIN QUERY PLAN
             SELECT (SELECT COUNT(*) FROM scan_jobs
                     WHERE status IN ('PENDING', 'RUNNING')) AS running,
                    (SELECT COUNT(*) FROM scan_jobs WHERE status = 'FAILED') AS failed",
        )
        .fetch_all(database.pool())
        .await?;
    let plan_details = plan
        .iter()
        .map(|row| row.get::<String, _>("detail"))
        .collect::<Vec<_>>();
    assert!(
        plan_details
            .iter()
            .any(|detail| detail.contains("idx_scan_jobs_activity")),
        "active scan count should use its partial index: {plan_details:?}"
    );
    assert!(
        plan_details
            .iter()
            .any(|detail| detail.contains("idx_scan_jobs_failed_count")),
        "failed scan count should use its partial index: {plan_details:?}"
    );
    assert!(
        plan_details
            .iter()
            .all(|detail| !detail.contains("idx_scan_jobs_library_status")),
        "status counts must not scan the broad scan_jobs index: {plan_details:?}"
    );
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn scan_manifest_creation_is_atomic_and_idempotent() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    database
        .query("INSERT INTO libraries (id, name, kind) VALUES ('lib', 'Library', 'MOVIE')")
        .execute(database.pool())
        .await?;
    for (id, available) in [("root-available", 1_i64), ("root-unavailable", 0_i64)] {
        database
            .query(
                "INSERT INTO library_roots (
                     id, library_id, canonical_path, display_path, is_available, is_writable
                 ) VALUES (?, 'lib', ?, ?, ?, 0)",
            )
            .bind(id)
            .bind(format!("/{id}"))
            .bind(format!("/{id}"))
            .bind(available)
            .execute(database.pool())
            .await?;
    }
    database
        .query(
            "WITH RECURSIVE root_numbers(n) AS (
                 SELECT 1 UNION ALL SELECT n + 1 FROM root_numbers WHERE n < 250
             )
             INSERT INTO library_roots (
                 id, library_id, canonical_path, display_path, is_available, is_writable
             )
             SELECT 'bulk-root-' || n, 'lib', '/bulk/' || n, '/bulk/' || n, 1, 0
             FROM root_numbers",
        )
        .execute(database.pool())
        .await?;
    database
        .create_scan_job("job", "lib", "RECONCILE_LIBRARY", "generation", 0, false)
        .await?;

    let bulk_root_ids: Vec<String> = (1..=250)
        .map(|number| format!("bulk-root-{number}"))
        .collect();
    let mut roots = vec![
        NewScanManifestRoot {
            library_root_id: "root-available",
        },
        NewScanManifestRoot {
            library_root_id: "root-unavailable",
        },
    ];
    roots.extend(
        bulk_root_ids
            .iter()
            .map(|library_root_id| NewScanManifestRoot { library_root_id }),
    );
    let manifest = NewScanManifest {
        id: "manifest",
        job_id: "job",
        library_id: "lib",
        roots: &roots,
    };
    database.create_scan_manifest(&manifest).await?;
    database.create_scan_manifest(&manifest).await?;

    let stored = database
        .get_scan_manifest("manifest")
        .await?
        .ok_or("manifest was not stored")?;
    assert_eq!(stored.id, "manifest");
    assert_eq!(stored.job_id, "job");
    assert_eq!(stored.library_id, "lib");
    assert_eq!(stored.state, "DISCOVERING");
    assert_eq!(stored.root_count, 252);
    assert_eq!(stored.discovered_directory_count, 252);
    assert_eq!(stored.completed_directory_count, 0);
    assert_eq!(stored.observed_file_count, 0);
    assert_eq!(stored.remove_count, 0);
    assert_eq!(stored.applied_delta_count, 0);

    let root_states: Vec<(String, String)> = database
        .query_as(
            "SELECT library_root_id, state FROM scan_manifest_roots
             WHERE manifest_id = ? AND library_root_id IN ('root-available', 'root-unavailable')
             ORDER BY library_root_id",
        )
        .bind("manifest")
        .fetch_all(database.pool())
        .await?;
    assert_eq!(
        root_states,
        vec![
            ("root-available".to_owned(), "PENDING".to_owned()),
            ("root-unavailable".to_owned(), "PENDING".to_owned()),
        ]
    );
    let initial_frontiers: i64 = database
        .query_scalar(
            "SELECT COUNT(*) FROM scan_manifest_directories
             WHERE manifest_id = 'manifest' AND state = 'PENDING'",
        )
        .fetch_one(database.pool())
        .await?;
    assert_eq!(initial_frontiers, 252);

    assert!(
        database
            .transition_scan_manifest_state("manifest", "DISCOVERING", "READY_TO_DIFF")
            .await?
    );
    let delta_ids: Vec<String> = (1..=250).map(|number| format!("delta-{number}")).collect();
    let delta_paths: Vec<String> = (1..=250)
        .map(|number| format!("missing-{number}.mkv"))
        .collect();
    let baseline_fingerprint = [1_u8, 2, 3];
    let deltas: Vec<NewScanManifestDelta<'_>> = delta_ids
        .iter()
        .zip(&delta_paths)
        .map(|(id, relative_path)| NewScanManifestDelta {
            id,
            library_root_id: "root-available",
            relative_path,
            observation_sequence: None,
            delta_kind: "REMOVE",
            base_filesystem_entry_id: Some("baseline-entry"),
            base_fingerprint: Some(&baseline_fingerprint),
        })
        .collect();
    assert_eq!(
        database
            .insert_scan_manifest_deltas("manifest", &deltas)
            .await?,
        250
    );
    assert_eq!(
        database
            .insert_scan_manifest_deltas("manifest", &deltas)
            .await?,
        0
    );
    let conflicting_retry = [NewScanManifestDelta {
        id: "different-delta-id",
        library_root_id: "root-available",
        relative_path: &delta_paths[0],
        observation_sequence: None,
        delta_kind: "REMOVE",
        base_filesystem_entry_id: Some("different-baseline-entry"),
        base_fingerprint: Some(&baseline_fingerprint),
    }];
    assert!(
        database
            .insert_scan_manifest_deltas("manifest", &conflicting_retry)
            .await
            .is_err()
    );
    database
        .query(
            "INSERT INTO scan_manifest_entries (
                 manifest_id, library_root_id, relative_path, observation_sequence,
                 entry_kind, size, modified_at
             ) VALUES ('manifest', 'root-available', 'odd-add.mkv', 1, 'FILE', 10, 20)",
        )
        .execute(database.pool())
        .await?;
    let malformed_add = [NewScanManifestDelta {
        id: "malformed-add",
        library_root_id: "root-available",
        relative_path: "odd-add.mkv",
        observation_sequence: Some(1),
        delta_kind: "ADD",
        base_filesystem_entry_id: None,
        base_fingerprint: Some(&baseline_fingerprint),
    }];
    assert!(
        database
            .insert_scan_manifest_deltas("manifest", &malformed_add)
            .await
            .is_err()
    );
    let stored = database
        .get_scan_manifest("manifest")
        .await?
        .ok_or("manifest disappeared after delta writes")?;
    assert_eq!(stored.remove_count, 250);
    assert!(
        database
            .transition_scan_manifest_state("manifest", "READY_TO_DIFF", "APPLYING")
            .await?
    );
    assert!(
        database
            .transition_scan_manifest_state("manifest", "DISCOVERING", "APPLYING")
            .await
            .is_err()
    );
    database.close().await;
    Ok(())
}

async fn create_manifest_diff_finish_fixture(
    database: &Database,
    suffix: &str,
    workflow_version: i64,
    discovery_format_version: i64,
    discovery_mode: &str,
) -> Result<(String, String, String), Box<dyn std::error::Error>> {
    let library_id = format!("manifest-diff-library-{suffix}");
    let root_id = format!("manifest-diff-root-{suffix}");
    let job_id = format!("manifest-diff-job-{suffix}");
    let manifest_id = format!("manifest-diff-manifest-{suffix}");
    database
        .query("INSERT INTO libraries (id, name, kind) VALUES (?, ?, 'MOVIE')")
        .bind(&library_id)
        .bind(&library_id)
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO library_roots (
                 id, library_id, canonical_path, display_path, is_available, is_writable
             ) VALUES (?, ?, ?, ?, 1, 0)",
        )
        .bind(&root_id)
        .bind(&library_id)
        .bind(format!("/{root_id}"))
        .bind(format!("/{root_id}"))
        .execute(database.pool())
        .await?;
    database
        .create_scan_job(
            &job_id,
            &library_id,
            "RECONCILE_LIBRARY",
            &format!("generation-{suffix}"),
            0,
            false,
        )
        .await?;
    database
        .query("UPDATE scan_jobs SET status = 'RUNNING' WHERE id = ?")
        .bind(&job_id)
        .execute(database.pool())
        .await?;
    let roots = [NewScanManifestRoot {
        library_root_id: &root_id,
    }];
    database
        .create_scan_manifest(&NewScanManifest {
            id: &manifest_id,
            job_id: &job_id,
            library_id: &library_id,
            roots: &roots,
        })
        .await?;
    database
        .query(
            "UPDATE scan_manifests
             SET state = 'READY_TO_DIFF', workflow_version = ?,
                 discovery_format_version = ?, discovery_mode = ?
             WHERE id = ?",
        )
        .bind(workflow_version)
        .bind(discovery_format_version)
        .bind(discovery_mode)
        .bind(&manifest_id)
        .execute(database.pool())
        .await?;
    database
        .query(
            "UPDATE scan_manifest_roots SET state = 'COMPLETE'
             WHERE manifest_id = ? AND library_root_id = ?",
        )
        .bind(&manifest_id)
        .bind(&root_id)
        .execute(database.pool())
        .await?;
    Ok((job_id, manifest_id, root_id))
}

async fn insert_manifest_diff_file(
    database: &Database,
    root_id: &str,
    entry_id: &str,
    relative_path: &str,
    generation: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    database
        .query(
            "INSERT INTO filesystem_entries (
                 id, library_root_id, relative_path, entry_kind, size, modified_at,
                 fingerprint, last_seen_generation
             ) VALUES (?, ?, ?, 'FILE', 1, 1, NULL, ?)",
        )
        .bind(entry_id)
        .bind(root_id)
        .bind(relative_path)
        .bind(generation)
        .execute(database.pool())
        .await?;
    Ok(())
}

async fn insert_manifest_diff_delta(
    database: &Database,
    manifest_id: &str,
    delta: NewScanManifestDelta<'_>,
) -> Result<(), Box<dyn std::error::Error>> {
    let delta = [delta];
    assert_eq!(
        database
            .insert_scan_manifest_deltas(manifest_id, &delta)
            .await?,
        1
    );
    Ok(())
}

async fn collect_manifest_removal_candidate_paths(
    database: &Database,
    manifest_id: &str,
    root_id: &str,
    discovery_format_version: i64,
    uncovered_only: bool,
    directories: Option<&[StoredScanManifestDirectory]>,
    page_size: i64,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut paths = Vec::new();
    let mut after_path: Option<String> = None;
    loop {
        let candidates = database
            .list_scan_manifest_removal_candidates_for_root(ScanManifestRemovalCandidateQuery {
                manifest_id,
                library_root_id: root_id,
                discovery_format_version,
                uncovered_only,
                directories,
                after_relative_path: after_path.as_deref(),
                limit: page_size,
            })
            .await?;
        let Some(last_path) = candidates
            .last()
            .map(|candidate| candidate.relative_path.clone())
        else {
            break;
        };
        assert!(candidates.len() <= usize::try_from(page_size)?);
        paths.extend(
            candidates
                .into_iter()
                .map(|candidate| candidate.relative_path),
        );
        after_path = Some(last_path);
    }
    Ok(paths)
}

#[tokio::test]
async fn manifest_diff_completion_preserves_all_format_candidate_checks()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;

    // Format-3 LITE: a live file with an old generation and no seen-path row is pending.
    let (job_id, manifest_id, root_id) =
        create_manifest_diff_finish_fixture(&database, "lite", 3, 3, "LITE").await?;
    insert_manifest_diff_file(&database, &root_id, "lite-entry", "movie.mkv", "old").await?;
    assert!(
        !database
            .finish_scan_manifest_diff(&manifest_id, &job_id)
            .await?
    );
    insert_manifest_diff_delta(
        &database,
        &manifest_id,
        NewScanManifestDelta {
            id: "lite-delta",
            library_root_id: &root_id,
            relative_path: "movie.mkv",
            observation_sequence: None,
            delta_kind: "REMOVE",
            base_filesystem_entry_id: Some("lite-entry"),
            base_fingerprint: None,
        },
    )
    .await?;
    assert!(
        database
            .finish_scan_manifest_diff(&manifest_id, &job_id)
            .await?
    );

    // Format-3 PERSISTED: only a completed directory's direct children are candidates.
    let (job_id, manifest_id, root_id) =
        create_manifest_diff_finish_fixture(&database, "persisted", 3, 3, "PERSISTED").await?;
    insert_manifest_diff_file(
        &database,
        &root_id,
        "persisted-entry",
        "big/movie.mkv",
        "old",
    )
    .await?;
    database
        .query(
            "INSERT INTO scan_manifest_directories (
                 manifest_id, library_root_id, relative_path, state
             ) VALUES (?, ?, 'big', 'COMPLETE')",
        )
        .bind(&manifest_id)
        .bind(&root_id)
        .execute(database.pool())
        .await?;
    assert!(
        !database
            .finish_scan_manifest_diff(&manifest_id, &job_id)
            .await?
    );
    insert_manifest_diff_delta(
        &database,
        &manifest_id,
        NewScanManifestDelta {
            id: "persisted-delta",
            library_root_id: &root_id,
            relative_path: "big/movie.mkv",
            observation_sequence: None,
            delta_kind: "REMOVE",
            base_filesystem_entry_id: Some("persisted-entry"),
            base_fingerprint: None,
        },
    )
    .await?;
    assert!(
        database
            .finish_scan_manifest_diff(&manifest_id, &job_id)
            .await?
    );

    // Workflow 2/3 with format 2 treats a latest non-file observation as a removal.
    let (job_id, manifest_id, root_id) =
        create_manifest_diff_finish_fixture(&database, "format2", 2, 2, "PERSISTED").await?;
    insert_manifest_diff_file(&database, &root_id, "format2-entry", "legacy.mkv", "old").await?;
    database
        .query(
            "INSERT INTO scan_manifest_entries (
                 manifest_id, library_root_id, relative_path, observation_sequence,
                 entry_kind, size, modified_at
             ) VALUES (?, ?, 'legacy.mkv', 1, 'DIRECTORY', 0, 1)",
        )
        .bind(&manifest_id)
        .bind(&root_id)
        .execute(database.pool())
        .await?;
    assert!(
        !database
            .finish_scan_manifest_diff(&manifest_id, &job_id)
            .await?
    );
    insert_manifest_diff_delta(
        &database,
        &manifest_id,
        NewScanManifestDelta {
            id: "format2-delta",
            library_root_id: &root_id,
            relative_path: "legacy.mkv",
            observation_sequence: None,
            delta_kind: "REMOVE",
            base_filesystem_entry_id: Some("format2-entry"),
            base_fingerprint: None,
        },
    )
    .await?;
    assert!(
        database
            .finish_scan_manifest_diff(&manifest_id, &job_id)
            .await?
    );

    // Workflow 1 has two independent candidate sets; each must be cleared before finishing.
    let (job_id, manifest_id, root_id) =
        create_manifest_diff_finish_fixture(&database, "workflow1", 1, 2, "PERSISTED").await?;
    database
        .query(
            "INSERT INTO scan_manifest_entries (
                 manifest_id, library_root_id, relative_path, observation_sequence,
                 entry_kind, size, modified_at, fingerprint
             ) VALUES (?, ?, 'orphan.mkv', 1, 'FILE', 1, 1, X'01')",
        )
        .bind(&manifest_id)
        .bind(&root_id)
        .execute(database.pool())
        .await?;
    insert_manifest_diff_file(
        &database,
        &root_id,
        "workflow1-entry",
        "unobserved.mkv",
        "old",
    )
    .await?;
    assert!(
        !database
            .finish_scan_manifest_diff(&manifest_id, &job_id)
            .await?
    );
    insert_manifest_diff_delta(
        &database,
        &manifest_id,
        NewScanManifestDelta {
            id: "workflow1-orphan-delta",
            library_root_id: &root_id,
            relative_path: "orphan.mkv",
            observation_sequence: Some(1),
            delta_kind: "ADD",
            base_filesystem_entry_id: None,
            base_fingerprint: None,
        },
    )
    .await?;
    assert!(
        !database
            .finish_scan_manifest_diff(&manifest_id, &job_id)
            .await?
    );
    insert_manifest_diff_delta(
        &database,
        &manifest_id,
        NewScanManifestDelta {
            id: "workflow1-unobserved-delta",
            library_root_id: &root_id,
            relative_path: "unobserved.mkv",
            observation_sequence: None,
            delta_kind: "REMOVE",
            base_filesystem_entry_id: Some("workflow1-entry"),
            base_fingerprint: None,
        },
    )
    .await?;
    assert!(
        database
            .finish_scan_manifest_diff(&manifest_id, &job_id)
            .await?
    );

    database.close().await;
    Ok(())
}

#[tokio::test]
async fn manifest_removal_candidates_page_within_root_and_preserve_directory_scopes()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let (_job_id, manifest_id, root_id) =
        create_manifest_diff_finish_fixture(&database, "paging", 3, 3, "LITE").await?;
    let library_id = "manifest-diff-library-paging";
    let second_root_id = format!("{root_id}-b");
    database
        .query(
            "INSERT INTO library_roots (
                 id, library_id, canonical_path, display_path, is_available, is_writable
             ) VALUES (?, ?, ?, ?, 1, 0)",
        )
        .bind(&second_root_id)
        .bind(library_id)
        .bind(format!("/{second_root_id}"))
        .bind(format!("/{second_root_id}"))
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO scan_manifest_roots (manifest_id, library_root_id, state)
             VALUES (?, ?, 'COMPLETE')",
        )
        .bind(&manifest_id)
        .bind(&second_root_id)
        .execute(database.pool())
        .await?;

    let mut candidate_paths = vec![
        "root.mkv",
        "A/a.mkv",
        "A/Nested/deep.mkv",
        "AB/ab.mkv",
        "wild%_\\/w.mkv",
        "中文/电影.mkv",
        "current.mkv",
        "seen.mkv",
        "delta.mkv",
    ];
    candidate_paths.sort_unstable();
    let delta_entry_id = format!(
        "paging-entry-{}",
        candidate_paths
            .iter()
            .position(|path| *path == "delta.mkv")
            .ok_or("delta fixture path is missing")?
    );
    for (index, path) in candidate_paths.iter().enumerate() {
        let generation = if *path == "current.mkv" {
            "generation-paging"
        } else {
            "old"
        };
        insert_manifest_diff_file(
            &database,
            &root_id,
            &format!("paging-entry-{index}"),
            path,
            generation,
        )
        .await?;
    }
    insert_manifest_diff_file(
        &database,
        &second_root_id,
        "paging-entry-second-root",
        "A/a.mkv",
        "old",
    )
    .await?;
    database
        .query(
            "INSERT INTO scan_manifest_seen_paths (manifest_id, library_root_id, relative_path)
             VALUES (?, ?, 'seen.mkv')",
        )
        .bind(&manifest_id)
        .bind(&root_id)
        .execute(database.pool())
        .await?;
    insert_manifest_diff_delta(
        &database,
        &manifest_id,
        NewScanManifestDelta {
            id: "paging-existing-delta",
            library_root_id: &root_id,
            relative_path: "delta.mkv",
            observation_sequence: None,
            delta_kind: "REMOVE",
            base_filesystem_entry_id: Some(&delta_entry_id),
            base_fingerprint: None,
        },
    )
    .await?;

    let roots = database
        .list_scan_manifest_complete_roots(&manifest_id, None, 1)
        .await?;
    assert_eq!(roots.as_slice(), std::slice::from_ref(&root_id));
    let next_roots = database
        .list_scan_manifest_complete_roots(&manifest_id, roots.last().map(String::as_str), 1)
        .await?;
    assert_eq!(next_roots, [second_root_id]);

    let lite_candidates = collect_manifest_removal_candidate_paths(
        &database,
        &manifest_id,
        &root_id,
        3,
        false,
        None,
        2,
    )
    .await?;
    let mut expected_lite = vec![
        "root.mkv".to_owned(),
        "A/a.mkv".to_owned(),
        "A/Nested/deep.mkv".to_owned(),
        "AB/ab.mkv".to_owned(),
        "wild%_\\/w.mkv".to_owned(),
        "中文/电影.mkv".to_owned(),
    ];
    expected_lite.sort_unstable();
    assert_eq!(lite_candidates, expected_lite);

    for directory in ["", "A", "AB", "wild%_\\", "中文"] {
        if directory.is_empty() {
            database
                .query(
                    "UPDATE scan_manifest_directories SET state = 'COMPLETE'
                     WHERE manifest_id = ? AND library_root_id = ? AND relative_path = ''",
                )
                .bind(&manifest_id)
                .bind(&root_id)
                .execute(database.pool())
                .await?;
        } else {
            database
                .query(
                    "INSERT INTO scan_manifest_directories (
                         manifest_id, library_root_id, relative_path, state
                     ) VALUES (?, ?, ?, 'COMPLETE')",
                )
                .bind(&manifest_id)
                .bind(&root_id)
                .bind(directory)
                .execute(database.pool())
                .await?;
        }
    }
    let mut complete_directories = Vec::new();
    let mut after_directory: Option<String> = None;
    loop {
        let page = database
            .list_scan_manifest_complete_directories_for_root(
                &manifest_id,
                &root_id,
                after_directory.as_deref(),
                2,
            )
            .await?;
        let Some(last_directory) = page.last() else {
            break;
        };
        after_directory = Some(last_directory.relative_path.clone());
        complete_directories.extend(page);
    }
    assert_eq!(complete_directories.len(), 5);
    let persisted_candidates = collect_manifest_removal_candidate_paths(
        &database,
        &manifest_id,
        &root_id,
        3,
        false,
        Some(&complete_directories),
        2,
    )
    .await?;
    let mut expected_persisted = vec![
        "root.mkv".to_owned(),
        "A/a.mkv".to_owned(),
        "AB/ab.mkv".to_owned(),
        "wild%_\\/w.mkv".to_owned(),
        "中文/电影.mkv".to_owned(),
    ];
    expected_persisted.sort_unstable();
    assert_eq!(persisted_candidates, expected_persisted);

    let uncovered_candidates = collect_manifest_removal_candidate_paths(
        &database,
        &manifest_id,
        &root_id,
        3,
        true,
        None,
        2,
    )
    .await?;
    assert_eq!(uncovered_candidates, ["A/Nested/deep.mkv"]);
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn manifest_directory_frontier_templates_preserve_insert_and_completion_counts()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    database
        .query("INSERT INTO libraries (id, name, kind) VALUES ('lib', 'Library', 'MOVIE')")
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO library_roots (
                 id, library_id, canonical_path, display_path, is_available, is_writable
             ) VALUES ('root', 'lib', '/root', '/root', 1, 0)",
        )
        .execute(database.pool())
        .await?;
    let roots = [NewScanManifestRoot {
        library_root_id: "root",
    }];
    let manifest = NewScanManifest {
        id: "manifest",
        job_id: "job",
        library_id: "lib",
        roots: &roots,
    };
    database
        .create_full_scan_manifest_job("job", "generation", false, &manifest, None)
        .await?;
    assert!(database.claim_scan_job("job").await?);

    let child_paths = (0..201)
        .map(|index| format!("child-{index:03}"))
        .collect::<Vec<_>>();
    let lite_chunk = NewScanManifestDiscoveryChunk {
        manifest_id: "manifest",
        job_id: "job",
        library_root_id: "root",
        child_directories: &child_paths,
        entries: &[],
        positive_indexes: &[],
        unchanged_paths: &[],
        seen_filesystem_entries: &[],
        completed_directory: None,
    };
    assert_eq!(
        database
            .commit_scan_manifest_discovery_chunk(&lite_chunk)
            .await?,
        0
    );
    let lite_child_count: i64 = database
        .query_scalar(
            "SELECT COUNT(*) FROM scan_manifest_directories
             WHERE manifest_id = 'manifest' AND relative_path <> ''",
        )
        .fetch_one(database.pool())
        .await?;
    assert_eq!(
        lite_child_count, 0,
        "lite mode must not persist frontier rows"
    );
    let lite_root_state: String = database
        .query_scalar(
            "SELECT state FROM scan_manifest_directories
             WHERE manifest_id = 'manifest' AND relative_path = ''",
        )
        .fetch_one(database.pool())
        .await?;
    assert_eq!(
        lite_root_state, "PENDING",
        "lite mode must skip completion DML"
    );
    database
        .query("UPDATE scan_manifests SET discovery_mode = 'PERSISTED' WHERE id = 'manifest'")
        .execute(database.pool())
        .await?;

    let chunk = NewScanManifestDiscoveryChunk {
        manifest_id: "manifest",
        job_id: "job",
        library_root_id: "root",
        child_directories: &child_paths,
        entries: &[],
        positive_indexes: &[],
        unchanged_paths: &[],
        seen_filesystem_entries: &[],
        completed_directory: Some(""),
    };

    database.reset_query_count();
    let first_observed_file_count = database
        .commit_scan_manifest_discovery_chunk(&chunk)
        .await?;
    let first_query_count = database.query_count();
    assert_eq!(first_observed_file_count, 0);
    assert_eq!(first_query_count, 7);

    let discovered_child_count: i64 = database
        .query_scalar(
            "SELECT COUNT(*) FROM scan_manifest_directories
             WHERE manifest_id = 'manifest' AND relative_path <> ''",
        )
        .fetch_one(database.pool())
        .await?;
    assert_eq!(discovered_child_count, 201);
    let root_state: String = database
        .query_scalar(
            "SELECT state FROM scan_manifest_directories
             WHERE manifest_id = 'manifest' AND relative_path = ''",
        )
        .fetch_one(database.pool())
        .await?;
    assert_eq!(root_state, "COMPLETE");
    let stored = database
        .get_scan_manifest("manifest")
        .await?
        .ok_or("manifest disappeared after frontier commit")?;
    assert_eq!(stored.discovered_directory_count, 202);
    assert_eq!(stored.completed_directory_count, 1);

    database.reset_query_count();
    let replay_observed_file_count = database
        .commit_scan_manifest_discovery_chunk(&chunk)
        .await?;
    let replay_query_count = database.query_count();
    assert_eq!(replay_query_count, first_query_count);
    assert_eq!(replay_observed_file_count, 0);
    let replayed = database
        .get_scan_manifest("manifest")
        .await?
        .ok_or("manifest disappeared after frontier replay")?;
    assert_eq!(replayed.discovered_directory_count, 202);
    assert_eq!(replayed.completed_directory_count, 1);

    database.close().await;
    Ok(())
}

#[tokio::test]
async fn manifest_discovery_chunk_does_not_complete_directory_after_cancel_request()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    database
        .query("INSERT INTO libraries (id, name, kind) VALUES ('lib', 'Library', 'MOVIE')")
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO library_roots (
                 id, library_id, canonical_path, display_path, is_available, is_writable
             ) VALUES ('root', 'lib', '/root', '/root', 1, 0)",
        )
        .execute(database.pool())
        .await?;
    let roots = [NewScanManifestRoot {
        library_root_id: "root",
    }];
    let manifest = NewScanManifest {
        id: "manifest",
        job_id: "job",
        library_id: "lib",
        roots: &roots,
    };
    database
        .create_full_scan_manifest_job("job", "generation", false, &manifest, None)
        .await?;
    assert!(database.claim_scan_job("job").await?);
    database
        .query("UPDATE scan_jobs SET cancel_requested = 1 WHERE id = 'job'")
        .execute(database.pool())
        .await?;
    let entries = [NewScanManifestEntry {
        relative_path: String::new(),
        entry_kind: "DIRECTORY".to_owned(),
        size: 0,
        modified_at: 0,
        device: None,
        inode: None,
        fingerprint: Vec::new(),
        fingerprint_pending: false,
        is_symlink: false,
        resolved_path: None,
    }];
    let chunk = NewScanManifestDiscoveryChunk {
        manifest_id: "manifest",
        job_id: "job",
        library_root_id: "root",
        child_directories: &[],
        entries: &entries,
        positive_indexes: &[],
        unchanged_paths: &[],
        seen_filesystem_entries: &[],
        completed_directory: Some(""),
    };

    assert!(
        database
            .commit_scan_manifest_discovery_chunk(&chunk)
            .await
            .is_err()
    );
    let directory_state: String = database
        .query_scalar(
            "SELECT state FROM scan_manifest_directories
             WHERE manifest_id = 'manifest' AND relative_path = ''",
        )
        .fetch_one(database.pool())
        .await?;
    assert_eq!(directory_state, "PENDING");
    let root_state: String = database
        .query_scalar(
            "SELECT state FROM scan_manifest_roots
             WHERE manifest_id = 'manifest' AND library_root_id = 'root'",
        )
        .fetch_one(database.pool())
        .await?;
    assert_eq!(root_state, "PENDING");
    let observations: i64 = database
        .query_scalar("SELECT COUNT(*) FROM scan_manifest_entries")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(observations, 0);
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn manifest_discovery_does_not_count_a_path_already_seen_in_the_generation()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    database
        .query("INSERT INTO libraries (id, name, kind) VALUES ('lib', 'Library', 'MOVIE')")
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO library_roots (
                 id, library_id, canonical_path, display_path, is_available, is_writable
             ) VALUES ('root', 'lib', '/root', '/root', 1, 0)",
        )
        .execute(database.pool())
        .await?;
    let roots = [NewScanManifestRoot {
        library_root_id: "root",
    }];
    let manifest = NewScanManifest {
        id: "manifest",
        job_id: "job",
        library_id: "lib",
        roots: &roots,
    };
    database
        .create_full_scan_manifest_job("job", "generation", false, &manifest, None)
        .await?;
    assert!(database.claim_scan_job("job").await?);
    database
        .query(
            "INSERT INTO filesystem_entries (
                 id, library_root_id, relative_path, entry_kind, size, modified_at,
                 fingerprint, last_seen_generation, is_missing
             ) VALUES ('seen-entry', 'root', 'Already.Seen.2024.mkv', 'FILE', 1, 1,
                       ?, 'generation', 0)",
        )
        .bind(vec![1_u8; 32])
        .execute(database.pool())
        .await?;
    let entries = [NewScanManifestEntry {
        relative_path: "Already.Seen.2024.mkv".to_owned(),
        entry_kind: "FILE".to_owned(),
        size: 1,
        modified_at: 1,
        device: None,
        inode: None,
        fingerprint: vec![1_u8; 32],
        fingerprint_pending: false,
        is_symlink: false,
        resolved_path: None,
    }];
    let chunk = NewScanManifestDiscoveryChunk {
        manifest_id: "manifest",
        job_id: "job",
        library_root_id: "root",
        child_directories: &[],
        entries: &entries,
        positive_indexes: &[],
        unchanged_paths: &[],
        seen_filesystem_entries: &[],
        completed_directory: Some(""),
    };

    let committed = database
        .commit_scan_manifest_discovery_chunk(&chunk)
        .await?;
    assert_eq!(committed, 0);
    let stored_count: i64 = database
        .query_scalar("SELECT observed_file_count FROM scan_manifests WHERE id = 'manifest'")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(stored_count, 0);
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn manifest_discovery_presence_update_preserves_fingerprint_cas()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    database
        .query("INSERT INTO libraries (id, name, kind) VALUES ('lib', 'Library', 'MOVIE')")
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO library_roots (
                 id, library_id, canonical_path, display_path, is_available, is_writable
             ) VALUES ('root', 'lib', '/root', '/root', 1, 0)",
        )
        .execute(database.pool())
        .await?;
    let roots = [NewScanManifestRoot {
        library_root_id: "root",
    }];
    let manifest = NewScanManifest {
        id: "manifest",
        job_id: "job",
        library_id: "lib",
        roots: &roots,
    };
    database
        .create_full_scan_manifest_job("job", "generation", false, &manifest, None)
        .await?;
    assert!(database.claim_scan_job("job").await?);

    for (id, path, stored_fingerprint, last_seen_change_kind, is_missing) in [
        (
            "matched",
            "Matched.mkv",
            vec![1_u8; 32],
            Some("CHANGED"),
            0_i64,
        ),
        ("changed", "Changed.mkv", vec![2_u8; 32], Some("NEW"), 0_i64),
        (
            "missing",
            "Missing.mkv",
            vec![4_u8; 32],
            Some("CHANGED"),
            1_i64,
        ),
    ] {
        database
            .query(
                "INSERT INTO filesystem_entries (
                     id, library_root_id, relative_path, entry_kind, size, modified_at,
                     fingerprint, last_seen_generation, last_seen_change_kind, is_missing
                 ) VALUES (?, 'root', ?, 'FILE', 1, 1, ?, 'old-generation', ?, ?)",
            )
            .bind(id)
            .bind(path)
            .bind(stored_fingerprint)
            .bind(last_seen_change_kind)
            .bind(is_missing)
            .execute(database.pool())
            .await?;
    }

    let entries = [
        NewScanManifestEntry {
            relative_path: "Matched.mkv".to_owned(),
            entry_kind: "FILE".to_owned(),
            size: 1,
            modified_at: 1,
            device: None,
            inode: None,
            fingerprint: vec![1_u8; 32],
            fingerprint_pending: false,
            is_symlink: false,
            resolved_path: None,
        },
        NewScanManifestEntry {
            relative_path: "Changed.mkv".to_owned(),
            entry_kind: "FILE".to_owned(),
            size: 1,
            modified_at: 1,
            device: None,
            inode: None,
            fingerprint: vec![3_u8; 32],
            fingerprint_pending: false,
            is_symlink: false,
            resolved_path: None,
        },
        NewScanManifestEntry {
            relative_path: "Missing.mkv".to_owned(),
            entry_kind: "FILE".to_owned(),
            size: 1,
            modified_at: 1,
            device: None,
            inode: None,
            fingerprint: vec![4_u8; 32],
            fingerprint_pending: false,
            is_symlink: false,
            resolved_path: None,
        },
    ];
    let seen_filesystem_entries = [
        NewScanManifestSeenFilesystemEntry {
            filesystem_entry_id: "matched".to_owned(),
            relative_path: "Matched.mkv".to_owned(),
            size: 1,
            modified_at: 1,
            device: None,
            inode: None,
            fingerprint: vec![1_u8; 32],
        },
        NewScanManifestSeenFilesystemEntry {
            filesystem_entry_id: "changed".to_owned(),
            relative_path: "Changed.mkv".to_owned(),
            size: 1,
            modified_at: 1,
            device: None,
            inode: None,
            fingerprint: vec![3_u8; 32],
        },
        NewScanManifestSeenFilesystemEntry {
            filesystem_entry_id: "missing".to_owned(),
            relative_path: "Missing.mkv".to_owned(),
            size: 1,
            modified_at: 1,
            device: None,
            inode: None,
            fingerprint: vec![4_u8; 32],
        },
    ];
    let chunk = NewScanManifestDiscoveryChunk {
        manifest_id: "manifest",
        job_id: "job",
        library_root_id: "root",
        child_directories: &[],
        entries: &entries,
        positive_indexes: &[],
        unchanged_paths: &[],
        seen_filesystem_entries: &seen_filesystem_entries,
        completed_directory: Some(""),
    };

    database
        .commit_scan_manifest_discovery_chunk(&chunk)
        .await?;

    let matched: (String, Option<String>, i64) = database
        .query_as(
            "SELECT last_seen_generation, last_seen_change_kind, is_missing
             FROM filesystem_entries WHERE id = 'matched'",
        )
        .fetch_one(database.pool())
        .await?;
    assert_eq!(matched, ("generation".to_owned(), None, 0));
    let changed: (String, Option<String>, i64) = database
        .query_as(
            "SELECT last_seen_generation, last_seen_change_kind, is_missing
             FROM filesystem_entries WHERE id = 'changed'",
        )
        .fetch_one(database.pool())
        .await?;
    assert_eq!(
        changed,
        ("old-generation".to_owned(), Some("NEW".to_owned()), 0)
    );
    let missing: (String, Option<String>, i64) = database
        .query_as(
            "SELECT last_seen_generation, last_seen_change_kind, is_missing
             FROM filesystem_entries WHERE id = 'missing'",
        )
        .fetch_one(database.pool())
        .await?;
    assert_eq!(
        missing,
        ("old-generation".to_owned(), Some("CHANGED".to_owned()), 1)
    );
    let mut ledger_paths: Vec<String> = database
        .query_scalar(
            "SELECT relative_path FROM scan_manifest_seen_paths WHERE manifest_id = 'manifest'",
        )
        .fetch_all(database.pool())
        .await?;
    ledger_paths.sort();
    assert_eq!(ledger_paths, ["Changed.mkv", "Missing.mkv"]);

    database.close().await;
    Ok(())
}

#[tokio::test]
async fn manifest_discovery_finalize_does_not_ignore_cancel_request()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    database
        .query("INSERT INTO libraries (id, name, kind) VALUES ('lib', 'Library', 'MOVIE')")
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO library_roots (
                 id, library_id, canonical_path, display_path, is_available, is_writable
             ) VALUES ('root', 'lib', '/root', '/root', 1, 0)",
        )
        .execute(database.pool())
        .await?;
    let roots = [NewScanManifestRoot {
        library_root_id: "root",
    }];
    let manifest = NewScanManifest {
        id: "manifest",
        job_id: "job",
        library_id: "lib",
        roots: &roots,
    };
    database
        .create_full_scan_manifest_job("job", "generation", false, &manifest, None)
        .await?;
    assert!(database.claim_scan_job("job").await?);
    let entries = [NewScanManifestEntry {
        relative_path: String::new(),
        entry_kind: "DIRECTORY".to_owned(),
        size: 0,
        modified_at: 0,
        device: None,
        inode: None,
        fingerprint: Vec::new(),
        fingerprint_pending: false,
        is_symlink: false,
        resolved_path: None,
    }];
    let chunk = NewScanManifestDiscoveryChunk {
        manifest_id: "manifest",
        job_id: "job",
        library_root_id: "root",
        child_directories: &[],
        entries: &entries,
        positive_indexes: &[],
        unchanged_paths: &[],
        seen_filesystem_entries: &[],
        completed_directory: Some(""),
    };
    database
        .commit_scan_manifest_discovery_chunk(&chunk)
        .await?;
    database
        .query("UPDATE scan_jobs SET cancel_requested = 1 WHERE id = 'job'")
        .execute(database.pool())
        .await?;

    assert!(
        database
            .finish_scan_manifest_discovery("manifest", "job")
            .await
            .is_err()
    );
    let job_state: (i64, String) = database
        .query_as(
            "SELECT discovery_completed,
                    (SELECT state FROM scan_manifests WHERE id = 'manifest')
             FROM scan_jobs WHERE id = 'job'",
        )
        .fetch_one(database.pool())
        .await?;
    assert_eq!(job_state, (0, "DISCOVERING".to_owned()));
    database.close().await;
    Ok(())
}

#[test]
fn sidecar_target_query_uses_indexable_directory_ranges() {
    let query = sidecar_target_query("(?)");
    assert!(query.contains("fe.relative_path >= sd.directory || '/'"));
    assert!(query.contains("fe.relative_path < sd.directory || '0'"));
    assert!(!query.contains("substr("));
}

#[test]
fn nested_sidecar_directories_are_covered_by_their_ancestor() {
    let directories = prune_sidecar_directories(vec![
        "Show/Season 01".to_owned(),
        "Show".to_owned(),
        "Show2".to_owned(),
        "Show/Extras".to_owned(),
        "Show".to_owned(),
    ]);

    assert_eq!(directories, vec!["Show".to_owned(), "Show2".to_owned()]);
    assert_eq!(
        prune_sidecar_directories(vec!["Show/Season 01".to_owned(), ".".to_owned()]),
        vec![".".to_owned()]
    );
}

#[tokio::test]
async fn deleting_media_sources_batches_item_cleanup_queries()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    database
        .query("INSERT INTO libraries (id, name, kind) VALUES ('lib', 'Library', 'MOVIE')")
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('item', 'lib', 'MOVIE', 'Item', 'item', 'LOCAL_CONFIRMED')",
        )
        .execute(database.pool())
        .await?;
    for source_id in ["source-a", "source-b"] {
        database
            .query(
                "INSERT INTO media_sources (id, item_id, source_kind, is_default, probe_status)
                 VALUES (?, 'item', 'LOCAL_FILE', 1, 'PENDING')",
            )
            .bind(source_id)
            .execute(database.pool())
            .await?;
    }

    database.reset_query_count();
    let invalid_sources = [("wrong-item", "source-a"), ("item", "missing")]
        .map(|(item_id, source_id)| (item_id.to_owned(), source_id.to_owned()));
    assert!(
        !database
            .delete_media_sources_atomically(&invalid_sources)
            .await?
    );
    assert_eq!(
        database.query_count(),
        1,
        "invalid pairs stop before writes"
    );
    assert_eq!(
        database
            .query_scalar::<i64>("SELECT COUNT(*) FROM media_sources WHERE item_id = 'item'")
            .fetch_one(database.pool())
            .await?,
        2
    );
    database.reset_query_count();
    let sources = [("item", "source-a"), ("item", "source-b")]
        .map(|(item_id, source_id)| (item_id.to_owned(), source_id.to_owned()));
    assert!(database.delete_media_sources_atomically(&sources).await?);
    assert_eq!(
        database.query_count(),
        3,
        "two source rows should share one lookup, delete, and hierarchy cleanup"
    );
    assert_eq!(
        database
            .query_scalar::<i64>("SELECT COUNT(*) FROM media_sources WHERE item_id = 'item'")
            .fetch_one(database.pool())
            .await?,
        0
    );
    assert!(
        database
            .query_scalar::<Option<i64>>("SELECT removed_at FROM media_items WHERE id = 'item'")
            .fetch_one(database.pool())
            .await?
            .is_some()
    );
    Ok(())
}

#[tokio::test]
async fn deleting_media_sources_atomically_rolls_back_across_batches()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    database
        .query("INSERT INTO libraries (id, name, kind) VALUES ('lib', 'Library', 'MOVIE')")
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('item', 'lib', 'MOVIE', 'Item', 'item', 'LOCAL_CONFIRMED')",
        )
        .execute(database.pool())
        .await?;
    let sources = (0..=MAX_MEDIA_SOURCE_DELETE_BATCH_SIZE)
        .map(|index| ("item".to_owned(), format!("source-{index}")))
        .collect::<Vec<_>>();
    for (_, source_id) in &sources {
        database
            .query(
                "INSERT INTO media_sources (id, item_id, source_kind, is_default, probe_status)
                 VALUES (?, 'item', 'LOCAL_FILE', 1, 'PENDING')",
            )
            .bind(source_id)
            .execute(database.pool())
            .await?;
    }

    let mut missing_source = sources.clone();
    missing_source[MAX_MEDIA_SOURCE_DELETE_BATCH_SIZE].1 = "missing".to_owned();
    assert!(
        !database
            .delete_media_sources_atomically(&missing_source)
            .await?
    );
    assert_eq!(
        database
            .query_scalar::<i64>("SELECT COUNT(*) FROM media_sources WHERE item_id = 'item'")
            .fetch_one(database.pool())
            .await?,
        i64::try_from(sources.len())?
    );

    database.reset_query_count();
    assert!(database.delete_media_sources_atomically(&sources).await?);
    assert_eq!(
        database.query_count(),
        6,
        "each of two source batches should use one lookup, delete, and hierarchy cleanup"
    );
    assert_eq!(
        database
            .query_scalar::<i64>("SELECT COUNT(*) FROM media_sources WHERE item_id = 'item'")
            .fetch_one(database.pool())
            .await?,
        0
    );
    assert!(
        database
            .query_scalar::<Option<i64>>("SELECT removed_at FROM media_items WHERE id = 'item'")
            .fetch_one(database.pool())
            .await?
            .is_some()
    );
    Ok(())
}

#[tokio::test]
async fn reassigned_media_source_does_not_add_a_second_default_source()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    database
        .query("INSERT INTO libraries (id, name, kind) VALUES ('lib', 'Library', 'MOVIE')")
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO library_roots (
                 id, library_id, canonical_path, display_path, is_available, is_writable
             ) VALUES ('root', 'lib', '/media', '/media', 1, 0)",
        )
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES
                ('item-a', 'lib', 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED'),
                ('item-b', 'lib', 'MOVIE', 'Movie B', 'movie b', 'LOCAL_CONFIRMED')",
        )
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO filesystem_entries (
                 id, library_root_id, relative_path, entry_kind, size, modified_at,
                 last_seen_generation
             ) VALUES
                ('entry-1', 'root', 'Movie/a.mkv', 'FILE', 10, 1, 'generation'),
                ('entry-2', 'root', 'Movie/b.mkv', 'FILE', 10, 1, 'generation'),
                ('entry-3', 'root', 'Movie/c.mkv', 'FILE', 10, 1, 'generation')",
        )
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO media_sources (
                 id, item_id, source_kind, filesystem_entry_id, is_default, probe_status
             ) VALUES
                ('source-1', 'item-a', 'LOCAL_FILE', 'entry-1', 1, 'READY'),
                ('source-2', 'item-b', 'LOCAL_FILE', 'entry-2', 1, 'READY'),
                ('source-3', 'item-b', 'LOCAL_FILE', 'entry-3', 0, 'READY')",
        )
        .execute(database.pool())
        .await?;

    // Moving a default source onto an item that already has one keeps a single default.
    assert!(
        database
            .reassign_media_source_item("entry-2", "item-a")
            .await?
    );
    let a_defaults: Vec<String> = database
        .query_scalar("SELECT id FROM media_sources WHERE item_id = 'item-a' AND is_default = 1")
        .fetch_all(database.pool())
        .await?;
    assert_eq!(a_defaults, ["source-1"]);
    // The item that lost its default source still has exactly one.
    let b_defaults: Vec<String> = database
        .query_scalar("SELECT id FROM media_sources WHERE item_id = 'item-b' AND is_default = 1")
        .fetch_all(database.pool())
        .await?;
    assert_eq!(b_defaults, ["source-3"]);
    Ok(())
}

#[tokio::test]
async fn metadata_reidentify_job_reports_items_left_pending()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    database
        .query("INSERT INTO libraries (id, name, kind) VALUES ('lib', 'Library', 'MOVIE')")
        .execute(database.pool())
        .await?;
    database
        .query(
            "INSERT INTO metadata_reidentify_jobs (id, status, mode)
             VALUES ('job', 'RUNNING', 'FILL_MISSING')",
        )
        .execute(database.pool())
        .await?;
    for (item_id, status) in [("done", "COMPLETED"), ("waiting", "PENDING")] {
        database
            .query(
                "INSERT INTO media_items (id, library_id, item_type, title, sort_title,
                 identification_status, has_available_source)
                 VALUES (?, 'lib', 'MOVIE', 'Title', 'title', 'LOCAL_CONFIRMED', 1)",
            )
            .bind(item_id)
            .execute(database.pool())
            .await?;
        database
            .query(
                "INSERT INTO metadata_reidentify_job_items (job_id, item_id, status)
                 VALUES ('job', ?, ?)",
            )
            .bind(item_id)
            .bind(status)
            .execute(database.pool())
            .await?;
    }
    assert!(
        database
            .metadata_reidentify_job_has_pending_items("job")
            .await?
    );
    database
        .query("UPDATE metadata_reidentify_job_items SET status = 'COMPLETED'")
        .execute(database.pool())
        .await?;
    assert!(
        !database
            .metadata_reidentify_job_has_pending_items("job")
            .await?
    );
    database.close().await;
    Ok(())
}

#[tokio::test]
#[ignore = "synthetic SQLx comparison; run explicitly before and after LUX-561 changes"]
async fn manifest_diff_antijoin_benchmark() -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Instant;

    const PAGE_SIZE: i64 = 500;
    let file_count = std::env::var("LUX_ANTIJOIN_FILE_COUNT")
        .unwrap_or_else(|_| "60000".to_owned())
        .parse::<usize>()?;
    if !(1..=240_000).contains(&file_count) {
        return Err("LUX_ANTIJOIN_FILE_COUNT must be between 1 and 240000".into());
    }
    let backend = std::env::var("LUX_ANTIJOIN_BACKEND").unwrap_or_else(|_| "sqlite".to_owned());
    let database_configuration = match backend.as_str() {
        "sqlite" => DatabaseConfiguration::Sqlite,
        "postgres" => {
            let database = std::env::var("POSTGRES_TEST_DATABASE")?;
            if database.is_empty()
                || matches!(database.as_str(), "postgres" | "template0" | "template1")
            {
                return Err(
                    "POSTGRES_TEST_DATABASE must name a disposable non-system database".into(),
                );
            }
            DatabaseConfiguration::Postgres(PostgresConnection {
                host: std::env::var("POSTGRES_TEST_HOST")
                    .unwrap_or_else(|_| "127.0.0.1".to_owned()),
                port: std::env::var("POSTGRES_TEST_PORT")
                    .unwrap_or_else(|_| "55432".to_owned())
                    .parse()?,
                database,
                username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
                password: std::env::var("POSTGRES_TEST_PASSWORD")
                    .unwrap_or_else(|_| "lux-test-password".to_owned()),
                ssl_mode: "disable".to_owned(),
            })
        }
        unsupported => {
            return Err(format!(
                "unsupported LUX_ANTIJOIN_BACKEND {unsupported:?}; expected sqlite or postgres"
            )
            .into());
        }
    };
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect_with_configuration(&config, &database_configuration).await?;
    let sqlite_version = if backend == "sqlite" {
        database
            .query_scalar("SELECT sqlite_version()")
            .fetch_one(database.pool())
            .await?
    } else {
        "not SQLite".to_owned()
    };
    let postgres_version: Option<String> = if backend == "postgres" {
        database
            .query_scalar("SELECT version()")
            .fetch_optional(database.pool())
            .await?
    } else {
        None
    };
    database
        .query(
            "INSERT INTO libraries (id, name, kind) VALUES ('lib', 'Anti-join fixture', 'MOVIE')",
        )
        .execute(database.pool())
        .await?;
    for root_id in ["root-a", "root-b"] {
        database
            .query(
                "INSERT INTO library_roots (
                     id, library_id, canonical_path, display_path, is_available, is_writable
                 ) VALUES (?, 'lib', ?, ?, 1, 0)",
            )
            .bind(root_id)
            .bind(format!("/{root_id}"))
            .bind(format!("/{root_id}"))
            .execute(database.pool())
            .await?;
    }

    database
        .create_scan_job(
            "job",
            "lib",
            "RECONCILE_LIBRARY",
            "generation-current",
            0,
            false,
        )
        .await?;
    database
        .query("UPDATE scan_jobs SET status = 'RUNNING' WHERE id = 'job'")
        .execute(database.pool())
        .await?;
    let roots = [NewScanManifestRoot {
        library_root_id: "root-a",
    }];
    database
        .create_scan_manifest(&crate::storage::NewScanManifest {
            id: "manifest",
            job_id: "job",
            library_id: "lib",
            roots: &roots,
        })
        .await?;
    database
        .query(
            "UPDATE scan_manifests SET state = 'READY_TO_DIFF', workflow_version = 2,
                 discovery_format_version = 3, discovery_mode = 'LITE' WHERE id = 'manifest'",
        )
        .execute(database.pool())
        .await?;
    database
        .query(
            "UPDATE scan_manifest_roots SET state = 'COMPLETE'
             WHERE manifest_id = 'manifest' AND library_root_id = 'root-a'",
        )
        .execute(database.pool())
        .await?;

    for root_id in ["root-a", "root-b"] {
        let query = if backend == "sqlite" {
            "WITH RECURSIVE file_numbers(number) AS (
                 SELECT 0 UNION ALL SELECT number + 1 FROM file_numbers WHERE number + 1 < ?
             )
             INSERT INTO filesystem_entries (
                 id, library_root_id, relative_path, entry_kind, size, modified_at,
                 fingerprint, last_seen_generation
             )
             SELECT ? || '-' || printf('%06d', number), ?,
                    'big/file-' || printf('%06d', number) || '.mkv',
                    'FILE', 1, 1, NULL, 'generation-old'
             FROM file_numbers"
        } else {
            "INSERT INTO filesystem_entries (
                 id, library_root_id, relative_path, entry_kind, size, modified_at,
                 fingerprint, last_seen_generation
             )
             SELECT ? || '-' || LPAD(number::TEXT, 6, '0'), ?,
                    'big/file-' || LPAD(number::TEXT, 6, '0') || '.mkv',
                    'FILE', 1, 1, NULL, 'generation-old'
             FROM generate_series(0, ? - 1) AS numbers(number)"
        };
        let mut statement = database.query(query);
        if backend == "sqlite" {
            statement = statement.bind(i64::try_from(file_count)?);
        }
        statement = statement.bind(root_id).bind(root_id);
        if backend == "postgres" {
            statement = statement.bind(i64::try_from(file_count)?);
        }
        statement.execute(database.pool()).await?;
    }
    database.query("ANALYZE").execute(database.pool()).await?;

    const LITE_COMPLETION_COUNT: &str = "SELECT COUNT(*)
         FROM filesystem_entries fe
         JOIN scan_manifest_roots root
           ON root.manifest_id = ? AND root.library_root_id = fe.library_root_id
          AND root.state = 'COMPLETE'
         JOIN scan_manifests manifest ON manifest.id = root.manifest_id
         JOIN scan_jobs job ON job.id = manifest.job_id
         WHERE fe.entry_kind = 'FILE' AND fe.is_missing = 0
           AND COALESCE(fe.last_seen_generation, '') <> job.generation
           AND NOT EXISTS (
               SELECT 1 FROM scan_manifest_seen_paths seen
               WHERE seen.manifest_id = root.manifest_id
                 AND seen.library_root_id = fe.library_root_id
                 AND seen.relative_path = fe.relative_path
           )
           AND NOT EXISTS (
               SELECT 1 FROM scan_manifest_deltas delta
               WHERE delta.manifest_id = root.manifest_id
                 AND delta.library_root_id = fe.library_root_id
                 AND delta.relative_path = fe.relative_path
           )";
    const LITE_COMPLETION_EXISTS: &str = "SELECT CASE WHEN EXISTS (
             SELECT 1
             FROM filesystem_entries fe
             JOIN scan_manifest_roots root
               ON root.manifest_id = ? AND root.library_root_id = fe.library_root_id
              AND root.state = 'COMPLETE'
             JOIN scan_manifests manifest ON manifest.id = root.manifest_id
             JOIN scan_jobs job ON job.id = manifest.job_id
             WHERE fe.entry_kind = 'FILE' AND fe.is_missing = 0
               AND COALESCE(fe.last_seen_generation, '') <> job.generation
               AND NOT EXISTS (
                   SELECT 1 FROM scan_manifest_seen_paths seen
                   WHERE seen.manifest_id = root.manifest_id
                     AND seen.library_root_id = fe.library_root_id
                     AND seen.relative_path = fe.relative_path
               )
               AND NOT EXISTS (
                   SELECT 1 FROM scan_manifest_deltas delta
                   WHERE delta.manifest_id = root.manifest_id
                     AND delta.library_root_id = fe.library_root_id
                     AND delta.relative_path = fe.relative_path
               )
         ) THEN 1 ELSE 0 END";
    let (first_candidate_count_ms, first_candidate_exists_ms) =
        measure_manifest_diff_completion_queries(
            &database,
            LITE_COMPLETION_COUNT,
            LITE_COMPLETION_EXISTS,
            i64::try_from(file_count)?,
            1,
        )
        .await?;
    database
        .query(
            "UPDATE filesystem_entries SET last_seen_generation = 'generation-current'
             WHERE library_root_id = 'root-a'",
        )
        .execute(database.pool())
        .await?;
    let last_entry_id = format!("root-a-{:06}", file_count - 1);
    database
        .query(
            "UPDATE filesystem_entries SET last_seen_generation = 'generation-old'
             WHERE id = ?",
        )
        .bind(&last_entry_id)
        .execute(database.pool())
        .await?;
    let (last_candidate_count_ms, last_candidate_exists_ms) =
        measure_manifest_diff_completion_queries(
            &database,
            LITE_COMPLETION_COUNT,
            LITE_COMPLETION_EXISTS,
            1,
            1,
        )
        .await?;
    database
        .query(
            "UPDATE filesystem_entries SET last_seen_generation = 'generation-current'
             WHERE library_root_id = 'root-a'",
        )
        .execute(database.pool())
        .await?;
    let (no_candidate_count_ms, no_candidate_exists_ms) = measure_manifest_diff_completion_queries(
        &database,
        LITE_COMPLETION_COUNT,
        LITE_COMPLETION_EXISTS,
        0,
        0,
    )
    .await?;
    database
        .query(
            "UPDATE filesystem_entries SET last_seen_generation = 'generation-old'
             WHERE library_root_id = 'root-a'",
        )
        .execute(database.pool())
        .await?;

    let lite_plan = if backend == "sqlite" {
        database
            .query(
                "EXPLAIN QUERY PLAN
                 SELECT fe.library_root_id, fe.relative_path, fe.id, fe.fingerprint
                 FROM filesystem_entries fe
                 JOIN scan_manifest_roots root
                   ON root.manifest_id = ? AND root.library_root_id = fe.library_root_id
                  AND root.state = 'COMPLETE'
                 JOIN scan_manifests manifest ON manifest.id = root.manifest_id
                 JOIN scan_jobs job ON job.id = manifest.job_id
                 WHERE fe.entry_kind = 'FILE' AND fe.is_missing = 0
                   AND (fe.library_root_id, fe.relative_path) > (?, ?)
                   AND COALESCE(fe.last_seen_generation, '') <> job.generation
                   AND NOT EXISTS (
                       SELECT 1 FROM scan_manifest_seen_paths seen
                       WHERE seen.manifest_id = root.manifest_id
                         AND seen.library_root_id = fe.library_root_id
                         AND seen.relative_path = fe.relative_path
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM scan_manifest_deltas delta
                       WHERE delta.manifest_id = root.manifest_id
                         AND delta.library_root_id = fe.library_root_id
                         AND delta.relative_path = fe.relative_path
                   )
                 ORDER BY fe.library_root_id, fe.relative_path LIMIT ?",
            )
            .bind("manifest")
            .bind("")
            .bind("")
            .bind(PAGE_SIZE)
            .fetch_all(database.pool())
            .await?
            .into_iter()
            .map(|row| row.get::<String, _>("detail"))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let persisted_plan = if backend == "sqlite" {
        database
            .query(
                r"EXPLAIN QUERY PLAN
                  SELECT fe.library_root_id, fe.relative_path, fe.id, fe.fingerprint
                  FROM filesystem_entries fe
                  JOIN scan_manifest_roots root
                    ON root.manifest_id = ? AND root.library_root_id = fe.library_root_id
                   AND root.state = 'COMPLETE'
                  JOIN scan_manifests manifest ON manifest.id = root.manifest_id
                  JOIN scan_jobs job ON job.id = manifest.job_id
                  WHERE fe.entry_kind = 'FILE' AND fe.is_missing = 0
                    AND fe.library_root_id = ?
                    AND fe.relative_path LIKE ? ESCAPE '\'
                    AND fe.relative_path NOT LIKE ? ESCAPE '\'
                    AND COALESCE(fe.last_seen_generation, '') <> job.generation
                    AND NOT EXISTS (
                        SELECT 1 FROM scan_manifest_seen_paths seen
                        WHERE seen.manifest_id = ? AND seen.library_root_id = fe.library_root_id
                          AND seen.relative_path = fe.relative_path
                    )
                    AND NOT EXISTS (
                        SELECT 1 FROM scan_manifest_deltas delta
                        WHERE delta.manifest_id = ? AND delta.library_root_id = fe.library_root_id
                          AND delta.relative_path = fe.relative_path
                    )
                  ORDER BY fe.library_root_id, fe.relative_path LIMIT ?",
            )
            .bind("manifest")
            .bind("root-a")
            .bind("big/%")
            .bind("big/%/%")
            .bind("manifest")
            .bind("manifest")
            .bind(PAGE_SIZE)
            .fetch_all(database.pool())
            .await?
            .into_iter()
            .map(|row| row.get::<String, _>("detail"))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    let fixed_root_lite_plan = if backend == "sqlite" {
        database
            .query(
                "EXPLAIN QUERY PLAN
                 SELECT fe.library_root_id, fe.relative_path, fe.id, fe.fingerprint
                 FROM filesystem_entries fe
                 WHERE fe.library_root_id = ? AND fe.relative_path > ?
                   AND fe.entry_kind = 'FILE' AND fe.is_missing = 0
                   AND EXISTS (
                       SELECT 1 FROM scan_manifest_roots root
                       WHERE root.manifest_id = ?
                         AND root.library_root_id = fe.library_root_id
                         AND root.state = 'COMPLETE'
                   )
                   AND COALESCE(fe.last_seen_generation, '') <> (
                       SELECT job.generation
                       FROM scan_manifests manifest
                       JOIN scan_jobs job ON job.id = manifest.job_id
                       WHERE manifest.id = ?
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM scan_manifest_seen_paths seen
                       WHERE seen.manifest_id = ?
                         AND seen.library_root_id = fe.library_root_id
                         AND seen.relative_path = fe.relative_path
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM scan_manifest_deltas delta
                       WHERE delta.manifest_id = ?
                         AND delta.library_root_id = fe.library_root_id
                         AND delta.relative_path = fe.relative_path
                   )
                 ORDER BY fe.relative_path LIMIT ?",
            )
            .bind("root-a")
            .bind("")
            .bind("manifest")
            .bind("manifest")
            .bind("manifest")
            .bind("manifest")
            .bind(PAGE_SIZE)
            .fetch_all(database.pool())
            .await?
            .into_iter()
            .map(|row| row.get::<String, _>("detail"))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let fixed_root_persisted_plan = if backend == "sqlite" {
        database
            .query(
                r"EXPLAIN QUERY PLAN
                  SELECT fe.library_root_id, fe.relative_path, fe.id, fe.fingerprint
                  FROM filesystem_entries fe
                  WHERE fe.library_root_id = ? AND fe.relative_path > ?
                    AND fe.entry_kind = 'FILE' AND fe.is_missing = 0
                    AND EXISTS (
                        SELECT 1 FROM scan_manifest_roots root
                        WHERE root.manifest_id = ?
                          AND root.library_root_id = fe.library_root_id
                          AND root.state = 'COMPLETE'
                    )
                    AND COALESCE(fe.last_seen_generation, '') <> (
                        SELECT job.generation
                        FROM scan_manifests manifest
                        JOIN scan_jobs job ON job.id = manifest.job_id
                        WHERE manifest.id = ?
                    )
                    AND fe.relative_path LIKE ? ESCAPE '\'
                    AND fe.relative_path NOT LIKE ? ESCAPE '\'
                    AND NOT EXISTS (
                        SELECT 1 FROM scan_manifest_seen_paths seen
                        WHERE seen.manifest_id = ?
                          AND seen.library_root_id = fe.library_root_id
                          AND seen.relative_path = fe.relative_path
                    )
                    AND NOT EXISTS (
                        SELECT 1 FROM scan_manifest_deltas delta
                        WHERE delta.manifest_id = ?
                          AND delta.library_root_id = fe.library_root_id
                          AND delta.relative_path = fe.relative_path
                    )
                  ORDER BY fe.relative_path LIMIT ?",
            )
            .bind("root-a")
            .bind("")
            .bind("manifest")
            .bind("manifest")
            .bind("big/%")
            .bind("big/%/%")
            .bind("manifest")
            .bind("manifest")
            .bind(PAGE_SIZE)
            .fetch_all(database.pool())
            .await?
            .into_iter()
            .map(|row| row.get::<String, _>("detail"))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    let directory = [StoredScanManifestDirectory {
        library_root_id: "root-a".to_owned(),
        relative_path: "big".to_owned(),
    }];
    let mut lite_samples_ms = Vec::with_capacity(3);
    let mut lite_query_counts = Vec::with_capacity(3);
    for _ in 0..3 {
        database.reset_query_count();
        let started = Instant::now();
        let mut after_path: Option<String> = None;
        let mut total = 0_usize;
        loop {
            let candidates = database
                .list_scan_manifest_removal_candidates_for_root(ScanManifestRemovalCandidateQuery {
                    manifest_id: "manifest",
                    library_root_id: "root-a",
                    discovery_format_version: 3,
                    uncovered_only: false,
                    directories: None,
                    after_relative_path: after_path.as_deref(),
                    limit: PAGE_SIZE,
                })
                .await?;
            let Some(last) = candidates.last() else {
                break;
            };
            total += candidates.len();
            after_path = Some(last.relative_path.clone());
        }
        lite_samples_ms.push(started.elapsed().as_secs_f64() * 1_000.0);
        lite_query_counts.push(database.query_count());
        assert_eq!(total, file_count);
    }

    let mut persisted_samples_ms = Vec::with_capacity(3);
    let mut persisted_candidate_read_ms = Vec::with_capacity(3);
    let mut persisted_delta_write_ms = Vec::with_capacity(3);
    let mut persisted_query_counts = Vec::with_capacity(3);
    for _ in 0..3 {
        database
            .query("DELETE FROM scan_manifest_deltas WHERE manifest_id = 'manifest'")
            .execute(database.pool())
            .await?;
        database.reset_query_count();
        let started = Instant::now();
        let mut total = 0_usize;
        let mut after_path: Option<String> = None;
        let mut candidate_read_elapsed_ms = 0.0;
        let mut delta_write_elapsed_ms = 0.0;
        loop {
            let query_started = Instant::now();
            let candidates = database
                .list_scan_manifest_removal_candidates_for_root(ScanManifestRemovalCandidateQuery {
                    manifest_id: "manifest",
                    library_root_id: "root-a",
                    discovery_format_version: 3,
                    uncovered_only: false,
                    directories: Some(&directory),
                    after_relative_path: after_path.as_deref(),
                    limit: PAGE_SIZE,
                })
                .await?;
            candidate_read_elapsed_ms += query_started.elapsed().as_secs_f64() * 1_000.0;
            let Some(last) = candidates.last() else { break };
            total += candidates.len();
            let delta_ids = candidates
                .iter()
                .map(|_| uuid::Uuid::now_v7().to_string())
                .collect::<Vec<_>>();
            let deltas = candidates
                .iter()
                .zip(&delta_ids)
                .map(|(candidate, id)| NewScanManifestDelta {
                    id,
                    library_root_id: &candidate.library_root_id,
                    relative_path: &candidate.relative_path,
                    observation_sequence: None,
                    delta_kind: "REMOVE",
                    base_filesystem_entry_id: Some(&candidate.base_filesystem_entry_id),
                    base_fingerprint: candidate.base_fingerprint.as_deref(),
                })
                .collect::<Vec<_>>();
            let insert_started = Instant::now();
            database
                .insert_scan_manifest_deltas("manifest", &deltas)
                .await?;
            delta_write_elapsed_ms += insert_started.elapsed().as_secs_f64() * 1_000.0;
            after_path = Some(last.relative_path.clone());
        }
        persisted_samples_ms.push(started.elapsed().as_secs_f64() * 1_000.0);
        persisted_candidate_read_ms.push(candidate_read_elapsed_ms);
        persisted_delta_write_ms.push(delta_write_elapsed_ms);
        persisted_query_counts.push(database.query_count());
        assert_eq!(total, file_count);
    }

    eprintln!(
        "LUX-561 SQLx comparison: backend={backend}, sqlite_version={sqlite_version}, \
         postgres_version={}, file_count_per_root={file_count}, page_size={PAGE_SIZE}, \
         completion_first_candidate_count_ms={first_candidate_count_ms:?}, \
         completion_first_candidate_exists_ms={first_candidate_exists_ms:?}, \
         completion_last_candidate_count_ms={last_candidate_count_ms:?}, \
         completion_last_candidate_exists_ms={last_candidate_exists_ms:?}, \
         completion_no_candidate_count_ms={no_candidate_count_ms:?}, \
         completion_no_candidate_exists_ms={no_candidate_exists_ms:?}, \
         lite_ms={lite_samples_ms:?}, lite_query_calls={lite_query_counts:?}, \
         tuple_cursor_plan={lite_plan:?}, fixed_root_lite_plan={fixed_root_lite_plan:?}, \
         persisted_ms={persisted_samples_ms:?}, \
         persisted_candidate_read_ms={persisted_candidate_read_ms:?}, \
         persisted_delta_write_ms={persisted_delta_write_ms:?}, \
         persisted_query_calls={persisted_query_counts:?}, \
         restarted_directory_plan={persisted_plan:?}, \
         fixed_root_persisted_plan={fixed_root_persisted_plan:?}",
        postgres_version.unwrap_or_else(|| "not PostgreSQL".to_owned()),
    );
    database.close().await;
    Ok(())
}

async fn measure_manifest_diff_completion_queries(
    database: &Database,
    count_query: &str,
    exists_query: &str,
    expected_count: i64,
    expected_exists: i64,
) -> Result<(Vec<f64>, Vec<f64>), Box<dyn std::error::Error>> {
    let mut count_samples_ms = Vec::with_capacity(3);
    let mut exists_samples_ms = Vec::with_capacity(3);
    for round in 0..3 {
        if round % 2 == 0 {
            let started = std::time::Instant::now();
            let count: i64 = database
                .query_scalar(sqlx::AssertSqlSafe(count_query.to_owned()))
                .bind("manifest")
                .fetch_one(database.pool())
                .await?;
            count_samples_ms.push(started.elapsed().as_secs_f64() * 1_000.0);
            assert_eq!(count, expected_count);

            let started = std::time::Instant::now();
            let exists: i64 = database
                .query_scalar(sqlx::AssertSqlSafe(exists_query.to_owned()))
                .bind("manifest")
                .fetch_one(database.pool())
                .await?;
            exists_samples_ms.push(started.elapsed().as_secs_f64() * 1_000.0);
            assert_eq!(exists, expected_exists);
        } else {
            let started = std::time::Instant::now();
            let exists: i64 = database
                .query_scalar(sqlx::AssertSqlSafe(exists_query.to_owned()))
                .bind("manifest")
                .fetch_one(database.pool())
                .await?;
            exists_samples_ms.push(started.elapsed().as_secs_f64() * 1_000.0);
            assert_eq!(exists, expected_exists);

            let started = std::time::Instant::now();
            let count: i64 = database
                .query_scalar(sqlx::AssertSqlSafe(count_query.to_owned()))
                .bind("manifest")
                .fetch_one(database.pool())
                .await?;
            count_samples_ms.push(started.elapsed().as_secs_f64() * 1_000.0);
            assert_eq!(count, expected_count);
        }
    }
    Ok((count_samples_ms, exists_samples_ms))
}
