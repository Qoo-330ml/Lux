use std::{
    io,
    path::{Path, PathBuf},
};

use serde_json::{Map, Value};
use tokio::{fs, io::AsyncWriteExt};
use uuid::Uuid;

const TMDB_PLUGIN_ID: &str = "org.lux.tmdb";
const PLUGIN_CONFIG_DIR: &str = "plugin-config";
const TMDB_API_KEY_FILE: &str = "tmdb_api_key";
const TMDB_READ_ACCESS_TOKEN_FILE: &str = "tmdb_read_access_token";
const TMDB_SETTINGS_FILE: &str = "tmdb_settings.json";

const SETTINGS_KEYS: &[(&str, &[&str])] = &[
    (
        "preferredLanguage",
        &["preferredLanguage", "preferred_language"],
    ),
    (
        "languageFallbackEnabled",
        &["languageFallbackEnabled", "language_fallback_enabled"],
    ),
    (
        "titleAliasReplacementEnabled",
        &[
            "titleAliasReplacementEnabled",
            "title_alias_replacement_enabled",
        ],
    ),
    (
        "fallbackLanguages",
        &["fallbackLanguages", "fallback_languages"],
    ),
    (
        "alternateApiEnabled",
        &["alternateApiEnabled", "alternate_api_enabled"],
    ),
    ("apiBaseUrl", &["apiBaseUrl", "api_base_url"]),
];

#[derive(Debug)]
pub enum PluginCompatibilityError {
    Io(io::Error),
    InvalidPluginConfig,
}

impl std::fmt::Display for PluginCompatibilityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "plugin compatibility migration failed: {error}"),
            Self::InvalidPluginConfig => {
                formatter.write_str("existing metadata plugin configuration is invalid")
            }
        }
    }
}

impl std::error::Error for PluginCompatibilityError {}

impl From<io::Error> for PluginCompatibilityError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Copies legacy TMDb settings into the isolated plugin configuration once.
///
/// Existing plugin values always win. Legacy files are deliberately retained so
/// an upgrade can be rolled back without losing the user's original settings.
pub async fn migrate_legacy_tmdb_config(
    config_dir: &Path,
) -> Result<bool, PluginCompatibilityError> {
    let config_path = plugin_config_path(config_dir);
    let mut values = read_existing_config(&config_path).await?;
    let original_len = values.len();

    merge_secret_file(config_dir.join(TMDB_API_KEY_FILE), "apiKey", &mut values).await?;
    merge_secret_file(
        config_dir.join(TMDB_READ_ACCESS_TOKEN_FILE),
        "readAccessToken",
        &mut values,
    )
    .await?;
    merge_settings_file(config_dir.join(TMDB_SETTINGS_FILE), &mut values).await?;

    if values.len() == original_len {
        return Ok(false);
    }
    write_config(&config_path, &values).await?;
    Ok(true)
}

async fn read_existing_config(path: &Path) -> Result<Map<String, Value>, PluginCompatibilityError> {
    let contents = match fs::read_to_string(path).await {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Map::new()),
        Err(error) => return Err(error.into()),
    };
    serde_json::from_str(&contents).map_err(|_| PluginCompatibilityError::InvalidPluginConfig)
}

async fn merge_secret_file(
    path: PathBuf,
    key: &str,
    values: &mut Map<String, Value>,
) -> Result<(), PluginCompatibilityError> {
    if values.contains_key(key) {
        return Ok(());
    }
    let contents = match fs::read_to_string(path).await {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let value = contents.trim();
    if !value.is_empty() {
        values.insert(key.to_owned(), Value::String(value.to_owned()));
    }
    Ok(())
}

async fn merge_settings_file(
    path: PathBuf,
    values: &mut Map<String, Value>,
) -> Result<(), PluginCompatibilityError> {
    let contents = match fs::read_to_string(path).await {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let Ok(settings) = serde_json::from_str::<Map<String, Value>>(&contents) else {
        return Ok(());
    };
    for (target_key, source_keys) in SETTINGS_KEYS {
        if values.contains_key(*target_key) {
            continue;
        }
        if let Some(value) = source_keys
            .iter()
            .find_map(|key| settings.get(*key).filter(|value| !value.is_null()))
        {
            values.insert((*target_key).to_owned(), value.clone());
        }
    }
    Ok(())
}

async fn write_config(
    path: &Path,
    values: &Map<String, Value>,
) -> Result<(), PluginCompatibilityError> {
    let directory = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "plugin config path has no parent",
        )
    })?;
    fs::create_dir_all(directory).await?;
    let temporary = directory.join(format!(
        ".{TMDB_PLUGIN_ID}.{uuid}.tmp",
        uuid = Uuid::now_v7()
    ));
    let contents = serde_json::to_vec_pretty(values)
        .map_err(|_| PluginCompatibilityError::InvalidPluginConfig)?;
    let mut file = fs::File::create(&temporary).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = file.metadata().await?.permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(&temporary, permissions).await?;
    }
    file.write_all(&contents).await?;
    file.sync_all().await?;
    drop(file);
    fs::rename(temporary, path).await?;
    Ok(())
}

fn plugin_config_path(config_dir: &Path) -> PathBuf {
    config_dir
        .join(PLUGIN_CONFIG_DIR)
        .join(format!("{TMDB_PLUGIN_ID}.json"))
}

#[cfg(test)]
mod tests {
    use super::migrate_legacy_tmdb_config;
    use serde_json::Value;

    #[tokio::test]
    async fn migrates_legacy_values_without_overwriting_new_config_or_deleting_old_files() {
        let directory = tempfile::tempdir().expect("temporary directory");
        tokio::fs::write(directory.path().join("tmdb_api_key"), "legacy-key\n")
            .await
            .expect("legacy api key");
        tokio::fs::write(
            directory.path().join("tmdb_read_access_token"),
            "legacy-token\n",
        )
        .await
        .expect("legacy token");
        tokio::fs::write(
            directory.path().join("tmdb_settings.json"),
            r#"{"preferredLanguage":"zh-HK","languageFallbackEnabled":true,"apiBaseUrl":"https://legacy.example"}"#,
        )
        .await
        .expect("legacy settings");
        let config_path = directory.path().join("plugin-config/org.lux.tmdb.json");
        tokio::fs::create_dir_all(config_path.parent().expect("config parent"))
            .await
            .expect("config directory");
        tokio::fs::write(
            &config_path,
            r#"{"apiKey":"new-key","preferredLanguage":"zh-CN"}"#,
        )
        .await
        .expect("new config");

        assert!(
            migrate_legacy_tmdb_config(directory.path())
                .await
                .expect("migration")
        );
        let migrated: Value = serde_json::from_str(
            &tokio::fs::read_to_string(&config_path)
                .await
                .expect("read config"),
        )
        .expect("json config");
        assert_eq!(migrated["apiKey"], "new-key");
        assert_eq!(migrated["readAccessToken"], "legacy-token");
        assert_eq!(migrated["preferredLanguage"], "zh-CN");
        assert_eq!(migrated["languageFallbackEnabled"], true);
        assert_eq!(migrated["apiBaseUrl"], "https://legacy.example");
        assert!(directory.path().join("tmdb_api_key").exists());
        assert!(directory.path().join("tmdb_read_access_token").exists());
        assert!(directory.path().join("tmdb_settings.json").exists());

        assert!(
            !migrate_legacy_tmdb_config(directory.path())
                .await
                .expect("second migration")
        );
    }

    #[tokio::test]
    async fn ignores_invalid_legacy_settings_without_writing_credentials_to_logs_or_errors() {
        let directory = tempfile::tempdir().expect("temporary directory");
        tokio::fs::write(directory.path().join("tmdb_settings.json"), "not-json")
            .await
            .expect("legacy settings");

        assert!(
            !migrate_legacy_tmdb_config(directory.path())
                .await
                .expect("migration")
        );
        assert!(
            !directory
                .path()
                .join("plugin-config/org.lux.tmdb.json")
                .exists()
        );
    }
}
