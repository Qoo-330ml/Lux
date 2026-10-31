use std::{env, ffi::OsStr, fs, path::Path};

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
    observability::logs::LogStore,
    storage::Database,
};
use serde_json::json;
use sqlx::{migrate::Migrator, postgres::PgPoolOptions};
use uuid::Uuid;

fn latest_migration_version(directory: &str) -> Result<i64, Box<dyn std::error::Error>> {
    let migration_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(directory);
    let mut latest_version = None;
    for entry in fs::read_dir(migration_dir)? {
        let path = entry?.path();
        if path.extension() != Some(OsStr::new("sql")) {
            continue;
        }
        let Some(version) = path
            .file_stem()
            .and_then(OsStr::to_str)
            .and_then(|stem| stem.split('_').next())
            .and_then(|version| version.parse::<i64>().ok())
        else {
            continue;
        };
        latest_version = Some(latest_version.map_or(version, |latest: i64| latest.max(version)));
    }
    latest_version.ok_or_else(|| "migration directory contains no numbered SQL files".into())
}

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
async fn postgres_content_restriction_queue_migration_upgrades_v207_and_preserves_triggers()
-> Result<(), Box<dyn std::error::Error>> {
    let (connection, database_name) = create_postgres_test_database().await?;
    let url = connection.postgres_url()?.ok_or("missing PostgreSQL URL")?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await?;
    let result = async {
        let source_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations-postgres");
        let migrator = Migrator::new(source_dir.as_path()).await?;
        let old = Migrator::with_migrations(
            migrator.iter().filter(|m| m.version <= 207).cloned().collect(),
        );
        old.run(&pool).await?;
        sqlx::raw_sql(
            "INSERT INTO libraries (id, name, kind) VALUES ('queue-library', 'Queue', 'MOVIE');
             INSERT INTO media_items
                 (id, library_id, item_type, title, sort_title, identification_status, removed_at)
             VALUES ('queue-missing', 'queue-library', 'MOVIE', 'Missing', 'missing', 'LOCAL_CONFIRMED', NULL),
                    ('queue-indexed', 'queue-library', 'MOVIE', 'Indexed', 'indexed', 'LOCAL_CONFIRMED', NULL),
                    ('queue-removed', 'queue-library', 'MOVIE', 'Removed', 'removed', 'LOCAL_CONFIRMED', 1);
             INSERT INTO media_content_restriction_index (item_id, certification_code)
             VALUES ('queue-indexed', 'PG');
             INSERT INTO media_content_restriction_tags (item_id, tag)
             VALUES ('queue-indexed', 'family');",
        )
        .execute(&pool)
        .await?;

        migrator.run(&pool).await?;
        // A normal restart must accept the recorded migrations without reapplying them.
        migrator.run(&pool).await?;
        let queued: Vec<String> = sqlx::query_scalar(
            "SELECT item_id FROM media_content_restriction_index_queue ORDER BY item_id",
        )
        .fetch_all(&pool)
        .await?;
        assert_eq!(queued, ["queue-missing"]);

        sqlx::query(
            "UPDATE media_items SET nfo_metadata_json = nfo_metadata_json
             WHERE id = 'queue-indexed'",
        )
        .execute(&pool)
        .await?;
        let unchanged_index: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM media_content_restriction_index WHERE item_id = 'queue-indexed'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(unchanged_index, 1);

        sqlx::raw_sql(
            "UPDATE media_items SET nfo_metadata_json = '{\"certification\":\"R\"}'
             WHERE id = 'queue-indexed';
             UPDATE media_content_restriction_index_queue SET queued_at = -1
             WHERE item_id = 'queue-indexed';
             UPDATE media_items SET nfo_metadata_json = '{\"certification\":\"PG\"}'
             WHERE id = 'queue-indexed';",
        )
        .execute(&pool)
        .await?;
        let invalidated_rows: i64 = sqlx::query_scalar(
            "SELECT (SELECT COUNT(*) FROM media_content_restriction_index
                     WHERE item_id = 'queue-indexed')
                  + (SELECT COUNT(*) FROM media_content_restriction_tags
                     WHERE item_id = 'queue-indexed')",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(invalidated_rows, 0);
        let requeued_at: i64 = sqlx::query_scalar(
            "SELECT queued_at FROM media_content_restriction_index_queue
             WHERE item_id = 'queue-indexed'",
        )
        .fetch_one(&pool)
        .await?;
        assert!(requeued_at > 0);

        sqlx::query(
            "INSERT INTO media_items
                 (id, library_id, item_type, title, sort_title, identification_status)
             VALUES ('queue-new', 'queue-library', 'MOVIE', 'New', 'new', 'LOCAL_CONFIRMED')",
        )
        .execute(&pool)
        .await?;
        let inserted_rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM media_content_restriction_index_queue WHERE item_id = 'queue-new'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(inserted_rows, 1);
        sqlx::query("DELETE FROM media_items WHERE id = 'queue-new'")
            .execute(&pool)
            .await?;
        let deleted_rows: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM media_content_restriction_index_queue WHERE item_id = 'queue-new'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(deleted_rows, 0);
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    pool.close().await;
    drop_postgres_test_database(&database_name).await?;
    result
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_media_content_restriction_index_migration()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (connection, database_name) = create_postgres_test_database().await?;
    let database = Database::connect_with_configuration(&config, &connection).await?;
    let level_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM content_rating_levels")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(level_count, 18);
    let tv_levels: Vec<(String, i64)> = sqlx::query_as(
        "SELECT rating_code, rating_order FROM content_rating_levels
         WHERE rating_system = 'TVPG' ORDER BY rating_order",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(
        tv_levels,
        [
            ("TV-Y".to_owned(), 0),
            ("TV-G".to_owned(), 1),
            ("TV-Y7".to_owned(), 2),
            ("TV-PG".to_owned(), 3),
            ("TV-14".to_owned(), 4),
            ("TV-MA".to_owned(), 5),
        ]
    );
    let restriction_indexes: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pg_indexes
         WHERE schemaname = current_schema() AND indexname IN (
             'idx_media_content_restriction_index_certification',
             'idx_media_content_restriction_tags_tag'
         )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(restriction_indexes, 2);
    assert!(
        sqlx::query(
            "INSERT INTO content_rating_levels (rating_system, rating_code, rating_order)
             VALUES ('MPA', 'UNKNOWN', 5)",
        )
        .execute(database.pool())
        .await
        .is_err()
    );

    sqlx::query("INSERT INTO libraries (id, name, kind) VALUES ('restriction-index-library', 'Restrictions', 'MOVIE')")
        .execute(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO media_items
            (id, library_id, item_type, title, sort_title, identification_status, nfo_metadata_json)
         VALUES ('restriction-index-item', 'restriction-index-library', 'MOVIE',
                 'Restriction', 'restriction', 'LOCAL_CONFIRMED', '{\"certification\":\"PG-13\"}')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_content_restriction_index (item_id, certification_code)
         VALUES ('restriction-index-item', 'PG-13')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_content_restriction_tags (item_id, tag)
         VALUES ('restriction-index-item', 'family')",
    )
    .execute(database.pool())
    .await?;

    sqlx::query(
        "UPDATE media_items SET nfo_metadata_json = nfo_metadata_json
         WHERE id = 'restriction-index-item'",
    )
    .execute(database.pool())
    .await?;
    let unchanged_index_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_content_restriction_index
         WHERE item_id = 'restriction-index-item'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(unchanged_index_rows, 1);

    sqlx::query(
        "UPDATE media_items SET nfo_metadata_json = '{\"certification\":\"R\"}'
         WHERE id = 'restriction-index-item'",
    )
    .execute(database.pool())
    .await?;
    let changed_index_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_content_restriction_index
         WHERE item_id = 'restriction-index-item'",
    )
    .fetch_one(database.pool())
    .await?;
    let changed_tag_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_content_restriction_tags
         WHERE item_id = 'restriction-index-item'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(changed_index_rows, 0);
    assert_eq!(changed_tag_rows, 0);

    sqlx::query(
        "INSERT INTO media_content_restriction_index (item_id, certification_code)
         VALUES ('restriction-index-item', 'R')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_content_restriction_tags (item_id, tag)
         VALUES ('restriction-index-item', 'family')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "UPDATE media_items SET nfo_metadata_json = NULL WHERE id = 'restriction-index-item'",
    )
    .execute(database.pool())
    .await?;
    let cleared_index_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_content_restriction_index
         WHERE item_id = 'restriction-index-item'",
    )
    .fetch_one(database.pool())
    .await?;
    let cleared_tag_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_content_restriction_tags
         WHERE item_id = 'restriction-index-item'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(cleared_index_rows, 0);
    assert_eq!(cleared_tag_rows, 0);

    sqlx::query(
        "INSERT INTO media_content_restriction_index (item_id, certification_code)
         VALUES ('restriction-index-item', NULL)",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_content_restriction_tags (item_id, tag)
         VALUES ('restriction-index-item', 'family')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query("DELETE FROM media_items WHERE id = 'restriction-index-item'")
        .execute(database.pool())
        .await?;

    let stale_index_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_content_restriction_index
         WHERE item_id = 'restriction-index-item'",
    )
    .fetch_one(database.pool())
    .await?;
    let stale_tag_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_content_restriction_tags
         WHERE item_id = 'restriction-index-item'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(stale_index_rows, 0);
    assert_eq!(stale_tag_rows, 0);

    drop(database);
    drop_postgres_test_database(&database_name).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_user_device_inventory_migration_runs_from_an_empty_database()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (connection, database_name) = create_postgres_test_database().await?;
    let database = Database::connect_with_configuration(&config, &connection).await?;
    assert!(database.schema_version().await? >= 196);

    let session_columns: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.columns
         WHERE table_schema = current_schema() AND table_name = 'web_sessions'
           AND column_name = 'device_id'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(session_columns, 1);

    let columns: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.columns
         WHERE table_schema = current_schema() AND table_name = 'user_devices'
           AND column_name IN (
               'user_id', 'device_id', 'device_name', 'device_type', 'client_name',
               'client_version', 'first_seen_at', 'last_seen_at', 'last_remote_ip'
           )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(columns, 9);

    let index_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pg_indexes
         WHERE schemaname = current_schema()
           AND indexname = 'idx_user_devices_user_activity'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(index_count, 1);

    sqlx::query(
        "INSERT INTO users (id, username_normalized, display_name, password_hash)
         VALUES ('device-owner', 'device-owner', 'Device Owner', 'test-hash')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO web_sessions (
             id, user_id, session_token_hash, csrf_token_hash, expires_at
         ) VALUES (
             'legacy-session', 'device-owner', decode('01', 'hex'),
             decode('02', 'hex'), 9999999999
         )",
    )
    .execute(database.pool())
    .await?;
    let legacy_device_id: Option<String> =
        sqlx::query_scalar("SELECT device_id FROM web_sessions WHERE id = 'legacy-session'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(legacy_device_id, None);
    sqlx::query(
        "INSERT INTO user_devices (
             user_id, device_id, client_name, first_seen_at, last_seen_at
         ) VALUES ('device-owner', 'browser-device', 'Lux Web', 10, 20)",
    )
    .execute(database.pool())
    .await?;
    sqlx::query("DELETE FROM users WHERE id = 'device-owner'")
        .execute(database.pool())
        .await?;
    let cascaded_devices: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM user_devices WHERE user_id = 'device-owner'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(cascaded_devices, 0);

    drop(database);
    drop_postgres_test_database(&database_name).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_search_reindex_moves_to_the_outbox_consumer()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (connection, database_name) = create_postgres_test_database().await?;
    let database = Database::connect_with_configuration(&config, &connection).await?;

    sqlx::query("INSERT INTO libraries (id, name, kind) VALUES ('search-lib', 'Search', 'MOVIE')")
        .execute(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status,
             has_available_source
         ) VALUES (
             'search-item', 'search-lib', 'MOVIE', 'Alpha Movie', 'Alpha Movie',
             'LOCAL_CONFIRMED', 1
         )",
    )
    .execute(database.pool())
    .await?;

    async fn queue_len(database: &Database) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM search_outbox
             WHERE kind = 'search' AND item_id = 'search-item'",
        )
        .fetch_one(database.pool())
        .await
    }

    // The writer only queues: the search row must appear after the consumer drains.
    let queued_after_insert = queue_len(&database).await?;
    assert!(
        queued_after_insert >= 1,
        "insert must enqueue a search event"
    );
    let before: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_search WHERE item_id = 'search-item'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(
        before, 0,
        "media_search is built by the consumer, not the writer"
    );

    assert!(database.drain_search_reindex(100).await? >= 1);
    let title: String =
        sqlx::query_scalar("SELECT title FROM media_search WHERE item_id = 'search-item'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(title, "Alpha Movie");

    sqlx::query(
        "INSERT INTO item_aliases (id, item_id, alias, alias_normalized)
         VALUES ('alias-1', 'search-item', 'Alias One', 'alias one')",
    )
    .execute(database.pool())
    .await?;
    assert!(
        queue_len(&database).await? > queued_after_insert,
        "alias insert must enqueue"
    );
    database.drain_search_reindex(100).await?;
    let aliases: String =
        sqlx::query_scalar("SELECT aliases FROM media_search WHERE item_id = 'search-item'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(aliases, "Alias One");

    // A column outside the search projection must not enqueue anything.
    let queued_before_overview = queue_len(&database).await?;
    sqlx::query("UPDATE media_items SET overview = 'not indexed' WHERE id = 'search-item'")
        .execute(database.pool())
        .await?;
    assert_eq!(
        queue_len(&database).await?,
        queued_before_overview,
        "overview is not part of search"
    );

    sqlx::query("UPDATE media_items SET title = 'Beta Movie' WHERE id = 'search-item'")
        .execute(database.pool())
        .await?;
    assert!(
        queue_len(&database).await? > queued_before_overview,
        "title update must enqueue"
    );
    database.drain_search_reindex(100).await?;
    let title: String =
        sqlx::query_scalar("SELECT title FROM media_search WHERE item_id = 'search-item'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(title, "Beta Movie");

    sqlx::query("DELETE FROM media_items WHERE id = 'search-item'")
        .execute(database.pool())
        .await?;
    database.drain_search_reindex(100).await?;
    let remaining: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_search WHERE item_id = 'search-item'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(remaining, 0);

    drop(database);
    drop_postgres_test_database(&database_name).await?;
    Ok(())
}
#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_user_access_blacklist_rules_migration_runs_from_an_empty_database()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (connection, database_name) = create_postgres_test_database().await?;
    let database = Database::connect_with_configuration(&config, &connection).await?;
    assert!(database.schema_version().await? >= 196);

    let columns: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.columns
         WHERE table_schema = current_schema()
           AND table_name = 'user_access_blacklist_rules'
           AND column_name IN (
               'id', 'user_id', 'rule_type', 'match_value', 'is_enabled',
               'created_by', 'created_at', 'updated_at'
           )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(columns, 8);
    let index_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pg_indexes
         WHERE schemaname = current_schema()
           AND indexname IN (
               'idx_user_access_blacklist_global_active',
               'idx_user_access_blacklist_user_active',
               'uq_user_access_blacklist_global_value',
               'uq_user_access_blacklist_user_value'
           )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(index_count, 4);
    let (target_delete_action, creator_delete_action): (String, String) = sqlx::query_as(
        "SELECT
             (SELECT confdeltype::TEXT FROM pg_constraint
              WHERE conrelid = 'user_access_blacklist_rules'::regclass
                AND conname = 'user_access_blacklist_rules_user_id_fkey'),
             (SELECT confdeltype::TEXT FROM pg_constraint
              WHERE conrelid = 'user_access_blacklist_rules'::regclass
                AND conname = 'user_access_blacklist_rules_created_by_fkey')",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(target_delete_action, "c");
    assert_eq!(creator_delete_action, "n");

    drop(database);
    drop_postgres_test_database(&database_name).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_user_access_blacklist_rules_migration_upgrades_v195_database()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let (connection, database_name) = create_postgres_test_database().await?;
    let database_url = connection.postgres_url()?.ok_or("missing PostgreSQL URL")?;
    let migration_dir = temp_dir.path().join("migrations-v195");
    fs::create_dir(&migration_dir)?;
    let source_dir =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("migrations-postgres");
    for entry in fs::read_dir(&source_dir)? {
        let entry = entry?;
        let filename = entry.file_name();
        let version = filename
            .to_str()
            .and_then(|name| name.split_once('_'))
            .ok_or("invalid migration filename")?
            .0
            .parse::<i64>()?;
        if version <= 195 {
            fs::copy(entry.path(), migration_dir.join(filename))?;
        }
    }
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await?;
    Migrator::new(migration_dir.as_path())
        .await?
        .run(&pool)
        .await?;
    let old_version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await?;
    assert_eq!(old_version, 195);
    sqlx::query(
        "INSERT INTO users (id, username_normalized, display_name, password_hash)
         VALUES ('legacy-blacklist-owner', 'legacy-blacklist-owner', 'Legacy Owner', 'test-hash')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO user_devices (
             user_id, device_id, client_name, first_seen_at, last_seen_at
         ) VALUES ('legacy-blacklist-owner', 'legacy-device', 'Lux Web', 10, 20)",
    )
    .execute(&pool)
    .await?;
    fs::copy(
        source_dir.join("0196_user_access_blacklist_rules.sql"),
        migration_dir.join("0196_user_access_blacklist_rules.sql"),
    )?;
    Migrator::new(migration_dir.as_path())
        .await?
        .run(&pool)
        .await?;
    let new_version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await?;
    let preserved_devices: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM user_devices
         WHERE user_id = 'legacy-blacklist-owner' AND device_id = 'legacy-device'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(new_version, 196);
    assert_eq!(preserved_devices, 1);

    pool.close().await;
    drop_postgres_test_database(&database_name).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_user_device_listing_correlates_active_web_and_emby_logins()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (connection, database_name) = create_postgres_test_database().await?;
    let database = Database::connect_with_configuration(&config, &connection).await?;
    let setup = SetupService::new(database.clone())?;
    setup
        .complete("pg-admin", "Postgres Admin", "admin password")
        .await?;
    let target = luxd::auth::users::UserStore::new(database.clone())?
        .create_user(
            "pg-device-target",
            "Device Target",
            "target password",
            false,
        )
        .await?;
    let user_id = target.id.to_string();
    for device_id in ["web-device", "emby-device", "inactive-device"] {
        sqlx::query(
            "INSERT INTO user_devices (
                 user_id, device_id, client_name, first_seen_at, last_seen_at
             ) VALUES ($1, $2, 'Test Client', 10, 20)",
        )
        .bind(&user_id)
        .bind(device_id)
        .execute(database.pool())
        .await?;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs() as i64;
    sqlx::query(
        "INSERT INTO web_sessions (
             id, user_id, session_token_hash, csrf_token_hash, expires_at, device_id
         ) VALUES ('pg-active-web-session', $1, decode('31', 'hex'), decode('41', 'hex'), $2, 'web-device'),
                  ('pg-expired-web-session', $1, decode('32', 'hex'), decode('42', 'hex'), $3, 'inactive-device')",
    )
    .bind(&user_id)
    .bind(now + 3600)
    .bind(now - 1)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO access_tokens (
             id, token_hash, user_id, device_id, client_name, device_name,
             client_version, created_at, updated_at, revoked_at
         ) VALUES ('pg-active-emby-token', decode('51', 'hex'), $1, 'emby-device', 'Emby', 'TV', '1.0', $2, $2, NULL),
                  ('pg-revoked-emby-token', decode('52', 'hex'), $1, 'inactive-device', 'Emby', 'TV', '1.0', $2, $2, $3)",
    )
    .bind(&user_id)
    .bind(now)
    .bind(now)
    .execute(database.pool())
    .await?;

    let app = luxd::api::app_with_state(luxd::api::AppState::ready(
        config,
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        luxd::auth::emby::EmbyAuthService::new(database.clone())?,
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = reqwest::Client::builder().no_proxy().build()?;
    let login = client
        .post(format!("http://{address}/api/v1/auth/login"))
        .json(&json!({ "username": "pg-admin", "password": "admin password" }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let session_cookie = login
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| {
            let pair = value.split(';').next()?;
            pair.starts_with("lux_session=").then(|| pair.to_owned())
        })
        .ok_or("missing Web session cookie")?;
    let response = client
        .get(format!(
            "http://{address}/api/v1/admin/users/{user_id}/devices"
        ))
        .header(reqwest::header::COOKIE, session_cookie)
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await?;
    let devices = body["devices"].as_array().ok_or("missing devices")?;
    for (device_id, expected) in [
        ("web-device", true),
        ("emby-device", true),
        ("inactive-device", false),
    ] {
        let device = devices
            .iter()
            .find(|device| device["deviceId"] == device_id)
            .ok_or("missing test device")?;
        assert_eq!(device["hasActiveLogin"], expected);
    }

    server.abort();
    let _ = server.await;
    drop(client);
    database.close().await;
    drop_postgres_test_database(&database_name).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_web_session_device_id_migration_preserves_existing_sessions()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (connection, database_name) = create_postgres_test_database().await?;
    let database_url = connection.postgres_url()?.ok_or("missing PostgreSQL URL")?;
    let migration_dir = temp_dir.path().join("migrations-v194");
    fs::create_dir(&migration_dir)?;
    let source_dir =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("migrations-postgres");
    for entry in fs::read_dir(&source_dir)? {
        let entry = entry?;
        let filename = entry.file_name();
        let version = filename
            .to_str()
            .and_then(|name| name.split_once('_'))
            .ok_or("invalid migration filename")?
            .0
            .parse::<i64>()?;
        if version <= 194 {
            fs::copy(entry.path(), migration_dir.join(filename))?;
        }
    }
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await?;
    Migrator::new(migration_dir.as_path())
        .await?
        .run(&pool)
        .await?;
    sqlx::query(
        "INSERT INTO users (id, username_normalized, display_name, password_hash)
         VALUES ('legacy-session-owner', 'legacy-session-owner', 'Legacy Session', 'test-hash')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO web_sessions (
             id, user_id, session_token_hash, csrf_token_hash, expires_at
         ) VALUES (
             'legacy-session', 'legacy-session-owner', decode('01', 'hex'),
             decode('02', 'hex'), 9999999999
         )",
    )
    .execute(&pool)
    .await?;
    pool.close().await;

    let database = Database::connect_with_configuration(&config, &connection).await?;
    let device_id: Option<String> =
        sqlx::query_scalar("SELECT device_id FROM web_sessions WHERE id = 'legacy-session'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(device_id, None);
    assert!(database.schema_version().await? >= 196);

    drop(database);
    drop_postgres_test_database(&database_name).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_mail_and_invitation_migrations_apply_to_an_empty_database()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (connection, database_name) = create_postgres_test_database().await?;
    let result = async {
        let database = Database::connect_with_configuration(&config, &connection).await?;
        if database.schema_version().await? < 201 {
            return Err("invitation migration was not applied".into());
        }
        let column_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM information_schema.columns
             WHERE table_schema = current_schema() AND table_name = 'mail_outbox'",
        )
        .fetch_one(database.pool())
        .await?;
        if column_count != 13 {
            return Err("unexpected mail outbox schema".into());
        }
        let sensitive_column_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM information_schema.columns
             WHERE table_schema = current_schema() AND table_name = 'mail_outbox'
               AND column_name IN ('body', 'html_body', 'text_body', 'payload', 'token', 'password')",
        )
        .fetch_one(database.pool())
        .await?;
        if sensitive_column_count != 0 {
            return Err("mail outbox schema contains a sensitive payload column".into());
        }
        let challenge_column_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM information_schema.columns
             WHERE table_schema = current_schema() AND table_name = 'mail_challenges'",
        )
        .fetch_one(database.pool())
        .await?;
        if challenge_column_count != 9 {
            return Err("unexpected mail challenge schema".into());
        }
        let sensitive_challenge_column_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM information_schema.columns
             WHERE table_schema = current_schema() AND table_name = 'mail_challenges'
               AND column_name IN ('token', 'raw_token', 'body', 'password')",
        )
        .fetch_one(database.pool())
        .await?;
        if sensitive_challenge_column_count != 0 {
            return Err("mail challenge schema contains raw credential data".into());
        }
        let invitation_column_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM information_schema.columns
             WHERE table_schema = current_schema() AND table_name = 'user_invitations'",
        )
        .fetch_one(database.pool())
        .await?;
        if invitation_column_count != 14 {
            return Err("unexpected invitation schema".into());
        }
        let invitation_sensitive_column_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM information_schema.columns
             WHERE table_schema = current_schema() AND table_name = 'user_invitations'
               AND column_name IN ('token', 'raw_token', 'password', 'body', 'html_body', 'text_body')",
        )
        .fetch_one(database.pool())
        .await?;
        if invitation_sensitive_column_count != 0 {
            return Err("invitation schema contains sensitive material".into());
        }
        let invitation_unique_index_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pg_indexes
             WHERE schemaname = current_schema()
               AND indexname = 'ux_user_invitations_pending_email'",
        )
        .fetch_one(database.pool())
        .await?;
        if invitation_unique_index_count != 1 {
            return Err("invitation pending-email uniqueness index is missing".into());
        }
        let invitation_set_null_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pg_constraint
             WHERE conrelid = 'user_invitations'::regclass AND contype = 'f' AND confdeltype = 'n'",
        )
        .fetch_one(database.pool())
        .await?;
        if invitation_set_null_count != 2 {
            return Err("invitation history foreign keys must preserve rows".into());
        }
        database.close().await;
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    drop_postgres_test_database(&database_name).await?;
    result?;
    Ok(())
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
    assert!(database.schema_version().await? >= 200);
    let mail_outbox_column_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.columns
         WHERE table_schema = current_schema() AND table_name = 'mail_outbox'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(mail_outbox_column_count, 13);
    let sensitive_mail_outbox_column_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.columns
         WHERE table_schema = current_schema() AND table_name = 'mail_outbox'
           AND column_name IN ('body', 'html_body', 'text_body', 'payload', 'token', 'password')",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(sensitive_mail_outbox_column_count, 0);
    let setup = SetupService::new(database.clone())?;
    setup
        .complete("postgres-admin", "PostgreSQL Admin", "test-password")
        .await?;
    let postgres_admin_id: String =
        sqlx::query_scalar("SELECT id FROM users WHERE username_normalized = 'postgres-admin'")
            .fetch_one(database.pool())
            .await?;
    let restriction_columns: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.columns
         WHERE table_schema = current_schema()
           AND table_name = 'user_content_restrictions'
           AND column_name IN (
               'user_id', 'rating_system', 'maximum_rating', 'allow_unrated',
               'version', 'updated_at'
           )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(restriction_columns, 6);
    let restriction_tag_table: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.tables
         WHERE table_schema = current_schema()
           AND table_name = 'user_content_restriction_tags'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(restriction_tag_table, 1);
    sqlx::query(
        "INSERT INTO user_content_restrictions
            (user_id, rating_system, maximum_rating, allow_unrated)
         VALUES ($1, 'MPAA', 'PG-13', 0)",
    )
    .bind(&postgres_admin_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO user_content_restriction_tags (user_id, tag_kind, tag)
         VALUES ($1, 'ALLOW', 'family'), ($1, 'BLOCK', 'horror')",
    )
    .bind(&postgres_admin_id)
    .execute(database.pool())
    .await?;
    assert!(
        sqlx::query("UPDATE user_content_restrictions SET allow_unrated = 2 WHERE user_id = $1",)
            .bind(&postgres_admin_id)
            .execute(database.pool())
            .await
            .is_err()
    );
    for invalid_tag in [
        "INSERT INTO user_content_restriction_tags (user_id, tag_kind, tag)
         VALUES ($1, 'NEUTRAL', 'other')",
        "INSERT INTO user_content_restriction_tags (user_id, tag_kind, tag)
         VALUES ($1, 'ALLOW', '   ')",
        "INSERT INTO user_content_restriction_tags (user_id, tag_kind, tag)
         VALUES ($1, 'ALLOW', 'family')",
    ] {
        assert!(
            sqlx::query(invalid_tag)
                .bind(&postgres_admin_id)
                .execute(database.pool())
                .await
                .is_err()
        );
    }
    let preference_columns: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.columns
         WHERE table_schema = current_schema()
           AND table_name = 'user_account_preferences'
           AND column_name IN (
               'user_id', 'theme', 'accent_color', 'show_media_libraries',
               'show_continue_watching', 'audio_language', 'subtitle_language',
               'auto_play_next_episode', 'version', 'updated_at'
           )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(preference_columns, 10);
    sqlx::query("INSERT INTO user_account_preferences (user_id) VALUES ($1)")
        .bind(&postgres_admin_id)
        .execute(database.pool())
        .await?;
    let preference_defaults: (String, String, i64, i64, String, String, i64, i64) = sqlx::query_as(
        "SELECT theme, accent_color, show_media_libraries, show_continue_watching,
                    audio_language, subtitle_language, auto_play_next_episode, version
             FROM user_account_preferences WHERE user_id = $1",
    )
    .bind(&postgres_admin_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        preference_defaults,
        (
            "dark".to_owned(),
            "silver".to_owned(),
            1,
            1,
            "原始音轨".to_owned(),
            "简体中文".to_owned(),
            1,
            1,
        )
    );
    assert!(
        sqlx::query("UPDATE user_account_preferences SET theme = 'sepia' WHERE user_id = $1",)
            .bind(&postgres_admin_id)
            .execute(database.pool())
            .await
            .is_err()
    );
    assert!(
        sqlx::query(
            "UPDATE user_account_preferences SET accent_color = 'ultraviolet' WHERE user_id = $1",
        )
        .bind(&postgres_admin_id)
        .execute(database.pool())
        .await
        .is_err()
    );
    assert!(
        sqlx::query("UPDATE user_account_preferences SET version = 0 WHERE user_id = $1",)
            .bind(&postgres_admin_id)
            .execute(database.pool())
            .await
            .is_err()
    );
    sqlx::query(
        "INSERT INTO users (id, username_normalized, display_name, password_hash)
         VALUES ('postgres-pref-cascade', 'postgres-pref-cascade', 'Cascade', 'test')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO user_account_preferences (user_id)
         VALUES ('postgres-pref-cascade')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO user_content_restrictions
            (user_id, rating_system, maximum_rating, allow_unrated)
         VALUES ('postgres-pref-cascade', 'MPAA', NULL, 1)",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO user_content_restriction_tags (user_id, tag_kind, tag)
         VALUES ('postgres-pref-cascade', 'BLOCK', 'horror')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query("DELETE FROM user_content_restrictions WHERE user_id = 'postgres-pref-cascade'")
        .execute(database.pool())
        .await?;
    let cascaded_restriction_tags: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM user_content_restriction_tags
         WHERE user_id = 'postgres-pref-cascade'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(cascaded_restriction_tags, 0);
    sqlx::query(
        "INSERT INTO user_content_restrictions
            (user_id, rating_system, maximum_rating, allow_unrated)
         VALUES ('postgres-pref-cascade', 'MPAA', NULL, 1)",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO user_content_restriction_tags (user_id, tag_kind, tag)
         VALUES ('postgres-pref-cascade', 'BLOCK', 'horror')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query("DELETE FROM users WHERE id = 'postgres-pref-cascade'")
        .execute(database.pool())
        .await?;
    let cascaded_preferences: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM user_account_preferences
         WHERE user_id = 'postgres-pref-cascade'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(cascaded_preferences, 0);
    let cascaded_restrictions: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM user_content_restrictions
         WHERE user_id = 'postgres-pref-cascade'",
    )
    .fetch_one(database.pool())
    .await?;
    let cascaded_user_tags: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM user_content_restriction_tags
         WHERE user_id = 'postgres-pref-cascade'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(cascaded_restrictions, 0);
    assert_eq!(cascaded_user_tags, 0);
    sqlx::query(
        "INSERT INTO users (id, username_normalized, display_name, password_hash)
         VALUES ('playback-policy-user', 'playback-policy-user', 'Playback Policy', 'test')",
    )
    .execute(database.pool())
    .await?;
    let playback_policy: (i64, i64, i64, i64, i64, String) = sqlx::query_as(
        "SELECT can_play, max_simultaneous_playbacks, allow_remux,
                allow_audio_transcode, allow_video_transcode, library_access_mode
         FROM users WHERE id = 'playback-policy-user'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(playback_policy, (1, 0, 1, 1, 1, "ALL".to_owned()));
    let playback_admission_columns: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.columns
         WHERE table_schema = current_schema()
           AND table_name = 'playback_admissions'
           AND column_name IN (
               'user_id', 'play_session_id', 'state', 'admitted_at', 'last_seen_at'
           )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(playback_admission_columns, 5);
    let playback_admission_constraints: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pg_constraint
         WHERE conrelid = 'playback_admissions'::regclass
           AND conname IN (
               'pk_playback_admissions', 'fk_playback_admissions_user',
               'chk_playback_admissions_state', 'chk_playback_admissions_timestamps'
           )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(playback_admission_constraints, 4);
    let playback_admission_index: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('idx_playback_admissions_active')::text")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(
        playback_admission_index.as_deref(),
        Some("idx_playback_admissions_active")
    );
    let playback_admission_index_columns: Option<String> = sqlx::query_scalar(
        "SELECT indexdef FROM pg_indexes
         WHERE schemaname = current_schema()
           AND indexname = 'idx_playback_admissions_active'",
    )
    .fetch_optional(database.pool())
    .await?;
    assert!(
        playback_admission_index_columns
            .as_deref()
            .is_some_and(|definition| definition.contains("(user_id, state, last_seen_at)"))
    );
    let reconciliation_index: Option<String> = sqlx::query_scalar(
        "SELECT to_regclass('idx_media_items_nfo_collection_reference_pending')::text",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        reconciliation_index.as_deref(),
        Some("idx_media_items_nfo_collection_reference_pending")
    );
    let global_collection_tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.tables
         WHERE table_schema = current_schema()
           AND table_name IN (
               'collection_item_references', 'collection_work_items',
               'collection_work_sources', 'collection_sync_outbox',
               'global_collection_work_references', 'user_collection_source_preferences'
           )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(global_collection_tables, 6);
    let rootless_system_collection_library: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM libraries library
         WHERE library.id = '00000000-0000-7000-8000-000000000538'
           AND library.kind = 'MIXED'
           AND library.system_kind = 'COLLECTIONS'
           AND NOT EXISTS (
               SELECT 1 FROM library_roots root WHERE root.library_id = library.id
           )",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(rootless_system_collection_library, 1);
    let nfo_collection_reference_column: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.columns
         WHERE table_schema = current_schema()
           AND table_name = 'media_items'
           AND column_name = 'nfo_collection_reference_key'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(nfo_collection_reference_column, 1);
    let dispatch_source_default: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.columns
         WHERE table_schema = current_schema()
           AND table_name = 'metadata_reidentify_jobs'
           AND column_name = 'dispatch_source'
           AND column_default LIKE '%EXPLICIT%'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(dispatch_source_default, 1);
    let automatic_queue_index: bool = sqlx::query_scalar(
        "SELECT to_regclass('idx_metadata_reidentify_jobs_automatic_queue') IS NOT NULL",
    )
    .fetch_one(database.pool())
    .await?;
    assert!(automatic_queue_index);
    let relation_checksum_is_nullable: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.columns
         WHERE table_schema = current_schema()
           AND table_name = 'person_index_item_state'
           AND column_name = 'relation_checksum'
           AND is_nullable = 'YES'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(relation_checksum_is_nullable, 1);
    // Deletes must efficiently check every referencing FK, including NO ACTION references.
    for index_name in [
        "idx_danmaku_match_job_items_media_source_id",
        "idx_chapter_detection_job_items_source_id",
        "idx_chapter_detection_job_items_item_id",
        "idx_chapter_detection_job_items_season_id",
        "idx_reconciliation_scan_entries_library_root_id",
        "idx_scan_job_paths_library_root_id",
        "idx_emby_migration_import_records_lux_item_id",
        "idx_user_item_state_item_id",
        "idx_playback_sessions_item_id",
        "idx_chapter_detection_jobs_library_id",
        "idx_strm_probe_jobs_library_id",
        "idx_web_playback_sessions_item_id",
        "idx_danmaku_match_jobs_library_id",
        "idx_scan_manifest_roots_library_root_id",
        "idx_library_cover_jobs_library_id",
        "idx_user_library_order_library_id",
        "idx_scan_local_metadata_batches_library_root_id",
        "idx_scan_manifest_deltas_manifest_root_id",
        "idx_emby_migration_item_matches_lux_item_id",
        "idx_metadata_reidentify_jobs_library_id",
        "idx_playback_sessions_media_source_id",
        "idx_web_playback_sessions_media_source_id",
        "idx_scan_manifest_deltas_parent_key",
        "idx_scan_jobs_failed_count",
    ] {
        let index_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1
                 FROM pg_class idx
                 JOIN pg_namespace ns ON ns.oid = idx.relnamespace
                 JOIN pg_index ix ON ix.indexrelid = idx.oid
                 WHERE ns.nspname = current_schema()
                   AND idx.relname = $1
                   AND ix.indisvalid
                   AND ix.indisready
             )",
        )
        .bind(index_name)
        .fetch_one(database.pool())
        .await?;
        assert!(
            index_exists,
            "missing required PostgreSQL index {index_name}"
        );
    }
    insert_postgres_homevideos_video(&database).await?;
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
    let progressive_metadata_tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM information_schema.tables
         WHERE table_schema = current_schema()
           AND table_name IN ('scan_local_metadata_batches', 'item_metadata_completeness')",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(progressive_metadata_tables, 2);
    let scan_metadata_policy: i64 = sqlx::query_scalar(
        "SELECT scan_missing_metadata_auto_match_enabled
         FROM libraries WHERE kind = 'HOMEVIDEOS' LIMIT 1",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(scan_metadata_policy, 1);
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
    assert_eq!(provider_row_triggers, 1);
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
    let manifest_discovery_mode_column: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM information_schema.columns
         WHERE table_schema = current_schema()
           AND table_name = 'scan_manifests' AND column_name = 'discovery_mode'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(manifest_discovery_mode_column, 1);
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
    let providerless_item_id = Uuid::now_v7().to_string();
    let empty_provider_item_id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO media_items (
            id, library_id, item_type, title, sort_title, provider_ids_json,
            identification_status, has_available_source
        ) VALUES
            ($1, $3, 'MOVIE', 'Postgres Providerless Movie', 'postgres providerless movie',
             NULL, 'LOCAL_CONFIRMED', 1),
            ($2, $3, 'MOVIE', 'Postgres Empty Provider Movie', 'postgres empty provider movie',
             '{}', 'LOCAL_CONFIRMED', 1)",
    )
    .bind(&providerless_item_id)
    .bind(&empty_provider_item_id)
    .bind(library_id.to_string())
    .execute(database.pool())
    .await?;
    let providerless_index_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_item_provider_ids
         WHERE media_item_id IN ($1, $2)",
    )
    .bind(&providerless_item_id)
    .bind(&empty_provider_item_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(providerless_index_rows, 0);
    database.drain_search_reindex(500).await?;
    let providerless_search_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_search
         WHERE item_id IN ($1, $2)",
    )
    .bind(&providerless_item_id)
    .bind(&empty_provider_item_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(providerless_search_rows, 2);
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

    let collections_library_id: String =
        sqlx::query_scalar("SELECT id FROM libraries WHERE system_kind = 'COLLECTIONS'")
            .fetch_one(database.pool())
            .await?;
    let logical_work_id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO media_items (
            id, library_id, item_type, title, sort_title,
            identification_status, identity_key, has_available_source
         ) VALUES ($1, $2, 'MOVIE', '银河搜索电影', '银河搜索电影',
                   'ONLINE_CONFIRMED', 'global-work:tmdb:postgres-search-test', 1)",
    )
    .bind(&logical_work_id)
    .bind(&collections_library_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO collection_work_items (work_item_id, movie_provider, movie_provider_id)
         VALUES ($1, 'tmdb', 'postgres-search-test')",
    )
    .bind(&logical_work_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO collection_work_sources (work_item_id, source_item_id, sort_order)
         VALUES ($1, $2, 0)",
    )
    .bind(&logical_work_id)
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
#[allow(clippy::type_complexity)]
async fn postgres_legacy_user_and_library_access_upgrade_from_v183_to_current_schema()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let migration_prefix = temp_dir
        .path()
        .join("postgres-migrations-before-account-profile");
    fs::create_dir(&migration_prefix)?;
    for entry in fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations-postgres"))? {
        let path = entry?.path();
        if path.extension() != Some(OsStr::new("sql")) {
            continue;
        }
        let Some(version) = path
            .file_stem()
            .and_then(OsStr::to_str)
            .and_then(|name| name.split('_').next())
            .and_then(|value| value.parse::<i64>().ok())
        else {
            continue;
        };
        if version <= 183 {
            let file_name = path.file_name().ok_or("migration has no file name")?;
            fs::copy(&path, migration_prefix.join(file_name))?;
        }
    }

    let (connection, database_name) = create_postgres_test_database().await?;
    let result = async {
        let database_url = connection.postgres_url()?.ok_or("missing PostgreSQL URL")?;
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await?;
        Migrator::new(migration_prefix.as_path())
            .await?
            .run(&pool)
            .await?;
        sqlx::query(
            "INSERT INTO users (id, username_normalized, display_name, password_hash, is_disabled)
             VALUES ('legacy-enabled', 'legacy-enabled', 'Legacy Enabled', 'hash', 0),
                    ('legacy-disabled', 'legacy-disabled', 'Legacy Disabled', 'hash', 1),
                    ('legacy-denials-only', 'legacy-denials-only', 'Legacy Denials', 'hash', 0),
                    ('legacy-selected', 'legacy-selected', 'Legacy Selected', 'hash', 0)",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO libraries (id, name, kind)
             VALUES ('legacy-library', 'Legacy Library', 'MOVIE')",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "INSERT INTO user_library_access (user_id, library_id, can_view)
             VALUES ('legacy-denials-only', 'legacy-library', 0),
                    ('legacy-selected', 'legacy-library', 1)",
        )
        .execute(&pool)
        .await?;
        pool.close().await;

        let database = Database::connect_with_configuration(&config, &connection).await?;
        assert_eq!(
            database.schema_version().await?,
            latest_migration_version("migrations-postgres")?
        );
        let users: Vec<(String, Option<String>, Option<i64>, Option<i64>, i64, i64)> =
            sqlx::query_as(
                "SELECT id, email, email_verified_at, deletion_purge_at,
                        is_disabled, is_disabled_before_delete
                 FROM users WHERE id LIKE 'legacy-%' ORDER BY id",
            )
            .fetch_all(database.pool())
            .await?;
        assert_eq!(
            users[0],
            ("legacy-denials-only".to_owned(), None, None, None, 0, 0)
        );
        assert_eq!(
            users[1],
            ("legacy-disabled".to_owned(), None, None, None, 1, 0)
        );
        assert_eq!(
            users[2],
            ("legacy-enabled".to_owned(), None, None, None, 0, 0)
        );
        assert_eq!(
            users[3],
            ("legacy-selected".to_owned(), None, None, None, 0, 0)
        );
        assert_eq!(users.len(), 4);

        let legacy_access_modes: Vec<(String, String)> = sqlx::query_as(
            "SELECT id, library_access_mode FROM users WHERE id LIKE 'legacy-%' ORDER BY id",
        )
        .fetch_all(database.pool())
        .await?;
        assert_eq!(
            legacy_access_modes,
            [
                ("legacy-denials-only".to_owned(), "ALL".to_owned()),
                ("legacy-disabled".to_owned(), "ALL".to_owned()),
                ("legacy-enabled".to_owned(), "ALL".to_owned()),
                ("legacy-selected".to_owned(), "SELECTED".to_owned()),
            ]
        );
        let legacy_access_rows: Vec<(String, String, i64)> = sqlx::query_as(
            "SELECT user_id, library_id, can_view FROM user_library_access
             WHERE user_id LIKE 'legacy-%' ORDER BY user_id",
        )
        .fetch_all(database.pool())
        .await?;
        assert_eq!(
            legacy_access_rows,
            [
                (
                    "legacy-denials-only".to_owned(),
                    "legacy-library".to_owned(),
                    0
                ),
                ("legacy-selected".to_owned(), "legacy-library".to_owned(), 1),
            ]
        );

        sqlx::query("UPDATE users SET email = 'legacy@example.com' WHERE id = 'legacy-enabled'")
            .execute(database.pool())
            .await?;
        let duplicate_email = sqlx::query(
            "UPDATE users SET email = 'LEGACY@example.com' WHERE id = 'legacy-disabled'",
        )
        .execute(database.pool())
        .await;
        assert!(
            duplicate_email.is_err(),
            "email uniqueness must ignore case"
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    drop_postgres_test_database(&database_name).await?;
    result
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_user_library_access_mode_migration_preserves_legacy_semantics()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let migration_prefix = temp_dir
        .path()
        .join("postgres-migrations-before-library-access-mode");
    fs::create_dir(&migration_prefix)?;
    for entry in fs::read_dir("migrations-postgres")? {
        let path = entry?.path();
        if path.extension() != Some(OsStr::new("sql")) {
            continue;
        }
        let Some(version) = path
            .file_stem()
            .and_then(OsStr::to_str)
            .and_then(|name| name.split('_').next())
            .and_then(|value| value.parse::<i64>().ok())
        else {
            continue;
        };
        if version <= 186 {
            let file_name = path.file_name().ok_or("migration has no file name")?;
            fs::copy(&path, migration_prefix.join(file_name))?;
        }
    }

    let (connection, database_name) = create_postgres_test_database().await?;
    let result = async {
        let database_url = connection.postgres_url()?.ok_or("missing PostgreSQL URL")?;
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await?;
        Migrator::new(migration_prefix.as_path())
            .await?
            .run(&pool)
            .await?;
        sqlx::raw_sql(
            "INSERT INTO libraries (id, name, kind)
             VALUES ('access-library', 'Access library', 'MOVIE');
             INSERT INTO users (id, username_normalized, display_name, password_hash)
             VALUES ('legacy-all', 'legacy-all', 'Legacy All', 'hash'),
                    ('legacy-denials-only', 'legacy-denials-only', 'Legacy Denials', 'hash'),
                    ('legacy-selected', 'legacy-selected', 'Legacy Selected', 'hash');
             INSERT INTO user_library_access (user_id, library_id, can_view)
             VALUES ('legacy-denials-only', 'access-library', 0),
                    ('legacy-selected', 'access-library', 1);",
        )
        .execute(&pool)
        .await?;
        pool.close().await;

        let database = Database::connect_with_configuration(&config, &connection).await?;
        assert!(database.schema_version().await? >= 190);
        let modes: Vec<(String, String)> = sqlx::query_as(
            "SELECT id, library_access_mode FROM users WHERE id LIKE 'legacy-%' ORDER BY id",
        )
        .fetch_all(database.pool())
        .await?;
        assert_eq!(
            modes,
            [
                ("legacy-all".to_owned(), "ALL".to_owned()),
                ("legacy-denials-only".to_owned(), "ALL".to_owned()),
                ("legacy-selected".to_owned(), "SELECTED".to_owned()),
            ]
        );
        let preserved_grant_rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT user_id, can_view FROM user_library_access
             WHERE user_id LIKE 'legacy-%' ORDER BY user_id",
        )
        .fetch_all(database.pool())
        .await?;
        assert_eq!(
            preserved_grant_rows,
            [
                ("legacy-denials-only".to_owned(), 0),
                ("legacy-selected".to_owned(), 1),
            ]
        );
        assert!(
            sqlx::query(
                "UPDATE users SET library_access_mode = 'INVALID' WHERE id = 'legacy-all'",
            )
            .execute(database.pool())
            .await
            .is_err(),
            "PostgreSQL must reject modes outside the supported values"
        );
        assert!(
            sqlx::query("UPDATE users SET library_access_mode = NULL WHERE id = 'legacy-all'")
                .execute(database.pool())
                .await
                .is_err(),
            "PostgreSQL mode must remain non-null"
        );
        database.close().await;
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    drop_postgres_test_database(&database_name).await?;
    result
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_merged_into_partial_index_preserves_set_null_behavior()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (connection, database_name) = create_postgres_test_database().await?;
    let database = Database::connect_with_configuration(&config, &connection).await?;

    let index_definition: String = sqlx::query_scalar(
        "SELECT indexdef
         FROM pg_indexes
         WHERE schemaname = current_schema()
           AND indexname = 'idx_media_items_merged_into'",
    )
    .fetch_one(database.pool())
    .await?;
    let normalized_index_definition = index_definition
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();

    let library_id = Uuid::now_v7().to_string();
    let target_item_id = Uuid::now_v7().to_string();
    let merged_item_id = Uuid::now_v7().to_string();
    sqlx::query("INSERT INTO libraries (id, name, kind) VALUES ($1, $2, 'MOVIE')")
        .bind(&library_id)
        .bind("Partial merged index test")
        .execute(database.pool())
        .await?;
    for (item_id, title) in [
        (&target_item_id, "Merge target"),
        (&merged_item_id, "Merged source"),
    ] {
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ($1, $2, 'MOVIE', $3, $4, 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .bind(&library_id)
        .bind(title)
        .bind(title.to_ascii_lowercase())
        .execute(database.pool())
        .await?;
    }
    sqlx::query("UPDATE media_items SET merged_into_item_id = $1 WHERE id = $2")
        .bind(&target_item_id)
        .bind(&merged_item_id)
        .execute(database.pool())
        .await?;
    sqlx::query("DELETE FROM media_items WHERE id = $1")
        .bind(&target_item_id)
        .execute(database.pool())
        .await?;
    let remaining_merged_items: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_items
         WHERE id = $1 AND merged_into_item_id IS NULL",
    )
    .bind(&merged_item_id)
    .fetch_one(database.pool())
    .await?;

    database.close().await;
    drop_postgres_test_database(&database_name).await?;

    assert!(
        normalized_index_definition.contains("where (merged_into_item_id is not null)"),
        "expected a partial merged-into index, found: {index_definition}"
    );
    assert_eq!(remaining_merged_items, 1);
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_person_match_decisions_migrate_and_restore_snapshots()
-> Result<(), Box<dyn std::error::Error>> {
    for upgrade_from_175 in [true, false] {
        let temp_dir = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        };
        let (connection, database_name) = create_postgres_test_database().await?;
        let database_url = connection.postgres_url()?.ok_or("missing PostgreSQL URL")?;
        let outcome: Result<(), Box<dyn std::error::Error>> = async {
            let source =
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("migrations-postgres");
            let migrations = temp_dir.path().join("migrations");
            fs::create_dir(&migrations)?;
            for entry in fs::read_dir(source)? {
                let entry = entry?;
                let filename = entry.file_name();
                let version = filename
                    .to_str()
                    .and_then(|name| name.split_once('_'))
                    .ok_or("invalid migration filename")?
                    .0
                    .parse::<i64>()?;
                if !upgrade_from_175 || version <= 175 {
                    fs::copy(entry.path(), migrations.join(filename))?;
                }
            }
            let pool = PgPoolOptions::new()
                .max_connections(1)
                .connect(&database_url)
                .await?;
            sqlx::migrate::Migrator::new(migrations.as_path())
                .await?
                .run(&pool)
                .await?;
            let history: Vec<(i64, Vec<u8>)> =
                sqlx::query_as("SELECT version, checksum FROM _sqlx_migrations ORDER BY version")
                    .fetch_all(&pool)
                    .await?;
            if upgrade_from_175 {
                assert_eq!(history.last().map(|row| row.0), Some(175));
                let missing_columns: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM information_schema.columns
                     WHERE table_schema = 'public' AND table_name = 'person_match_candidates'
                       AND column_name IN ('target_person_id', 'previous_person_id')",
                )
                .fetch_one(&pool)
                .await?;
                assert_eq!(missing_columns, 0);
            }
            sqlx::raw_sql(
                r#"INSERT INTO libraries (id, name, kind)
                 VALUES ('match-library', 'People matches', 'MOVIE');
                 INSERT INTO media_items (
                     id, library_id, item_type, title, sort_title, identification_status
                 ) VALUES ('match-item', 'match-library', 'MOVIE', 'Match', 'match',
                           'LOCAL_CONFIRMED');
                 INSERT INTO people (id, display_name, directory_name, normalized_name,
                                     created_at, updated_at)
                 VALUES ('lux-000001', 'Previous', 'Previous-lux-000001', 'previous', 1, 1),
                        ('lux-000002', 'Target', 'Target-lux-000002', 'target', 1, 1);
                 INSERT INTO person_identities (person_id, provider, provider_id, match_method,
                                                created_at, updated_at)
                 VALUES ('lux-000001', 'tmdb', 'actor-match', 'MANUAL_CONFIRM', 1, 1);
                 INSERT INTO person_match_candidates (
                     id, item_id, provider, provider_id, candidate_person_ids_json,
                     status, score, evidence_json, created_at, updated_at
                 ) VALUES ('match-candidate', 'match-item', 'tmdb', 'actor-match',
                           '["lux-000002"]', 'PENDING', 0.5, '{}', 1, 1);"#,
            )
            .execute(&pool)
            .await?;
            pool.close().await;

            let database = Database::connect_with_configuration(&config, &connection).await?;
            let service =
                PeopleService::new(config.config_dir.clone()).with_database(database.clone());
            let (candidates, total) = service.list_pending_person_match_candidates(0, 10).await?;
            assert_eq!(total, 1);
            assert_eq!(candidates[0].id, "match-candidate");
            assert_eq!(candidates[0].score, Some(0.5));
            let columns: Vec<(String, String, String)> = sqlx::query_as(
                "SELECT column_name::text, data_type::text, is_nullable::text
                 FROM information_schema.columns
                 WHERE table_schema = 'public' AND table_name = 'person_match_candidates'
                   AND column_name IN ('target_person_id', 'previous_person_id')
                 ORDER BY column_name",
            )
            .fetch_all(database.pool())
            .await?;
            assert_eq!(
                columns,
                [
                    (
                        "previous_person_id".to_owned(),
                        "text".to_owned(),
                        "YES".to_owned()
                    ),
                    (
                        "target_person_id".to_owned(),
                        "text".to_owned(),
                        "YES".to_owned()
                    ),
                ]
            );
            assert!(database.schema_version().await? >= 190);
            let preserved_history: Vec<(i64, Vec<u8>)> = sqlx::query_as(
                "SELECT version, checksum FROM _sqlx_migrations
                 WHERE version <= $1 ORDER BY version",
            )
            .bind(history.last().ok_or("missing migration history")?.0)
            .fetch_all(database.pool())
            .await?;
            assert_eq!(preserved_history, history);
            let index: Option<String> = sqlx::query_scalar(
                "SELECT to_regclass('idx_person_match_candidates_target')::text",
            )
            .fetch_one(database.pool())
            .await?;
            assert_eq!(index.as_deref(), Some("idx_person_match_candidates_target"));

            let movement = service
                .confirm_person_match_candidate("match-candidate", "lux-000002", "{}")
                .await?;
            assert_eq!(movement.previous_person_id.as_deref(), Some("lux-000001"));
            let snapshot = config
                .config_dir
                .join("metadata/people/matches/match-candidate.json");
            let snapshot_bytes = fs::read(&snapshot)?;
            let snapshot_value: serde_json::Value = serde_json::from_slice(&snapshot_bytes)?;
            assert_eq!(snapshot_value["targetPersonId"], "lux-000002");
            assert_eq!(snapshot_value["previousPersonId"], "lux-000001");
            sqlx::query("DELETE FROM person_match_candidates WHERE id = 'match-candidate'")
                .execute(database.pool())
                .await?;
            // Repeated rebuilds must preserve one candidate recovered from the snapshot.
            let restarted =
                PeopleService::new(config.config_dir.clone()).with_database(database.clone());
            restarted.rebuild_person_credit_index().await?;
            restarted.rebuild_person_credit_index().await?;
            let restored: Vec<(String, String, String)> = sqlx::query_as(
                "SELECT status, target_person_id, previous_person_id
                 FROM person_match_candidates WHERE id = 'match-candidate'",
            )
            .fetch_all(database.pool())
            .await?;
            assert_eq!(
                restored,
                [(
                    "CONFIRMED".to_owned(),
                    "lux-000002".to_owned(),
                    "lux-000001".to_owned()
                )]
            );
            assert_eq!(fs::read(snapshot)?, snapshot_bytes);
            restarted
                .undo_person_match_candidate("match-candidate", "{}")
                .await?;
            let owner: String = sqlx::query_scalar(
                "SELECT person_id FROM person_identities
                 WHERE provider = 'tmdb' AND provider_id = 'actor-match'",
            )
            .fetch_one(database.pool())
            .await?;
            assert_eq!(owner, "lux-000001");
            database.close().await;
            Ok(())
        }
        .await;
        drop_postgres_test_database(&database_name).await?;
        outcome?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_upgrade_from_deployed_migration_160_preserves_history()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (connection, database_name) = create_postgres_test_database().await?;
    let database_url = connection.postgres_url()?.ok_or("missing PostgreSQL URL")?;
    let outcome: Result<(), Box<dyn std::error::Error>> = async {
        let source_migrations =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("migrations-postgres");
        let deployed_migrations = temp_dir.path().join("migrations-v160");
        fs::create_dir(&deployed_migrations)?;
        for entry in fs::read_dir(source_migrations)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("sql") {
                continue;
            }
            let filename = entry
                .file_name()
                .into_string()
                .map_err(|_| "migration filename is not UTF-8")?;
            let version = filename
                .split_once('_')
                .ok_or("migration filename has no version separator")?
                .0
                .parse::<i64>()?;
            if version <= 160 {
                fs::copy(path, deployed_migrations.join(filename))?;
            }
        }

        let migration_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&database_url)
            .await?;
        sqlx::migrate::Migrator::new(deployed_migrations.as_path())
            .await?
            .run(&migration_pool)
            .await?;
        let deployed_version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
            .fetch_one(&migration_pool)
            .await?;
        assert_eq!(deployed_version, 160);
        sqlx::query(
            "INSERT INTO metadata_reidentify_jobs (
                 id, status, processed_count, total_count, mode, library_id, job_scope
             ) VALUES ('legacy-dispatch-source', 'QUEUED', 0, 0,
                       'FILL_MISSING', NULL, 'ITEMS')",
        )
        .execute(&migration_pool)
        .await?;
        migration_pool.close().await;

        let upgraded_database = Database::connect_with_configuration(&config, &connection).await?;
        assert!(upgraded_database.schema_version().await? >= 190);
        let legacy_dispatch_source: String = sqlx::query_scalar(
            "SELECT dispatch_source FROM metadata_reidentify_jobs
             WHERE id = 'legacy-dispatch-source'",
        )
        .fetch_one(upgraded_database.pool())
        .await?;
        assert_eq!(legacy_dispatch_source, "EXPLICIT");
        upgraded_database.close().await;
        Ok(())
    }
    .await;

    drop_postgres_test_database(&database_name).await?;
    outcome?;
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
    assert!(database.schema_version().await? >= 190);
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
    sqlx::query(
        "UPDATE scan_manifests
         SET workflow_version = 1, discovery_format_version = 2, discovery_mode = 'PERSISTED'
         WHERE job_id = $1",
    )
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
    sqlx::query(
        "UPDATE scan_manifests
         SET discovery_format_version = 2, discovery_mode = 'PERSISTED'
         WHERE job_id = $1",
    )
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
async fn postgres_manifest_add_claim_preserves_incremental_race_winner()
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
            &format!("PostgreSQL claim race {}", Uuid::now_v7()),
            luxd::library::LibraryKind::Movie,
            false,
        )
        .await?;
    let root = temp_dir.path().join("media");
    fs::create_dir_all(&root)?;
    fs::write(root.join("Raced.Movie.2024.mkv"), b"scanned version")?;
    fs::write(root.join("Clear.Movie.2023.mkv"), b"uncontested version")?;
    let root_id = libraries
        .add_root(library.id, root.to_str().ok_or("non-UTF-8 media root")?)
        .await?
        .root
        .id
        .to_string();

    let jobs = ScanJobService::new(database.clone());
    let job = jobs.create_movie_scan_job(library.id).await?;
    let function_sql = format!(
        "CREATE FUNCTION claim_manifest_add_after_observation() RETURNS trigger
         LANGUAGE plpgsql AS $$
         BEGIN
             IF NEW.state = 'SCANNING'
               AND OLD.completed_directory_count = 0
               AND NEW.completed_directory_count > 0
               AND NEW.manifest_id = (
                   SELECT id FROM scan_manifests WHERE job_id = '{}'
               )
             THEN
                 INSERT INTO filesystem_entries (
                     id, library_root_id, relative_path, entry_kind, size, modified_at,
                     inode, fingerprint, last_seen_generation, is_missing
                 ) VALUES (
                     'incremental-entry', NEW.library_root_id, 'Raced.Movie.2024.mkv', 'FILE',
                     777, 888, NULL, decode('09080706', 'hex'), 'incremental-generation', 0
                 ) ON CONFLICT(library_root_id, relative_path) DO NOTHING;
             END IF;
             RETURN NEW;
         END;
         $$",
        job.id
    );
    sqlx::query(sqlx::AssertSqlSafe(function_sql))
        .execute(database.pool())
        .await?;
    sqlx::query(
        "CREATE TRIGGER claim_manifest_add_after_observation
         AFTER UPDATE ON scan_manifest_roots
         FOR EACH ROW EXECUTE FUNCTION claim_manifest_add_after_observation()",
    )
    .execute(database.pool())
    .await?;

    jobs.run_to_completion(&job.id, 100, None).await?;

    let filesystem_entry: (String, i64, Vec<u8>, String) = sqlx::query_as(
        "SELECT id, size, fingerprint, last_seen_generation FROM filesystem_entries
         WHERE library_root_id = $1 AND relative_path = 'Raced.Movie.2024.mkv'",
    )
    .bind(&root_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        filesystem_entry,
        (
            "incremental-entry".to_owned(),
            777,
            vec![9, 8, 7, 6],
            "incremental-generation".to_owned()
        )
    );
    let raced_source_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_sources WHERE filesystem_entry_id = 'incremental-entry'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(raced_source_count, 0);
    let uncontested_source_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM media_sources source
         JOIN filesystem_entries entry ON entry.id = source.filesystem_entry_id
         WHERE entry.library_root_id = $1 AND entry.relative_path = 'Clear.Movie.2023.mkv'",
    )
    .bind(&root_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(uncontested_source_count, 1);

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
    let failed_checkpoint: (String, String, i64, String) = sqlx::query_as(
        "SELECT job.status, job.scan_phase, manifest.postprocessing_targets_ready,
                root.postprocessing_target_stage
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
        )
    );
    let error: Option<String> = sqlx::query_scalar("SELECT error FROM scan_jobs WHERE id = $1")
        .bind(&job.id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(error.as_deref(), Some("POSTPROCESSING_FAILED"));
    let (_, events) = LogStore::new(&config.config_dir)
        .list_scan_job_events(&job.id, None, None, 0, 100)
        .await?;
    assert!(
        events
            .iter()
            .any(|event| { event.event_code == "POSTPROCESSING_FAILED" && event.level == "ERROR" })
    );

    fs::remove_dir_all(&root)?;
    fs::rename(&backup, &root)?;
    let retried = jobs.retry(&job.id).await?;
    assert_eq!(retried.id, job.id);
    assert_eq!(retried.error, None);
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
async fn postgres_postprocessing_targets_preserve_changed_and_new_item_kinds()
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
            &format!("PostgreSQL target changes {}", Uuid::now_v7()),
            luxd::library::LibraryKind::Movie,
            false,
        )
        .await?;
    let root = temp_dir.path().join("media");
    fs::create_dir_all(&root)?;
    let changed = root.join("Example.Movie.2024.1080p.mkv");
    let changed_only = root.join("Different.Movie.2023.1080p.mkv");
    fs::write(&changed, b"before")?;
    fs::write(root.join("Example.Movie.2024.2160p.mkv"), b"stable")?;
    fs::write(&changed_only, b"before")?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-UTF-8 media root")?)
        .await?;

    let jobs = ScanJobService::new(database.clone());
    let first = jobs.create_movie_scan_job(library.id).await?;
    jobs.run_to_completion(&first.id, 100, None).await?;
    fs::write(&changed, b"after-with-a-new-size")?;
    fs::write(&changed_only, b"after-with-a-new-size")?;
    fs::write(root.join("Example.Movie.2024.720p.mkv"), b"new version")?;

    let second = jobs.create_movie_scan_job(library.id).await?;
    loop {
        if jobs.run_batch(&second.id, 100).await?.completed {
            break;
        }
    }
    jobs.materialize_manifest_postprocessing_targets(&second.id)
        .await?;
    let source_kinds: Vec<String> = sqlx::query_scalar(
        "SELECT change_kind FROM scan_job_targets
         WHERE job_id = $1 AND target_type = 'SOURCE' ORDER BY change_kind",
    )
    .bind(&second.id)
    .fetch_all(database.pool())
    .await?;
    assert_eq!(source_kinds, vec!["CHANGED", "CHANGED", "NEW"]);
    let item_kinds: Vec<String> = sqlx::query_scalar(
        "SELECT change_kind FROM scan_job_targets
         WHERE job_id = $1 AND target_type = 'ITEM' ORDER BY change_kind",
    )
    .bind(&second.id)
    .fetch_all(database.pool())
    .await?;
    assert_eq!(item_kinds, vec!["CHANGED", "NEW"]);

    database.close().await;
    drop_postgres_test_database(&database_name).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_scan_targets_drop_job_fk_and_library_delete_cleans_targets()
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
            &format!("PostgreSQL target FK {}", Uuid::now_v7()),
            luxd::library::LibraryKind::Movie,
            false,
        )
        .await?;
    let job_id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation)
         VALUES ($1, $2, 'RECONCILE_LIBRARY', 'COMPLETED', $3)",
    )
    .bind(&job_id)
    .bind(library.id.to_string())
    .bind(Uuid::now_v7().to_string())
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO scan_job_targets (
             job_id, target_type, target_id, item_id, change_kind
         ) VALUES ($1, 'ITEM', 'target-item', 'target-item', 'NEW')",
    )
    .bind(&job_id)
    .execute(database.pool())
    .await?;

    let job_foreign_keys: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM pg_constraint constraint_row
         JOIN pg_class table_row ON table_row.oid = constraint_row.conrelid
         JOIN pg_attribute column_row
           ON column_row.attrelid = table_row.oid
          AND column_row.attnum = ANY(constraint_row.conkey)
         WHERE table_row.relname = 'scan_job_targets'
           AND constraint_row.contype = 'f'
           AND column_row.attname = 'job_id'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(job_foreign_keys, 0);

    libraries.delete_library(library.id).await?;
    let remaining_targets: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM scan_job_targets WHERE job_id = $1")
            .bind(job_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(remaining_targets, 0);

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
async fn postgres_resume_visibility_preserves_progress_and_recent_order()
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

    let app = luxd::api::app_with_state(luxd::api::AppState::ready(
        config,
        database.clone(),
        SetupService::new(database.clone())?,
        WebAuthService::new(database.clone())?,
        luxd::auth::emby::EmbyAuthService::new(database.clone())?,
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");
    let login: serde_json::Value = client
        .post(format!("http://{address}/Users/AuthenticateByName"))
        .header(
            reqwest::header::AUTHORIZATION,
            r#"Emby Client="ResumeTest", Device="Mac", DeviceId="postgres-resume", Version="1""#,
        )
        .json(&json!({"Username": "postgres-admin", "Pw": "test-password"}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let token = login["AccessToken"].as_str().ok_or("missing token")?;
    let item_id = Uuid::parse_str(&fresh_item_id)?.as_u128().to_string();
    for hidden in [true, true, false, false] {
        let response = client
            .post(format!(
                "http://{address}/emby/Users/{user_id}/Items/{item_id}/HideFromResume"
            ))
            .header("X-Emby-Token", token)
            .json(&json!({"Hide": hidden}))
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = response.json().await?;
        assert_eq!(body["HideFromResume"], hidden);
        assert_eq!(body["PlaybackPositionTicks"], 6_000_000_000_i64);
        assert_eq!(body["Played"], false);
        let page = catalog
            .list_continue_watching(AccessPrincipal::new(admin.id, true), &user_id, 0, 10)
            .await?;
        assert_eq!(page.total, if hidden { 10 } else { 11 });
        assert_eq!(
            page.items.iter().any(|item| item.id == fresh_item_id),
            !hidden
        );
        let empty = catalog
            .list_continue_watching(AccessPrincipal::new(admin.id, true), &user_id, 100, 10)
            .await?;
        assert!(empty.items.is_empty());
        assert_eq!(empty.total, page.total);
    }
    let state: (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT position_ticks, last_played_at, hide_from_resume, version
         FROM user_item_state WHERE user_id = $1 AND item_id = $2",
    )
    .bind(&user_id)
    .bind(&fresh_item_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(state, (6_000_000_000, 100, 0, 2));
    server.abort();
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
    let media_search_foreign_key_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pg_constraint
         WHERE conrelid = 'media_search'::regclass AND contype = 'f'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(media_search_foreign_key_count, 0);
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
    let queued_search_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_search WHERE item_id IN ($1, $2)")
            .bind(&item_one)
            .bind(&item_two)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(queued_search_count, 0, "insert only queues a reindex event");
    database.drain_search_reindex(100).await?;
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
    database.drain_search_reindex(100).await?;
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
    database.drain_search_reindex(100).await?;
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
    database.drain_search_reindex(100).await?;
    let cleared_alias: String =
        sqlx::query_scalar("SELECT aliases FROM media_search WHERE item_id = $1")
            .bind(&item_one)
            .fetch_one(database.pool())
            .await?;
    assert!(cleared_alias.is_empty());

    sqlx::query("DELETE FROM media_items WHERE id = $1")
        .bind(&item_two)
        .execute(database.pool())
        .await?;
    database.drain_search_reindex(100).await?;
    let deleted_item_search_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_search WHERE item_id = $1")
            .bind(&item_two)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(deleted_item_search_rows, 0);

    database.close().await;
    drop_postgres_test_database(&database_name).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_unixepoch_uses_statement_time_and_is_parallel_safe()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (connection, database_name) = create_postgres_test_database().await?;
    let database = Database::connect_with_configuration(&config, &connection).await?;

    let function_properties: (String, String) = sqlx::query_as(
        "SELECT p.provolatile::text, p.proparallel::text
         FROM pg_proc p
         WHERE p.oid = 'unixepoch()'::regprocedure",
    )
    .fetch_one(database.pool())
    .await?;

    let mut transaction = database.pool().begin().await?;
    let first: i64 = sqlx::query_scalar("SELECT unixepoch()")
        .fetch_one(&mut *transaction)
        .await?;
    sqlx::query("SELECT pg_sleep(1.2)")
        .execute(&mut *transaction)
        .await?;
    let second: i64 = sqlx::query_scalar("SELECT unixepoch()")
        .fetch_one(&mut *transaction)
        .await?;
    let advances_between_statements = second > first;
    let matches_statement_time: bool = sqlx::query_scalar(
        "SELECT unixepoch() = floor(extract(epoch FROM statement_timestamp()))::BIGINT",
    )
    .fetch_one(&mut *transaction)
    .await?;
    transaction.rollback().await?;

    database.close().await;
    drop_postgres_test_database(&database_name).await?;
    assert_eq!(function_properties, ("s".to_owned(), "s".to_owned()));
    assert!(
        advances_between_statements,
        "unixepoch should advance between statements in one transaction"
    );
    assert!(matches_statement_time);
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_homevideos_video_type_migration_preserves_existing_data()
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
    let old_migrations = temp_dir.path().join("migrations-v149");
    fs::create_dir(&old_migrations)?;
    for entry in fs::read_dir(source_migrations)? {
        let entry = entry?;
        let name = entry.file_name();
        let migration_name = name.to_str().ok_or("migration name is not UTF-8")?;
        let version = migration_name
            .split_once('_')
            .map(|(version, _)| version.parse::<i64>())
            .transpose()?
            .ok_or("migration name has no version")?;
        if version <= 149 {
            fs::copy(entry.path(), old_migrations.join(name))?;
        }
    }

    let migration_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await?;
    sqlx::migrate::Migrator::new(old_migrations.as_path())
        .await?
        .run(&migration_pool)
        .await?;
    let library_id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO libraries (id, name, kind, scraper_id)
         VALUES ($1, 'Existing Mixed', 'MIXED', 'tmdb')",
    )
    .bind(&library_id)
    .execute(&migration_pool)
    .await?;
    sqlx::query(
        "INSERT INTO library_scrapers (library_id, scraper_id, position, role)
         VALUES ($1, 'tmdb', 0, 'PRIMARY')",
    )
    .bind(&library_id)
    .execute(&migration_pool)
    .await?;
    sqlx::query(
        "INSERT INTO library_roots (
             id, library_id, canonical_path, display_path, is_available, is_writable
         ) VALUES ($1, $2, '/media', '/media', 1, 1)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(&library_id)
    .execute(&migration_pool)
    .await?;
    let old_item_id = Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES ($1, $2, 'MOVIE', 'Existing Movie', 'existing movie', 'LOCAL_CONFIRMED')",
    )
    .bind(&old_item_id)
    .bind(&library_id)
    .execute(&migration_pool)
    .await?;
    migration_pool.close().await;

    let database = Database::connect_with_configuration(&config, &connection).await?;
    assert!(database.schema_version().await? >= 190);
    let existing_library_kind: String =
        sqlx::query_scalar("SELECT kind FROM libraries WHERE id = $1")
            .bind(&library_id)
            .fetch_one(database.pool())
            .await?;
    let existing_item_type: String =
        sqlx::query_scalar("SELECT item_type FROM media_items WHERE id = $1")
            .bind(&old_item_id)
            .fetch_one(database.pool())
            .await?;
    let existing_scrapers: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM library_scrapers WHERE library_id = $1 AND scraper_id = 'tmdb'",
    )
    .bind(&library_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(existing_library_kind, "MIXED");
    assert_eq!(existing_item_type, "MOVIE");
    assert_eq!(existing_scrapers, 1);
    insert_postgres_homevideos_video(&database).await?;

    database.close().await;
    drop_postgres_test_database(&database_name).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_progressive_scan_metadata_migration_preserves_policy_and_queue_contract()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let (connection, database_name) = create_postgres_test_database().await?;
    let database_url = connection.postgres_url()?.ok_or("missing PostgreSQL URL")?;
    let source_migrations =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("migrations-postgres");
    let old_migrations = temp_dir.path().join("migrations-v150");
    fs::create_dir(&old_migrations)?;
    for entry in fs::read_dir(&source_migrations)? {
        let entry = entry?;
        let name = entry.file_name();
        let migration_name = name.to_str().ok_or("migration name is not UTF-8")?;
        let version = migration_name
            .split_once('_')
            .map(|(version, _)| version.parse::<i64>())
            .transpose()?
            .ok_or("migration name has no version")?;
        if version <= 150 {
            fs::copy(entry.path(), old_migrations.join(name))?;
        }
    }

    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await?;
    sqlx::migrate::Migrator::new(old_migrations.as_path())
        .await?
        .run(&pool)
        .await?;
    for (id, enabled) in [("progressive-off", 0_i64), ("progressive-on", 1_i64)] {
        sqlx::query(
            "INSERT INTO libraries (
                 id, name, kind, realtime_metadata_auto_match_enabled
             ) VALUES ($1, $2, 'MOVIE', $3)",
        )
        .bind(id)
        .bind(id)
        .bind(enabled)
        .execute(&pool)
        .await?;
    }
    sqlx::query(
        "INSERT INTO library_roots (
             id, library_id, canonical_path, display_path, is_available, is_writable
         ) VALUES ('progressive-root', 'progressive-on', '/media', '/media', 1, 1)",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES (
             'progressive-item', 'progressive-on', 'MOVIE', 'Progressive', 'progressive',
             'LOCAL_CONFIRMED'
         )",
    )
    .execute(&pool)
    .await?;
    pool.close().await;

    fs::copy(
        source_migrations.join("0151_progressive_scan_metadata.sql"),
        old_migrations.join("0151_progressive_scan_metadata.sql"),
    )?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await?;
    sqlx::migrate::Migrator::new(old_migrations.as_path())
        .await?
        .run(&pool)
        .await?;

    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation)
         VALUES ('workflow-two-job', 'progressive-on', 'RECONCILE_LIBRARY', 'COMPLETED', 'g2')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_manifests (
             id, job_id, library_id, state, workflow_version, resume_state
         ) VALUES (
             'workflow-two-manifest', 'workflow-two-job', 'progressive-on', 'FAILED', 2,
             'DISCOVERING'
         )",
    )
    .execute(&pool)
    .await?;

    fs::copy(
        source_migrations.join("0152_scan_manifest_workflow_three.sql"),
        old_migrations.join("0152_scan_manifest_workflow_three.sql"),
    )?;
    sqlx::migrate::Migrator::new(old_migrations.as_path())
        .await?
        .run(&pool)
        .await?;
    sqlx::query(
        "INSERT INTO scan_local_metadata_batches (
             id, job_id, library_root_id, batch_sequence, source_refs_json, source_count
         ) VALUES (
             'pre-image-stage-batch', 'cleaned-scan-job', 'progressive-root', 10,
             '[\"source-old\"]', 1
         )",
    )
    .execute(&pool)
    .await?;
    fs::copy(
        source_migrations.join("0153_scan_local_metadata_image_stage.sql"),
        old_migrations.join("0153_scan_local_metadata_image_stage.sql"),
    )?;
    sqlx::migrate::Migrator::new(old_migrations.as_path())
        .await?
        .run(&pool)
        .await?;
    let legacy_manifest: (i32, Option<String>) = sqlx::query_as(
        "SELECT workflow_version, resume_state FROM scan_manifests
         WHERE id = 'workflow-two-manifest'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(legacy_manifest, (2, Some("DISCOVERING".to_owned())));
    let old_batch_image_stage: Option<i64> = sqlx::query_scalar(
        "SELECT images_completed_at FROM scan_local_metadata_batches
         WHERE id = 'pre-image-stage-batch'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(old_batch_image_stage, None);
    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation)
         VALUES ('workflow-three-job', 'progressive-on', 'RECONCILE_LIBRARY', 'PENDING', 'g3')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO scan_manifests (id, job_id, library_id, state, workflow_version)
         VALUES ('workflow-three-manifest', 'workflow-three-job', 'progressive-on', 'DISCOVERING', 3)",
    )
    .execute(&pool)
    .await?;

    let existing_policy: Vec<(String, i64)> = sqlx::query_as(
        "SELECT id, scan_missing_metadata_auto_match_enabled
         FROM libraries WHERE id IN ('progressive-off', 'progressive-on') ORDER BY id",
    )
    .fetch_all(&pool)
    .await?;
    assert_eq!(
        existing_policy,
        vec![
            ("progressive-off".to_owned(), 0),
            ("progressive-on".to_owned(), 1)
        ]
    );
    sqlx::query(
        "INSERT INTO libraries (id, name, kind) VALUES ('progressive-new', 'New', 'MOVIE')",
    )
    .execute(&pool)
    .await?;
    let new_policy: i64 = sqlx::query_scalar(
        "SELECT scan_missing_metadata_auto_match_enabled
         FROM libraries WHERE id = 'progressive-new'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(new_policy, 1);

    sqlx::query(
        "INSERT INTO scan_local_metadata_batches (
             id, job_id, library_root_id, batch_sequence, source_refs_json, source_count
         ) VALUES ('progressive-batch', 'cleaned-scan-job', 'progressive-root', 0, '[\"source-1\"]', 1)",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO item_metadata_completeness (
             item_id, capability, local_state, is_missing
         ) VALUES ('progressive-item', 'POSTER', 'READY', 1)",
    )
    .execute(&pool)
    .await?;
    assert!(
        sqlx::query(
            "INSERT INTO item_metadata_completeness (
                 item_id, capability, local_state, is_missing
             ) VALUES ('progressive-item', 'POSTER', 'READY', 0)",
        )
        .execute(&pool)
        .await
        .is_err()
    );
    assert!(
        sqlx::query(
            "INSERT INTO item_metadata_completeness (
                 item_id, capability, local_state, is_missing
             ) VALUES ('progressive-item', 'BACKDROP', 'FAILED', 1)",
        )
        .execute(&pool)
        .await
        .is_err()
    );
    assert!(
        sqlx::query(
            "INSERT INTO scan_local_metadata_batches (
                 id, job_id, library_root_id, batch_sequence, source_refs_json, source_count
             ) VALUES ('progressive-empty', 'cleaned-scan-job', 'progressive-root', 2, '[\"source-1\"]', 0)",
        )
        .execute(&pool)
        .await
        .is_err()
    );
    assert!(
        sqlx::query(
            "INSERT INTO scan_local_metadata_batches (
                 id, job_id, library_root_id, batch_sequence, source_refs_json, source_count
             ) VALUES ('progressive-oversized', 'cleaned-scan-job', 'progressive-root', 1, '[]', 257)",
        )
        .execute(&pool)
        .await
        .is_err()
    );

    sqlx::query("DELETE FROM libraries WHERE id = 'progressive-on'")
        .execute(&pool)
        .await?;
    let cascaded_batch_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM scan_local_metadata_batches WHERE id = 'progressive-batch'",
    )
    .fetch_one(&pool)
    .await?;
    let cascaded_completeness_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM item_metadata_completeness WHERE item_id = 'progressive-item'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(cascaded_batch_count, 0);
    assert_eq!(cascaded_completeness_count, 0);

    pool.close().await;
    drop_postgres_test_database(&database_name).await?;
    Ok(())
}

async fn insert_postgres_homevideos_video(
    database: &Database,
) -> Result<(), Box<dyn std::error::Error>> {
    let library_id = Uuid::now_v7().to_string();
    let item_id = Uuid::now_v7().to_string();
    sqlx::query("INSERT INTO libraries (id, name, kind) VALUES ($1, 'Home videos', 'HOMEVIDEOS')")
        .bind(&library_id)
        .execute(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status
         ) VALUES ($1, $2, 'VIDEO', 'Clip', 'clip', 'LOCAL_CONFIRMED')",
    )
    .bind(item_id)
    .bind(library_id)
    .execute(database.pool())
    .await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a PostgreSQL test server"]
async fn postgres_resume_visibility_upgrade_preserves_existing_playback_state()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let migration_dir = temp_dir.path().join("old-migrations");
    fs::create_dir(&migration_dir)?;
    let source_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations-postgres");
    for entry in fs::read_dir(&source_dir)? {
        let entry = entry?;
        let filename = entry.file_name();
        let version: i64 = filename
            .to_str()
            .ok_or("invalid filename")?
            .split('_')
            .next()
            .ok_or("missing migration version")?
            .parse()?;
        if version <= 176 {
            fs::copy(entry.path(), migration_dir.join(filename))?;
        }
    }
    let (configuration, database_name) = create_postgres_test_database().await?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(
            &configuration
                .postgres_url()?
                .ok_or("missing PostgreSQL URL")?,
        )
        .await?;
    sqlx::migrate::Migrator::new(migration_dir)
        .await?
        .run(&pool)
        .await?;
    sqlx::query(
        "INSERT INTO users (id, username_normalized, display_name, password_hash)
                 VALUES ('resume-user', 'resume-user', 'Resume User', 'test-only')",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO libraries (id, name, kind) VALUES ('resume-library', 'Resume', 'MOVIE')",
    )
    .execute(&pool)
    .await?;
    sqlx::query("INSERT INTO media_items (id, library_id, item_type, title, sort_title, identification_status)
                 VALUES ('resume-item', 'resume-library', 'MOVIE', 'Resume', 'resume', 'LOCAL_CONFIRMED')")
        .execute(&pool).await?;
    sqlx::query("INSERT INTO user_item_state (user_id, item_id, position_ticks, is_favorite,
                 play_count, last_played_at, version) VALUES ('resume-user', 'resume-item', 1300000000, 1, 2, 12345, 7)")
        .execute(&pool).await?;
    sqlx::migrate::Migrator::new(source_dir)
        .await?
        .run(&pool)
        .await?;
    let row: (i64, i64, i64, i64, i64, i64, i64) = sqlx::query_as(
        "SELECT position_ticks, is_played, is_favorite, play_count, last_played_at,
                version, hide_from_resume FROM user_item_state WHERE item_id = 'resume-item'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(row, (1300000000, 0, 1, 2, 12345, 7, 0));
    assert!(
        sqlx::query("UPDATE user_item_state SET hide_from_resume = 2")
            .execute(&pool)
            .await
            .is_err()
    );
    let version: i64 = sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await?;
    assert_eq!(version, latest_migration_version("migrations-postgres")?);
    pool.close().await;
    drop_postgres_test_database(&database_name).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_user_registration_requests_migration_runs_from_empty_and_upgrades_existing_database()
-> Result<(), Box<dyn std::error::Error>> {
    let source_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations-postgres");

    let (configuration, database_name) = create_postgres_test_database().await?;
    let empty_migration_dir = source_dir.clone();
    let empty_result = tokio::spawn(async move {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(
                &configuration
                    .postgres_url()?
                    .ok_or("missing PostgreSQL URL")?,
            )
            .await?;
        Migrator::new(empty_migration_dir).await?.run(&pool).await?;
        assert_postgres_registration_request_schema(&pool).await?;
        pool.close().await;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await;
    drop_postgres_test_database(&database_name).await?;
    if let Err(error) = empty_result? {
        return Err(error.to_string().into());
    }

    let temp = tempfile::tempdir()?;
    let old_migration_dir = temp.path().join("migrations-through-0202");
    fs::create_dir(&old_migration_dir)?;
    for entry in fs::read_dir(&source_dir)? {
        let entry = entry?;
        let filename = entry.file_name();
        let version: i64 = filename
            .to_str()
            .ok_or("invalid filename")?
            .split('_')
            .next()
            .ok_or("missing migration version")?
            .parse()?;
        if version <= 202 {
            fs::copy(entry.path(), old_migration_dir.join(filename))?;
        }
    }

    let (configuration, database_name) = create_postgres_test_database().await?;
    let upgrade_source_dir = source_dir.clone();
    let upgrade_migration_dir = old_migration_dir.clone();
    let upgrade_result = tokio::spawn(async move {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(
                &configuration
                    .postgres_url()?
                    .ok_or("missing PostgreSQL URL")?,
            )
            .await?;
        Migrator::new(upgrade_migration_dir)
            .await?
            .run(&pool)
            .await?;
        sqlx::query(
            "INSERT INTO users (id, username_normalized, display_name, password_hash)
             VALUES ('preserved-registration-owner', 'preserved-registration-owner', 'Preserved', 'test-hash')",
        )
        .execute(&pool)
        .await?;
        Migrator::new(upgrade_source_dir)
            .await?
            .run(&pool)
            .await?;
        assert_postgres_registration_request_schema(&pool).await?;
        let preserved_name: String = sqlx::query_scalar(
            "SELECT display_name FROM users WHERE id = 'preserved-registration-owner'",
        )
        .fetch_one(&pool)
        .await?;
        assert_eq!(preserved_name, "Preserved");
        pool.close().await;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    })
    .await;
    drop_postgres_test_database(&database_name).await?;
    if let Err(error) = upgrade_result? {
        return Err(error.to_string().into());
    }
    Ok(())
}

async fn assert_postgres_registration_request_schema(
    pool: &sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let columns: Vec<String> = sqlx::query_scalar(
        "SELECT column_name FROM information_schema.columns
         WHERE table_schema = current_schema() AND table_name = 'user_registration_requests'
         ORDER BY ordinal_position",
    )
    .fetch_all(pool)
    .await?;
    assert_eq!(columns.len(), 15);
    for column in [
        "id",
        "recipient_email",
        "username_normalized",
        "display_name",
        "password_hash",
        "permission_snapshot_json",
        "status",
        "expires_at",
        "email_verified_at",
        "reviewed_at",
        "reviewed_by_user_id",
        "approved_user_id",
        "last_error_code",
        "created_at",
        "updated_at",
    ] {
        assert!(
            columns.iter().any(|existing| existing == column),
            "missing {column}"
        );
    }
    let index_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pg_indexes WHERE schemaname = current_schema()
         AND indexname IN (
             'ux_user_registration_requests_pending_email',
             'idx_user_registration_requests_status_created',
             'idx_user_registration_requests_expiry'
         )",
    )
    .fetch_one(pool)
    .await?;
    assert_eq!(index_count, 3);

    sqlx::query(
        "INSERT INTO users (id, username_normalized, display_name, password_hash)
         VALUES ('registration-reviewer', 'registration-reviewer', 'Reviewer', 'test-hash')",
    )
    .execute(pool)
    .await?;
    let insert = "INSERT INTO user_registration_requests (
        id, recipient_email, username_normalized, display_name, password_hash,
        permission_snapshot_json, expires_at, created_at, updated_at
    ) VALUES ($1, 'registration@example.test', 'registration-user', 'Registration User',
        '$argon2id$test-hash', '{}', 20, 10, 10)";
    sqlx::query(insert)
        .bind("registration-main")
        .execute(pool)
        .await?;
    assert!(
        sqlx::query(insert)
            .bind("registration-duplicate")
            .execute(pool)
            .await
            .is_err()
    );
    for (query, value) in [
        (
            "UPDATE user_registration_requests SET recipient_email = $1 WHERE id = 'registration-main'",
            "UPPER@example.test",
        ),
        (
            "UPDATE user_registration_requests SET password_hash = $1 WHERE id = 'registration-main'",
            "plaintext",
        ),
        (
            "UPDATE user_registration_requests SET password_hash = $1 WHERE id = 'registration-main'",
            "$ARGON2ID$test-hash",
        ),
        (
            "UPDATE user_registration_requests SET permission_snapshot_json = $1 WHERE id = 'registration-main'",
            "[]",
        ),
        (
            "UPDATE user_registration_requests SET permission_snapshot_json = $1 WHERE id = 'registration-main'",
            "not-json",
        ),
        (
            "UPDATE user_registration_requests SET permission_snapshot_json = $1 WHERE id = 'registration-main'",
            &format!(r#"{{"x":"{}"}}"#, "界".repeat(12_000)),
        ),
    ] {
        assert!(
            sqlx::query(query).bind(value).execute(pool).await.is_err(),
            "accepted invalid update: {query}"
        );
    }
    for query in [
        "UPDATE user_registration_requests SET id = repeat('x', 129) WHERE id = 'registration-main'",
        "UPDATE user_registration_requests SET recipient_email = repeat('x', 255) WHERE id = 'registration-main'",
        "UPDATE user_registration_requests SET recipient_email = ' registration@example.test' WHERE id = 'registration-main'",
        "UPDATE user_registration_requests SET username_normalized = repeat('x', 129) WHERE id = 'registration-main'",
        "UPDATE user_registration_requests SET display_name = repeat('x', 129) WHERE id = 'registration-main'",
        "UPDATE user_registration_requests SET status = 'UNKNOWN' WHERE id = 'registration-main'",
        "UPDATE user_registration_requests SET status = 'PENDING_APPROVAL' WHERE id = 'registration-main'",
        "UPDATE user_registration_requests SET status = 'REJECTED' WHERE id = 'registration-main'",
        "UPDATE user_registration_requests SET password_hash = NULL WHERE id = 'registration-main'",
        "UPDATE user_registration_requests SET expires_at = 10 WHERE id = 'registration-main'",
        "UPDATE user_registration_requests SET updated_at = 9 WHERE id = 'registration-main'",
        "UPDATE user_registration_requests SET email_verified_at = 9 WHERE id = 'registration-main'",
        "UPDATE user_registration_requests SET reviewed_by_user_id = 'unknown' WHERE id = 'registration-main'",
        "UPDATE user_registration_requests SET last_error_code = 'private error' WHERE id = 'registration-main'",
    ] {
        assert!(
            sqlx::query(query).execute(pool).await.is_err(),
            "accepted invalid transition: {query}"
        );
    }
    sqlx::query(
        "UPDATE user_registration_requests
         SET status = 'PENDING_APPROVAL', email_verified_at = 11, updated_at = 11
         WHERE id = 'registration-main'",
    )
    .execute(pool)
    .await?;
    assert!(
        sqlx::query(insert)
            .bind("registration-approval-duplicate")
            .execute(pool)
            .await
            .is_err()
    );
    sqlx::query(
        "UPDATE user_registration_requests
         SET status = 'REJECTED', reviewed_at = 12, updated_at = 12,
             reviewed_by_user_id = 'registration-reviewer', password_hash = NULL
         WHERE id = 'registration-main'",
    )
    .execute(pool)
    .await?;
    sqlx::query(insert)
        .bind("registration-next")
        .execute(pool)
        .await?;
    sqlx::query(
        "UPDATE user_registration_requests
         SET status = 'APPROVED', email_verified_at = 11, reviewed_at = 12, updated_at = 12,
             reviewed_by_user_id = 'registration-reviewer', approved_user_id = 'registration-reviewer',
             password_hash = NULL
         WHERE id = 'registration-next'",
    )
    .execute(pool)
    .await?;
    assert!(
        sqlx::query(
            "UPDATE user_registration_requests SET approved_user_id = 'unknown'
             WHERE id = 'registration-next'",
        )
        .execute(pool)
        .await
        .is_err()
    );
    sqlx::query("DELETE FROM users WHERE id = 'registration-reviewer'")
        .execute(pool)
        .await?;
    let cleared_user_refs: (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT reviewed_by_user_id, approved_user_id
         FROM user_registration_requests WHERE id = 'registration-next'",
    )
    .fetch_one(pool)
    .await?;
    assert_eq!(cleared_user_refs, (None, None));
    sqlx::query(insert)
        .bind("registration-expired")
        .execute(pool)
        .await?;
    sqlx::query(
        "UPDATE user_registration_requests
         SET status = 'EXPIRED', updated_at = 21, password_hash = NULL
         WHERE id = 'registration-expired'",
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_removes_personal_image_preference_without_changing_other_preferences()
-> Result<(), Box<dyn std::error::Error>> {
    let (connection, database_name) = create_postgres_test_database().await?;
    let url = connection.postgres_url()?.ok_or("missing PostgreSQL URL")?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await?;
    let migrator = sqlx::migrate!("./migrations-postgres");
    let old = Migrator::with_migrations(
        migrator
            .iter()
            .filter(|m| m.version <= 206)
            .cloned()
            .collect(),
    );
    old.run(&pool).await?;
    sqlx::raw_sql(
        "INSERT INTO users (id, username_normalized, display_name, password_hash)
        VALUES ('image-user', 'image-user', 'Images', 'test');
        INSERT INTO user_account_preferences (user_id, theme, accent_color, auto_play_next_episode,
            optimized_images, version, updated_at)
        VALUES ('image-user', 'light', 'ocean', 0, 1, 7, 1234);",
    )
    .execute(&pool)
    .await?;
    migrator.run(&pool).await?;
    let columns: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'user_account_preferences'
        AND column_name = 'optimized_images'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(columns, 0);
    let values: (String, String, i64, i64, i64) = sqlx::query_as(
        "SELECT theme, accent_color, auto_play_next_episode, version, updated_at
         FROM user_account_preferences WHERE user_id = 'image-user'",
    )
    .fetch_one(&pool)
    .await?;
    assert_eq!(values, ("light".into(), "ocean".into(), 0, 7, 1234));
    pool.close().await;
    drop_postgres_test_database(&database_name).await?;

    let (connection, database_name) = create_postgres_test_database().await?;
    let temp = tempfile::tempdir()?;
    let database = Database::connect_with_configuration(
        &Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp.path().join("config"),
        },
        &connection,
    )
    .await?;
    let columns: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'user_account_preferences'
        AND column_name = 'optimized_images'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(columns, 0);
    database.pool().close().await;
    drop_postgres_test_database(&database_name).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_search_outbox_gap_repair_is_bounded_and_idempotent()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (connection, database_name) = create_postgres_test_database().await?;
    let database = Database::connect_with_configuration(&config, &connection).await?;

    async fn rows(database: &Database, table: &str) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) FROM {table}")))
            .fetch_one(database.pool())
            .await
    }

    sqlx::query("INSERT INTO libraries (id, name, kind) VALUES ('gap-library', 'Gap', 'MOVIE')")
        .execute(database.pool())
        .await?;
    for item_id in ["gap-a", "gap-b", "gap-c"] {
        sqlx::query(
            "INSERT INTO media_items
                (id, library_id, item_type, title, sort_title, identification_status)
             VALUES ($1, 'gap-library', 'MOVIE', $1, $1, 'LOCAL_CONFIRMED')",
        )
        .bind(item_id)
        .execute(database.pool())
        .await?;
    }
    // The catalog stream and the search stream are separate tables with separate sequences.
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM catalog_outbox WHERE kind = 'search'")
            .fetch_one(database.pool())
            .await?,
        0
    );
    while database.drain_search_reindex(100).await? > 0 {}

    // Rewind the watermark and drop all but the newest event: a hole below the oldest remaining row.
    sqlx::query("UPDATE catalog_consumers SET applied_seq = 0 WHERE name = 'search_reindex@2'")
        .execute(database.pool())
        .await?;
    sqlx::query("DELETE FROM search_outbox WHERE id < (SELECT MAX(id) FROM search_outbox)")
        .execute(database.pool())
        .await?;

    let before = rows(&database, "search_outbox").await?;
    database.drain_search_reindex(100).await?;
    let after_first = rows(&database, "search_outbox").await?;
    assert_eq!(after_first - before, 3, "exactly one full re-queue");
    for _ in 0..3 {
        database.drain_search_reindex(100).await?;
    }
    assert_eq!(
        rows(&database, "search_outbox").await?,
        after_first,
        "later drains must not repeat the re-queue"
    );

    // A second hole inside the cooldown is closed without another full re-queue.
    sqlx::query("UPDATE catalog_consumers SET applied_seq = 0 WHERE name = 'search_reindex@2'")
        .execute(database.pool())
        .await?;
    sqlx::query("DELETE FROM search_outbox WHERE id < (SELECT MAX(id) FROM search_outbox)")
        .execute(database.pool())
        .await?;
    let before_cooldown = rows(&database, "search_outbox").await?;
    database.drain_search_reindex(100).await?;
    assert_eq!(rows(&database, "search_outbox").await?, before_cooldown);

    drop(database);
    drop_postgres_test_database(&database_name).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_media_item_update_triggers_are_column_specific()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (connection, database_name) = create_postgres_test_database().await?;
    let database = Database::connect_with_configuration(&config, &connection).await?;

    async fn search_enqueue_count(database: &Database) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM search_outbox
             WHERE kind = 'search' AND item_id = 'trigger-item'",
        )
        .fetch_one(database.pool())
        .await
    }

    sqlx::query(
        "INSERT INTO libraries (id, name, kind) VALUES ('trigger-library', 'Trigger', 'MOVIE')",
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_items
            (id, library_id, item_type, title, sort_title, identification_status, provider_ids_json)
         VALUES ('trigger-item', 'trigger-library', 'MOVIE', 'Before', 'before',
                 'LOCAL_CONFIRMED', '{\"tmdb\":\"1\"}')",
    )
    .execute(database.pool())
    .await?;

    let triggers: Vec<(String, String)> = sqlx::query_as(
        "SELECT tgname::text, pg_get_triggerdef(oid)
         FROM pg_trigger
         WHERE tgrelid = 'media_items'::regclass
           AND tgname IN ('media_items_search_au', 'media_item_provider_ids_au')
         ORDER BY tgname",
    )
    .fetch_all(database.pool())
    .await?;
    let search_trigger = triggers
        .iter()
        .find(|(name, _)| name == "media_items_search_au")
        .map(|(_, definition)| definition)
        .ok_or("missing media_items_search_au")?;
    let provider_trigger = triggers
        .iter()
        .find(|(name, _)| name == "media_item_provider_ids_au")
        .map(|(_, definition)| definition)
        .ok_or("missing media_item_provider_ids_au")?;
    // The search refresh is statement-level now: the column filter moved into the enqueue function.
    assert!(search_trigger.contains("FOR EACH STATEMENT"));
    assert!(!search_trigger.contains("UPDATE OF title"));
    let search_function: String = sqlx::query_scalar(
        "SELECT pg_get_functiondef(p.oid) FROM pg_proc p
         WHERE p.proname = 'lux_enqueue_search_reindex_items_update_stmt'",
    )
    .fetch_one(database.pool())
    .await?;
    assert!(search_function.contains("n.title IS DISTINCT FROM o.title"));
    assert!(provider_trigger.contains("FOR EACH ROW"));
    assert!(provider_trigger.contains("UPDATE OF item_type, provider_ids_json"));

    let queued_before = search_enqueue_count(&database).await?;
    sqlx::query("UPDATE media_items SET overview = 'not indexed' WHERE id = 'trigger-item'")
        .execute(database.pool())
        .await?;
    assert_eq!(
        search_enqueue_count(&database).await?,
        queued_before,
        "a column outside the search projection must not enqueue"
    );

    sqlx::query("UPDATE media_items SET title = 'After' WHERE id = 'trigger-item'")
        .execute(database.pool())
        .await?;
    assert!(
        search_enqueue_count(&database).await? > queued_before,
        "a title update must enqueue"
    );
    database.drain_search_reindex(100).await?;
    let search_title: String =
        sqlx::query_scalar("SELECT title FROM media_search WHERE item_id = 'trigger-item'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(search_title, "After");

    sqlx::query(
        "UPDATE media_items SET provider_ids_json = '{\"tmdb\":\"2\"}'
         WHERE id = 'trigger-item'",
    )
    .execute(database.pool())
    .await?;
    let provider_ids: Vec<String> = sqlx::query_scalar(
        "SELECT provider_id FROM media_item_provider_ids
         WHERE media_item_id = 'trigger-item' ORDER BY provider_id",
    )
    .fetch_all(database.pool())
    .await?;
    assert_eq!(provider_ids, ["2"]);

    drop(database);
    drop_postgres_test_database(&database_name).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_home_index_and_statement_timestamp_defaults_exist()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (connection, database_name) = create_postgres_test_database().await?;
    let database = Database::connect_with_configuration(&config, &connection).await?;

    let index_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM pg_indexes
         WHERE schemaname = current_schema()
           AND indexname = 'idx_media_items_home_unavailable_series'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(index_count, 1, "the home query needs its partial index");

    let default_expression: String = sqlx::query_scalar(
        "SELECT column_default FROM information_schema.columns
         WHERE table_schema = current_schema()
           AND table_name = 'user_access_blacklist_rules'
           AND column_name = 'created_at'",
    )
    .fetch_one(database.pool())
    .await?;
    assert!(
        default_expression.contains("unixepoch"),
        "statement-timestamp defaults must use unixepoch(), found {default_expression}"
    );

    database.close().await;
    drop_postgres_test_database(&database_name).await?;
    Ok(())
}

/// The pooled connection setup applies several `SET` statements at once. PostgreSQL
/// rejects a multi-statement string on the extended query protocol with
/// `cannot insert multiple commands into a prepared statement`, which makes every
/// `after_connect` fail and the pool time out before startup. This regression keeps the
/// connection setup on the simple query protocol and asserts the settings actually land.
#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_pool_after_connect_applies_session_settings()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (connection, database_name) = create_postgres_test_database().await?;

    let result = async {
        let database = Database::connect_with_configuration(&config, &connection).await?;
        let (time_zone, application_name, work_mem, jit): (String, String, String, String) =
            sqlx::query_as(
                "SELECT current_setting('TimeZone'), current_setting('application_name'),
                        current_setting('work_mem'), current_setting('jit')",
            )
            .fetch_one(database.pool())
            .await?;
        assert_eq!(time_zone, "UTC");
        assert_eq!(application_name, "lux");
        assert_eq!(work_mem, "16MB");
        assert_eq!(jit, "off");

        let idle_in_transaction_timeout: String =
            sqlx::query_scalar("SELECT current_setting('idle_in_transaction_session_timeout')")
                .fetch_one(database.pool())
                .await?;
        assert_eq!(idle_in_transaction_timeout, "1min");

        let statement_timeout: String =
            sqlx::query_scalar("SELECT current_setting('statement_timeout')")
                .fetch_one(database.pool())
                .await?;
        assert_eq!(statement_timeout, "30min");

        database.close().await;
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;

    drop_postgres_test_database(&database_name).await?;
    result
}
