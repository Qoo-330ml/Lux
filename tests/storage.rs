use std::fs;

use sha2::{Digest, Sha384};

use luxd::{
    application::{libraries::LibraryService, scanner::LibraryScanner},
    config::Config,
    library::LibraryKind,
    storage::Database,
};

#[test]
fn historical_media_catalog_migration_keeps_its_original_checksum() {
    let migration = include_str!("../migrations/0006_media_catalog.sql");

    assert_eq!(
        format!("{:x}", Sha384::digest(migration.as_bytes())),
        "4c0ce4f36416069631e85e66ad25c6808cf42b9ea60287db68e945e67f32860ab07bf9706dadc5a120fda8eba5bae323"
    );
}

#[test]
fn postgres_bootstrap_migration_keeps_its_original_checksum() {
    let migration = include_str!("../migrations-postgres/0001_bootstrap.sql");

    assert_eq!(
        format!("{:x}", Sha384::digest(migration.as_bytes())),
        "81fb302801af162714b21496d70ca696af6f710145070a1b69b31c229d879806a2e36acd9aca651c0c96867d1bd5d4ca"
    );
}

#[tokio::test]
async fn metadata_candidate_identity_migration_collapses_pending_duplicates()
-> Result<(), Box<dyn std::error::Error>> {
    let pool = sqlx::SqlitePool::connect("sqlite::memory:").await?;
    sqlx::query(
        "CREATE TABLE metadata_candidates (
            id TEXT PRIMARY KEY,
            item_id TEXT NOT NULL,
            provider TEXT NOT NULL,
            provider_id TEXT NOT NULL,
            candidate_json TEXT NOT NULL,
            score REAL NOT NULL,
            status TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        )",
    )
    .execute(&pool)
    .await?;
    for (id, score, created_at) in [
        ("older", 90.0, 10_i64),
        ("best", 95.0, 11),
        ("newest", 95.0, 12),
    ] {
        sqlx::query(
            "INSERT INTO metadata_candidates (
                id, item_id, provider, provider_id, candidate_json,
                score, status, created_at, updated_at
             ) VALUES (?, 'item', 'TMDB', '603', '{}', ?, 'PENDING', ?, ?)",
        )
        .bind(id)
        .bind(score)
        .bind(created_at)
        .bind(created_at)
        .execute(&pool)
        .await?;
    }

    for statement in include_str!("../migrations/0095_metadata_candidate_identity.sql")
        .split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
    {
        sqlx::query(statement).execute(&pool).await?;
    }

    let pending: (String, f64) =
        sqlx::query_as("SELECT id, score FROM metadata_candidates WHERE status = 'PENDING'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(pending.0, "newest");
    assert_eq!(pending.1, 95.0);
    let rejected: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM metadata_candidates WHERE status = 'REJECTED'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(rejected, 2);
    let index_name: String = sqlx::query_scalar(
        "SELECT name FROM sqlite_master
         WHERE type = 'index' AND name = 'idx_metadata_candidates_pending_identity'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(index_name, "idx_metadata_candidates_pending_identity");
    Ok(())
}

#[tokio::test]
async fn empty_config_dir_runs_migrations_and_configures_sqlite()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    };

    let database = Database::connect(&config).await?;

    assert_eq!(database.schema_version().await?, 103);
    assert!(config_dir.join("lux.db").is_file());

    let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(database.pool())
        .await?;
    let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(database.pool())
        .await?;
    let busy_timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
        .fetch_one(database.pool())
        .await?;

    assert_eq!(journal_mode.to_ascii_lowercase(), "wal");
    assert_eq!(foreign_keys, 1);
    assert_eq!(busy_timeout, 5_000);

    database.close().await;

    let second_database = Database::connect(&config).await?;
    assert_eq!(second_database.schema_version().await?, 103);
    second_database.close().await;
    Ok(())
}

#[tokio::test]
async fn scan_job_targets_schema_is_available_from_an_empty_database()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let table_name: String = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'scan_job_targets'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(table_name, "scan_job_targets");
    assert_eq!(database.schema_version().await?, 103);
    Ok(())
}

#[tokio::test]
async fn emby_migration_migration_creates_state_and_history_tables()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;

    for table in [
        "emby_migration_jobs",
        "emby_migration_user_links",
        "emby_migration_item_matches",
        "emby_migration_import_records",
        "emby_migration_user_bindings",
        "emby_migration_person_favorites",
        "playback_history_events",
        "legacy_person_migration_state",
        "person_manifest_restore_state",
        "person_manifest_index_state",
    ] {
        let exists: i64 = sqlx::query_scalar(
            "SELECT EXISTS(
                SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?
            )",
        )
        .bind(table)
        .fetch_one(database.pool())
        .await?;
        assert_eq!(exists, 1, "missing migration table {table}");
    }
    assert_eq!(database.schema_version().await?, 103);
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn metadata_job_scope_migration_adds_explicit_scope_and_summary_indexes()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;

    let columns: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('metadata_reidentify_jobs')
         WHERE name IN ('library_id', 'job_scope')",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(columns, 2);
    let indexes: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'index' AND name IN (
             'idx_metadata_reidentify_items_item_job',
             'idx_metadata_reidentify_jobs_scope_status'
         )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(indexes, 2);
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn empty_sqlite_database_creates_people_index_tables_and_indexes()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;

    let tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table' AND name IN (
             'person_index_rebuild_jobs',
             'person_index_item_state',
             'person_credits'
         )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(tables, 3);
    let indexes: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'index' AND name IN (
             'idx_person_index_rebuild_jobs_status',
             'idx_person_credits_person_item',
             'idx_media_items_people_visible'
         )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(indexes, 3);

    database.close().await;

    let database = Database::connect(&config).await?;
    sqlx::query("DROP INDEX idx_media_items_people_visible")
        .execute(database.pool())
        .await?;
    database.close().await;

    let database = Database::connect(&config).await?;
    let restored_indexes: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'index' AND name = 'idx_media_items_people_visible'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(restored_indexes, 1);
    Ok(())
}

#[tokio::test]
async fn sqlite_media_item_search_triggers_follow_rebuilt_table()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;

    let trigger_sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master
         WHERE type = 'trigger' AND name = 'media_items_search_ai'",
    )
    .fetch_one(database.pool())
    .await?;
    assert!(!trigger_sql.contains("media_items_legacy"));
    let stale_references: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE sql LIKE '%media_items_legacy%'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(stale_references, 0);

    database.close().await;
    Ok(())
}

#[tokio::test]
async fn media_chapter_migration_creates_source_scoped_table()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;

    assert_eq!(database.schema_version().await?, 103);
    let table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'media_chapters'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(table_count, 1);
    let job_table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table' AND name IN ('chapter_detection_jobs', 'chapter_detection_job_items')",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(job_table_count, 2);

    let create_sql: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'media_chapters'",
    )
    .fetch_one(database.pool())
    .await?;
    assert!(create_sql.contains("REFERENCES media_sources(id) ON DELETE CASCADE"));
    assert!(create_sql.contains("INTRO_START"));
    assert!(create_sql.contains("CREDITS_START"));
    assert!(!create_sql.contains("'CHAPTER'"));

    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Marker migration", LibraryKind::Movie, false)
        .await?;
    let media_root = temp_dir.path().join("media");
    tokio::fs::create_dir_all(&media_root).await?;
    tokio::fs::write(media_root.join("Marker.Movie.2026.mkv"), b"fixture").await?;
    libraries
        .add_root(library.id, media_root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let source_id: String = sqlx::query_scalar("SELECT id FROM media_sources")
        .fetch_one(database.pool())
        .await?;

    sqlx::query(
        "INSERT INTO media_chapters (
            id, media_source_id, start_position_ticks, marker_type,
            chapter_index, provider_id, confidence
         ) VALUES (?, ?, 10000000, 'INTRO_START', 0, 'org.lux.detector', 0.95)",
    )
    .bind("intro-start")
    .bind(&source_id)
    .execute(database.pool())
    .await?;
    let ordinary_chapter = sqlx::query(
        "INSERT INTO media_chapters (
            id, media_source_id, start_position_ticks, marker_type,
            chapter_index, provider_id, confidence
         ) VALUES (?, ?, 20000000, 'CHAPTER', 1, 'org.lux.detector', 0.95)",
    )
    .bind("ordinary-chapter")
    .bind(&source_id)
    .execute(database.pool())
    .await;
    assert!(ordinary_chapter.is_err());
    let duplicate_marker = sqlx::query(
        "INSERT INTO media_chapters (
            id, media_source_id, start_position_ticks, marker_type,
            chapter_index, provider_id, confidence
         ) VALUES (?, ?, 30000000, 'INTRO_START', 1, 'org.lux.detector', 0.90)",
    )
    .bind("duplicate-intro-start")
    .bind(&source_id)
    .execute(database.pool())
    .await;
    assert!(duplicate_marker.is_err());
    let negative_start = sqlx::query(
        "INSERT INTO media_chapters (
            id, media_source_id, start_position_ticks, marker_type,
            chapter_index, provider_id, confidence
         ) VALUES (?, ?, -1, 'INTRO_END', 1, 'org.lux.detector', 0.90)",
    )
    .bind("negative-intro-end")
    .bind(&source_id)
    .execute(database.pool())
    .await;
    assert!(negative_start.is_err());
    let invalid_confidence = sqlx::query(
        "INSERT INTO media_chapters (
            id, media_source_id, start_position_ticks, marker_type,
            chapter_index, provider_id, confidence
         ) VALUES (?, ?, 40000000, 'CREDITS_START', 2, 'org.lux.detector', 1.1)",
    )
    .bind("invalid-credits-start")
    .bind(&source_id)
    .execute(database.pool())
    .await;
    assert!(invalid_confidence.is_err());
    let blank_provider = sqlx::query(
        "INSERT INTO media_chapters (
            id, media_source_id, start_position_ticks, marker_type,
            chapter_index, provider_id, confidence
         ) VALUES (?, ?, 40000000, 'CREDITS_START', 2, '   ', 0.90)",
    )
    .bind("blank-provider")
    .bind(&source_id)
    .execute(database.pool())
    .await;
    assert!(blank_provider.is_err());

    sqlx::query("DELETE FROM media_sources WHERE id = ?")
        .bind(&source_id)
        .execute(database.pool())
        .await?;
    let remaining_markers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media_chapters")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(remaining_markers, 0);

    database.close().await;
    Ok(())
}

#[tokio::test]
async fn strm_probe_scan_job_reference_prevents_scan_job_deletion()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let library = LibraryService::new(database.clone())
        .create_library("Migration test", LibraryKind::Movie, false)
        .await?;

    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation)
         VALUES (?, ?, 'INCREMENTAL_SCAN', 'COMPLETED', 'migration-test')",
    )
    .bind("migration-scan-job")
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO strm_probe_jobs (
            id, operation_id, library_id, status, concurrency, target_scan_job_id
         ) VALUES (?, ?, ?, 'COMPLETED', 1, ?)",
    )
    .bind("migration-probe-job")
    .bind("migration-operation")
    .bind(library.id.to_string())
    .bind("migration-scan-job")
    .execute(database.pool())
    .await?;

    let deletion = sqlx::query("DELETE FROM scan_jobs WHERE id = ?")
        .bind("migration-scan-job")
        .execute(database.pool())
        .await;
    assert!(deletion.is_err());

    let target_scan_job_id: Option<String> =
        sqlx::query_scalar("SELECT target_scan_job_id FROM strm_probe_jobs WHERE id = ?")
            .bind("migration-probe-job")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(target_scan_job_id.as_deref(), Some("migration-scan-job"));

    database.close().await;
    Ok(())
}

#[tokio::test]
async fn sqlite_write_probe_succeeds_and_only_persists_reserved_marker()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;

    database.probe_write().await?;
    assert_eq!(database.schema_version().await?, 103);
    let probe_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM lux_meta WHERE key = '__lux_write_probe__'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(probe_rows, 1);

    database.close().await;
    Ok(())
}

#[tokio::test]
async fn library_registers_reconciliation_and_metadata_tasks_only()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, true)
        .await?;

    let task_types: Vec<String> = sqlx::query_scalar(
        "SELECT task_type FROM scheduled_task_configs
         WHERE owner_type = 'LIBRARY' AND owner_id = ?
         ORDER BY task_type",
    )
    .bind(library.id.to_string())
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        task_types,
        vec![
            "METADATA_PARSE".to_owned(),
            "RECONCILIATION_SCAN".to_owned()
        ]
    );

    database.close().await;
    Ok(())
}

#[tokio::test]
async fn library_persists_chapter_source_selection() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library_with_scraper_and_chapter_source(
            "Shows",
            LibraryKind::Series,
            false,
            None,
            None,
            false,
        )
        .await?;
    assert_eq!(library.chapter_source_id, None);

    let updated = libraries
        .update_settings(
            library.id,
            luxd::application::libraries::LibrarySettingsPatch {
                chapter_source_id: Some(Some("org.lux.intro-outro-detector".to_owned())),
                ..Default::default()
            },
        )
        .await?;
    assert_eq!(
        updated.library.chapter_source_id.as_deref(),
        Some("org.lux.intro-outro-detector")
    );

    let reopened = libraries.get_library(library.id).await?;
    assert_eq!(
        reopened.chapter_source_id.as_deref(),
        Some("org.lux.intro-outro-detector")
    );
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn read_only_config_dir_returns_a_clear_database_error()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("readonly");
    fs::create_dir(&config_dir)?;
    let mut permissions = fs::metadata(&config_dir)?.permissions();
    permissions.set_mode(0o500);
    fs::set_permissions(&config_dir, permissions)?;

    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    };
    let result = Database::connect(&config).await;

    let mut permissions = fs::metadata(&config_dir)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&config_dir, permissions)?;

    let error = match result {
        Ok(database) => {
            database.close().await;
            return Err("read-only config directory unexpectedly opened".into());
        }
        Err(error) => error,
    };
    assert!(error.to_string().contains("lux.db"));
    Ok(())
}

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
