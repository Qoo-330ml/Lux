use std::{fmt, path::PathBuf};

use crate::{
    application::settings::TMDB_TOKEN_FILE,
    storage::{Database, StorageError},
};

pub const TMDB_PLUGIN_ID: &str = "tmdb";
const TMDB_PLUGIN_NAME: &str = "TMDb 元数据插件";
const TMDB_PLUGIN_DESCRIPTION: &str = "使用 TMDb 补全电影和剧集元数据、海报与背景图。";

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

    pub async fn list(
        &self,
        offset: i64,
        limit: i64,
        tmdb_configured: bool,
    ) -> Result<PluginPage, PluginServiceError> {
        let installed = self.database.is_plugin_installed(TMDB_PLUGIN_ID).await?;
        let plugin = plugin_view(installed, self.tmdb_configured(tmdb_configured).await);
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

    pub async fn install(
        &self,
        plugin_id: &str,
        tmdb_configured: bool,
    ) -> Result<PluginInstall, PluginServiceError> {
        ensure_known_plugin(plugin_id)?;
        let was_installed = self.database.is_plugin_installed(plugin_id).await?;
        self.database.install_plugin(plugin_id).await?;
        Ok(PluginInstall {
            plugin: plugin_view(true, self.tmdb_configured(tmdb_configured).await),
            was_installed,
        })
    }

    pub async fn validate_selection(
        &self,
        scraper_id: Option<&str>,
        tmdb_configured: bool,
    ) -> Result<(), PluginServiceError> {
        let Some(scraper_id) = scraper_id.map(str::trim).filter(|value| !value.is_empty()) else {
            return Ok(());
        };
        ensure_known_plugin(scraper_id)?;
        if !self.database.is_plugin_installed(scraper_id).await?
            || !self.tmdb_configured(tmdb_configured).await
        {
            return Err(PluginServiceError::Unavailable(scraper_id.to_owned()));
        }
        Ok(())
    }

    async fn tmdb_configured(&self, configured_by_runtime: bool) -> bool {
        if configured_by_runtime {
            return true;
        }
        tokio::fs::read_to_string(self.config_dir.join(TMDB_TOKEN_FILE))
            .await
            .ok()
            .is_some_and(|token| !token.trim().is_empty())
    }
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
}

#[derive(Debug)]
pub enum PluginServiceError {
    UnknownPlugin(String),
    Unavailable(String),
    Storage(StorageError),
}

impl fmt::Display for PluginServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownPlugin(plugin_id) => write!(formatter, "unknown plugin: {plugin_id}"),
            Self::Unavailable(plugin_id) => {
                write!(formatter, "plugin is unavailable: {plugin_id}")
            }
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for PluginServiceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Storage(error) => Some(error),
            Self::UnknownPlugin(_) | Self::Unavailable(_) => None,
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

fn plugin_view(installed: bool, configured: bool) -> PluginView {
    PluginView {
        id: TMDB_PLUGIN_ID,
        name: TMDB_PLUGIN_NAME,
        description: TMDB_PLUGIN_DESCRIPTION,
        installed,
        enabled: installed,
        configured,
        available: installed && configured,
        unavailable_reason: if !installed {
            Some("NOT_INSTALLED")
        } else if !configured {
            Some("NOT_CONFIGURED")
        } else {
            None
        },
    }
}
