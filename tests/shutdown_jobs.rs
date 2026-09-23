use luxd::{
    application::{libraries::LibraryService, scanner::ScanJobService, setup::SetupService},
    config::Config,
    library::LibraryKind,
    storage::Database,
};

#[tokio::test]
async fn shutdown_cancels_every_incomplete_persistent_job() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let admin = SetupService::new(database.clone())?
        .complete("Admin", "Admin", "correct password")
        .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let library_id = library.id.to_string();
    let admin_id = admin.id.to_string();
    let legacy_root_path = temp_dir.path().join("legacy-root");
    tokio::fs::create_dir_all(&legacy_root_path).await?;
    let legacy_root = libraries
        .add_root(
            library.id,
            legacy_root_path.to_str().ok_or("non-UTF-8 root path")?,
        )
        .await?
        .root;

    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation, scan_phase)
         VALUES ('shutdown-scan-pending', ?, 'RECONCILE_LIBRARY', 'PENDING', 'generation-1', 'DISCOVERY'),
                ('shutdown-scan-postprocessing', ?, 'RECONCILE_LIBRARY', 'COMPLETED', 'generation-2', 'POSTPROCESSING')",
    )
    .bind(&library_id)
    .bind(&library_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO reconciliation_scan_entries (
             job_id, library_root_id, relative_path, entry_type
         ) VALUES ('shutdown-scan-pending', ?, 'legacy.mkv', 'FILE')",
    )
    .bind(legacy_root.id.to_string())
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO scan_manifests (id, job_id, library_id, state)
         VALUES ('shutdown-manifest', 'shutdown-scan-postprocessing', ?, 'POSTPROCESSING')",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO strm_probe_jobs (id, operation_id, library_id, status, concurrency)
         VALUES ('shutdown-strm', 'shutdown-operation', ?, 'RUNNING', 1)",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO chapter_detection_jobs (
             id, library_id, plugin_id, status, concurrency,
             intro_window_seconds, credits_window_seconds, match_threshold
         ) VALUES ('shutdown-chapter', ?, 'test.chapter-detector', 'PENDING', 1, 30, 60, 0.8)",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO library_cover_jobs (id, library_id, status)
         VALUES ('shutdown-cover', ?, 'RUNNING')",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO danmaku_match_jobs (id, library_id, status, concurrency)
         VALUES ('shutdown-danmaku', ?, 'PENDING', 1)",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO metadata_reidentify_jobs (id, status)
         VALUES ('shutdown-metadata', 'RUNNING')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO emby_migration_jobs (
             id, plugin_id, created_by_user_id, source_label, source_base_url,
             secret_ref, status, phase, merge_policy
         ) VALUES ('shutdown-emby', 'org.lux.emby-migration', ?, 'Emby', 'http://emby.test',
                   'secret-ref', 'PENDING', 'TESTING', 'MERGE')",
    )
    .bind(&admin_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO person_index_rebuild_jobs (library_id, status)
         VALUES (?, 'QUEUED')",
    )
    .bind(&library_id)
    .execute(database.pool())
    .await?;

    let cancelled = database.cancel_incomplete_jobs_for_shutdown().await?;
    assert_eq!(cancelled, 9);

    let statuses: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT status, error FROM scan_jobs
         WHERE id IN ('shutdown-scan-pending', 'shutdown-scan-postprocessing')
         UNION ALL SELECT status, error FROM strm_probe_jobs WHERE id = 'shutdown-strm'
         UNION ALL SELECT status, error FROM chapter_detection_jobs WHERE id = 'shutdown-chapter'
         UNION ALL SELECT status, error FROM library_cover_jobs WHERE id = 'shutdown-cover'
         UNION ALL SELECT status, error FROM danmaku_match_jobs WHERE id = 'shutdown-danmaku'
         UNION ALL SELECT status, error FROM metadata_reidentify_jobs WHERE id = 'shutdown-metadata'
         UNION ALL SELECT status, error FROM emby_migration_jobs WHERE id = 'shutdown-emby'
         UNION ALL SELECT status, error FROM person_index_rebuild_jobs WHERE library_id = ?",
    )
    .bind(&library_id)
    .fetch_all(database.pool())
    .await?;
    assert_eq!(statuses.len(), 9);
    assert!(statuses.iter().all(|(status, _)| status == "CANCELLED"));
    assert_eq!(
        statuses
            .iter()
            .filter(|(_, error)| error.as_deref() == Some("SERVER_SHUTDOWN"))
            .count(),
        8
    );
    let legacy_scan_status: (String, Option<String>) =
        sqlx::query_as("SELECT status, error FROM scan_jobs WHERE id = 'shutdown-scan-pending'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(
        legacy_scan_status,
        (
            "CANCELLED".to_owned(),
            Some("LEGACY_SCAN_REQUIRES_NEW_MANIFEST".to_owned())
        )
    );
    let legacy_scan_event: (String, String) = sqlx::query_as(
        "SELECT event_code, message FROM scan_job_events
         WHERE job_id = 'shutdown-scan-pending'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(legacy_scan_event.0, "LEGACY_SCAN_REQUIRES_NEW_MANIFEST");
    assert!(legacy_scan_event.1.contains("重试"));
    let preserved_legacy_entries: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM reconciliation_scan_entries
         WHERE job_id = 'shutdown-scan-pending'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(preserved_legacy_entries, 1);

    database.run_database_lifecycle_cleanup().await?;
    let retained_for_retry: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM reconciliation_scan_entries
         WHERE job_id = 'shutdown-scan-pending'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(retained_for_retry, 1);
    let jobs = ScanJobService::new(database.clone());
    let postprocessing_retry = jobs.retry("shutdown-scan-postprocessing").await?;
    assert_eq!(postprocessing_retry.id, "shutdown-scan-postprocessing");
    jobs.run_to_completion(&postprocessing_retry.id, 100, None)
        .await?;
    let completed_manifest_state: String = sqlx::query_scalar(
        "SELECT state FROM scan_manifests WHERE job_id = 'shutdown-scan-postprocessing'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(completed_manifest_state, "COMPLETED");

    let retry = jobs.retry("shutdown-scan-pending").await?;
    assert_ne!(retry.id, "shutdown-scan-pending");
    let replacement_manifest_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_manifests WHERE job_id = ? AND state = 'DISCOVERING'",
    )
    .bind(&retry.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(replacement_manifest_count, 1);
    let copied_legacy_entries: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM reconciliation_scan_entries
         WHERE job_id = 'shutdown-scan-pending'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(copied_legacy_entries, 0);
    jobs.run_to_completion(&retry.id, 100, None).await?;

    let active_count: i64 = sqlx::query_scalar(
        "SELECT SUM(count) FROM (
             SELECT COUNT(*) AS count FROM scan_jobs
             WHERE status IN ('PENDING', 'RUNNING')
                OR (status = 'COMPLETED' AND scan_phase = 'POSTPROCESSING')
             UNION ALL SELECT COUNT(*) AS count FROM strm_probe_jobs WHERE status IN ('PENDING', 'RUNNING')
             UNION ALL SELECT COUNT(*) AS count FROM chapter_detection_jobs WHERE status IN ('PENDING', 'RUNNING')
             UNION ALL SELECT COUNT(*) AS count FROM library_cover_jobs WHERE status IN ('PENDING', 'RUNNING')
             UNION ALL SELECT COUNT(*) AS count FROM danmaku_match_jobs WHERE status IN ('PENDING', 'RUNNING')
             UNION ALL SELECT COUNT(*) AS count FROM metadata_reidentify_jobs WHERE status IN ('QUEUED', 'RUNNING')
             UNION ALL SELECT COUNT(*) AS count FROM emby_migration_jobs WHERE status IN ('PENDING', 'RUNNING')
             UNION ALL SELECT COUNT(*) AS count FROM person_index_rebuild_jobs WHERE status IN ('QUEUED', 'RUNNING')
         )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(active_count, 0);
    Ok(())
}
