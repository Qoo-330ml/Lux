use std::{collections::BTreeMap, ffi::OsStr, fs, path::Path};

use sha2::{Digest, Sha384};

use luxd::{
    application::{libraries::LibraryService, scanner::LibraryScanner},
    config::Config,
    library::LibraryKind,
    storage::Database,
};

#[test]
fn migration_versions_are_unique_per_backend() -> Result<(), Box<dyn std::error::Error>> {
    for directory in ["migrations", "migrations-postgres"] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(directory);
        let mut versions = BTreeMap::<i64, Vec<String>>::new();

        for entry in fs::read_dir(path)? {
            let path = entry?.path();
            if path.extension() != Some(OsStr::new("sql")) {
                continue;
            }
            let file_name = path
                .file_name()
                .and_then(OsStr::to_str)
                .ok_or("migration filename is not valid UTF-8")?;
            let version = file_name
                .split_once('_')
                .ok_or("migration filename has no version separator")?
                .0
                .parse::<i64>()?;
            versions
                .entry(version)
                .or_default()
                .push(file_name.to_owned());
        }

        let duplicates: Vec<_> = versions
            .into_iter()
            .filter(|(_, files)| files.len() > 1)
            .collect();
        assert!(
            duplicates.is_empty(),
            "{directory} has duplicate migration versions: {duplicates:?}"
        );
    }
    Ok(())
}

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

#[test]
fn postgres_derived_index_refresh_uses_statement_level_triggers() {
    let migration = include_str!("../migrations-postgres/0137_statement_derived_index_refresh.sql");

    for trigger in [
        "media_items_search_ai",
        "media_items_search_au",
        "media_items_search_ad",
        "item_aliases_search_ai",
        "item_aliases_search_au",
        "item_aliases_search_ad",
        "media_sources_availability_ai",
        "media_sources_availability_au",
        "media_sources_availability_ad",
        "filesystem_entries_availability_au",
    ] {
        assert!(
            migration.contains(&format!("DROP TRIGGER IF EXISTS {trigger}")),
            "migration must replace the row trigger {trigger}"
        );
    }

    assert!(migration.contains("FOR EACH STATEMENT"));
    assert!(!migration.contains("FOR EACH ROW"));
    assert!(migration.contains("REFERENCING NEW TABLE"));
}

#[test]
fn postgres_media_search_refresh_does_not_rescan_aliases_per_item() {
    let migration =
        include_str!("../migrations-postgres/0138_avoid_alias_rescan_on_media_item_refresh.sql");

    assert!(migration.contains("lux_refresh_media_search_items_insert_stmt"));
    assert!(migration.contains("           ''"));
    assert!(migration.contains("LEFT JOIN media_search existing"));
    assert!(!migration.contains("FROM item_aliases"));
}

#[test]
fn postgres_media_source_insert_refresh_only_promotes_unavailable_items() {
    let migration =
        include_str!("../migrations-postgres/0139_promote_media_availability_on_source_insert.sql");

    assert!(migration.contains("SET has_available_source = 1"));
    assert!(migration.contains("has_available_source = 0"));
    assert!(migration.contains("JOIN filesystem_entries"));
    assert!(!migration.contains("CASE WHEN EXISTS"));
}

#[test]
fn postgres_media_search_insert_refresh_avoids_redundant_upsert() {
    let migration =
        include_str!("../migrations-postgres/0140_media_search_insert_without_conflict_probe.sql");

    assert!(migration.contains("INSERT INTO media_search"));
    assert!(migration.contains("lux_refresh_media_search_items_insert_stmt"));
    assert!(!migration.contains("ON CONFLICT"));
}

#[tokio::test]
async fn postgres_scan_job_migration_allows_one_active_job_per_type()
-> Result<(), Box<dyn std::error::Error>> {
    let pool = sqlx::SqlitePool::connect(":memory:").await?;
    sqlx::query(
        "CREATE TABLE scan_jobs (
            id TEXT PRIMARY KEY,
            library_id TEXT NOT NULL,
            job_type TEXT NOT NULL,
            status TEXT NOT NULL
        )",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "CREATE UNIQUE INDEX idx_scan_jobs_one_active
         ON scan_jobs(library_id)
         WHERE status IN ('PENDING', 'RUNNING')",
    )
    .execute(&pool)
    .await?;

    for statement in include_str!("../migrations-postgres/0109_scan_job_active_by_type.sql")
        .split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
    {
        sqlx::query(statement).execute(&pool).await?;
    }

    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status)
         VALUES ('full', 'library', 'RECONCILE_LIBRARY', 'RUNNING'),
                ('incremental', 'library', 'INCREMENTAL_SCAN', 'PENDING')",
    )
    .execute(&pool)
    .await?;
    let active_jobs: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_jobs
         WHERE library_id = 'library' AND status IN ('PENDING', 'RUNNING')",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(active_jobs, 2);

    let duplicate_incremental = sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status)
         VALUES ('incremental-2', 'library', 'INCREMENTAL_SCAN', 'RUNNING')",
    )
    .execute(&pool)
    .await;
    assert!(duplicate_incremental.is_err());
    Ok(())
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

    assert_eq!(database.schema_version().await?, 136);
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
    assert_eq!(second_database.schema_version().await?, 136);
    second_database.close().await;
    Ok(())
}

#[tokio::test]
async fn full_scan_manifest_schema_is_created_for_sqlite() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;

    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master
         WHERE type = 'table' AND name LIKE 'scan_manifest_%'
         ORDER BY name",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        tables,
        vec![
            "scan_manifest_deltas",
            "scan_manifest_directories",
            "scan_manifest_entries",
            "scan_manifest_roots",
            "scan_manifest_seen_paths",
            "scan_manifests",
        ]
    );

    let indexes: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master
         WHERE type = 'index' AND name LIKE 'idx_scan_manifest_%'
         ORDER BY name",
    )
    .fetch_all(database.pool())
    .await?;
    assert!(indexes.contains(&"idx_scan_manifest_directories_frontier".to_owned()));
    assert!(
        !indexes.contains(&"idx_scan_manifest_entries_path".to_owned()),
        "the entries primary key already has the same column order"
    );
    assert!(indexes.contains(&"idx_scan_manifest_deltas_pending".to_owned()));
    let entry_lookup_plan: Vec<(i64, i64, i64, String)> = sqlx::query_as(
        "EXPLAIN QUERY PLAN
         SELECT device, inode FROM scan_manifest_entries
         WHERE manifest_id = 'manifest' AND library_root_id = 'root'
           AND relative_path = ''
         ORDER BY observation_sequence DESC LIMIT 1",
    )
    .fetch_all(database.pool())
    .await?;
    assert!(
        entry_lookup_plan
            .iter()
            .any(|(_, _, _, detail)| detail.contains("sqlite_autoindex_scan_manifest_entries_1")),
        "the primary-key index should retain the root-observation lookup: {entry_lookup_plan:?}"
    );
    let workflow_version_column: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('scan_manifests')
         WHERE name = 'workflow_version'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(workflow_version_column, 1);
    let discovery_format_version_column: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('scan_manifests')
         WHERE name = 'discovery_format_version'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(discovery_format_version_column, 1);
    let sequence_column: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('scan_manifest_roots')
         WHERE name = 'next_observation_sequence'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(sequence_column, 1);
    let last_seen_change_kind_column: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('filesystem_entries')
         WHERE name = 'last_seen_change_kind'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(last_seen_change_kind_column, 1);

    sqlx::query(
        "INSERT INTO libraries (id, name, kind) VALUES ('manifest-library', 'Manifest', 'MOVIE')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO library_roots (
             id, library_id, canonical_path, display_path, is_available, is_writable
         ) VALUES ('manifest-root', 'manifest-library', '/media', '/media', 1, 1)",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation)
         VALUES ('manifest-job', 'manifest-library', 'RECONCILE_LIBRARY', 'PENDING', 'generation-1')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO scan_manifests (id, job_id, library_id, state)
         VALUES ('manifest-job', 'manifest-job', 'manifest-library', 'DISCOVERING')",
    )
    .execute(database.pool())
    .await?;
    let default_discovery_format_version: i64 = sqlx::query_scalar(
        "SELECT discovery_format_version FROM scan_manifests WHERE id = 'manifest-job'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(default_discovery_format_version, 2);
    let default_targets_ready: i64 = sqlx::query_scalar(
        "SELECT postprocessing_targets_ready FROM scan_manifests WHERE id = 'manifest-job'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(default_targets_ready, 1);
    sqlx::query(
        "INSERT INTO scan_manifest_roots (manifest_id, library_root_id, state)
         VALUES ('manifest-job', 'manifest-root', 'PENDING')",
    )
    .execute(database.pool())
    .await?;
    let default_target_stage: String = sqlx::query_scalar(
        "SELECT postprocessing_target_stage FROM scan_manifest_roots
         WHERE manifest_id = 'manifest-job' AND library_root_id = 'manifest-root'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(default_target_stage, "DONE");
    let first_observation = sqlx::query(
        "INSERT INTO scan_manifest_entries (
             manifest_id, library_root_id, relative_path, observation_sequence,
             entry_kind, size, modified_at, fingerprint
         ) VALUES ('manifest-job', 'manifest-root', 'movie.mkv', 1, 'FILE', 123, 456, ?)",
    )
    .bind(vec![1_u8, 2, 3])
    .execute(database.pool())
    .await?;
    assert_eq!(first_observation.rows_affected(), 1);
    let duplicate_observation = sqlx::query(
        "INSERT INTO scan_manifest_entries (
             manifest_id, library_root_id, relative_path, observation_sequence,
             entry_kind, size, modified_at
         ) VALUES ('manifest-job', 'manifest-root', 'movie.mkv', 1, 'FILE', 999, 999)",
    )
    .execute(database.pool())
    .await;
    assert!(duplicate_observation.is_err());
    sqlx::query(
        "INSERT INTO scan_manifest_entries (
             manifest_id, library_root_id, relative_path, observation_sequence,
             entry_kind, size, modified_at, fingerprint
         ) VALUES ('manifest-job', 'manifest-root', 'movie.mkv', 2, 'FILE', 124, 457, ?)",
    )
    .bind(vec![4_u8, 5, 6])
    .execute(database.pool())
    .await?;
    let observation_versions: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_manifest_entries
         WHERE manifest_id = 'manifest-job' AND library_root_id = 'manifest-root'
           AND relative_path = 'movie.mkv'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(observation_versions, 2);
    let invalid_state = sqlx::query(
        "UPDATE scan_manifest_roots SET state = 'DONE'
         WHERE manifest_id = 'manifest-job' AND library_root_id = 'manifest-root'",
    )
    .execute(database.pool())
    .await;
    assert!(invalid_state.is_err());

    let manifest_schema: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master
         WHERE type = 'table' AND name = 'scan_manifests'",
    )
    .fetch_one(database.pool())
    .await?;
    assert!(manifest_schema.contains("'DISCOVERING'"));
    assert!(manifest_schema.contains("'POSTPROCESSING'"));
    let manifest_entry_device: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('scan_manifest_entries')
         WHERE name = 'device'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(manifest_entry_device, 1);
    let manifest_resume_state: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('scan_manifests')
         WHERE name = 'resume_state'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(manifest_resume_state, 1);
    assert_eq!(database.schema_version().await?, 136);

    database.close().await;
    Ok(())
}

#[tokio::test]
async fn completed_manifest_payload_cleanup_is_batched_and_preserves_checkpoints()
-> Result<(), Box<dyn std::error::Error>> {
    const OBSERVATION_COUNT: i64 = 1_001;

    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Manifest cleanup", LibraryKind::Movie, false)
        .await?;
    let root_path = temp_dir.path().join("Movies");
    fs::create_dir_all(&root_path)?;
    let root = libraries
        .add_root(library.id, root_path.to_str().ok_or("non-UTF-8 root path")?)
        .await?
        .root;
    let library_id = library.id.to_string();
    let root_id = root.id.to_string();
    for (job_id, status) in [
        ("cleanup-completed-manifest-job", "COMPLETED"),
        ("cleanup-failed-manifest-job", "FAILED"),
    ] {
        sqlx::query(
            "INSERT INTO scan_jobs (id, library_id, job_type, status, generation, scan_phase)
             VALUES (?, ?, 'RECONCILE_LIBRARY', ?, ?, 'IDLE')",
        )
        .bind(job_id)
        .bind(&library_id)
        .bind(status)
        .bind(format!("generation-{job_id}"))
        .execute(database.pool())
        .await?;
    }
    sqlx::query(
        "INSERT INTO scan_manifests (
             id, job_id, library_id, state, resume_state, root_count,
             observed_file_count, add_count, applied_delta_count, completed_at
         ) VALUES
             ('cleanup-completed-manifest', 'cleanup-completed-manifest-job', ?,
              'COMPLETED', NULL, 1, ?, ?, ?, unixepoch()),
             ('cleanup-failed-manifest', 'cleanup-failed-manifest-job', ?,
              'FAILED', 'APPLYING', 1, 1, 1, 0, NULL)",
    )
    .bind(&library_id)
    .bind(OBSERVATION_COUNT)
    .bind(OBSERVATION_COUNT)
    .bind(OBSERVATION_COUNT)
    .bind(&library_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO scan_manifest_roots (manifest_id, library_root_id, state)
         VALUES ('cleanup-completed-manifest', ?, 'COMPLETE'),
                ('cleanup-failed-manifest', ?, 'INCOMPLETE')",
    )
    .bind(&root_id)
    .bind(&root_id)
    .execute(database.pool())
    .await?;

    let mut transaction = database.pool().begin().await?;
    for index in 0..OBSERVATION_COUNT {
        let relative_path = format!("movie-{index:04}.mkv");
        let directory_path = format!("folder-{index:04}");
        sqlx::query(
            "INSERT INTO scan_manifest_directories (
                 manifest_id, library_root_id, relative_path, state
             ) VALUES ('cleanup-completed-manifest', ?, ?, 'COMPLETE')",
        )
        .bind(&root_id)
        .bind(directory_path)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "INSERT INTO scan_manifest_entries (
                 manifest_id, library_root_id, relative_path, observation_sequence,
                 entry_kind, size, modified_at, fingerprint
             ) VALUES ('cleanup-completed-manifest', ?, ?, 1, 'FILE', 1, 1, X'01')",
        )
        .bind(&root_id)
        .bind(&relative_path)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "INSERT INTO scan_manifest_deltas (
                 id, manifest_id, library_root_id, relative_path,
                 observation_sequence, delta_kind, state
             ) VALUES (?, 'cleanup-completed-manifest', ?, ?, 1, 'ADD', 'APPLIED')",
        )
        .bind(format!("delta-{index:04}"))
        .bind(&root_id)
        .bind(relative_path)
        .execute(&mut *transaction)
        .await?;
    }
    sqlx::query(
        "INSERT INTO scan_manifest_directories (
             manifest_id, library_root_id, relative_path, state
         ) VALUES ('cleanup-failed-manifest', ?, 'retained', 'PENDING')",
    )
    .bind(&root_id)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "INSERT INTO scan_manifest_entries (
             manifest_id, library_root_id, relative_path, observation_sequence,
             entry_kind, size, modified_at
         ) VALUES ('cleanup-failed-manifest', ?, 'retained.mkv', 1, 'FILE', 1, 1)",
    )
    .bind(&root_id)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "INSERT INTO scan_manifest_deltas (
             id, manifest_id, library_root_id, relative_path,
             observation_sequence, delta_kind, state
         ) VALUES ('retained-delta', 'cleanup-failed-manifest', ?, 'retained.mkv',
                  1, 'ADD', 'PENDING')",
    )
    .bind(&root_id)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "INSERT INTO scan_manifest_seen_paths (
             manifest_id, library_root_id, relative_path
         ) VALUES ('cleanup-completed-manifest', ?, 'seen.mkv'),
                  ('cleanup-failed-manifest', ?, 'retained.mkv')",
    )
    .bind(&root_id)
    .bind(&root_id)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;

    let report = database
        .run_database_lifecycle_cleanup()
        .await?
        .ok_or("cleanup should report completed manifest payloads")?;
    assert_eq!(
        report.scan_manifest_entries_deleted,
        OBSERVATION_COUNT as u64 + 1
    );
    assert_eq!(
        report.scan_manifest_directories_deleted,
        OBSERVATION_COUNT as u64
    );
    assert_eq!(
        report.scan_manifest_deltas_deleted,
        OBSERVATION_COUNT as u64
    );

    let completed_summary: (String, i64, i64) = sqlx::query_as(
        "SELECT state, observed_file_count, add_count FROM scan_manifests
         WHERE id = 'cleanup-completed-manifest'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        completed_summary,
        ("COMPLETED".to_owned(), OBSERVATION_COUNT, OBSERVATION_COUNT)
    );
    let retained_checkpoint: (String, String, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT m.state, m.resume_state,
                (SELECT COUNT(*) FROM scan_manifest_entries e WHERE e.manifest_id = m.id),
                (SELECT COUNT(*) FROM scan_manifest_directories d WHERE d.manifest_id = m.id),
                (SELECT COUNT(*) FROM scan_manifest_deltas x WHERE x.manifest_id = m.id),
                (SELECT COUNT(*) FROM scan_manifest_seen_paths s WHERE s.manifest_id = m.id)
         FROM scan_manifests m WHERE m.id = 'cleanup-failed-manifest'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        retained_checkpoint,
        ("FAILED".to_owned(), "APPLYING".to_owned(), 1, 1, 1, 1)
    );
    assert!(database.run_database_lifecycle_cleanup().await?.is_none());
    Ok(())
}

#[tokio::test]
async fn full_scan_manifest_upgrade_preserves_existing_scan_data()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let migration_dir = temp_dir.path().join("migrations");
    fs::create_dir(&migration_dir)?;
    let source_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    for entry in fs::read_dir(&source_dir)? {
        let source = entry?.path();
        let version = source
            .file_name()
            .and_then(OsStr::to_str)
            .and_then(|name| name.split_once('_'))
            .map(|(version, _)| version.parse::<i64>())
            .transpose()?;
        if version.is_some_and(|version| version <= 127) {
            fs::copy(
                &source,
                migration_dir.join(source.file_name().ok_or("missing filename")?),
            )?;
        }
    }

    let database_path = temp_dir.path().join("manifest-upgrade.db");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&database_path)
                .create_if_missing(true),
        )
        .await?;
    sqlx::migrate::Migrator::new(migration_dir.clone())
        .await?
        .run(&pool)
        .await?;
    sqlx::query(
        "INSERT INTO libraries (id, name, kind) VALUES ('legacy-library', 'Legacy', 'MOVIE')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO library_roots (
             id, library_id, canonical_path, display_path, is_available, is_writable
         ) VALUES ('legacy-root', 'legacy-library', '/media', '/media', 1, 1)",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation)
         VALUES ('legacy-job', 'legacy-library', 'RECONCILE_LIBRARY', 'RUNNING', 'legacy-generation')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO reconciliation_scan_entries (
             job_id, library_root_id, relative_path, entry_type
         ) VALUES ('legacy-job', 'legacy-root', 'movie.mkv', 'FILE')",
    )
    .execute(&pool)
    .await?;

    fs::copy(
        source_dir.join("0128_full_scan_manifest.sql"),
        migration_dir.join("0128_full_scan_manifest.sql"),
    )?;
    sqlx::migrate::Migrator::new(migration_dir.clone())
        .await?
        .run(&pool)
        .await?;

    sqlx::query(
        "INSERT INTO scan_manifests (id, job_id, library_id, state)
         VALUES ('legacy-manifest', 'legacy-job', 'legacy-library', 'DISCOVERING')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_manifest_roots (manifest_id, library_root_id, state)
         VALUES ('legacy-manifest', 'legacy-root', 'COMPLETE')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_manifest_entries (
             manifest_id, library_root_id, relative_path, observation_sequence,
             entry_kind, size, modified_at, inode, fingerprint
         ) VALUES ('legacy-manifest', 'legacy-root', 'movie.mkv', 1,
                   'FILE', 1234, 100, 42, X'010203')",
    )
    .execute(&pool)
    .await?;

    fs::copy(
        source_dir.join("0129_login_background_plugin_cache.sql"),
        migration_dir.join("0129_login_background_plugin_cache.sql"),
    )?;
    fs::copy(
        source_dir.join("0130_scan_manifest_entry_device.sql"),
        migration_dir.join("0130_scan_manifest_entry_device.sql"),
    )?;
    fs::copy(
        source_dir.join("0131_scan_manifest_resume_state.sql"),
        migration_dir.join("0131_scan_manifest_resume_state.sql"),
    )?;
    fs::copy(
        source_dir.join("0132_streamed_manifest_indexing.sql"),
        migration_dir.join("0132_streamed_manifest_indexing.sql"),
    )?;
    sqlx::migrate::Migrator::new(migration_dir.clone())
        .await?
        .run(&pool)
        .await?;

    sqlx::query("INSERT INTO libraries (id, name, kind) VALUES ('v2-library', 'V2', 'MOVIE')")
        .execute(&pool)
        .await?;
    sqlx::query(
        "INSERT INTO library_roots (
             id, library_id, canonical_path, display_path, is_available, is_writable
         ) VALUES ('v2-root', 'v2-library', '/v2-root', '/v2-root', 1, 1)",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation)
         VALUES ('active-v2-job', 'v2-library', 'RECONCILE_LIBRARY', 'RUNNING',
                 'active-v2-generation')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_manifests (
             id, job_id, library_id, state, workflow_version, root_count
         ) VALUES ('active-v2-manifest', 'active-v2-job', 'v2-library',
                  'DISCOVERING', 2, 1)",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_manifest_roots (manifest_id, library_root_id, state)
         VALUES ('active-v2-manifest', 'v2-root', 'SCANNING')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_manifest_entries (
             manifest_id, library_root_id, relative_path, observation_sequence,
             entry_kind, size, modified_at, device, inode, fingerprint
         ) VALUES ('active-v2-manifest', 'v2-root', 'still-present.mkv', 1,
                   'FILE', 123, 456, 1, 2, X'010203')",
    )
    .execute(&pool)
    .await?;
    fs::copy(
        source_dir.join("0133_skip_redundant_source_availability_update.sql"),
        migration_dir.join("0133_skip_redundant_source_availability_update.sql"),
    )?;
    fs::copy(
        source_dir.join("0134_drop_redundant_manifest_entry_index.sql"),
        migration_dir.join("0134_drop_redundant_manifest_entry_index.sql"),
    )?;
    sqlx::migrate::Migrator::new(migration_dir.clone())
        .await?
        .run(&pool)
        .await?;
    fs::copy(
        source_dir.join("0135_manifest_discovery_format_and_seen_paths.sql"),
        migration_dir.join("0135_manifest_discovery_format_and_seen_paths.sql"),
    )?;
    fs::copy(
        source_dir.join("0136_manifest_postprocessing_target_checkpoint.sql"),
        migration_dir.join("0136_manifest_postprocessing_target_checkpoint.sql"),
    )?;
    sqlx::migrate::Migrator::new(migration_dir.clone())
        .await?
        .run(&pool)
        .await?;

    let schema_version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await?;
    assert_eq!(schema_version, 136);
    let legacy_workflow_version: i64 = sqlx::query_scalar(
        "SELECT workflow_version FROM scan_manifests WHERE id = 'legacy-manifest'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(legacy_workflow_version, 1);
    let legacy_discovery_format_version: i64 = sqlx::query_scalar(
        "SELECT discovery_format_version FROM scan_manifests WHERE id = 'legacy-manifest'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(legacy_discovery_format_version, 2);
    let active_v2_versions: (i64, i64, i64) = sqlx::query_as(
        "SELECT manifest.workflow_version, manifest.discovery_format_version,
                (SELECT COUNT(*) FROM scan_manifest_entries entry
                 WHERE entry.manifest_id = manifest.id AND entry.entry_kind = 'FILE')
         FROM scan_manifests manifest WHERE manifest.id = 'active-v2-manifest'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(active_v2_versions, (2, 2, 1));
    let sequence_allocator: i64 = sqlx::query_scalar(
        "SELECT next_observation_sequence FROM scan_manifest_roots
         WHERE manifest_id = 'legacy-manifest' AND library_root_id = 'legacy-root'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(sequence_allocator, 0);
    let legacy_status: String =
        sqlx::query_scalar("SELECT status FROM scan_jobs WHERE id = 'legacy-job'")
            .fetch_one(&pool)
            .await?;
    assert_eq!(legacy_status, "RUNNING");
    let legacy_entries: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM reconciliation_scan_entries WHERE job_id = 'legacy-job'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(legacy_entries, 1);
    let manifests: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scan_manifests")
        .fetch_one(&pool)
        .await?;
    assert_eq!(manifests, 2);
    let manifest_entry: (i64, i64, Option<i64>) = sqlx::query_as(
        "SELECT size, inode, device FROM scan_manifest_entries
         WHERE manifest_id = 'legacy-manifest' AND relative_path = 'movie.mkv'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(manifest_entry, (1234, 42, None));
    let resume_state_column: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('scan_manifests')
         WHERE name = 'resume_state'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(resume_state_column, 1);

    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn postgres_manifest_migration_uses_only_supported_schema_constructs()
-> Result<(), Box<dyn std::error::Error>> {
    let postgres_migration = include_str!("../migrations-postgres/0128_full_scan_manifest.sql");
    for forbidden in ["COPY ", "UPDATE ", "FOR UPDATE", "TEMP TABLE"] {
        assert!(
            !postgres_migration.contains(forbidden),
            "manifest migration contains unsupported core SQL construct {forbidden}"
        );
    }

    let sqlite_compatible_migration = postgres_migration
        .replace("BYTEA", "BLOB")
        .replace("BIGINT", "INTEGER")
        .replace(
            "DEFAULT (EXTRACT(EPOCH FROM NOW())::INTEGER)",
            "DEFAULT (unixepoch())",
        );
    let postgres_device_migration =
        include_str!("../migrations-postgres/0130_scan_manifest_entry_device.sql");
    assert!(!postgres_device_migration.contains("BIGSERIAL"));
    let postgres_resume_state_migration =
        include_str!("../migrations-postgres/0131_scan_manifest_resume_state.sql");
    assert!(!postgres_resume_state_migration.contains("BIGSERIAL"));
    let postgres_streamed_manifest_migration =
        include_str!("../migrations-postgres/0132_streamed_manifest_indexing.sql");
    assert!(!postgres_streamed_manifest_migration.contains("BIGSERIAL"));
    let postgres_availability_migration =
        include_str!("../migrations-postgres/0133_skip_redundant_source_availability_update.sql");
    assert!(postgres_availability_migration.contains("has_available_source = 0"));
    assert_eq!(
        include_str!("../migrations-postgres/0134_drop_redundant_manifest_entry_index.sql"),
        include_str!("../migrations/0134_drop_redundant_manifest_entry_index.sql")
    );
    let postgres_seen_paths_migration =
        include_str!("../migrations-postgres/0135_manifest_discovery_format_and_seen_paths.sql");
    assert_eq!(
        postgres_seen_paths_migration.replace("BIGINT", "INTEGER"),
        include_str!("../migrations/0135_manifest_discovery_format_and_seen_paths.sql")
    );
    let postgres_target_checkpoint_migration =
        include_str!("../migrations-postgres/0136_manifest_postprocessing_target_checkpoint.sql");
    assert_eq!(
        postgres_target_checkpoint_migration.replace("BIGINT", "INTEGER"),
        include_str!("../migrations/0136_manifest_postprocessing_target_checkpoint.sql")
    );
    let temp_dir = tempfile::tempdir()?;
    let migration_dir = temp_dir.path().join("migrations");
    fs::create_dir(&migration_dir)?;
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await?;
    sqlx::query("CREATE TABLE libraries (id TEXT PRIMARY KEY)")
        .execute(&pool)
        .await?;
    sqlx::query("CREATE TABLE library_roots (id TEXT PRIMARY KEY)")
        .execute(&pool)
        .await?;
    sqlx::query("CREATE TABLE scan_jobs (id TEXT PRIMARY KEY)")
        .execute(&pool)
        .await?;
    fs::write(
        migration_dir.join("0128_full_scan_manifest.sql"),
        sqlite_compatible_migration,
    )?;
    sqlx::migrate::Migrator::new(migration_dir.clone())
        .await?
        .run(&pool)
        .await?;

    sqlx::query("INSERT INTO libraries (id) VALUES ('library')")
        .execute(&pool)
        .await?;
    sqlx::query("INSERT INTO library_roots (id) VALUES ('root')")
        .execute(&pool)
        .await?;
    sqlx::query("INSERT INTO scan_jobs (id) VALUES ('job')")
        .execute(&pool)
        .await?;
    sqlx::query(
        "INSERT INTO scan_manifests (id, job_id, library_id, state)
         VALUES ('manifest', 'job', 'library', 'DISCOVERING')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_manifest_roots (manifest_id, library_root_id, state)
         VALUES ('manifest', 'root', 'COMPLETE')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_manifest_entries (
             manifest_id, library_root_id, relative_path, observation_sequence,
             entry_kind, size, modified_at, inode, fingerprint
         ) VALUES ('manifest', 'root', 'movie.mkv', 1, 'FILE', 987, 100, 42, X'010203')",
    )
    .execute(&pool)
    .await?;

    fs::write(
        migration_dir.join("0130_scan_manifest_entry_device.sql"),
        postgres_device_migration.replace("BIGINT", "INTEGER"),
    )?;
    fs::write(
        migration_dir.join("0131_scan_manifest_resume_state.sql"),
        postgres_resume_state_migration,
    )?;
    fs::write(
        migration_dir.join("0132_streamed_manifest_indexing.sql"),
        postgres_streamed_manifest_migration.replace("BIGINT", "INTEGER"),
    )?;
    sqlx::migrate::Migrator::new(migration_dir)
        .await?
        .run(&pool)
        .await?;
    let tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'
         AND name IN (
             'scan_manifests', 'scan_manifest_roots', 'scan_manifest_directories',
             'scan_manifest_entries', 'scan_manifest_deltas'
         )",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(tables, 5);
    let device_column: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('scan_manifest_entries')
         WHERE name = 'device'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(device_column, 1);
    let resume_state_column: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pragma_table_info('scan_manifests')
         WHERE name = 'resume_state'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(resume_state_column, 1);
    let manifest_entry: (i64, i64, Option<i64>) = sqlx::query_as(
        "SELECT size, inode, device FROM scan_manifest_entries
         WHERE manifest_id = 'manifest' AND relative_path = 'movie.mkv'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(manifest_entry, (987, 42, None));
    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn filesystem_entry_path_keeps_only_the_unique_index()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;

    let duplicate_index: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'index' AND name = 'idx_filesystem_entries_root_path'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(duplicate_index, 0);
    let filesystem_schema: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master
         WHERE type = 'table' AND name = 'filesystem_entries'",
    )
    .fetch_one(database.pool())
    .await?;
    assert!(filesystem_schema.contains("UNIQUE (library_root_id, relative_path)"));
    Ok(())
}

#[tokio::test]
async fn redundant_child_indexes_are_removed_after_migration()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;

    let redundant_indexes: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'index' AND name IN (
             'idx_item_images_item_id',
             'idx_media_streams_source_id',
             'idx_person_credits_item',
             'idx_person_credits_person'
         )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(redundant_indexes, 0);
    Ok(())
}

#[tokio::test]
async fn scan_indexes_keep_only_required_rows_and_lookup_order()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;

    let reconciliation_schema: String = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master
         WHERE type = 'table' AND name = 'reconciliation_scan_entries'",
    )
    .fetch_one(database.pool())
    .await?;
    assert!(
        reconciliation_schema
            .contains("PRIMARY KEY (job_id, entry_type, library_root_id, relative_path)")
    );

    let wide_reconciliation_index: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'index' AND name = 'idx_reconciliation_scan_entries_pending'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(wide_reconciliation_index, 0);

    let scan_target_indexes: Vec<(String, String)> = sqlx::query_as(
        "SELECT name, sql FROM sqlite_master
         WHERE type = 'index' AND name IN (
             'idx_scan_job_targets_probe',
             'idx_scan_job_targets_metadata',
             'idx_scan_job_targets_thumbnail'
         )
         ORDER BY name",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(scan_target_indexes.len(), 3);
    for (_, sql) in scan_target_indexes {
        assert!(
            sql.replace(' ', "").contains("IN('PENDING','FAILED')"),
            "unexpected index: {sql}"
        );
    }

    let external_stream_index: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'index' AND name = 'idx_media_streams_external_path'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(external_stream_index, 0);
    assert_eq!(database.schema_version().await?, 136);
    Ok(())
}

#[tokio::test]
async fn scan_index_compaction_preserves_existing_rows_during_upgrade()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let migration_dir = temp_dir.path().join("migrations");
    fs::create_dir(&migration_dir)?;
    let source_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    for entry in fs::read_dir(&source_dir)? {
        let source = entry?.path();
        let version = source
            .file_name()
            .and_then(OsStr::to_str)
            .and_then(|name| name.split_once('_'))
            .map(|(version, _)| version.parse::<i64>())
            .transpose()?;
        if version.is_some_and(|version| version <= 117) {
            fs::copy(
                &source,
                migration_dir.join(source.file_name().ok_or("missing filename")?),
            )?;
        }
    }

    let database_path = temp_dir.path().join("upgrade.db");
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&database_path)
                .create_if_missing(true),
        )
        .await?;
    sqlx::migrate::Migrator::new(migration_dir.clone())
        .await?
        .run(&pool)
        .await?;

    sqlx::query(
        "INSERT INTO libraries (id, name, kind) VALUES ('migration-library', 'Migration', 'MOVIE')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO library_roots (
             id, library_id, canonical_path, display_path, is_available, is_writable
         ) VALUES ('migration-root', 'migration-library', '/media', '/media', 1, 1)",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation)
         VALUES ('migration-job', 'migration-library', 'RECONCILE_LIBRARY', 'RUNNING', 'generation-1')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO reconciliation_scan_entries (
             job_id, library_root_id, relative_path, entry_type, created_at
         ) VALUES
             ('migration-job', 'migration-root', '', 'DIRECTORY', 101),
             ('migration-job', 'migration-root', 'movie.mkv', 'FILE', 102)",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_job_targets (
             job_id, target_type, target_id, item_id, change_kind,
             probe_state, metadata_state, thumbnail_state
         ) VALUES
             ('migration-job', 'ITEM', 'target-pending', 'item-pending', 'NEW',
              'SKIPPED', 'PENDING', 'PENDING'),
             ('migration-job', 'ITEM', 'target-done', 'item-done', 'CHANGED',
              'SKIPPED', 'DONE', 'DONE')",
    )
    .execute(&pool)
    .await?;

    fs::copy(
        source_dir.join("0118_scan_index_compaction.sql"),
        migration_dir.join("0118_scan_index_compaction.sql"),
    )?;
    sqlx::migrate::Migrator::new(migration_dir)
        .await?
        .run(&pool)
        .await?;

    let entries: Vec<(String, String, String, i64)> = sqlx::query_as(
        "SELECT job_id, library_root_id, relative_path, created_at
         FROM reconciliation_scan_entries
         ORDER BY entry_type, relative_path",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(
        entries,
        vec![
            (
                "migration-job".to_owned(),
                "migration-root".to_owned(),
                "".to_owned(),
                101,
            ),
            (
                "migration-job".to_owned(),
                "migration-root".to_owned(),
                "movie.mkv".to_owned(),
                102,
            ),
        ]
    );

    let target_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM scan_job_targets")
        .fetch_one(&pool)
        .await?;
    assert_eq!(target_count, 2);
    let foreign_key_violations: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pragma_foreign_key_check")
            .fetch_one(&pool)
            .await?;
    assert_eq!(foreign_key_violations, 0);
    let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(&pool)
        .await?;
    assert_eq!(foreign_keys, 1);
    pool.close().await;
    Ok(())
}

#[tokio::test]
async fn recommendation_query_indexes_are_created_from_an_empty_database()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;

    let indexes: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'index' AND name IN (
             'idx_user_item_state_recommendation_last_played',
             'idx_playback_sessions_recommendation_last_event',
             'idx_user_item_state_recommendation_favorites',
             'idx_item_images_recommendation_lookup'
         )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(indexes, 4);
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
    assert_eq!(database.schema_version().await?, 136);
    Ok(())
}

#[tokio::test]
async fn login_background_plugin_cache_schema_is_available_from_an_empty_database()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let database = Database::connect(&Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
    .await?;

    let table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table' AND name = 'login_background_plugin_cache'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(table_count, 1);

    sqlx::query("INSERT INTO installed_plugins (plugin_id) VALUES ('org.lux.background-test')")
        .execute(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO login_background_plugin_cache (plugin_id, payload_json, refreshed_at)
         VALUES ('org.lux.background-test', '{}', 100)",
    )
    .execute(database.pool())
    .await?;

    let oversized_payload = "x".repeat(262_145);
    let oversized_insert = sqlx::query(
        "INSERT INTO login_background_plugin_cache (plugin_id, payload_json, refreshed_at)
         VALUES ('org.lux.background-test', ?, 101)",
    )
    .bind(oversized_payload)
    .execute(database.pool())
    .await;
    assert!(oversized_insert.is_err());

    sqlx::query("DELETE FROM installed_plugins WHERE plugin_id = 'org.lux.background-test'")
        .execute(database.pool())
        .await?;
    let remaining_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM login_background_plugin_cache")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(remaining_rows, 0);
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
        "emby_migration_handled_items",
        "emby_migration_user_bindings",
        "emby_migration_person_favorites",
        "playback_history_events",
        "legacy_person_migration_state",
        "person_manifest_restore_state",
        "person_manifest_index_state",
        "media_item_provider_ids",
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
    assert_eq!(database.schema_version().await?, 136);
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

    assert_eq!(database.schema_version().await?, 136);
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
    assert_eq!(database.schema_version().await?, 136);
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
async fn scheduled_task_plans_group_new_library_registrations()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let first = libraries
        .create_library("Movies", LibraryKind::Movie, true)
        .await?;
    let second = libraries
        .create_library("More Movies", LibraryKind::Movie, true)
        .await?;

    let plan_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scheduled_task_plans WHERE task_type = 'RECONCILIATION_SCAN'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(plan_count, 1);
    let membership_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scheduled_task_plan_libraries l
         JOIN scheduled_task_plans p ON p.id = l.plan_id
         WHERE p.task_type = 'RECONCILIATION_SCAN'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(membership_count, 2);
    let plan_ids: Vec<String> = sqlx::query_scalar(
        "SELECT plan_id FROM scheduled_task_configs
         WHERE task_type = 'RECONCILIATION_SCAN' AND owner_id IN (?, ?)
         ORDER BY owner_id",
    )
    .bind(first.id.to_string())
    .bind(second.id.to_string())
    .fetch_all(database.pool())
    .await?;
    assert_eq!(plan_ids.len(), 2);
    assert_eq!(plan_ids[0], plan_ids[1]);
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
