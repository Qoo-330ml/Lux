use std::time::Duration;

use luxd::{
    api::AppState,
    application::{libraries::LibraryService, scanner::ScanJobService, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    domain::ids::LibraryId,
    library::LibraryKind,
    observability::logs::LogStore,
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
    sqlx::query("DROP TABLE scan_job_events")
        .execute(database.pool())
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
    let (_, legacy_scan_events) = LogStore::new(&config.config_dir)
        .list_scan_job_events("shutdown-scan-pending", None, None, 0, 10)
        .await?;
    assert_eq!(legacy_scan_events.len(), 1);
    assert_eq!(
        legacy_scan_events[0].event_code,
        "LEGACY_SCAN_REQUIRES_NEW_MANIFEST"
    );
    assert!(legacy_scan_events[0].message.contains("重试"));
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

/// A database that looks like the state a shutdown leaves behind: one library whose first
/// `RECONCILE_LIBRARY` scan was cancelled with `SERVER_SHUTDOWN`.
struct InterruptedFirstScan {
    _temp_dir: tempfile::TempDir,
    config: Config,
    database: Database,
    library_id: LibraryId,
    cancelled_job_id: String,
}

async fn interrupted_first_scan(
    with_root: bool,
) -> Result<InterruptedFirstScan, Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    SetupService::new(database.clone())?
        .complete("Admin", "Admin", "correct password")
        .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    if with_root {
        let root_path = temp_dir.path().join("movies");
        tokio::fs::create_dir_all(&root_path).await?;
        libraries
            .add_root(library.id, root_path.to_str().ok_or("non-UTF-8 root path")?)
            .await?;
    }
    let job = ScanJobService::new(database.clone())
        .create_movie_scan_job(library.id)
        .await?;
    database.cancel_incomplete_jobs_for_shutdown().await?;
    Ok(InterruptedFirstScan {
        _temp_dir: temp_dir,
        config,
        database,
        library_id: library.id,
        cancelled_job_id: job.id,
    })
}

/// Runs the startup recovery through the same state `main` builds.
async fn resume_interrupted_first_scans(
    config: &Config,
    database: &Database,
) -> Result<usize, Box<dyn std::error::Error>> {
    let database = database.clone();
    let state = AppState::ready(
        config.clone(),
        database.clone(),
        SetupService::new(database.clone())?,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database)?,
    );
    Ok(state.resume_shutdown_interrupted_first_scans().await?)
}

async fn resume_fixture_first_scans(
    fixture: &InterruptedFirstScan,
) -> Result<usize, Box<dyn std::error::Error>> {
    resume_interrupted_first_scans(&fixture.config, &fixture.database).await
}

async fn scan_job_ids(database: &Database, library_id: LibraryId) -> Vec<String> {
    sqlx::query_scalar("SELECT id FROM scan_jobs WHERE library_id = ? ORDER BY id")
        .bind(library_id.to_string())
        .fetch_all(database.pool())
        .await
        .expect("scan job ids")
}

async fn scan_job_status(database: &Database, job_id: &str) -> String {
    sqlx::query_scalar("SELECT status FROM scan_jobs WHERE id = ?")
        .bind(job_id)
        .fetch_one(database.pool())
        .await
        .expect("scan job status")
}

async fn wait_for_scan_job_status(
    database: &Database,
    job_id: &str,
    expected: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let status = scan_job_status(database, job_id).await;
        if status == expected {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(
                format!("scan job {job_id} stayed in {status}, expected {expected}").into(),
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn startup_resumes_first_scans_interrupted_by_shutdown()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = interrupted_first_scan(true).await?;
    let cancelled: (String, Option<String>) =
        sqlx::query_as("SELECT status, error FROM scan_jobs WHERE id = ?")
            .bind(&fixture.cancelled_job_id)
            .fetch_one(fixture.database.pool())
            .await?;
    assert_eq!(
        cancelled,
        ("CANCELLED".to_owned(), Some("SERVER_SHUTDOWN".to_owned()))
    );

    assert_eq!(resume_fixture_first_scans(&fixture).await?, 1);

    let jobs = scan_job_ids(&fixture.database, fixture.library_id).await;
    assert_eq!(jobs.len(), 2, "exactly one replacement scan job: {jobs:?}");
    let replacement = jobs
        .iter()
        .find(|id| **id != fixture.cancelled_job_id)
        .ok_or("the recovery must queue a replacement scan job")?
        .clone();
    let job_type: String = sqlx::query_scalar("SELECT job_type FROM scan_jobs WHERE id = ?")
        .bind(&replacement)
        .fetch_one(fixture.database.pool())
        .await?;
    assert_eq!(job_type, "RECONCILE_LIBRARY");

    // The recovered scan really runs: once it completes, `last_scan_at` is written and the library
    // no longer matches the recovery predicate.
    wait_for_scan_job_status(&fixture.database, &replacement, "COMPLETED").await?;
    let last_scan_at: Option<i64> =
        sqlx::query_scalar("SELECT last_scan_at FROM libraries WHERE id = ?")
            .bind(fixture.library_id.to_string())
            .fetch_one(fixture.database.pool())
            .await?;
    assert!(last_scan_at.is_some(), "the resumed scan must be recorded");

    assert_eq!(resume_fixture_first_scans(&fixture).await?, 0);
    assert_eq!(
        scan_job_ids(&fixture.database, fixture.library_id)
            .await
            .len(),
        2
    );
    Ok(())
}

#[tokio::test]
async fn startup_skips_first_scans_of_libraries_that_were_scanned()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = interrupted_first_scan(true).await?;
    sqlx::query("UPDATE libraries SET last_scan_at = unixepoch() WHERE id = ?")
        .bind(fixture.library_id.to_string())
        .execute(fixture.database.pool())
        .await?;

    assert_eq!(resume_fixture_first_scans(&fixture).await?, 0);
    assert_eq!(
        scan_job_ids(&fixture.database, fixture.library_id).await,
        vec![fixture.cancelled_job_id.clone()]
    );
    Ok(())
}

#[tokio::test]
async fn startup_skips_scans_cancelled_for_a_real_failure() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = interrupted_first_scan(true).await?;
    sqlx::query("UPDATE scan_jobs SET error = 'DISK_FULL' WHERE id = ?")
        .bind(&fixture.cancelled_job_id)
        .execute(fixture.database.pool())
        .await?;

    assert_eq!(resume_fixture_first_scans(&fixture).await?, 0);
    assert_eq!(
        scan_job_ids(&fixture.database, fixture.library_id).await,
        vec![fixture.cancelled_job_id.clone()]
    );
    Ok(())
}

#[tokio::test]
async fn startup_skips_first_scans_of_disabled_libraries() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = interrupted_first_scan(true).await?;
    sqlx::query("UPDATE libraries SET is_enabled = 0 WHERE id = ?")
        .bind(fixture.library_id.to_string())
        .execute(fixture.database.pool())
        .await?;

    assert_eq!(resume_fixture_first_scans(&fixture).await?, 0);
    assert_eq!(
        scan_job_ids(&fixture.database, fixture.library_id).await,
        vec![fixture.cancelled_job_id.clone()]
    );
    Ok(())
}

#[tokio::test]
async fn startup_skips_first_scans_of_libraries_without_a_root()
-> Result<(), Box<dyn std::error::Error>> {
    // `start_library_scan` itself accepts a rootless library, so the recovery predicate has to
    // reject it: scanning nothing would only write `last_scan_at` for an unusable library.
    let fixture = interrupted_first_scan(false).await?;
    assert_eq!(resume_fixture_first_scans(&fixture).await?, 0);
    assert_eq!(
        scan_job_ids(&fixture.database, fixture.library_id).await,
        vec![fixture.cancelled_job_id.clone()]
    );
    Ok(())
}

#[tokio::test]
async fn startup_does_not_queue_a_second_scan_while_one_is_active()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = interrupted_first_scan(true).await?;
    // A manual scan is already queued: the recovery must leave it alone instead of racing it.
    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation)
         VALUES ('active-scan', ?, 'RECONCILE_LIBRARY', 'PENDING', 'generation-active')",
    )
    .bind(fixture.library_id.to_string())
    .execute(fixture.database.pool())
    .await?;

    assert_eq!(resume_fixture_first_scans(&fixture).await?, 0);
    let jobs = scan_job_ids(&fixture.database, fixture.library_id).await;
    assert_eq!(jobs.len(), 2, "no third job: {jobs:?}");
    assert_eq!(
        scan_job_status(&fixture.database, "active-scan").await,
        "PENDING"
    );
    Ok(())
}

#[tokio::test]
async fn startup_resumes_more_interrupted_first_scans_than_a_single_batch()
-> Result<(), Box<dyn std::error::Error>> {
    // More libraries than one storage page holds: the recovery has to page instead of dropping the
    // rest silently.
    const LIBRARY_COUNT: usize = 33;
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    SetupService::new(database.clone())?
        .complete("Admin", "Admin", "correct password")
        .await?;
    let libraries = LibraryService::new(database.clone());
    let jobs = ScanJobService::new(database.clone());
    let mut library_ids = Vec::with_capacity(LIBRARY_COUNT);
    for index in 0..LIBRARY_COUNT {
        let library = libraries
            .create_library(&format!("Movies {index:02}"), LibraryKind::Movie, false)
            .await?;
        let root_path = temp_dir.path().join(format!("movies-{index:02}"));
        tokio::fs::create_dir_all(&root_path).await?;
        libraries
            .add_root(library.id, root_path.to_str().ok_or("non-UTF-8 root path")?)
            .await?;
        jobs.create_movie_scan_job(library.id).await?;
        library_ids.push(library.id);
    }
    database.cancel_incomplete_jobs_for_shutdown().await?;

    assert_eq!(
        resume_interrupted_first_scans(&config, &database).await?,
        LIBRARY_COUNT
    );

    for library_id in library_ids {
        let jobs = scan_job_ids(&database, library_id).await;
        assert_eq!(
            jobs.len(),
            2,
            "library {library_id} should have the cancelled job plus one replacement: {jobs:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn startup_skips_first_scans_whose_replacement_failed()
-> Result<(), Box<dyn std::error::Error>> {
    // A scan that keeps failing must not be retried on every startup: only the *latest* scan of the
    // library decides, and a `FAILED` one is not the "cancelled by a shutdown" evidence.
    let fixture = interrupted_first_scan(true).await?;
    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation, error, created_at)
         VALUES ('failed-replacement', ?, 'RECONCILE_LIBRARY', 'FAILED', 'generation-failed',
                 'DISK_READ_FAILED', unixepoch() + 10)",
    )
    .bind(fixture.library_id.to_string())
    .execute(fixture.database.pool())
    .await?;

    assert_eq!(resume_fixture_first_scans(&fixture).await?, 0);
    assert_eq!(
        scan_job_ids(&fixture.database, fixture.library_id).await.len(),
        2,
        "the failed replacement must not be retried"
    );
    Ok(())
}
