use std::fs;

use luxd::{
    application::libraries::LibraryService, config::Config, library::LibraryKind, storage::Database,
};

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

    assert_eq!(database.schema_version().await?, 44);
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
    assert_eq!(second_database.schema_version().await?, 44);
    second_database.close().await;
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
    assert_eq!(database.schema_version().await?, 44);
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
