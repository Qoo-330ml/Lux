use std::fs;

use luxd::{config::Config, storage::Database};

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

    assert_eq!(database.schema_version().await?, 20);
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
    assert_eq!(second_database.schema_version().await?, 20);
    second_database.close().await;
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
