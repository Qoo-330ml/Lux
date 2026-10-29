//! A configuration directory that has been set up before must not silently come back as a fresh
//! server just because its database went missing.

use luxd::{config::Config, storage::Database};

async fn config_for(temp_dir: &tempfile::TempDir) -> Result<Config, Box<dyn std::error::Error>> {
    Ok(Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    })
}

#[tokio::test]
async fn a_fresh_directory_starts_and_records_its_identity()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = config_for(&temp_dir).await?;

    let database = Database::connect(&config).await?;
    let server_id: String =
        sqlx::query_scalar("SELECT value FROM lux_meta WHERE key = 'server_id'")
            .fetch_one(database.pool())
            .await?;
    database.close().await;

    let marker = config.config_dir.join("server-state.json");
    assert!(
        marker.exists(),
        "the next start needs evidence that this directory was set up"
    );
    let recorded: serde_json::Value = serde_json::from_slice(&tokio::fs::read(&marker).await?)?;
    assert_eq!(recorded["serverId"], serde_json::json!(server_id));
    Ok(())
}

#[tokio::test]
async fn an_initialized_directory_refuses_a_missing_database()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = config_for(&temp_dir).await?;
    let database_file = config.config_dir.join("lux.db");
    Database::connect(&config).await?.close().await;
    assert!(database_file.exists());

    // The volume was not mounted, or only the configuration directory was restored.
    tokio::fs::remove_file(&database_file).await?;

    let error = Database::connect(&config)
        .await
        .err()
        .ok_or("connecting to a missing database must be refused")?;
    let message = error.to_string();
    assert!(
        message.contains("server-state.json"),
        "the operator needs to be told how to proceed, got: {message}"
    );
    assert!(
        !database_file.exists(),
        "the refused attempt must not create the empty database it was looking for"
    );
    Ok(())
}

#[tokio::test]
async fn removing_the_marker_allows_a_deliberate_fresh_start()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = config_for(&temp_dir).await?;
    let database_file = config.config_dir.join("lux.db");
    Database::connect(&config).await?.close().await;
    tokio::fs::remove_file(&database_file).await?;
    assert!(Database::connect(&config).await.is_err());

    tokio::fs::remove_file(config.config_dir.join("server-state.json")).await?;

    let database = Database::connect(&config).await?;
    let items: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media_items")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(items, 0, "the fresh start built an empty catalog");
    let identity: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM lux_meta WHERE key = 'server_id'")
        .fetch_one(database.pool())
        .await?;
    assert_eq!(identity, 1, "and gave itself a new identity");
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn an_existing_database_keeps_its_identity_across_restarts()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = config_for(&temp_dir).await?;

    let first = Database::connect(&config).await?;
    let server_id: String =
        sqlx::query_scalar("SELECT value FROM lux_meta WHERE key = 'server_id'")
            .fetch_one(first.pool())
            .await?;
    first.close().await;

    let second = Database::connect(&config).await?;
    let restarted: String =
        sqlx::query_scalar("SELECT value FROM lux_meta WHERE key = 'server_id'")
            .fetch_one(second.pool())
            .await?;
    second.close().await;

    assert_eq!(
        server_id, restarted,
        "the guard saw a database, not an impostor"
    );
    Ok(())
}
