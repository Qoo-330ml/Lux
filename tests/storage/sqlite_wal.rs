//! The database has to be one file again when the process stops, and the write-ahead log must not
//! grow without bound in between.

use luxd::{config::Config, storage::Database};

const EXPECTED_WAL_LIMIT: i64 = 64 * 1024 * 1024;

async fn test_database(
    temp_dir: &tempfile::TempDir,
) -> Result<(Database, Config), Box<dyn std::error::Error>> {
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    Ok((database, config))
}

#[tokio::test]
async fn every_connection_caps_the_write_ahead_log() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let (database, _) = test_database(&temp_dir).await?;

    let limit: i64 = sqlx::query_scalar("PRAGMA journal_size_limit")
        .fetch_one(database.pool())
        .await?;

    assert_eq!(
        limit, EXPECTED_WAL_LIMIT,
        "a bulk delete that grew the log must not leave it on disk at full size"
    );
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn closing_leaves_the_database_in_a_single_file() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let (database, config) = test_database(&temp_dir).await?;
    let database_file = config.config_dir.join("lux.db");
    let wal_file = config.config_dir.join("lux.db-wal");
    assert!(database_file.exists(), "the migration created no database");
    assert!(
        wal_file.exists(),
        "a write through the write-ahead log was expected"
    );

    database.close().await;

    assert!(
        !wal_file.exists(),
        "an operator copying lux.db has to get the whole database"
    );
    assert!(database_file.exists());
    Ok(())
}
