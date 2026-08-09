use std::env;

use luxd::{
    config::{Config, DatabaseConfiguration, PostgresConnection},
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

    let database = Database::connect_with_configuration(&config, &connection).await?;
    assert_eq!(database.backend(), luxd::config::DatabaseBackend::Postgres);
    assert!(database.schema_version().await? > 0);

    let library_id = uuid::Uuid::now_v7().to_string();
    let inserted = sqlx::query(
        "INSERT INTO libraries (
            id, name, kind, is_enabled, realtime_watch_enabled,
            scan_concurrency, probe_concurrency
        ) VALUES (?, ?, ?, 1, 1, 2, 1)",
    )
    .bind(&library_id)
    .bind("PostgreSQL Test Library")
    .bind("MOVIE")
    .execute(database.pool())
    .await?;
    assert_eq!(inserted.rows_affected(), 1);

    let stored_name: String = sqlx::query_scalar("SELECT name FROM libraries WHERE id = ?")
        .bind(&library_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(stored_name, "PostgreSQL Test Library");
    database.close().await;
    Ok(())
}
