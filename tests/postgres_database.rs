use std::env;

use luxd::{
    application::{
        access::{AccessPrincipal, MediaAccessService},
        catalog::CatalogService,
        libraries::{LibraryService, LibrarySettingsPatch},
        setup::SetupService,
    },
    auth::sessions::WebAuthService,
    config::{Config, DatabaseConfiguration, PostgresConnection},
    domain::ids::{LibraryId, UserId},
    storage::Database,
};

#[tokio::test]
#[ignore = "requires a local PostgreSQL instance"]
async fn postgres_bootstrap_runs_migrations_and_persists_core_state()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let connection = DatabaseConfiguration::Postgres(PostgresConnection {
        host: env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
        port: env::var("POSTGRES_TEST_PORT")
            .unwrap_or_else(|_| "55432".to_owned())
            .parse()?,
        database: env::var("POSTGRES_TEST_DATABASE").unwrap_or_else(|_| "lux".to_owned()),
        username: env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
        password: env::var("POSTGRES_TEST_PASSWORD")
            .unwrap_or_else(|_| "lux-test-password".to_owned()),
        ssl_mode: "disable".to_owned(),
    });

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
    assert!(database.schema_version().await? > 0);
    let chapter_table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM information_schema.tables
         WHERE table_schema = current_schema() AND table_name = 'media_chapters'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(chapter_table_count, 1);

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
            id, library_id, item_type, title, sort_title, identification_status, has_available_source
        ) VALUES ($1, $2, 'MOVIE', 'Postgres Search Movie', 'postgres search movie', 'LOCAL_CONFIRMED', 1)",
    )
    .bind(&item_id)
    .bind(library_id.to_string())
    .execute(database.pool())
    .await?;
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
    Ok(())
}
