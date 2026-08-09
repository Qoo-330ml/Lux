use std::path::Path;

use luxd::config::{Config, DatabaseConfiguration, PostgresConnection};

fn config(config_dir: &Path) -> Config {
    Config {
        http_addr: "127.0.0.1:8097".parse().expect("test address"),
        config_dir: config_dir.to_path_buf(),
    }
}

fn postgres_configuration() -> DatabaseConfiguration {
    DatabaseConfiguration::Postgres(PostgresConnection {
        host: "127.0.0.1".to_owned(),
        port: 5432,
        database: "lux".to_owned(),
        username: "lux".to_owned(),
        password: "test-only-password".to_owned(),
        ssl_mode: "prefer".to_owned(),
    })
}

#[tokio::test]
async fn empty_config_dir_has_no_database_selection() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = config(temp_dir.path());

    assert_eq!(config.load_database_configuration().await?, None);
    Ok(())
}

#[tokio::test]
async fn database_configuration_round_trips_without_debug_secret()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = config(temp_dir.path());
    let expected = postgres_configuration();

    config.save_database_configuration(&expected).await?;
    let restored = config
        .load_database_configuration()
        .await?
        .expect("database configuration");

    assert_eq!(restored, expected);
    assert!(!format!("{restored:?}").contains("test-only-password"));
    Ok(())
}

#[tokio::test]
async fn existing_sqlite_file_keeps_legacy_default() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = config(temp_dir.path());
    tokio::fs::create_dir_all(&config.config_dir).await?;
    tokio::fs::write(config.config_dir.join("lux.db"), b"legacy marker").await?;

    assert_eq!(
        config.load_database_configuration().await?,
        Some(DatabaseConfiguration::Sqlite)
    );
    Ok(())
}
