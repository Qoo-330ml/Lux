use luxd::{
    config::Config,
    storage::{Database, SqliteStatisticsRefresh},
};

static MAINTENANCE_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Builds a throwaway SQLite-backed database, the default backend every
/// self-hosted install starts with.
async fn test_database(
    temp_dir: &tempfile::TempDir,
) -> Result<Database, Box<dyn std::error::Error>> {
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    Ok(Database::connect(&config).await?)
}

#[tokio::test]
async fn maintenance_round_runs_on_sqlite_and_reports_outcome()
-> Result<(), Box<dyn std::error::Error>> {
    let _serial_guard = MAINTENANCE_TEST_LOCK.lock().await;
    let temp_dir = tempfile::tempdir()?;
    let database = test_database(&temp_dir).await?;
    let report = database.run_database_maintenance_round().await;
    // A freshly migrated database has no reclaimable pages, so the round must
    // complete without inventing work or recording failures.
    assert!(
        report.failed.is_empty(),
        "unexpected failures: {:?}",
        report.failed
    );
    assert!(
        report.parameters_applied.is_empty(),
        "sqlite must never record autovacuum overrides"
    );
    Ok(())
}

#[tokio::test]
async fn maintenance_never_reports_parameter_overrides_for_sqlite()
-> Result<(), Box<dyn std::error::Error>> {
    let _serial_guard = MAINTENANCE_TEST_LOCK.lock().await;
    let temp_dir = tempfile::tempdir()?;
    let database = test_database(&temp_dir).await?;
    // Seed reclaimable pages so the round has a real VACUUM to run.
    sqlx::query("CREATE TABLE maintenance_probe (id INTEGER PRIMARY KEY, payload TEXT NOT NULL)")
        .execute(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO maintenance_probe (payload)
         WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < 2000)
         SELECT hex(randomblob(2048)) FROM n",
    )
    .execute(database.pool())
    .await?;
    sqlx::query("DELETE FROM maintenance_probe")
        .execute(database.pool())
        .await?;

    let report = database.run_database_maintenance_round().await;
    assert!(
        report.failed.is_empty(),
        "unexpected failures: {:?}",
        report.failed
    );
    // SQLite has no autovacuum. The round must say so by leaving the list empty
    // rather than silently pretending an override was applied.
    assert!(
        report.parameters_applied.is_empty(),
        "sqlite has no autovacuum; overrides must stay empty, got {:?}",
        report.parameters_applied
    );
    Ok(())
}

#[tokio::test]
async fn concurrent_maintenance_rounds_do_not_overlap() -> Result<(), Box<dyn std::error::Error>> {
    let _serial_guard = MAINTENANCE_TEST_LOCK.lock().await;
    let temp_dir = tempfile::tempdir()?;
    let database = test_database(&temp_dir).await?;
    // Seed reclaimable pages so both rounds genuinely have work available and
    // the assertion cannot pass merely because there was nothing to do.
    sqlx::query("CREATE TABLE overlap_probe (id INTEGER PRIMARY KEY, payload TEXT NOT NULL)")
        .execute(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO overlap_probe (payload)
         WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < 2000)
         SELECT hex(randomblob(2048)) FROM n",
    )
    .execute(database.pool())
    .await?;
    sqlx::query("DELETE FROM overlap_probe")
        .execute(database.pool())
        .await?;

    let first = database.clone();
    let second = database.clone();
    let (left, right) = tokio::join!(
        first.run_database_maintenance_round(),
        second.run_database_maintenance_round()
    );
    // Exactly one round may touch tables; the other must short-circuit instead
    // of racing the same VACUUM.
    let touched_left = !left.vacuumed.is_empty();
    let touched_right = !right.vacuumed.is_empty();
    assert!(
        !(touched_left && touched_right),
        "two rounds ran concurrently: left={left:?} right={right:?}"
    );
    Ok(())
}

/// Confirms the probe really left reclaimable pages behind. Without this the
/// guards under test could "pass" simply because there was nothing to do.
async fn assert_freelist_is_nonzero(database: &Database) -> Result<(), Box<dyn std::error::Error>> {
    let freelist: i64 = sqlx::query_scalar("PRAGMA freelist_count")
        .fetch_one(database.pool())
        .await?;
    assert!(
        freelist > 0,
        "test setup must leave a non-empty freelist, got {freelist}"
    );
    Ok(())
}

#[tokio::test]
async fn maintenance_skips_round_while_a_scan_is_running() -> Result<(), Box<dyn std::error::Error>>
{
    let _serial_guard = MAINTENANCE_TEST_LOCK.lock().await;
    let temp_dir = tempfile::tempdir()?;
    let database = test_database(&temp_dir).await?;
    // Seed reclaimable pages so a round genuinely has work available.
    sqlx::query("CREATE TABLE scan_guard_probe (id INTEGER PRIMARY KEY, payload TEXT NOT NULL)")
        .execute(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO scan_guard_probe (payload)
         WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < 2000)
         SELECT hex(randomblob(2048)) FROM n",
    )
    .execute(database.pool())
    .await?;
    sqlx::query("DELETE FROM scan_guard_probe")
        .execute(database.pool())
        .await?;
    assert_freelist_is_nonzero(&database).await?;

    // Positive control first: with no scan running the round must do work.
    let idle_round = database.run_database_maintenance_round().await;
    assert!(
        !idle_round.vacuumed.is_empty(),
        "positive control: expected a vacuum with no scan running, got {idle_round:?}"
    );

    // Re-seed reclaimable pages so the guarded round would otherwise act.
    sqlx::query(
        "INSERT INTO scan_guard_probe (payload) SELECT hex(randomblob(64)) FROM (SELECT 1) LIMIT 0",
    )
    .execute(database.pool())
    .await?;
    sqlx::query("DROP TABLE IF EXISTS scan_guard_probe_2")
        .execute(database.pool())
        .await?;
    sqlx::query("CREATE TABLE scan_guard_probe_2 (id INTEGER PRIMARY KEY, payload TEXT NOT NULL)")
        .execute(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO scan_guard_probe_2 (payload)
         WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < 2000)
         SELECT hex(randomblob(2048)) FROM n",
    )
    .execute(database.pool())
    .await?;
    sqlx::query("DELETE FROM scan_guard_probe_2")
        .execute(database.pool())
        .await?;
    assert_freelist_is_nonzero(&database).await?;

    // Now an in-flight scan must win over maintenance.
    sqlx::query("INSERT INTO libraries (id, name, kind) VALUES ('guard-lib', 'Guard', 'MOVIE')")
        .execute(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation)
         VALUES ('guard-job', 'guard-lib', 'RECONCILE_LIBRARY', 'RUNNING', 'generation')",
    )
    .execute(database.pool())
    .await?;

    let guarded_round = database.run_database_maintenance_round().await;
    assert!(
        guarded_round.vacuumed.is_empty(),
        "maintenance must stand down while a scan runs, got {:?}",
        guarded_round.vacuumed
    );
    Ok(())
}

async fn statistics_table_exists(database: &Database) -> Result<bool, Box<dyn std::error::Error>> {
    let exists: i64 = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'sqlite_stat1')",
    )
    .fetch_one(database.pool())
    .await?;
    Ok(exists != 0)
}

async fn seed_catalog_row(database: &Database) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query("INSERT INTO libraries (id, name, kind) VALUES ('stats-lib', 'Stats', 'MOVIE')")
        .execute(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES ('stats-movie', 'stats-lib', 'MOVIE', 'Stats', 'stats', 'LOCAL_CONFIRMED')",
    )
    .execute(database.pool())
    .await?;
    Ok(())
}

#[tokio::test]
async fn statistics_refresh_does_not_describe_an_empty_catalog()
-> Result<(), Box<dyn std::error::Error>> {
    let _serial_guard = MAINTENANCE_TEST_LOCK.lock().await;
    let temp_dir = tempfile::tempdir()?;
    let database = test_database(&temp_dir).await?;

    let refreshed = database.refresh_query_statistics().await?;

    assert_eq!(refreshed, SqliteStatisticsRefresh::SkippedEmpty);
    assert!(
        !statistics_table_exists(&database).await?,
        "statistics of an empty library would tell the planner every table has one row"
    );
    Ok(())
}

#[tokio::test]
async fn statistics_refresh_analyzes_once_then_hands_over_to_optimize()
-> Result<(), Box<dyn std::error::Error>> {
    let _serial_guard = MAINTENANCE_TEST_LOCK.lock().await;
    let temp_dir = tempfile::tempdir()?;
    let database = test_database(&temp_dir).await?;
    seed_catalog_row(&database).await?;

    let first = database.refresh_query_statistics().await?;
    assert_eq!(first, SqliteStatisticsRefresh::Analyzed);
    assert!(statistics_table_exists(&database).await?);
    let described: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_stat1 WHERE tbl = 'media_items'")
            .fetch_one(database.pool())
            .await?;
    assert!(
        described > 0,
        "the catalog table the planner has to estimate is still undescribed"
    );

    let second = database.refresh_query_statistics().await?;
    assert_eq!(
        second,
        SqliteStatisticsRefresh::Optimized,
        "a second pass must not rescan every index"
    );
    Ok(())
}

#[tokio::test]
async fn statistics_refresh_stands_down_while_a_scan_runs() -> Result<(), Box<dyn std::error::Error>>
{
    let _serial_guard = MAINTENANCE_TEST_LOCK.lock().await;
    let temp_dir = tempfile::tempdir()?;
    let database = test_database(&temp_dir).await?;
    seed_catalog_row(&database).await?;
    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation)
         VALUES ('stats-job', 'stats-lib', 'RECONCILE_LIBRARY', 'RUNNING', 'generation')",
    )
    .execute(database.pool())
    .await?;

    let refreshed = database.refresh_query_statistics().await?;

    assert_eq!(refreshed, SqliteStatisticsRefresh::SkippedBusy);
    assert!(!statistics_table_exists(&database).await?);
    Ok(())
}

#[tokio::test]
async fn bulk_change_statistics_refresh_runs_a_full_analyze_each_time()
-> Result<(), Box<dyn std::error::Error>> {
    let _serial_guard = MAINTENANCE_TEST_LOCK.lock().await;
    let temp_dir = tempfile::tempdir()?;
    let database = test_database(&temp_dir).await?;
    seed_catalog_row(&database).await?;

    let first = database
        .refresh_query_statistics_after_bulk_change()
        .await?;
    assert_eq!(first, SqliteStatisticsRefresh::Analyzed);
    assert!(statistics_table_exists(&database).await?);
    let described: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_stat1 WHERE tbl = 'media_items'")
            .fetch_one(database.pool())
            .await?;
    assert!(described > 0, "the scanned catalog must be described");

    // A completed scan can change thousands of rows, so the next bulk pass re-samples instead of
    // trusting `PRAGMA optimize` to notice the growth.
    let second = database
        .refresh_query_statistics_after_bulk_change()
        .await?;
    assert_eq!(second, SqliteStatisticsRefresh::Analyzed);
    Ok(())
}

#[tokio::test]
async fn bulk_change_statistics_refresh_skips_an_empty_catalog()
-> Result<(), Box<dyn std::error::Error>> {
    let _serial_guard = MAINTENANCE_TEST_LOCK.lock().await;
    let temp_dir = tempfile::tempdir()?;
    let database = test_database(&temp_dir).await?;

    let refreshed = database
        .refresh_query_statistics_after_bulk_change()
        .await?;

    assert_eq!(refreshed, SqliteStatisticsRefresh::SkippedEmpty);
    assert!(!statistics_table_exists(&database).await?);
    Ok(())
}

#[tokio::test]
async fn bulk_change_statistics_refresh_stands_down_while_a_scan_runs()
-> Result<(), Box<dyn std::error::Error>> {
    let _serial_guard = MAINTENANCE_TEST_LOCK.lock().await;
    let temp_dir = tempfile::tempdir()?;
    let database = test_database(&temp_dir).await?;
    seed_catalog_row(&database).await?;
    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation)
         VALUES ('bulk-stats-job', 'stats-lib', 'RECONCILE_LIBRARY', 'RUNNING', 'generation')",
    )
    .execute(database.pool())
    .await?;

    let refreshed = database
        .refresh_query_statistics_after_bulk_change()
        .await?;

    assert_eq!(refreshed, SqliteStatisticsRefresh::SkippedBusy);
    assert!(!statistics_table_exists(&database).await?);
    Ok(())
}

/// Seeds reclaimable pages so a round genuinely has something to do.
async fn seed_reclaimable_pages(
    database: &Database,
    table: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE TABLE {table} (id INTEGER PRIMARY KEY, payload TEXT NOT NULL)"
    )))
    .execute(database.pool())
    .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO {table} (payload)
         WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < 2000)
         SELECT hex(randomblob(2048)) FROM n"
    )))
    .execute(database.pool())
    .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {table}")))
        .execute(database.pool())
        .await?;
    Ok(())
}

async fn seed_playback_session(
    database: &Database,
    last_event_at_sql: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    sqlx::query("INSERT INTO libraries (id, name, kind) VALUES ('stream-lib', 'Stream', 'MOVIE')")
        .execute(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES ('stream-movie', 'stream-lib', 'MOVIE', 'Stream', 'stream', 'LOCAL_CONFIRMED')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO users (id, username_normalized, display_name, password_hash)
         VALUES ('stream-user', 'stream-user', 'Stream', 'not-a-real-hash')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO playback_sessions (
             id, user_id, item_id, play_session_id, device_id, state, position_ticks,
             last_event_at
         ) VALUES ('stream-session', 'stream-user', 'stream-movie', 'play-1', 'device-1',
                   'PLAYING', 100, {last_event_at_sql})"
    )))
    .execute(database.pool())
    .await?;
    Ok(())
}

#[tokio::test]
async fn maintenance_defers_while_a_client_is_streaming() -> Result<(), Box<dyn std::error::Error>>
{
    let _serial_guard = MAINTENANCE_TEST_LOCK.lock().await;
    let temp_dir = tempfile::tempdir()?;
    let database = test_database(&temp_dir).await?;
    seed_reclaimable_pages(&database, "stream_probe").await?;
    seed_playback_session(&database, "unixepoch()").await?;

    let deferred = database.run_database_maintenance_round().await;

    assert!(
        deferred.vacuumed.is_empty(),
        "a VACUUM holding the write lock competes with progress writes, got {:?}",
        deferred.vacuumed
    );
    assert_freelist_is_nonzero(&database).await?;
    Ok(())
}

#[tokio::test]
async fn maintenance_resumes_once_the_stream_is_stale() -> Result<(), Box<dyn std::error::Error>> {
    let _serial_guard = MAINTENANCE_TEST_LOCK.lock().await;
    let temp_dir = tempfile::tempdir()?;
    let database = test_database(&temp_dir).await?;
    seed_reclaimable_pages(&database, "stale_probe").await?;
    seed_playback_session(&database, "unixepoch() - 86400").await?;

    let completed = database.run_database_maintenance_round().await;

    assert!(
        !completed.vacuumed.is_empty(),
        "a session that stopped reporting an hour ago must not block maintenance"
    );
    Ok(())
}
