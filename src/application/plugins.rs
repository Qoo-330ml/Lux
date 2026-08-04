use std::{env, fmt, io, path::PathBuf};

use serde_json::Value;

use crate::{
    application::{
        plugin_protocol::{PLUGIN_CATEGORY_SCRAPER, PluginConfigField},
        plugin_runtime::{DiscoveredPlugin, PluginCatalog, PluginRuntimeError, PluginSupervisor},
        settings::{TMDB_API_KEY_FILE, TMDB_TOKEN_FILE, write_tmdb_api_key},
        tmdb::EMBEDDED_TMDB_API_KEY,
    },
    storage::{Database, StorageError},
};

pub const TMDB_PLUGIN_ID: &str = "tmdb";
pub const TMDB_DYNAMIC_PLUGIN_ID: &str = "org.lux.tmdb";
const TMDB_PLUGIN_NAME: &str = "TMDb 元数据插件";
const TMDB_PLUGIN_DESCRIPTION: &str = "使用 TMDb 补全电影和剧集元数据、海报与背景图。";
const TMDB_PLUGIN_VERSION: &str = "1.0.0";
const CONFIG_SOURCE_BUILT_IN: &str = "BUILT_IN";
const CONFIG_SOURCE_CUSTOM: &str = "CUSTOM";
const CONFIG_SOURCE_ENVIRONMENT: &str = "ENVIRONMENT";
const CONFIG_SOURCE_READ_ACCESS_TOKEN: &str = "READ_ACCESS_TOKEN";
const CONFIG_SOURCE_NONE: &str = "NONE";

fn tmdb_config_fields() -> Vec<PluginConfigField> {
    vec![PluginConfigField {
        key: "apiKey".to_owned(),
        label: "TMDb API Key".to_owned(),
        input_type: "password".to_owned(),
        required: false,
        sensitive: true,
        description: Some("可选。留空时使用 Lux 内置的 TMDb Key。".to_owned()),
    }]
}

#[derive(Clone)]
pub struct PluginService {
    database: Database,
    config_dir: PathBuf,
    catalog: PluginCatalog,
    supervisor: PluginSupervisor,
}

impl PluginService {
    pub fn new(database: Database, config_dir: PathBuf) -> Self {
        let catalog = PluginCatalog::discover(&config_dir.join("plugins"));
        let supervisor = PluginSupervisor::new(catalog.clone()).with_config_dir(config_dir.clone());
        Self {
            database,
            config_dir,
            catalog,
            supervisor,
        }
    }

    pub async fn list(&self, offset: i64, limit: i64) -> Result<PluginPage, PluginServiceError> {
        self.list_filtered(offset, limit, false).await
    }

    pub async fn list_installed(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<PluginPage, PluginServiceError> {
        self.list_filtered(offset, limit, true).await
    }

    async fn list_filtered(
        &self,
        offset: i64,
        limit: i64,
        installed_only: bool,
    ) -> Result<PluginPage, PluginServiceError> {
        let mut views = Vec::with_capacity(self.catalog.plugins.len() + 1);
        let has_tmdb_package = self
            .catalog
            .plugins
            .iter()
            .any(|plugin| is_tmdb_plugin_id(&plugin.manifest.id));
        if !has_tmdb_package {
            let installed = self.database.is_plugin_installed(TMDB_PLUGIN_ID).await?;
            if !installed_only || installed {
                views.push(legacy_tmdb_view(installed, self.tmdb_config_source().await));
            }
        }
        for plugin in &self.catalog.plugins {
            let installed = self
                .database
                .is_plugin_installed(&plugin.manifest.id)
                .await?;
            if !installed_only || installed {
                views.push(self.dynamic_view(plugin, installed).await);
            }
        }
        let total = i64::try_from(views.len()).unwrap_or(i64::MAX);
        let start = offset.max(0).min(total) as usize;
        let end = (offset.max(0).saturating_add(limit.max(0))).min(total) as usize;
        Ok(PluginPage {
            plugins: views[start..end].to_vec(),
            total,
            offset,
            limit,
        })
    }

    pub async fn install(&self, plugin_id: &str) -> Result<PluginInstall, PluginServiceError> {
        self.ensure_known_plugin(plugin_id)?;
        let was_installed = self.database.is_plugin_installed(plugin_id).await?;
        self.database.install_plugin(plugin_id).await?;
        let plugin = self.view_for_id(plugin_id, true).await?;
        Ok(PluginInstall {
            plugin,
            was_installed,
        })
    }

    pub async fn update_config(
        &self,
        plugin_id: &str,
        api_key: &str,
    ) -> Result<PluginView, PluginServiceError> {
        self.ensure_known_plugin(plugin_id)?;
        if !is_tmdb_plugin_id(plugin_id) {
            return Err(PluginServiceError::InvalidConfig);
        }
        let api_key = api_key.trim();
        if api_key.chars().count() > 4096 {
            return Err(PluginServiceError::InvalidConfig);
        }
        write_tmdb_api_key(&self.config_dir, (!api_key.is_empty()).then_some(api_key))
            .await
            .map_err(PluginServiceError::ConfigIo)?;
        let installed = self.database.is_plugin_installed(plugin_id).await?;
        self.view_for_id(plugin_id, installed).await
    }

    pub async fn validate_selection(
        &self,
        scraper_id: Option<&str>,
    ) -> Result<(), PluginServiceError> {
        let Some(scraper_id) = scraper_id.map(str::trim).filter(|value| !value.is_empty()) else {
            return Ok(());
        };
        self.ensure_known_plugin(scraper_id)?;
        if !self.database.is_plugin_installed(scraper_id).await? {
            return Err(PluginServiceError::Unavailable(scraper_id.to_owned()));
        }
        let view = self.view_for_id(scraper_id, true).await?;
        if !view.available {
            return Err(PluginServiceError::Unavailable(scraper_id.to_owned()));
        }
        Ok(())
    }

    pub async fn call(
        &self,
        plugin_id: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, PluginServiceError> {
        self.ensure_known_plugin(plugin_id)?;
        if plugin_id == TMDB_PLUGIN_ID {
            return Err(PluginServiceError::Unavailable(plugin_id.to_owned()));
        }
        self.supervisor
            .call(plugin_id, method, params)
            .await
            .map_err(PluginServiceError::Runtime)
    }

    pub async fn call_tmdb(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, PluginServiceError> {
        let plugin = self
            .catalog
            .get(TMDB_DYNAMIC_PLUGIN_ID)
            .ok_or_else(|| PluginServiceError::Unavailable(TMDB_DYNAMIC_PLUGIN_ID.to_owned()))?;
        let installed = self
            .database
            .is_plugin_installed(TMDB_DYNAMIC_PLUGIN_ID)
            .await?;
        if !self.dynamic_view(plugin, installed).await.available {
            return Err(PluginServiceError::Unavailable(
                TMDB_DYNAMIC_PLUGIN_ID.to_owned(),
            ));
        }
        self.supervisor
            .call(TMDB_DYNAMIC_PLUGIN_ID, method, params)
            .await
            .map_err(PluginServiceError::Runtime)
    }

    pub async fn restart_tmdb(&self) {
        self.supervisor.stop(TMDB_DYNAMIC_PLUGIN_ID).await;
    }

    pub async fn stop_all(&self) {
        self.supervisor.stop_all().await;
    }

    async fn view_for_id(
        &self,
        plugin_id: &str,
        installed: bool,
    ) -> Result<PluginView, PluginServiceError> {
        if plugin_id == TMDB_PLUGIN_ID {
            return Ok(legacy_tmdb_view(installed, self.tmdb_config_source().await));
        }
        let Some(plugin) = self.catalog.get(plugin_id) else {
            return Err(PluginServiceError::UnknownPlugin(plugin_id.to_owned()));
        };
        Ok(self.dynamic_view(plugin, installed).await)
    }

    async fn dynamic_view(&self, plugin: &DiscoveredPlugin, installed: bool) -> PluginView {
        let runtime = self.supervisor.status(&plugin.manifest.id).await;
        let config_source = if is_tmdb_plugin_id(&plugin.manifest.id) {
            self.tmdb_config_source().await.to_owned()
        } else {
            CONFIG_SOURCE_NONE.to_owned()
        };
        let configured = plugin.manifest.config_fields.is_empty()
            || (is_tmdb_plugin_id(&plugin.manifest.id) && config_source != CONFIG_SOURCE_NONE);
        let available = installed && configured;
        PluginView {
            id: plugin.manifest.id.clone(),
            name: plugin.manifest.name.clone(),
            description: plugin.manifest.description.clone().unwrap_or_default(),
            category: plugin.manifest.category.clone(),
            version: Some(plugin.manifest.version.clone()),
            runtime: Some(plugin.manifest.runtime.kind.clone()),
            capabilities: plugin.manifest.capabilities.clone(),
            status: if runtime.running {
                "RUNNING".to_owned()
            } else if runtime.last_error.is_some() {
                "ERROR".to_owned()
            } else if installed {
                "READY".to_owned()
            } else {
                "AVAILABLE".to_owned()
            },
            running: runtime.running,
            last_error: runtime.last_error,
            installed,
            enabled: installed,
            configured,
            available,
            unavailable_reason: if !installed {
                Some("NOT_INSTALLED".to_owned())
            } else if !configured {
                Some("NOT_CONFIGURED".to_owned())
            } else {
                None
            },
            configurable: !plugin.manifest.config_fields.is_empty(),
            config_fields: if is_tmdb_plugin_id(&plugin.manifest.id) {
                tmdb_config_fields()
            } else {
                plugin.manifest.config_fields.clone()
            },
            config_source,
        }
    }

    fn ensure_known_plugin(&self, plugin_id: &str) -> Result<(), PluginServiceError> {
        if plugin_id == TMDB_PLUGIN_ID || self.catalog.get(plugin_id).is_some() {
            Ok(())
        } else {
            Err(PluginServiceError::UnknownPlugin(plugin_id.to_owned()))
        }
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

fn is_tmdb_plugin_id(plugin_id: &str) -> bool {
    plugin_id == TMDB_PLUGIN_ID || plugin_id == TMDB_DYNAMIC_PLUGIN_ID
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

#[derive(Clone, Debug)]
pub struct PluginView {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub version: Option<String>,
    pub runtime: Option<String>,
    pub capabilities: Vec<String>,
    pub status: String,
    pub running: bool,
    pub last_error: Option<String>,
    pub installed: bool,
    pub enabled: bool,
    pub configured: bool,
    pub available: bool,
    pub unavailable_reason: Option<String>,
    pub configurable: bool,
    pub config_fields: Vec<PluginConfigField>,
    pub config_source: String,
}

#[derive(Debug)]
pub enum PluginServiceError {
    UnknownPlugin(String),
    Unavailable(String),
    InvalidConfig,
    ConfigIo(io::Error),
    Runtime(PluginRuntimeError),
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
            Self::Runtime(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for PluginServiceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ConfigIo(error) => Some(error),
            Self::Runtime(error) => Some(error),
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

fn legacy_tmdb_view(installed: bool, config_source: &str) -> PluginView {
    PluginView {
        id: TMDB_PLUGIN_ID.to_owned(),
        name: TMDB_PLUGIN_NAME.to_owned(),
        description: TMDB_PLUGIN_DESCRIPTION.to_owned(),
        category: PLUGIN_CATEGORY_SCRAPER.to_owned(),
        version: Some(TMDB_PLUGIN_VERSION.to_owned()),
        runtime: Some("built-in".to_owned()),
        capabilities: vec![
            "metadata.search".to_owned(),
            "metadata.details".to_owned(),
            "metadata.images".to_owned(),
            "metadata.externalIds".to_owned(),
            "metadata.trailers".to_owned(),
        ],
        status: "BUILT_IN_COMPATIBILITY".to_owned(),
        running: true,
        last_error: None,
        installed,
        enabled: installed,
        configured: config_source != CONFIG_SOURCE_NONE,
        available: installed && config_source != CONFIG_SOURCE_NONE,
        unavailable_reason: if !installed {
            Some("NOT_INSTALLED".to_owned())
        } else if config_source == CONFIG_SOURCE_NONE {
            Some("NOT_CONFIGURED".to_owned())
        } else {
            None
        },
        configurable: true,
        config_fields: tmdb_config_fields(),
        config_source: config_source.to_owned(),
    }
}
