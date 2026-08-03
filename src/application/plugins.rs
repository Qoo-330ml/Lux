use std::{env, fmt, io, path::PathBuf};

use crate::{
    application::{
        settings::{TMDB_API_KEY_FILE, TMDB_TOKEN_FILE, write_tmdb_api_key},
        tmdb::EMBEDDED_TMDB_API_KEY,
    },
    storage::{Database, StorageError},
};

pub const TMDB_PLUGIN_ID: &str = "tmdb";
const TMDB_PLUGIN_NAME: &str = "TMDb 元数据插件";
const TMDB_PLUGIN_DESCRIPTION: &str = "使用 TMDb 补全电影和剧集元数据、海报与背景图。";
const CONFIG_SOURCE_BUILT_IN: &str = "BUILT_IN";
const CONFIG_SOURCE_CUSTOM: &str = "CUSTOM";
const CONFIG_SOURCE_ENVIRONMENT: &str = "ENVIRONMENT";
const CONFIG_SOURCE_READ_ACCESS_TOKEN: &str = "READ_ACCESS_TOKEN";
const CONFIG_SOURCE_NONE: &str = "NONE";

static TMDB_CONFIG_FIELDS: &[PluginConfigField] = &[PluginConfigField {
    key: "apiKey",
    label: "TMDb API Key",
    input_type: "password",
    required: false,
    sensitive: true,
    description: "可选。留空时使用 Lux 内置的 TMDb Key。",
}];

#[derive(Clone)]
pub struct PluginService {
    database: Database,
    config_dir: PathBuf,
}

impl PluginService {
    pub fn new(database: Database, config_dir: PathBuf) -> Self {
        Self {
            database,
            config_dir,
        }
    }

    pub async fn list(&self, offset: i64, limit: i64) -> Result<PluginPage, PluginServiceError> {
        let installed = self.database.is_plugin_installed(TMDB_PLUGIN_ID).await?;
        let plugin = plugin_view(installed, self.tmdb_config_source().await);
        Ok(PluginPage {
            plugins: if offset == 0 && limit > 0 {
                vec![plugin]
            } else {
                Vec::new()
            },
            total: 1,
            offset,
            limit,
        })
    }

    pub async fn install(&self, plugin_id: &str) -> Result<PluginInstall, PluginServiceError> {
        ensure_known_plugin(plugin_id)?;
        let was_installed = self.database.is_plugin_installed(plugin_id).await?;
        self.database.install_plugin(plugin_id).await?;
        Ok(PluginInstall {
            plugin: plugin_view(true, self.tmdb_config_source().await),
            was_installed,
        })
    }

    pub async fn update_config(
        &self,
        plugin_id: &str,
        api_key: &str,
    ) -> Result<PluginView, PluginServiceError> {
        ensure_known_plugin(plugin_id)?;
        let api_key = api_key.trim();
        if api_key.chars().count() > 4096 {
            return Err(PluginServiceError::InvalidConfig);
        }
        write_tmdb_api_key(&self.config_dir, (!api_key.is_empty()).then_some(api_key))
            .await
            .map_err(PluginServiceError::ConfigIo)?;
        let installed = self.database.is_plugin_installed(plugin_id).await?;
        Ok(plugin_view(installed, self.tmdb_config_source().await))
    }

    pub async fn validate_selection(
        &self,
        scraper_id: Option<&str>,
    ) -> Result<(), PluginServiceError> {
        let Some(scraper_id) = scraper_id.map(str::trim).filter(|value| !value.is_empty()) else {
            return Ok(());
        };
        ensure_known_plugin(scraper_id)?;
        if !self.database.is_plugin_installed(scraper_id).await? {
            return Err(PluginServiceError::Unavailable(scraper_id.to_owned()));
        }
        Ok(())
    }

    async fn tmdb_config_source(&self) -> &'static str {
        if secret_file_configured(&self.config_dir, TMDB_API_KEY_FILE).await {
            CONFIG_SOURCE_CUSTOM
        } else if has_environment_value("LUX_TMDB_API_KEY") {
            CONFIG_SOURCE_ENVIRONMENT
        } else if has_environment_value("LUX_TMDB_READ_ACCESS_TOKEN")
            || secret_file_configured(&self.config_dir, TMDB_TOKEN_FILE).await
        {
            CONFIG_SOURCE_READ_ACCESS_TOKEN
        } else if !EMBEDDED_TMDB_API_KEY.is_empty() {
            CONFIG_SOURCE_BUILT_IN
        } else {
            CONFIG_SOURCE_NONE
        }
    }
}

async fn secret_file_configured(config_dir: &std::path::Path, file_name: &str) -> bool {
    tokio::fs::read_to_string(config_dir.join(file_name))
        .await
        .ok()
        .is_some_and(|value| !value.trim().is_empty())
}

fn has_environment_value(name: &str) -> bool {
    env::var(name)
        .ok()
        .is_some_and(|value| !value.trim().is_empty())
}

#[derive(Debug)]
pub struct PluginPage {
    pub plugins: Vec<PluginView>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
}

#[derive(Debug)]
pub struct PluginInstall {
    pub plugin: PluginView,
    pub was_installed: bool,
}

#[derive(Debug)]
pub struct PluginView {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub installed: bool,
    pub enabled: bool,
    pub configured: bool,
    pub available: bool,
    pub unavailable_reason: Option<&'static str>,
    pub configurable: bool,
    pub config_fields: &'static [PluginConfigField],
    pub config_source: &'static str,
}

#[derive(Debug)]
pub struct PluginConfigField {
    pub key: &'static str,
    pub label: &'static str,
    pub input_type: &'static str,
    pub required: bool,
    pub sensitive: bool,
    pub description: &'static str,
}

#[derive(Debug)]
pub enum PluginServiceError {
    UnknownPlugin(String),
    Unavailable(String),
    InvalidConfig,
    ConfigIo(io::Error),
    Storage(StorageError),
}

impl fmt::Display for PluginServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownPlugin(plugin_id) => write!(formatter, "unknown plugin: {plugin_id}"),
            Self::Unavailable(plugin_id) => {
                write!(formatter, "plugin is unavailable: {plugin_id}")
            }
            Self::InvalidConfig => formatter.write_str("invalid plugin configuration"),
            Self::ConfigIo(error) => write!(formatter, "plugin configuration IO error: {error}"),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for PluginServiceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ConfigIo(error) => Some(error),
            Self::Storage(error) => Some(error),
            Self::UnknownPlugin(_) | Self::Unavailable(_) | Self::InvalidConfig => None,
        }
    }
}

impl From<StorageError> for PluginServiceError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

fn ensure_known_plugin(plugin_id: &str) -> Result<(), PluginServiceError> {
    (plugin_id == TMDB_PLUGIN_ID)
        .then_some(())
        .ok_or_else(|| PluginServiceError::UnknownPlugin(plugin_id.to_owned()))
}

fn plugin_view(installed: bool, config_source: &'static str) -> PluginView {
    PluginView {
        id: TMDB_PLUGIN_ID,
        name: TMDB_PLUGIN_NAME,
        description: TMDB_PLUGIN_DESCRIPTION,
        installed,
        enabled: installed,
        configured: config_source != CONFIG_SOURCE_NONE,
        available: installed && config_source != CONFIG_SOURCE_NONE,
        unavailable_reason: if !installed {
            Some("NOT_INSTALLED")
        } else if config_source == CONFIG_SOURCE_NONE {
            Some("NOT_CONFIGURED")
        } else {
            None
        },
        configurable: true,
        config_fields: TMDB_CONFIG_FIELDS,
        config_source,
    }
}
