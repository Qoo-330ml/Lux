use std::env;

use luxd::{
    application::{
        access::{AccessPrincipal, MediaAccessService},
        catalog::CatalogService,
    },
    auth::{password::PasswordService, users::UserStore},
    config::{Config, PostgresConnection},
    storage::{
        Database,
        migration::{MigrationOptions, migrate_sqlite_to_postgres},
    },
};
use sqlx::postgres::PgPoolOptions;

const USER_ID: &str = "00000000-0000-0000-0000-000000000001";

#[tokio::test]
#[ignore = "requires a disposable empty PostgreSQL database"]
async fn sqlite_data_migrates_to_empty_postgres_and_rebuilds_search()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:0".parse()?,
        config_dir: temp.path().join("config"),
    };
    let source = Database::connect(&config).await?;
    seed_source(&source).await?;
    source.close().await;
    let source_path = config.config_dir.join("lux.db");
    let source_bytes_before = std::fs::read(&source_path)?;

    let connection = test_connection()?;
    reset_public_schema(&connection).await?;
    let options = MigrationOptions::new(source_path.clone(), 2)?;
    let report = migrate_sqlite_to_postgres(&options, &connection).await?;
    assert!(
        report
            .tables
            .iter()
            .any(|table| table.table == "media_items" && table.rows == 1)
    );
    assert_eq!(std::fs::read(&source_path)?, source_bytes_before);

    let target_configuration = luxd::config::DatabaseConfiguration::Postgres(connection.clone());
    let url = target_configuration
        .postgres_url()?
        .ok_or("missing PostgreSQL URL")?;
    let pool = PgPoolOptions::new().connect(&url).await?;
    let display_name: String = sqlx::query_scalar("SELECT display_name FROM users WHERE id = $1")
        .bind(USER_ID)
        .fetch_one(&pool)
        .await?;
    assert_eq!(display_name, "Migration Admin");
    let token_hash: Vec<u8> =
        sqlx::query_scalar("SELECT session_token_hash FROM web_sessions WHERE id = $1")
            .bind("session-1")
            .fetch_one(&pool)
            .await?;
    assert_eq!(token_hash, vec![0, 1, 2, 255]);
    let search_aliases: String =
        sqlx::query_scalar("SELECT aliases FROM media_search WHERE item_id = $1")
            .bind("item-1")
            .fetch_one(&pool)
            .await?;
    assert_eq!(search_aliases, "迁移别名");
    let job_status: String = sqlx::query_scalar("SELECT status FROM scan_jobs WHERE id = $1")
        .bind("scan-1")
        .fetch_one(&pool)
        .await?;
    assert_eq!(job_status, "PENDING");
    pool.close().await;

    let target = Database::connect_with_configuration(&config, &target_configuration).await?;
    let authenticated = UserStore::new(target.clone())?
        .authenticate("migration-admin", "migration-password")
        .await?
        .ok_or("migrated administrator login failed")?;
    let page = CatalogService::new(target.clone(), MediaAccessService::new(target.clone()))
        .search_items(
            AccessPrincipal::new(authenticated.id, authenticated.is_admin),
            "迁移别名",
            "%迁移别名%",
            0,
            10,
        )
        .await?;
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].id, "item-1");
    target.close().await;

    let second = migrate_sqlite_to_postgres(&options, &connection).await;
    assert!(
        second
            .unwrap_err()
            .to_string()
            .contains("target table is not empty")
    );
    reset_public_schema(&connection).await?;
    Ok(())
}

async fn seed_source(database: &Database) -> Result<(), Box<dyn std::error::Error>> {
    let password_hash = PasswordService::new()?.hash_password("migration-password")?;
    sqlx::query(
        "INSERT INTO users (id, username_normalized, display_name, password_hash, is_admin,
          can_manage_server, can_remote_access, can_download)
         VALUES (?, 'migration-admin', 'Migration Admin', ?, 1, 1, 1, 1)",
    )
    .bind(USER_ID)
    .bind(password_hash)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO web_sessions (id, user_id, session_token_hash, csrf_token_hash, expires_at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind("session-1")
    .bind(USER_ID)
    .bind(vec![0_u8, 1, 2, 255])
    .bind(vec![5_u8, 6])
    .bind(4_000_000_000_i64)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO libraries (id, name, kind, is_enabled, realtime_watch_enabled,
          scan_concurrency, probe_concurrency) VALUES (?, 'Movies', 'MOVIE', 1, 1, 2, 1)",
    )
    .bind("library-1")
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO library_roots (id, library_id, canonical_path, display_path,
          is_available, is_writable) VALUES (?, ?, '/media', '/media', 1, 1)",
    )
    .bind("root-1")
    .bind("library-1")
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO media_items (id, library_id, item_type, title, sort_title,
         identification_status, metadata_fingerprint, has_available_source)
         VALUES (?, ?, 'MOVIE', 'Migration Movie', 'migration movie', 'LOCAL_CONFIRMED', ?, 1)",
    )
    .bind("item-1")
    .bind("library-1")
    .bind(vec![9_u8, 8, 7])
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO item_aliases (id, item_id, alias, language, alias_normalized)
         VALUES (?, ?, '迁移别名', 'zh-CN', '迁移别名')",
    )
    .bind("alias-1")
    .bind("item-1")
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO scan_jobs (id, library_id, job_type, status, generation)
         VALUES (?, ?, 'RECONCILE_LIBRARY', 'RUNNING', 'generation-1')",
    )
    .bind("scan-1")
    .bind("library-1")
    .execute(database.pool())
    .await?;
    Ok(())
}

fn test_connection() -> Result<PostgresConnection, Box<dyn std::error::Error>> {
    Ok(PostgresConnection {
        host: env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
        port: env::var("POSTGRES_TEST_PORT")
            .unwrap_or_else(|_| "55432".to_owned())
            .parse()?,
        database: env::var("POSTGRES_TEST_DATABASE").unwrap_or_else(|_| "lux_migration".to_owned()),
        username: env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
        password: env::var("POSTGRES_TEST_PASSWORD")
            .unwrap_or_else(|_| "lux-test-password".to_owned()),
        ssl_mode: "disable".to_owned(),
    })
}

async fn reset_public_schema(
    connection: &PostgresConnection,
) -> Result<(), Box<dyn std::error::Error>> {
    let url = luxd::config::DatabaseConfiguration::Postgres(connection.clone())
        .postgres_url()?
        .ok_or("missing PostgreSQL URL")?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await?;
    sqlx::query("DROP SCHEMA public CASCADE")
        .execute(&pool)
        .await?;
    sqlx::query("CREATE SCHEMA public").execute(&pool).await?;
    pool.close().await;
    Ok(())
}
