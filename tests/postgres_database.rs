use std::{env, fs};

use luxd::{
    application::{
        access::{AccessPrincipal, MediaAccessService},
        candidates::{MetadataSelectionMode, MetadataSelectionService},
        catalog::CatalogService,
        images::ImageWriteService,
        libraries::{LibraryService, LibrarySettingsPatch},
        metadata::MetadataEnricher,
        people::PeopleService,
        plugins::PluginService,
        scanner::{IncrementalScanChange, LibraryScanner, ScanJobService},
        setup::SetupService,
        strm_probe::{StrmProbeOptions, StrmProbeService},
        watch::ChangeKind,
        webhooks::WebhookService,
    },
    auth::sessions::WebAuthService,
    config::{Config, DatabaseConfiguration, PostgresConnection},
    domain::ids::{LibraryId, UserId},
    storage::Database,
};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

fn postgres_connection(database: String) -> PostgresConnection {
    PostgresConnection {
        host: env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
        port: env::var("POSTGRES_TEST_PORT")
            .unwrap_or_else(|_| "55432".to_owned())
            .parse()
            .unwrap_or(55432),
        database,
        username: env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
        password: env::var("POSTGRES_TEST_PASSWORD")
            .unwrap_or_else(|_| "lux-test-password".to_owned()),
        ssl_mode: "disable".to_owned(),
    }
}

async fn create_postgres_test_database()
-> Result<(DatabaseConfiguration, String), Box<dyn std::error::Error>> {
    let database_name = format!("lux_test_{}", Uuid::now_v7().simple());
    let admin = postgres_connection("postgres".to_owned());
    let admin_configuration = DatabaseConfiguration::Postgres(admin);
    let admin_url = admin_configuration
        .postgres_url()?
        .ok_or("missing PostgreSQL URL")?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {database_name}"
    )))
    .execute(&pool)
    .await?;
    pool.close().await;
    Ok((
        DatabaseConfiguration::Postgres(postgres_connection(database_name.clone())),
        database_name,
    ))
}

async fn drop_postgres_test_database(
    database_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let admin = postgres_connection("postgres".to_owned());
    let admin_configuration = DatabaseConfiguration::Postgres(admin);
    let admin_url = admin_configuration
        .postgres_url()?
        .ok_or("missing PostgreSQL URL")?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&admin_url)
        .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {database_name}"
    )))
    .execute(&pool)
    .await?;
    pool.close().await;
    Ok(())
}

async fn advance_postgres_manifest_to_applying(
    database: &Database,
    jobs: &ScanJobService,
    job_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    for _ in 0..16 {
        let state: String =
            sqlx::query_scalar("SELECT state FROM scan_manifests WHERE job_id = $1")
                .bind(job_id)
                .fetch_one(database.pool())
                .await?;
        if state == "APPLYING" {
            return Ok(());
        }
        if jobs.run_batch(job_id, 1).await?.completed {
            return Err("Manifest completed before entering APPLYING".into());
        }
    }
    Err("Manifest did not enter APPLYING within the test batch bound".into())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_bootstrap_runs_migrations_and_persists_core_state()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (connection, database_name) = create_postgres_test_database().await?;

    let database_url = connection.postgres_url()?.ok_or("missing PostgreSQL URL")?;
    let probe_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await?;
    sqlx::query("CREATE TABLE non_lux_application_table (id BIGINT PRIMARY KEY)")
        .execute(&probe_pool)
        .await?;
    let non_lux_result = Database::test_configuration(&connection).await;
    sqlx::query("DROP TABLE non_lux_application_table")
        .execute(&probe_pool)
        .await?;
    probe_pool.close().await;
    assert!(non_lux_result.is_err());

    let database = Database::connect_with_configuration(&config, &connection).await?;
    assert_eq!(database.backend(), luxd::config::DatabaseBackend::Postgres);
    assert_eq!(database.schema_version().await?, 143);
    let manifest_tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM information_schema.tables
         WHERE table_schema = current_schema()
           AND table_name IN (
               'scan_manifests', 'scan_manifest_roots', 'scan_manifest_directories',
               'scan_manifest_entries', 'scan_manifest_seen_paths', 'scan_manifest_deltas'
           )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(manifest_tables, 6);
    let removed_media_search_indexes: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM pg_indexes
         WHERE schemaname = current_schema()
           AND indexname IN ('idx_media_search_title', 'idx_media_search_sort_title')",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(removed_media_search_indexes, 0);
    let provider_row_triggers: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM pg_trigger trigger_row
         JOIN pg_class trigger_table ON trigger_table.oid = trigger_row.tgrelid
         WHERE trigger_table.relname = 'media_items'
           AND NOT trigger_row.tgisinternal
           AND trigger_row.tgname IN ('media_item_provider_ids_ai', 'media_item_provider_ids_au')",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(provider_row_triggers, 0);
    let merged_media_search_triggers: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM pg_trigger trigger_row
         JOIN pg_class trigger_table ON trigger_table.oid = trigger_row.tgrelid
         WHERE trigger_table.relname = 'media_items'
           AND NOT trigger_row.tgisinternal
           AND trigger_row.tgname IN ('media_items_search_ai', 'media_items_search_au')",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(merged_media_search_triggers, 2);
    let postprocessing_targets_ready_column: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.columns
         WHERE table_schema = current_schema()
           AND table_name = 'scan_manifests'
           AND column_name = 'postprocessing_targets_ready'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(postprocessing_targets_ready_column, 1);
    let root_target_checkpoint_columns: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.columns
         WHERE table_schema = current_schema()
           AND table_name = 'scan_manifest_roots'
           AND column_name IN ('postprocessing_target_stage', 'postprocessing_target_cursor')",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(root_target_checkpoint_columns, 2);
    let positive_change_kind_column: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.columns
         WHERE table_schema = current_schema()
           AND table_name = 'filesystem_entries'
           AND column_name = 'last_seen_change_kind'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(positive_change_kind_column, 1);
    let manifest_resume_state_column: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM information_schema.columns
         WHERE table_schema = current_schema()
           AND table_name = 'scan_manifests' AND column_name = 'resume_state'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(manifest_resume_state_column, 1);
    let manifest_discovery_format_column: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM information_schema.columns
         WHERE table_schema = current_schema()
           AND table_name = 'scan_manifests' AND column_name = 'discovery_format_version'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(manifest_discovery_format_column, 1);
    let login_background_cache_tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM information_schema.tables
         WHERE table_schema = current_schema()
           AND table_name = 'login_background_plugin_cache'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(login_background_cache_tables, 1);

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
    assert!(
        sqlx::query(
            "INSERT INTO login_background_plugin_cache (plugin_id, payload_json, refreshed_at)
             VALUES ('org.lux.background-test', $1, 101)",
        )
        .bind(oversized_payload)
        .execute(database.pool())
        .await
        .is_err()
    );
    sqlx::query("DELETE FROM installed_plugins WHERE plugin_id = 'org.lux.background-test'")
        .execute(database.pool())
        .await?;
    let remaining_login_background_cache_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM login_background_plugin_cache")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(remaining_login_background_cache_rows, 0);
    let manifest_fingerprint_type: String = sqlx::query_scalar(
        "SELECT data_type
         FROM information_schema.columns
         WHERE table_schema = current_schema()
           AND table_name = 'scan_manifest_entries'
           AND column_name = 'fingerprint'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(manifest_fingerprint_type, "bytea");
    let manifest_frontier_index: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM pg_indexes
         WHERE schemaname = current_schema()
           AND indexname = 'idx_scan_manifest_directories_frontier'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(manifest_frontier_index, 1);
    let has_password_type: String = sqlx::query_scalar(
        "SELECT data_type
         FROM information_schema.columns
         WHERE table_schema = current_schema()
           AND table_name = 'users'
           AND column_name = 'has_password'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(has_password_type, "bigint");
    let chapter_table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM information_schema.tables
         WHERE table_schema = current_schema() AND table_name = 'media_chapters'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(chapter_table_count, 1);
    let chapter_job_table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM information_schema.tables
         WHERE table_schema = current_schema()
           AND table_name IN ('chapter_detection_jobs', 'chapter_detection_job_items')",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(chapter_job_table_count, 2);
    let scan_job_index_definition: String = sqlx::query_scalar(
        "SELECT indexdef FROM pg_indexes
         WHERE schemaname = current_schema()
           AND indexname = 'idx_scan_jobs_one_active'",
    )
    .fetch_one(database.pool())
    .await?;
    assert!(scan_job_index_definition.contains("(library_id, job_type)"));
    let redundant_indexes: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM pg_indexes
         WHERE schemaname = current_schema()
           AND indexname IN (
               'idx_item_images_item_id',
               'idx_media_streams_source_id',
               'idx_person_credits_item',
               'idx_person_credits_person'
           )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(redundant_indexes, 0);

    let reconciliation_index_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM pg_indexes
         WHERE schemaname = current_schema()
           AND indexname = 'idx_reconciliation_scan_entries_pending'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(reconciliation_index_count, 0);

    let reconciliation_primary_key: String = sqlx::query_scalar(
        "SELECT pg_get_constraintdef(oid)
         FROM pg_constraint
         WHERE conrelid = 'reconciliation_scan_entries'::regclass
           AND contype = 'p'",
    )
    .fetch_one(database.pool())
    .await?;
    assert!(
        reconciliation_primary_key.contains("(job_id, entry_type, library_root_id, relative_path)")
    );

    let scan_target_index_predicates: Vec<String> = sqlx::query_scalar(
        "SELECT indexdef
         FROM pg_indexes
         WHERE schemaname = current_schema()
           AND indexname IN (
               'idx_scan_job_targets_probe',
               'idx_scan_job_targets_metadata',
               'idx_scan_job_targets_thumbnail'
           )
         ORDER BY indexname",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(scan_target_index_predicates.len(), 3);
    for indexdef in scan_target_index_predicates {
        assert!(
            indexdef.contains("IN ('PENDING', 'FAILED')")
                || indexdef
                    .replace("::text", "")
                    .contains("ANY (ARRAY['PENDING', 'FAILED'])"),
            "unexpected index: {indexdef}"
        );
    }

    let external_stream_index_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM pg_indexes
         WHERE schemaname = current_schema()
           AND indexname = 'idx_media_streams_external_path'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(external_stream_index_count, 0);

    let setup = SetupService::new(database.clone())?;
    setup
        .complete("postgres-admin", "PostgreSQL Admin", "test-password")
        .await?;
    let auth = WebAuthService::new(database.clone())?;
    let login = auth
        .login("postgres-admin", "test-password")
        .await?
        .ok_or("PostgreSQL admin login failed")?;
    assert_eq!(login.user.username_normalized, "postgres-admin");
    assert!(auth.resolve(&login.session_token).await?.is_some());

    let library_id = uuid::Uuid::now_v7().to_string();
    let inserted = sqlx::query(
        "INSERT INTO libraries (
            id, name, kind, is_enabled, realtime_watch_enabled,
            scan_concurrency, probe_concurrency
        ) VALUES ($1, $2, $3, 1, 1, 2, 1)",
    )
    .bind(&library_id)
    .bind("PostgreSQL Test Library")
    .bind("MOVIE")
    .execute(database.pool())
    .await?;
    assert_eq!(inserted.rows_affected(), 1);

    let stored_name: String = sqlx::query_scalar("SELECT name FROM libraries WHERE id = $1")
        .bind(&library_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(stored_name, "PostgreSQL Test Library");

    let library_service = LibraryService::new(database.clone());
    let library_id = library_id.parse::<LibraryId>()?;
    let library = library_service
        .update_settings(
            library_id,
            LibrarySettingsPatch {
                is_enabled: Some(false),
                realtime_watch_enabled: Some(true),
                ..LibrarySettingsPatch::default()
            },
        )
        .await?;
    assert!(!library.library.is_enabled);
    assert!(library.library.realtime_watch_enabled);
    let library = library_service
        .update_settings(
            library_id,
            LibrarySettingsPatch {
                is_enabled: Some(true),
                ..LibrarySettingsPatch::default()
            },
        )
        .await?;
    assert!(library.library.is_enabled);

    let item_id = uuid::Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO media_items (
            id, library_id, item_type, title, sort_title, provider_ids_json,
            identification_status, has_available_source
        ) VALUES ($1, $2, 'MOVIE', 'Postgres Search Movie', 'postgres search movie',
                  '{\"TMDB\":\"123\"}', 'LOCAL_CONFIRMED', 1)",
    )
    .bind(&item_id)
    .bind(library_id.to_string())
    .execute(database.pool())
    .await?;

    let provider_row: (String, String, String) = sqlx::query_as(
        "SELECT provider, provider_id, item_type
         FROM media_item_provider_ids
         WHERE media_item_id = $1",
    )
    .bind(&item_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        provider_row,
        ("tmdb".to_owned(), "123".to_owned(), "MOVIE".to_owned())
    );
    sqlx::query(
        "UPDATE media_items
         SET provider_ids_json = '{\"TMDB\":\"456\"}'
         WHERE id = $1",
    )
    .bind(&item_id)
    .execute(database.pool())
    .await?;
    let updated_provider_id: String = sqlx::query_scalar(
        "SELECT provider_id
         FROM media_item_provider_ids
         WHERE media_item_id = $1",
    )
    .bind(&item_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(updated_provider_id, "456");
    sqlx::query("UPDATE media_items SET title = 'Postgres Search Movie Renamed' WHERE id = $1")
        .bind(&item_id)
        .execute(database.pool())
        .await?;
    let provider_after_title_update: String = sqlx::query_scalar(
        "SELECT provider_id
         FROM media_item_provider_ids
         WHERE media_item_id = $1",
    )
    .bind(&item_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(provider_after_title_update, "456");
    sqlx::query(
        "INSERT INTO item_aliases (id, item_id, alias, language, alias_normalized)
         VALUES ($1, $2, '银河搜索电影', 'zh-CN', '银河搜索电影')",
    )
    .bind(uuid::Uuid::now_v7().to_string())
    .bind(&item_id)
    .execute(database.pool())
    .await?;

    let catalog = CatalogService::new(database.clone(), MediaAccessService::new(database.clone()));
    let page = catalog
        .search_items(
            AccessPrincipal::new(UserId::new(), true),
            "银河",
            "%银河%",
            0,
            10,
        )
        .await?;
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].id, item_id);
    database.close().await;
    drop_postgres_test_database(&database_name).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_upgrade_recovers_legacy_scan_and_completes_manifest_scan()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (connection, database_name) = create_postgres_test_database().await?;
    let database_url = connection.postgres_url()?.ok_or("missing PostgreSQL URL")?;

    let source_migrations =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("migrations-postgres");
    let old_schema_migrations = temp_dir.path().join("migrations-v130");
    fs::create_dir(&old_schema_migrations)?;
    for entry in fs::read_dir(source_migrations)? {
        let entry = entry?;
        if !matches!(
            entry.file_name().to_str(),
            Some(
                "0131_scan_manifest_resume_state.sql"
                    | "0132_streamed_manifest_indexing.sql"
                    | "0133_skip_redundant_source_availability_update.sql"
                    | "0134_drop_redundant_manifest_entry_index.sql"
                    | "0135_manifest_discovery_format_and_seen_paths.sql"
                    | "0136_manifest_postprocessing_target_checkpoint.sql"
            )
        ) {
            fs::copy(entry.path(), old_schema_migrations.join(entry.file_name()))?;
        }
    }

    let migration_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await?;
    sqlx::migrate::Migrator::new(old_schema_migrations.as_path())
        .await?
        .run(&migration_pool)
        .await?;

    let media_root = temp_dir.path().join("legacy-media");
    fs::create_dir_all(&media_root)?;
    fs::write(media_root.join("Legacy.Movie.2024.mkv"), b"legacy fixture")?;
    let canonical_media_root = fs::canonicalize(&media_root)?;
    let library_id = Uuid::now_v7().to_string();
    let root_id = Uuid::now_v7().to_string();
    sqlx::query("INSERT INTO libraries (id, name, kind) VALUES ($1, $2, 'MOVIE')")
        .bind(&library_id)
        .bind("PostgreSQL Manifest Upgrade")
        .execute(&migration_pool)
        .await?;
    sqlx::query(
        "INSERT INTO library_roots (
             id, library_id, canonical_path, display_path, is_available, is_writable
         ) VALUES ($1, $2, $3, $3, 1, 1)",
    )
    .bind(&root_id)
    .bind(&library_id)
    .bind(
        canonical_media_root
            .to_str()
            .ok_or("non-UTF-8 media root")?,
    )
    .execute(&migration_pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_jobs (
             id, library_id, job_type, status, generation, discovery_completed, scan_phase
         ) VALUES ('legacy-postgres-scan', $1, 'RECONCILE_LIBRARY', 'RUNNING',
                   'legacy-generation', 0, 'DISCOVERY')",
    )
    .bind(&library_id)
    .execute(&migration_pool)
    .await?;
    sqlx::query(
        "INSERT INTO reconciliation_scan_entries (
             job_id, library_root_id, relative_path, entry_type
         ) VALUES ('legacy-postgres-scan', $1, 'Legacy.Movie.2024.mkv', 'FILE')",
    )
    .bind(&root_id)
    .execute(&migration_pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_jobs (
             id, library_id, job_type, status, generation, discovery_completed, scan_phase
         ) VALUES ('existing-manifest-job', $1, 'RECONCILE_LIBRARY', 'COMPLETED',
                   'completed-generation', 1, 'IDLE')",
    )
    .bind(&library_id)
    .execute(&migration_pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_manifests (
             id, job_id, library_id, state, observed_file_count, add_count
         ) VALUES ('existing-manifest', 'existing-manifest-job', $1, 'COMPLETED', 9, 7)",
    )
    .bind(&library_id)
    .execute(&migration_pool)
    .await?;
    migration_pool.close().await;

    let database = Database::connect_with_configuration(&config, &connection).await?;
    assert_eq!(database.schema_version().await?, 143);
    let migrated_manifest: (String, Option<String>, i64, i64) = sqlx::query_as(
        "SELECT state, resume_state, observed_file_count, add_count
         FROM scan_manifests WHERE id = 'existing-manifest'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(migrated_manifest, ("COMPLETED".to_owned(), None, 9, 7));
    let legacy_queue_before_retry: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM reconciliation_scan_entries
         WHERE job_id = 'legacy-postgres-scan'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(legacy_queue_before_retry, 1);

    assert_eq!(database.cancel_incomplete_jobs_for_shutdown().await?, 1);
    let legacy_state: (String, Option<String>) =
        sqlx::query_as("SELECT status, error FROM scan_jobs WHERE id = 'legacy-postgres-scan'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(
        legacy_state,
        (
            "CANCELLED".to_owned(),
            Some("LEGACY_SCAN_REQUIRES_NEW_MANIFEST".to_owned())
        )
    );
    database.run_database_lifecycle_cleanup().await?;
    let legacy_queue_after_cleanup: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM reconciliation_scan_entries
         WHERE job_id = 'legacy-postgres-scan'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(legacy_queue_after_cleanup, 1);

    let jobs = ScanJobService::new(database.clone());
    let retry = jobs.retry("legacy-postgres-scan").await?;
    assert_ne!(retry.id, "legacy-postgres-scan");
    for _ in 0..16 {
        let manifest_state: String =
            sqlx::query_scalar("SELECT state FROM scan_manifests WHERE job_id = $1")
                .bind(&retry.id)
                .fetch_one(database.pool())
                .await?;
        if manifest_state == "READY_TO_DIFF" {
            break;
        }
        assert!(!jobs.run_batch(&retry.id, 100).await?.completed);
    }
    let retry_discovery_state: (String, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT manifest.state, manifest.discovery_format_version,
                (SELECT COUNT(*) FROM scan_manifest_seen_paths seen
                 WHERE seen.manifest_id = manifest.id),
                (SELECT COUNT(*) FROM scan_manifest_entries entry
                 WHERE entry.manifest_id = manifest.id AND entry.entry_kind = 'FILE'),
                (SELECT COUNT(*) FROM filesystem_entries entry
                 JOIN scan_jobs job ON job.id = manifest.job_id
                 WHERE entry.last_seen_generation = job.generation)
         FROM scan_manifests manifest WHERE manifest.job_id = $1",
    )
    .bind(&retry.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        retry_discovery_state,
        ("READY_TO_DIFF".to_owned(), 3, 0, 0, 1)
    );
    jobs.run_to_completion(&retry.id, 100, None).await?;
    let manifest_summary: (String, Option<String>, i64, i64, i64) = sqlx::query_as(
        "SELECT manifest.state, manifest.resume_state, manifest.observed_file_count,
                (SELECT COUNT(*) FROM scan_manifest_entries entry
                 WHERE entry.manifest_id = manifest.id),
                (SELECT COUNT(*) FROM scan_manifest_deltas delta
                 WHERE delta.manifest_id = manifest.id)
         FROM scan_manifests manifest WHERE manifest.job_id = $1",
    )
    .bind(&retry.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(manifest_summary, ("COMPLETED".to_owned(), None, 1, 0, 0));
    let indexed_files: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM filesystem_entries
         WHERE library_root_id = $1 AND entry_kind = 'FILE' AND is_missing = 0",
    )
    .bind(root_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(indexed_files, 1);

    fs::remove_file(media_root.join("Legacy.Movie.2024.mkv"))?;
    let compact_remove_job = jobs.create_movie_scan_job(library_id.parse()?).await?;
    jobs.run_to_completion(&compact_remove_job.id, 100, None)
        .await?;
    let compact_removal_result: (i64, i64, i64) = sqlx::query_as(
        "SELECT entry.is_missing, manifest.remove_count, manifest.applied_delta_count
         FROM filesystem_entries entry
         JOIN scan_manifest_roots root ON root.library_root_id = entry.library_root_id
         JOIN scan_manifests manifest ON manifest.id = root.manifest_id
         WHERE manifest.job_id = $1 AND entry.relative_path = 'Legacy.Movie.2024.mkv'",
    )
    .bind(&compact_remove_job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(compact_removal_result, (1, 1, 1));

    database.close().await;
    drop_postgres_test_database(&database_name).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_manifest_cas_resume_and_root_replacement_safety()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (connection, database_name) = create_postgres_test_database().await?;
    let database = Database::connect_with_configuration(&config, &connection).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library(
            &format!("PostgreSQL Manifest CAS {}", Uuid::now_v7()),
            luxd::library::LibraryKind::Movie,
            false,
        )
        .await?;
    let media_root = temp_dir.path().join("media");
    fs::create_dir(&media_root)?;
    fs::write(media_root.join("A.Movie.2024.mkv"), b"incremental wins")?;
    fs::write(media_root.join("B.Movie.2025.mkv"), b"manifest adds")?;
    let root_id = libraries
        .add_root(
            library.id,
            media_root.to_str().ok_or("non-UTF-8 media root")?,
        )
        .await?
        .root
        .id
        .to_string();

    let webhooks = WebhookService::new(database.clone(), config.config_dir.clone())?;
    let event_types = vec!["SCAN_COMPLETED".to_owned()];
    webhooks
        .create_destination(
            "PostgreSQL scan completion",
            "https://example.com/lux-hook",
            true,
            false,
            &event_types,
            None,
        )
        .await?;
    let jobs = ScanJobService::new(database.clone()).with_webhooks(webhooks);
    let manifest_job = jobs.create_movie_scan_job(library.id).await?;
    sqlx::query("UPDATE scan_manifests SET discovery_format_version = 2 WHERE job_id = $1")
        .bind(&manifest_job.id)
        .execute(database.pool())
        .await?;
    sqlx::query("UPDATE scan_manifests SET workflow_version = 1 WHERE job_id = $1")
        .bind(&manifest_job.id)
        .execute(database.pool())
        .await?;
    advance_postgres_manifest_to_applying(&database, &jobs, &manifest_job.id).await?;

    let incremental = jobs
        .enqueue_incremental_changes(
            library.id,
            vec![IncrementalScanChange {
                root_id: root_id.clone(),
                relative_path: "A.Movie.2024.mkv".to_owned(),
                kind: ChangeKind::Create,
            }],
        )
        .await?;
    jobs.run_to_completion(&incremental.id, 100, None).await?;
    let incremental_entry: (String, String, i64) = sqlx::query_as(
        "SELECT entry.id, entry.last_seen_generation, COUNT(source.id)
         FROM filesystem_entries entry
         LEFT JOIN media_sources source ON source.filesystem_entry_id = entry.id
         WHERE entry.library_root_id = $1 AND entry.relative_path = $2
         GROUP BY entry.id, entry.last_seen_generation",
    )
    .bind(&root_id)
    .bind("A.Movie.2024.mkv")
    .fetch_one(database.pool())
    .await?;
    assert_eq!(incremental_entry.2, 1);

    let first_apply = jobs.run_batch(&manifest_job.id, 1).await?;
    assert_eq!(first_apply.processed, 1);
    let raced_delta: String = sqlx::query_scalar(
        "SELECT state FROM scan_manifest_deltas
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = $1)
           AND relative_path = 'A.Movie.2024.mkv'",
    )
    .bind(&manifest_job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(raced_delta, "CONFLICT");
    jobs.cancel(&manifest_job.id).await?;
    let cancelled = jobs.run_batch(&manifest_job.id, 1).await?;
    assert_eq!(cancelled.status, "CANCELLED");
    let checkpoint: (String, Option<String>, i64) = sqlx::query_as(
        "SELECT state, resume_state, applied_delta_count
         FROM scan_manifests WHERE job_id = $1",
    )
    .bind(&manifest_job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        checkpoint,
        ("CANCELLED".to_owned(), Some("APPLYING".to_owned()), 0)
    );

    let resumed = jobs.retry(&manifest_job.id).await?;
    assert_eq!(resumed.id, manifest_job.id);
    assert_eq!(resumed.status, "PENDING");
    while !jobs.run_batch(&manifest_job.id, 100).await?.completed {}

    let index_completion: (String, String) =
        sqlx::query_as("SELECT status, scan_phase FROM scan_jobs WHERE id = $1")
            .bind(&manifest_job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(
        index_completion,
        ("COMPLETED".to_owned(), "POSTPROCESSING".to_owned())
    );
    let scan_completed_events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notification_events
         WHERE event_type = 'SCAN_COMPLETED' AND payload_json::jsonb->>'jobId' = $1",
    )
    .bind(&manifest_job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        scan_completed_events, 1,
        "PostgreSQL persists ScanCompleted at index completion"
    );
    let indexed_sources: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_sources source
         JOIN filesystem_entries entry ON entry.id = source.filesystem_entry_id
         WHERE entry.library_root_id = $1 AND entry.is_missing = 0",
    )
    .bind(&root_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(indexed_sources, 2);
    let after_manifest_apply: (String, String, i64) = sqlx::query_as(
        "SELECT entry.id, entry.last_seen_generation, COUNT(source.id)
         FROM filesystem_entries entry
         LEFT JOIN media_sources source ON source.filesystem_entry_id = entry.id
         WHERE entry.library_root_id = $1 AND entry.relative_path = $2
         GROUP BY entry.id, entry.last_seen_generation",
    )
    .bind(&root_id)
    .bind("A.Movie.2024.mkv")
    .fetch_one(database.pool())
    .await?;
    assert_eq!(after_manifest_apply, incremental_entry);
    jobs.run_to_completion(&manifest_job.id, 100, None).await?;
    let scan_completed_after_postprocessing: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notification_events
         WHERE event_type = 'SCAN_COMPLETED' AND payload_json::jsonb->>'jobId' = $1",
    )
    .bind(&manifest_job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(scan_completed_after_postprocessing, 1);
    let completed_stage: (String, String) =
        sqlx::query_as("SELECT status, scan_phase FROM scan_jobs WHERE id = $1")
            .bind(&manifest_job.id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(completed_stage, ("COMPLETED".to_owned(), "IDLE".to_owned()));

    fs::remove_file(media_root.join("A.Movie.2024.mkv"))?;
    let replacement_job = jobs.create_movie_scan_job(library.id).await?;
    sqlx::query("UPDATE scan_manifests SET discovery_format_version = 2 WHERE job_id = $1")
        .bind(&replacement_job.id)
        .execute(database.pool())
        .await?;
    advance_postgres_manifest_to_applying(&database, &jobs, &replacement_job.id).await?;
    let moved_root = temp_dir.path().join("original-media");
    fs::rename(&media_root, &moved_root)?;
    fs::create_dir(&media_root)?;
    fs::write(
        media_root.join("Replacement.Movie.2030.mkv"),
        b"replacement root",
    )?;
    jobs.run_to_completion(&replacement_job.id, 100, None)
        .await?;
    let replaced_root_state: String = sqlx::query_scalar(
        "SELECT state FROM scan_manifest_roots
         WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = $1)
           AND library_root_id = $2",
    )
    .bind(&replacement_job.id)
    .bind(&root_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(replaced_root_state, "UNAVAILABLE");
    let original_entry_missing: i64 = sqlx::query_scalar(
        "SELECT is_missing FROM filesystem_entries
         WHERE library_root_id = $1 AND relative_path = 'A.Movie.2024.mkv'",
    )
    .bind(&root_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(original_entry_missing, 0);

    database.close().await;
    drop_postgres_test_database(&database_name).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_v3_postprocessing_targets_resume_after_root_restore()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (connection, database_name) = create_postgres_test_database().await?;
    let database = Database::connect_with_configuration(&config, &connection).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library(
            &format!("PostgreSQL target restore {}", Uuid::now_v7()),
            luxd::library::LibraryKind::Movie,
            false,
        )
        .await?;
    let root = temp_dir.path().join("media");
    fs::create_dir_all(&root)?;
    fs::write(root.join("Restore.Movie.2024.mkv"), b"indexed root")?;
    let root_id = libraries
        .add_root(library.id, root.to_str().ok_or("non-UTF-8 media root")?)
        .await?
        .root
        .id
        .to_string();

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    loop {
        if jobs.run_batch(&job.id, 100).await?.completed {
            break;
        }
    }
    let backup = temp_dir.path().join("media-original");
    fs::rename(&root, &backup)?;
    fs::create_dir_all(&root)?;
    fs::write(root.join("Restore.Movie.2024.mkv"), b"replacement root")?;

    assert!(jobs.run_to_completion(&job.id, 100, None).await.is_err());
    let failed_checkpoint: (String, String, i64, String, i64) = sqlx::query_as(
        "SELECT job.status, job.scan_phase, manifest.postprocessing_targets_ready,
                root.postprocessing_target_stage,
                (SELECT COUNT(*) FROM scan_job_events event
                 WHERE event.job_id = job.id AND event.event_code = 'POSTPROCESSING_FAILED')
         FROM scan_jobs job
         JOIN scan_manifests manifest ON manifest.job_id = job.id
         JOIN scan_manifest_roots root ON root.manifest_id = manifest.id
         WHERE job.id = $1",
    )
    .bind(&job.id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        failed_checkpoint,
        (
            "COMPLETED".to_owned(),
            "IDLE".to_owned(),
            0,
            "NEW".to_owned(),
            1,
        )
    );

    fs::remove_dir_all(&root)?;
    fs::rename(&backup, &root)?;
    let retried = jobs.retry(&job.id).await?;
    assert_eq!(retried.id, job.id);
    jobs.run_to_completion(&job.id, 100, None).await?;

    let resumed_checkpoint: (String, String, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT job.status, job.scan_phase, manifest.postprocessing_targets_ready,
                library_root.is_available, job.processed_count,
                (SELECT COUNT(*) FROM scan_job_targets target WHERE target.job_id = job.id)
         FROM scan_jobs job
         JOIN scan_manifests manifest ON manifest.job_id = job.id
         JOIN library_roots library_root ON library_root.id = $2
         WHERE job.id = $1",
    )
    .bind(&job.id)
    .bind(&root_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        resumed_checkpoint,
        ("COMPLETED".to_owned(), "IDLE".to_owned(), 1, 1, 1, 0)
    );

    database.close().await;
    drop_postgres_test_database(&database_name).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_rescan_of_existing_movie_uses_integer_boolean_projection()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (connection, database_name) = create_postgres_test_database().await?;
    let database = Database::connect_with_configuration(&config, &connection).await?;
    let libraries = LibraryService::new(database.clone());
    let library_name = format!("PostgreSQL rescan {}", uuid::Uuid::now_v7());
    let library = libraries
        .create_library(&library_name, luxd::library::LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Existing.Movie.2024.mkv"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;

    let scanner = LibraryScanner::new(database.clone());
    scanner.scan_movie_library(library.id).await?;
    scanner.scan_movie_library(library.id).await?;
    database.close().await;
    drop_postgres_test_database(&database_name).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_strm_probe_job_accepts_boolean_options() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (connection, database_name) = create_postgres_test_database().await?;
    let database = Database::connect_with_configuration(&config, &connection).await?;
    let libraries = LibraryService::new(database.clone());
    let library_name = format!("PostgreSQL STRM {}", uuid::Uuid::now_v7());
    let library = libraries
        .create_library(&library_name, luxd::library::LibraryKind::Movie, false)
        .await?;
    let plugins = PluginService::new(database.clone(), config.config_dir.clone());
    let service = StrmProbeService::new(database.clone(), plugins);
    let jobs = service
        .create_jobs(
            &[library.id],
            StrmProbeOptions {
                concurrency: 1,
                include_ready: false,
                write_sidecars: false,
                media_info_enabled: true,
                thumbnail_enabled: false,
                thumbnail_position_percent: 30,
            },
        )
        .await?;
    assert_eq!(jobs.len(), 1);
    assert!(!jobs[0].include_ready);
    database.close().await;
    drop_postgres_test_database(&database_name).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_resume_order_puts_recent_timestamp_before_legacy_nulls()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (connection, database_name) = create_postgres_test_database().await?;
    let database = Database::connect_with_configuration(&config, &connection).await?;
    let admin = SetupService::new(database.clone())?
        .complete("postgres-admin", "PostgreSQL Admin", "test-password")
        .await?;
    let library = LibraryService::new(database.clone())
        .create_library(
            &format!("PostgreSQL resume {}", Uuid::now_v7()),
            luxd::library::LibraryKind::Movie,
            false,
        )
        .await?;

    let mut legacy_item_ids = Vec::new();
    for index in 0..10 {
        let item_id = Uuid::now_v7().to_string();
        let title = format!("Legacy Resume Movie {index:02}");
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, runtime_ticks,
                identification_status, has_available_source
            ) VALUES ($1, $2, 'MOVIE', $3, $4, 36000000000, 'LOCAL_CONFIRMED', 1)",
        )
        .bind(&item_id)
        .bind(library.id.to_string())
        .bind(&title)
        .bind(title.to_lowercase())
        .execute(database.pool())
        .await?;
        legacy_item_ids.push(item_id);
    }

    let fresh_item_id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO media_items (
            id, library_id, item_type, title, sort_title, runtime_ticks,
            identification_status, has_available_source
        ) VALUES ($1, $2, 'MOVIE', 'Fresh Resume Movie', 'fresh resume movie',
                  36000000000, 'LOCAL_CONFIRMED', 1)",
    )
    .bind(&fresh_item_id)
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;

    let user_id = admin.id.to_string();
    for item_id in legacy_item_ids {
        sqlx::query(
            "INSERT INTO user_item_state (
                user_id, item_id, position_ticks, is_played, last_played_at
            ) VALUES ($1, $2, 6000000000, 0, NULL)",
        )
        .bind(&user_id)
        .bind(item_id)
        .execute(database.pool())
        .await?;
    }
    sqlx::query(
        "INSERT INTO user_item_state (
            user_id, item_id, position_ticks, is_played, last_played_at
        ) VALUES ($1, $2, 6000000000, 0, 100)",
    )
    .bind(&user_id)
    .bind(&fresh_item_id)
    .execute(database.pool())
    .await?;

    let catalog = CatalogService::new(database.clone(), MediaAccessService::new(database.clone()));
    let page = catalog
        .list_continue_watching(AccessPrincipal::new(UserId::new(), true), &user_id, 0, 10)
        .await?;

    assert_eq!(page.total, 11);
    assert_eq!(page.items.len(), 10);
    assert_eq!(page.items[0].id, fresh_item_id);
    database.close().await;
    drop_postgres_test_database(&database_name).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_metadata_priority_locks_images_and_people_regression()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (connection, database_name) = create_postgres_test_database().await?;
    let database = Database::connect_with_configuration(&config, &connection).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library(
            &format!("PostgreSQL metadata {}", Uuid::now_v7()),
            luxd::library::LibraryKind::Movie,
            false,
        )
        .await?;
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Local Movie (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Local.Movie.2024.mkv"), b"fixture").await?;
    tokio::fs::write(
        movie_dir.join("movie.nfo"),
        r#"<movie><title>本地标题</title><actor><name>本地演员</name><role>本地角色</role><order>0</order></actor></movie>"#,
    )
    .await?;
    tokio::fs::write(movie_dir.join("poster.jpg"), b"local-poster").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;

    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    MetadataEnricher::new(database.clone())
        .enrich_movie_library(library.id)
        .await?;
    let item_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items
         WHERE library_id = $1 AND item_type = 'MOVIE' AND removed_at IS NULL
         LIMIT 1",
    )
    .bind(library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    tokio::fs::write(
        movie_dir.join("movie.nfo"),
        r#"<movie><title>本地标题</title><rating>8.2</rating><actor><name>本地演员</name><role>本地角色</role><order>0</order></actor></movie>"#,
    )
    .await?;
    MetadataEnricher::new(database.clone())
        .enrich_movie_library(library.id)
        .await?;
    let nfo_rating: f64 = sqlx::query_scalar("SELECT rating FROM media_items WHERE id = $1")
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(nfo_rating, 8.2);

    sqlx::query("UPDATE media_items SET locked_fields_json = $1 WHERE id = $2")
        .bind(json!(["title"]).to_string())
        .bind(&item_id)
        .execute(database.pool())
        .await?;
    let candidate_id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO metadata_candidates (
            id, item_id, provider, provider_id, candidate_json, score, status
         ) VALUES ($1, $2, 'TMDB', '123', $3, 100, 'PENDING')",
    )
    .bind(&candidate_id)
    .bind(&item_id)
    .bind(
        json!({
            "title": "Online Title",
            "overview": "Online Overview",
            "posterUrl": "https://example.invalid/poster.jpg",
            "providerIds": {"Tmdb": "123"}
        })
        .to_string(),
    )
    .execute(database.pool())
    .await?;

    let image_writer =
        ImageWriteService::new_with_config_dir(database.clone(), config.config_dir.clone())?;
    let selection = MetadataSelectionService::with_config_dir(
        database.clone(),
        image_writer,
        config.config_dir.clone(),
    );
    selection
        .select(&item_id, &candidate_id, MetadataSelectionMode::FillMissing)
        .await?;

    let metadata: (String, String, f64, String, String) = sqlx::query_as(
        "SELECT title, overview, rating, locked_fields_json, metadata_provenance_json
         FROM media_items WHERE id = $1",
    )
    .bind(&item_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(metadata.0, "本地标题");
    assert_eq!(metadata.1, "Online Overview");
    assert_eq!(metadata.2, 8.2);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&metadata.3)?,
        json!(["title"])
    );
    let provenance: serde_json::Value = serde_json::from_str(&metadata.4)?;
    assert_eq!(provenance["title"], "LOCKED_LOCAL");
    assert_eq!(provenance["overview"], "SCRAPER_LOCALIZED");

    let image: (String, String, i64) = sqlx::query_as(
        "SELECT image_type, local_path, COUNT(*) OVER ()
         FROM item_images WHERE item_id = $1 AND image_type = 'POSTER'
         ORDER BY image_index LIMIT 1",
    )
    .bind(&item_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(image.0, "POSTER");
    let expected_image_path = tokio::fs::canonicalize(movie_dir.join("poster.jpg")).await?;
    assert_eq!(image.1, expected_image_path.to_string_lossy());
    assert_eq!(image.2, 1);
    assert_eq!(tokio::fs::read(&image.1).await?, b"local-poster");
    let attempted_images: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM metadata_image_attempts WHERE item_id = $1")
            .bind(&item_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(attempted_images, 0);

    let people = PeopleService::new(config.config_dir.clone()).with_database(database.clone());
    let actors = people.list_item_actors(&item_id).await?;
    assert_eq!(actors.len(), 1);
    assert_eq!(actors[0].name, "本地演员");
    assert_eq!(actors[0].character.as_deref(), Some("本地角色"));
    let nfo = tokio::fs::read_to_string(movie_dir.join("movie.nfo")).await?;
    assert!(nfo.contains("<title>本地标题</title>"));
    assert!(nfo.contains("<plot>Online Overview</plot>"));

    let rating_candidate_id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO metadata_candidates (
            id, item_id, provider, provider_id, candidate_json, score, status
         ) VALUES ($1, $2, 'TMDB', '124', $3, 100, 'PENDING')",
    )
    .bind(&rating_candidate_id)
    .bind(&item_id)
    .bind(
        json!({
            "title": "Online Title",
            "rating": 9.1,
            "providerIds": {"Tmdb": "124"}
        })
        .to_string(),
    )
    .execute(database.pool())
    .await?;
    selection
        .select(
            &item_id,
            &rating_candidate_id,
            MetadataSelectionMode::RefreshUnlocked,
        )
        .await?;
    let selected_rating: f64 = sqlx::query_scalar("SELECT rating FROM media_items WHERE id = $1")
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(selected_rating, 9.1);

    database.close().await;
    drop_postgres_test_database(&database_name).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_statement_triggers_refresh_search_and_availability_sets()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (connection, database_name) = create_postgres_test_database().await?;
    let database = Database::connect_with_configuration(&config, &connection).await?;
    let library_id = Uuid::now_v7().to_string();
    let root_id = Uuid::now_v7().to_string();
    let item_one = Uuid::now_v7().to_string();
    let item_two = Uuid::now_v7().to_string();
    let entry_one = Uuid::now_v7().to_string();
    let entry_two = Uuid::now_v7().to_string();
    let source_one = Uuid::now_v7().to_string();
    let source_two = Uuid::now_v7().to_string();
    let alias_id = Uuid::now_v7().to_string();

    sqlx::query("INSERT INTO libraries (id, name, kind) VALUES ($1, 'Triggers', 'MOVIE')")
        .bind(&library_id)
        .execute(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO library_roots (
             id, library_id, canonical_path, display_path, is_available, is_writable
         ) VALUES ($1, $2, '/tmp/trigger-test', '/tmp/trigger-test', 1, 1)",
    )
    .bind(&root_id)
    .bind(&library_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO filesystem_entries (
         id, library_root_id, relative_path, entry_kind, size, modified_at,
             fingerprint, last_seen_generation, is_missing
         ) VALUES
             ($1, $3, 'one.mkv', 'FILE', 1, 1, $4, 'generation', 0),
             ($2, $3, 'two.mkv', 'FILE', 1, 1, $5, 'generation', 0)",
    )
    .bind(&entry_one)
    .bind(&entry_two)
    .bind(&root_id)
    .bind(vec![1_u8; 32])
    .bind(vec![2_u8; 32])
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES
             ($1, $3, 'MOVIE', 'One', 'one', 'LOCAL_CONFIRMED'),
             ($2, $3, 'MOVIE', 'Two', 'two', 'LOCAL_CONFIRMED')",
    )
    .bind(&item_one)
    .bind(&item_two)
    .bind(&library_id)
    .execute(database.pool())
    .await?;
    let search_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_search WHERE item_id IN ($1, $2)")
            .bind(&item_one)
            .bind(&item_two)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(search_count, 2);

    sqlx::query(
        "INSERT INTO item_aliases (id, item_id, alias, alias_normalized)
         VALUES ($1, $2, 'First Alias', 'first alias')",
    )
    .bind(&alias_id)
    .bind(&item_one)
    .execute(database.pool())
    .await?;
    let alias_text: String =
        sqlx::query_scalar("SELECT aliases FROM media_search WHERE item_id = $1")
            .bind(&item_one)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(alias_text, "First Alias");

    sqlx::query(
        "UPDATE media_items
         SET title = CASE id WHEN $1 THEN 'One Updated' WHEN $2 THEN 'Two Updated' END
         WHERE id IN ($1, $2)",
    )
    .bind(&item_one)
    .bind(&item_two)
    .execute(database.pool())
    .await?;
    let preserved_alias: String =
        sqlx::query_scalar("SELECT aliases FROM media_search WHERE item_id = $1")
            .bind(&item_one)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(preserved_alias, "First Alias");

    sqlx::query(
        "INSERT INTO media_sources (
             id, item_id, source_kind, filesystem_entry_id, container, size
         ) VALUES
             ($1, $3, 'LOCAL_FILE', $5, 'mkv', 1),
             ($2, $4, 'LOCAL_FILE', $6, 'mkv', 1)",
    )
    .bind(&source_one)
    .bind(&source_two)
    .bind(&item_one)
    .bind(&item_two)
    .bind(&entry_one)
    .bind(&entry_two)
    .execute(database.pool())
    .await?;
    let available_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_items
         WHERE id IN ($1, $2) AND has_available_source = 1",
    )
    .bind(&item_one)
    .bind(&item_two)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(available_count, 2);

    sqlx::query("UPDATE media_sources SET size = size + 1 WHERE id IN ($1, $2)")
        .bind(&source_one)
        .bind(&source_two)
        .execute(database.pool())
        .await?;
    sqlx::query("UPDATE filesystem_entries SET is_missing = 1 WHERE id IN ($1, $2)")
        .bind(&entry_one)
        .bind(&entry_two)
        .execute(database.pool())
        .await?;
    let unavailable_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_items
         WHERE id IN ($1, $2) AND has_available_source = 0",
    )
    .bind(&item_one)
    .bind(&item_two)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(unavailable_count, 2);

    sqlx::query("DELETE FROM item_aliases WHERE id = $1")
        .bind(&alias_id)
        .execute(database.pool())
        .await?;
    let cleared_alias: String =
        sqlx::query_scalar("SELECT aliases FROM media_search WHERE item_id = $1")
            .bind(&item_one)
            .fetch_one(database.pool())
            .await?;
    assert!(cleared_alias.is_empty());

    database.close().await;
    drop_postgres_test_database(&database_name).await?;
    Ok(())
}
