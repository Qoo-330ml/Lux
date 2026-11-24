use std::path::Path;

use luxd::{
    config::{Config, DatabaseConfiguration, PostgresConnection},
    storage::Database,
};

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

#[cfg(unix)]
fn permissions(path: &Path) -> Result<u32, Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;

    Ok(std::fs::metadata(path)?.permissions().mode() & 0o777)
}

#[cfg(unix)]
fn temporary_files(config_dir: &Path) -> Result<Vec<std::path::PathBuf>, std::io::Error> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(config_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') && name.ends_with(".tmp") {
            files.push(entry.path());
        }
    }
    files.sort();
    Ok(files)
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

#[tokio::test]
async fn pending_database_selection_does_not_turn_bootstrap_sqlite_into_legacy_default()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = config(temp_dir.path());
    config.mark_database_selection_pending().await?;
    let database = Database::connect(&config).await?;
    database.close().await;

    assert_eq!(config.load_database_configuration().await?, None);
    config
        .save_database_configuration(&DatabaseConfiguration::Sqlite)
        .await?;
    assert!(
        !config
            .config_dir
            .join(".database-selection-required")
            .exists()
    );
    assert_eq!(
        config.load_database_configuration().await?,
        Some(DatabaseConfiguration::Sqlite)
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn saving_postgres_configuration_is_private_and_leaves_no_temporary_files()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = config(&temp_dir.path().join("config"));

    config
        .save_database_configuration(&postgres_configuration())
        .await?;

    assert_eq!(
        permissions(&config.config_dir.join("database.json"))?,
        0o600
    );
    assert_eq!(permissions(&config.config_dir)?, 0o700);
    assert!(temporary_files(&config.config_dir)?.is_empty());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn pending_selection_marker_is_private_and_leaves_no_temporary_files()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = config(&temp_dir.path().join("config"));

    config.mark_database_selection_pending().await?;

    assert_eq!(
        permissions(&config.config_dir.join(".database-selection-required"))?,
        0o600
    );
    assert!(temporary_files(&config.config_dir)?.is_empty());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn replacing_database_configuration_keeps_private_permissions_and_new_content()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = config(&temp_dir.path().join("config"));

    config
        .save_database_configuration(&postgres_configuration())
        .await?;
    config
        .save_database_configuration(&DatabaseConfiguration::Sqlite)
        .await?;

    assert_eq!(
        config.load_database_configuration().await?,
        Some(DatabaseConfiguration::Sqlite)
    );
    assert_eq!(
        permissions(&config.config_dir.join("database.json"))?,
        0o600
    );
    assert!(temporary_files(&config.config_dir)?.is_empty());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn existing_shared_config_directory_still_accepts_database_configuration()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    std::fs::create_dir(&config_dir)?;
    // A Docker or NAS mount normally arrives as 0755 and must keep working.
    std::fs::set_permissions(&config_dir, std::fs::Permissions::from_mode(0o755))?;
    let config = config(&config_dir);

    config
        .save_database_configuration(&DatabaseConfiguration::Sqlite)
        .await?;

    assert_eq!(permissions(&config_dir)?, 0o755);
    assert_eq!(permissions(&config_dir.join("database.json"))?, 0o600);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn world_writable_config_directory_is_tightened_before_writing()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    std::fs::create_dir(&config_dir)?;
    std::fs::set_permissions(&config_dir, std::fs::Permissions::from_mode(0o777))?;
    let config = config(&config_dir);

    config
        .save_database_configuration(&DatabaseConfiguration::Sqlite)
        .await?;

    assert_eq!(permissions(&config_dir)?, 0o755);
    assert_eq!(permissions(&config_dir.join("database.json"))?, 0o600);
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn failed_save_keeps_previous_configuration_and_removes_temporary_files()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = tempfile::tempdir()?;
    let config = config(&temp_dir.path().join("config"));
    let original = postgres_configuration();
    config.save_database_configuration(&original).await?;

    std::fs::set_permissions(&config.config_dir, std::fs::Permissions::from_mode(0o500))?;
    let failed = config
        .save_database_configuration(&DatabaseConfiguration::Sqlite)
        .await;
    std::fs::set_permissions(&config.config_dir, std::fs::Permissions::from_mode(0o700))?;

    assert!(failed.is_err());
    assert_eq!(config.load_database_configuration().await?, Some(original));
    assert!(temporary_files(&config.config_dir)?.is_empty());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn rename_failure_keeps_the_target_untouched_and_removes_temporary_files()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = config(&temp_dir.path().join("config"));
    std::fs::create_dir_all(config.config_dir.join("database.json"))?;

    let result = config
        .save_database_configuration(&DatabaseConfiguration::Sqlite)
        .await;

    assert!(result.is_err());
    assert!(config.config_dir.join("database.json").is_dir());
    assert!(temporary_files(&config.config_dir)?.is_empty());
    Ok(())
}
