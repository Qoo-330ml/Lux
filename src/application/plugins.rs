use std::{
    collections::HashSet,
    env, fmt, io,
    net::IpAddr,
    path::{Path, PathBuf},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde_json::{Map, Value, json};
use tokio::{fs, io::AsyncWriteExt};
use uuid::Uuid;

use crate::network::is_public_address;
use crate::{
    application::{
        plugin_protocol::{
            CONFIG_OPTIONS_SOURCE_MEDIA_LIBRARIES, IP_LOCATION_CAPABILITY, IpLocationRpcRequest,
            IpLocationRpcResult, MEDIA_PROBE_CAPABILITY, MediaProbeRpcResult,
            MediaProbeRpcStreamType, PLUGIN_CATEGORY_NETWORK, PLUGIN_CATEGORY_SCRAPER,
            PLUGIN_TYPE_IP_LOCATION, PLUGIN_TYPE_STRM_RESOLVER, PluginConfigField,
            PluginConfigOption, STRM_RESOLVE_CAPABILITY, STRM_RESOLVE_METHOD,
            StrmResolveRpcRequest, StrmResolveRpcResult, StrmResolveStatus,
        },
        plugin_runtime::{DiscoveredPlugin, PluginCatalog, PluginRuntimeError, PluginSupervisor},
        probe::{MediaProbeResult, MediaStreamResult, StreamType},
        schedule::{DEFAULT_STRM_MEDIA_INFO_SCHEDULE, validate_cron},
        settings::{
            TMDB_API_KEY_FILE, TMDB_TOKEN_FILE, TmdbSettings, read_tmdb_settings,
            tmdb_api_base_url_options, tmdb_language_options, write_tmdb_api_key,
            write_tmdb_settings,
        },
        tmdb::EMBEDDED_TMDB_API_KEY,
    },
    domain::ids::LibraryId,
    storage::{Database, StorageError},
};

pub const TMDB_PLUGIN_ID: &str = "tmdb";
pub const TMDB_DYNAMIC_PLUGIN_ID: &str = "org.lux.tmdb";
pub const MEDIA_INFO_PLUGIN_ID: &str = "org.lux.strm-media-info";
pub const MEDIA_INFO_LEGACY_PLUGIN_ID: &str = "org.lux.media-info";
pub const IP_HIOFD_PLUGIN_ID: &str = "org.lux.ip-hiofd";
pub const IP138_PLUGIN_ID: &str = "org.lux.qoo-ip138";
const TMDB_PLUGIN_NAME: &str = "TMDb 元数据插件";
const TMDB_PLUGIN_DESCRIPTION: &str = "使用 TMDb 补全电影和剧集元数据、海报与背景图。";
const TMDB_PLUGIN_VERSION: &str = "0.1.4";
const CONFIG_SOURCE_BUILT_IN: &str = "BUILT_IN";
const CONFIG_SOURCE_CUSTOM: &str = "CUSTOM";
const CONFIG_SOURCE_ENVIRONMENT: &str = "ENVIRONMENT";
const CONFIG_SOURCE_READ_ACCESS_TOKEN: &str = "READ_ACCESS_TOKEN";
const CONFIG_SOURCE_NONE: &str = "NONE";
const CONFIG_SOURCE_PLUGIN: &str = "PLUGIN_CONFIG";
const PLUGIN_CONFIG_DIR: &str = "plugin-config";
const MEDIA_INFO_EXISTING_INFO_POLICY_KEY: &str = "existingInfoPolicy";
const MEDIA_INFO_EXISTING_INFO_POLICY_SKIP: &str = "SKIP";
const MEDIA_INFO_EXISTING_INFO_POLICY_OVERWRITE: &str = "OVERWRITE";
const MAX_MEDIA_PROBE_THUMBNAIL_BYTES: usize = 8 * 1024 * 1024;
pub const DEFAULT_STRM_THUMBNAIL_POSITION_PERCENT: i64 = 30;
pub const MIN_STRM_THUMBNAIL_POSITION_PERCENT: i64 = 1;
pub const MAX_STRM_THUMBNAIL_POSITION_PERCENT: i64 = 99;

pub struct TmdbConfigUpdate<'a> {
    pub api_key: Option<&'a str>,
    pub preferred_language: Option<&'a str>,
    pub language_fallback_enabled: Option<bool>,
    pub fallback_languages: Option<Vec<String>>,
    pub alternate_api_enabled: Option<bool>,
    pub api_base_url: Option<&'a str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaInfoSettings {
    pub library_ids: Vec<LibraryId>,
    pub concurrency: i64,
    pub include_ready: bool,
    pub write_sidecars: bool,
    pub media_info_enabled: bool,
    pub thumbnail_enabled: bool,
    pub thumbnail_position_percent: i64,
    pub schedule: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaProbeOutput {
    pub media: MediaProbeResult,
    pub thumbnail_jpeg: Option<Vec<u8>>,
}
fn tmdb_config_fields() -> Vec<PluginConfigField> {
    let options = tmdb_language_options()
        .into_iter()
        .map(|option| PluginConfigOption {
            value: option.value,
            label: option.label,
        })
        .collect::<Vec<_>>();
    vec![
        PluginConfigField {
            key: "apiKey".to_owned(),
            label: "TMDb API Key".to_owned(),
            input_type: "password".to_owned(),
            required: false,
            sensitive: true,
            description: Some("可选。留空时使用 Lux 内置的 TMDb Key。".to_owned()),
            multiple: false,
            options: Vec::new(),
            options_source: None,
            default_value: None,
            minimum: None,
            maximum: None,
        },
        PluginConfigField {
            key: "preferredLanguage".to_owned(),
            label: "首选语言".to_owned(),
            input_type: "select".to_owned(),
            required: true,
            sensitive: false,
            description: Some("TMDb 电影、剧集、季和集元数据的首选语言。".to_owned()),
            multiple: false,
            options: options.clone(),
            options_source: None,
            default_value: None,
            minimum: None,
            maximum: None,
        },
        PluginConfigField {
            key: "languageFallbackEnabled".to_owned(),
            label: "TMDb 语言回退".to_owned(),
            input_type: "toggle".to_owned(),
            required: false,
            sensitive: false,
            description: Some("按备选语言顺序逐字段补全缺失元数据。".to_owned()),
            multiple: false,
            options: Vec::new(),
            options_source: None,
            default_value: None,
            minimum: None,
            maximum: None,
        },
        PluginConfigField {
            key: "fallbackLanguages".to_owned(),
            label: "备选语言顺序".to_owned(),
            input_type: "select".to_owned(),
            required: false,
            sensitive: false,
            description: Some("开启语言回退后按此顺序请求 TMDb。".to_owned()),
            multiple: true,
            options,
            options_source: None,
            default_value: None,
            minimum: None,
            maximum: None,
        },
        PluginConfigField {
            key: "alternateApiEnabled".to_owned(),
            label: "替代 API 地址".to_owned(),
            input_type: "toggle".to_owned(),
            required: false,
            sensitive: false,
            description: Some("开启后使用下方地址访问 TMDb，默认使用官方地址。".to_owned()),
            multiple: false,
            options: Vec::new(),
            options_source: None,
            default_value: None,
            minimum: None,
            maximum: None,
        },
        PluginConfigField {
            key: "apiBaseUrl".to_owned(),
            label: "TMDb API 地址".to_owned(),
            input_type: "select".to_owned(),
            required: true,
            sensitive: false,
            description: Some("可选择官方地址、替代地址，或填写自定义地址。".to_owned()),
            multiple: false,
            options: tmdb_api_base_url_options()
                .into_iter()
                .map(|option| PluginConfigOption {
                    value: option.value,
                    label: option.label,
                })
                .collect(),
            options_source: None,
            default_value: None,
            minimum: None,
            maximum: None,
        },
    ]
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
        Self::new_with_proxy(database, config_dir, None)
    }

    pub fn new_with_proxy(
        database: Database,
        config_dir: PathBuf,
        proxy_url: Option<String>,
    ) -> Self {
        let catalog = PluginCatalog::discover(&config_dir.join("plugins"));
        let supervisor = PluginSupervisor::new(catalog.clone())
            .with_config_dir(config_dir.clone())
            .with_network_proxy_url(proxy_url);
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
        self.ensure_builtin_plugins_installed().await?;
        let mut views = Vec::with_capacity(self.catalog.plugins.len() + 1);
        let has_tmdb_package = self
            .catalog
            .plugins
            .iter()
            .any(|plugin| is_tmdb_plugin_id(&plugin.manifest.id));
        if !has_tmdb_package {
            let status = self
                .database
                .plugin_installation_status(TMDB_PLUGIN_ID)
                .await?;
            let installed = status.is_some();
            let enabled = status == Some(true);
            if !installed_only || installed {
                views.push(
                    legacy_tmdb_view(
                        installed,
                        enabled,
                        self.tmdb_config_source().await,
                        &self.config_dir,
                    )
                    .await,
                );
            }
        }
        for plugin in &self.catalog.plugins {
            let status = self
                .database
                .plugin_installation_status(&plugin.manifest.id)
                .await?;
            let installed = status.is_some();
            let enabled = status == Some(true);
            if !installed_only || installed {
                views.push(self.dynamic_view(plugin, installed, enabled).await?);
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
        let plugin_id = self.canonical_plugin_id(plugin_id);
        self.ensure_known_plugin(&plugin_id)?;
        let was_installed = self.database.has_plugin_installation(&plugin_id).await?;
        self.database.install_plugin(&plugin_id).await?;
        if plugin_id == MEDIA_INFO_PLUGIN_ID {
            self.sync_media_info_scheduled_task().await?;
        }
        let plugin = self.view_for_id(&plugin_id, true, true).await?;
        Ok(PluginInstall {
            plugin,
            was_installed,
        })
    }

    pub async fn set_enabled(
        &self,
        plugin_id: &str,
        enabled: bool,
    ) -> Result<PluginView, PluginServiceError> {
        let plugin_id = self.canonical_plugin_id(plugin_id);
        self.ensure_known_plugin(&plugin_id)?;
        if !self.database.has_plugin_installation(&plugin_id).await? {
            return Err(PluginServiceError::Unavailable(plugin_id));
        }
        self.database
            .set_plugin_enabled(&plugin_id, enabled)
            .await?;
        if !enabled {
            self.supervisor.stop(&plugin_id).await;
        }
        if plugin_id == MEDIA_INFO_PLUGIN_ID {
            self.sync_media_info_scheduled_task().await?;
        }
        let (installed, enabled) = self.plugin_state(&plugin_id).await?;
        self.view_for_id(&plugin_id, installed, enabled).await
    }

    pub async fn update_config(
        &self,
        plugin_id: &str,
        update: TmdbConfigUpdate<'_>,
    ) -> Result<PluginView, PluginServiceError> {
        self.ensure_known_plugin(plugin_id)?;
        if !is_tmdb_plugin_id(plugin_id) {
            return Err(PluginServiceError::InvalidConfig);
        }
        if update
            .api_key
            .is_some_and(|value| value.trim().chars().count() > 4096)
        {
            return Err(PluginServiceError::InvalidConfig);
        }
        let current_settings = read_tmdb_settings(&self.config_dir).await;
        let settings = TmdbSettings::new_with_api_config(
            update
                .preferred_language
                .map(str::to_owned)
                .unwrap_or(current_settings.preferred_language),
            update
                .language_fallback_enabled
                .unwrap_or(current_settings.language_fallback_enabled),
            update
                .fallback_languages
                .unwrap_or(current_settings.fallback_languages),
            update
                .alternate_api_enabled
                .unwrap_or(current_settings.alternate_api_enabled),
            update
                .api_base_url
                .map(str::to_owned)
                .unwrap_or(current_settings.api_base_url),
        )
        .map_err(|_| PluginServiceError::InvalidConfig)?;
        if let Some(api_key) = update.api_key {
            let api_key = api_key.trim();
            write_tmdb_api_key(&self.config_dir, (!api_key.is_empty()).then_some(api_key))
                .await
                .map_err(PluginServiceError::ConfigIo)?;
        }
        write_tmdb_settings(&self.config_dir, &settings)
            .await
            .map_err(|error| match error {
                crate::application::settings::TmdbSettingsError::Io(error) => {
                    PluginServiceError::ConfigIo(error)
                }
                crate::application::settings::TmdbSettingsError::Invalid(_)
                | crate::application::settings::TmdbSettingsError::Serialization(_) => {
                    PluginServiceError::InvalidConfig
                }
            })?;
        let (installed, enabled) = self.plugin_state(plugin_id).await?;
        self.view_for_id(plugin_id, installed, enabled).await
    }

    pub async fn validate_selection(
        &self,
        scraper_id: Option<&str>,
    ) -> Result<(), PluginServiceError> {
        let Some(scraper_id) = scraper_id.map(str::trim).filter(|value| !value.is_empty()) else {
            return Ok(());
        };
        let scraper_id = self.canonical_plugin_id(scraper_id);
        self.ensure_known_plugin(&scraper_id)?;
        self.ensure_builtin_plugins_installed().await?;
        if !self.database.is_plugin_installed(&scraper_id).await? {
            return Err(PluginServiceError::Unavailable(scraper_id));
        }
        let view = self.view_for_id(&scraper_id, true, true).await?;
        if view.category != PLUGIN_CATEGORY_SCRAPER || !view.available {
            return Err(PluginServiceError::Unavailable(scraper_id));
        }
        Ok(())
    }

    pub async fn call(
        &self,
        plugin_id: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, PluginServiceError> {
        let plugin_id = self.canonical_plugin_id(plugin_id);
        self.ensure_known_plugin(&plugin_id)?;
        self.supervisor
            .call(&plugin_id, method, params)
            .await
            .map_err(PluginServiceError::Runtime)
    }

    pub async fn lookup_ip_location(
        &self,
        ip: IpAddr,
    ) -> Result<IpLocationRpcResult, PluginServiceError> {
        if !is_public_address(ip) {
            return Err(PluginServiceError::Unavailable("ip_location".to_owned()));
        }
        self.ensure_builtin_plugins_installed().await?;
        let query_ip = ip.to_string();
        let other_plugins = self.installed_other_ip_location_plugins().await?;
        let plugin_ids = if other_plugins.is_empty() {
            vec![IP138_PLUGIN_ID.to_owned()]
        } else {
            other_plugins
        };
        for plugin_id in plugin_ids {
            let Some(plugin) = self.catalog.get(&plugin_id) else {
                continue;
            };
            if !is_ip_location_plugin(plugin) {
                continue;
            }
            let value = match self
                .supervisor
                .call(
                    &plugin_id,
                    "ip.location",
                    serde_json::to_value(IpLocationRpcRequest {
                        ip: query_ip.clone(),
                    })
                    .map_err(|_| PluginServiceError::InvalidResponse)?,
                )
                .await
            {
                Ok(value) => value,
                Err(_) => continue,
            };
            if serde_json::to_vec(&value)
                .ok()
                .is_none_or(|bytes| bytes.len() > 64 * 1024)
            {
                continue;
            }
            let Ok(result) = serde_json::from_value::<IpLocationRpcResult>(value) else {
                continue;
            };
            if let Some(result) = normalize_ip_location_result(result, ip) {
                return Ok(result);
            }
        }
        Err(PluginServiceError::Unavailable("ip_location".to_owned()))
    }

    /// Calls a scraper by its persisted ID. The method is intentionally
    /// provider-neutral; a plugin selected by a library owns the upstream
    /// request details and the Lux process only speaks the scraper RPC.
    pub async fn call_scraper(
        &self,
        scraper_id: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, PluginServiceError> {
        self.call(scraper_id, method, params).await
    }

    pub async fn has_available_strm_resolver(&self) -> Result<bool, PluginServiceError> {
        Ok(!self.available_strm_resolver_ids().await?.is_empty())
    }

    pub async fn resolve_strm_target(
        &self,
        target: &str,
    ) -> Result<Option<String>, PluginServiceError> {
        let mut first_error = None;
        for plugin_id in self.available_strm_resolver_ids().await? {
            let request = StrmResolveRpcRequest {
                target: target.to_owned(),
            };
            let params =
                serde_json::to_value(request).map_err(|_| PluginServiceError::InvalidResponse)?;
            let value = match self
                .supervisor
                .call_isolated(&plugin_id, STRM_RESOLVE_METHOD, params)
                .await
            {
                Ok(value) => value,
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(PluginServiceError::Runtime(error));
                    }
                    continue;
                }
            };
            let result: StrmResolveRpcResult = match serde_json::from_value(value) {
                Ok(result) => result,
                Err(_) => {
                    if first_error.is_none() {
                        first_error = Some(PluginServiceError::InvalidResponse);
                    }
                    continue;
                }
            };
            match result.status {
                StrmResolveStatus::Unsupported if result.url.is_none() => {}
                StrmResolveStatus::Resolved => {
                    let Some(url) = result.url else {
                        if first_error.is_none() {
                            first_error = Some(PluginServiceError::InvalidResponse);
                        }
                        continue;
                    };
                    if validate_strm_resolver_url(&url) {
                        return Ok(Some(url));
                    }
                    if first_error.is_none() {
                        first_error = Some(PluginServiceError::InvalidResponse);
                    }
                }
                StrmResolveStatus::Unsupported => {
                    if first_error.is_none() {
                        first_error = Some(PluginServiceError::InvalidResponse);
                    }
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(None),
        }
    }

    async fn available_strm_resolver_ids(&self) -> Result<Vec<String>, PluginServiceError> {
        let mut plugin_ids = Vec::new();
        for plugin in &self.catalog.plugins {
            if !is_strm_resolver_plugin(plugin)
                || !self
                    .database
                    .is_plugin_installed(&plugin.manifest.id)
                    .await?
            {
                continue;
            }
            let view = self.dynamic_view(plugin, true, true).await?;
            if view.available {
                plugin_ids.push(plugin.manifest.id.clone());
            }
        }
        Ok(plugin_ids)
    }

    pub async fn update_dynamic_config(
        &self,
        plugin_id: &str,
        values: Map<String, Value>,
    ) -> Result<PluginView, PluginServiceError> {
        let plugin_id = self.canonical_plugin_id(plugin_id);
        self.ensure_known_plugin(&plugin_id)?;
        if is_tmdb_plugin_id(&plugin_id) {
            return Err(PluginServiceError::InvalidConfig);
        }
        let plugin = self
            .catalog
            .get(&plugin_id)
            .ok_or_else(|| PluginServiceError::UnknownPlugin(plugin_id.clone()))?;
        let fields = self.config_fields_for_plugin(plugin).await?;
        let values = merge_default_config_values(&fields, values);
        let values = normalize_plugin_config(&plugin_id, values);
        let values = validate_config_values(&fields, &values)?;
        if plugin_id == MEDIA_INFO_PLUGIN_ID {
            media_info_schedule(&values)?;
        }
        self.write_plugin_config(&plugin_id, &values).await?;
        if plugin_id == MEDIA_INFO_PLUGIN_ID {
            self.sync_media_info_scheduled_task().await?;
        }
        let (installed, enabled) = self.plugin_state(&plugin_id).await?;
        self.view_for_id(&plugin_id, installed, enabled).await
    }

    pub async fn update_media_info_schedule(
        &self,
        schedule: &str,
    ) -> Result<(), PluginServiceError> {
        let schedule = schedule.trim();
        validate_cron(schedule).map_err(|_| PluginServiceError::InvalidConfig)?;
        let plugin = self
            .catalog
            .get(MEDIA_INFO_PLUGIN_ID)
            .ok_or_else(|| PluginServiceError::UnknownPlugin(MEDIA_INFO_PLUGIN_ID.to_owned()))?;
        let fields = self.config_fields_for_plugin(plugin).await?;
        let mut values = merge_default_config_values(
            &fields,
            normalize_plugin_config(
                MEDIA_INFO_PLUGIN_ID,
                self.read_plugin_config(MEDIA_INFO_PLUGIN_ID).await?,
            ),
        );
        values.insert("schedule".to_owned(), Value::String(schedule.to_owned()));
        let values = validate_config_values(&fields, &values)?;
        media_info_schedule(&values)?;
        self.write_plugin_config(MEDIA_INFO_PLUGIN_ID, &values)
            .await?;
        self.sync_media_info_scheduled_task().await
    }

    pub async fn media_info_settings(&self) -> Result<MediaInfoSettings, PluginServiceError> {
        let plugin = self
            .catalog
            .get(MEDIA_INFO_PLUGIN_ID)
            .ok_or_else(|| PluginServiceError::UnknownPlugin(MEDIA_INFO_PLUGIN_ID.to_owned()))?;
        let fields = self.config_fields_for_plugin(plugin).await?;
        let values = merge_default_config_values(
            &fields,
            self.read_plugin_config(MEDIA_INFO_PLUGIN_ID).await?,
        );
        let values = validate_config_values(&fields, &values)?;
        let library_ids = values
            .get("libraryIds")
            .and_then(Value::as_array)
            .ok_or(PluginServiceError::InvalidConfig)?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .ok_or(PluginServiceError::InvalidConfig)
                    .and_then(|value| {
                        value
                            .parse::<LibraryId>()
                            .map_err(|_| PluginServiceError::InvalidConfig)
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let existing_info_policy = values
            .get(MEDIA_INFO_EXISTING_INFO_POLICY_KEY)
            .and_then(Value::as_str)
            .ok_or(PluginServiceError::InvalidConfig)?;
        let include_ready = match existing_info_policy {
            MEDIA_INFO_EXISTING_INFO_POLICY_SKIP => false,
            MEDIA_INFO_EXISTING_INFO_POLICY_OVERWRITE => true,
            _ => return Err(PluginServiceError::InvalidConfig),
        };
        let schedule = media_info_schedule(&values)?;
        Ok(MediaInfoSettings {
            library_ids,
            concurrency: values
                .get("concurrency")
                .and_then(Value::as_i64)
                .ok_or(PluginServiceError::InvalidConfig)?,
            include_ready,
            write_sidecars: values
                .get("writeSidecars")
                .and_then(Value::as_bool)
                .ok_or(PluginServiceError::InvalidConfig)?,
            media_info_enabled: optional_bool_config(&values, "mediaInfoEnabled", true)?,
            thumbnail_enabled: optional_bool_config(&values, "thumbnailEnabled", false)?,
            thumbnail_position_percent: optional_i64_config(
                &values,
                "thumbnailPositionPercent",
                DEFAULT_STRM_THUMBNAIL_POSITION_PERCENT,
                MIN_STRM_THUMBNAIL_POSITION_PERCENT,
                MAX_STRM_THUMBNAIL_POSITION_PERCENT,
            )?,
            schedule,
        })
    }

    pub async fn sync_media_info_scheduled_task(&self) -> Result<(), PluginServiceError> {
        let (installed, enabled) = self.plugin_state(MEDIA_INFO_PLUGIN_ID).await?;
        if !installed {
            return Ok(());
        }
        let plugin = self
            .catalog
            .get(MEDIA_INFO_PLUGIN_ID)
            .ok_or_else(|| PluginServiceError::UnknownPlugin(MEDIA_INFO_PLUGIN_ID.to_owned()))?;
        let fields = self.config_fields_for_plugin(plugin).await?;
        let values = merge_default_config_values(
            &fields,
            normalize_plugin_config(
                MEDIA_INFO_PLUGIN_ID,
                self.read_plugin_config(MEDIA_INFO_PLUGIN_ID).await?,
            ),
        );
        let schedule = media_info_schedule(&values)
            .unwrap_or_else(|_| DEFAULT_STRM_MEDIA_INFO_SCHEDULE.to_owned());
        let configured = validate_config_values(&fields, &values).is_ok()
            && self.media_info_settings().await.is_ok();
        self.database
            .upsert_strm_media_info_task(&schedule, enabled && configured)
            .await?;
        Ok(())
    }

    async fn config_fields_for_plugin(
        &self,
        plugin: &DiscoveredPlugin,
    ) -> Result<Vec<PluginConfigField>, PluginServiceError> {
        let mut fields = plugin.manifest.config_fields.clone();
        if fields.iter().any(|field| {
            field.options_source.as_deref() == Some(CONFIG_OPTIONS_SOURCE_MEDIA_LIBRARIES)
        }) {
            let options = self
                .database
                .list_libraries()
                .await?
                .into_iter()
                .filter(|library| library.is_enabled)
                .map(|library| PluginConfigOption {
                    value: library.id,
                    label: library.name,
                })
                .collect::<Vec<_>>();
            for field in &mut fields {
                if field.options_source.as_deref() == Some(CONFIG_OPTIONS_SOURCE_MEDIA_LIBRARIES) {
                    field.options = options.clone();
                }
            }
        }
        Ok(fields)
    }

    async fn read_plugin_config(
        &self,
        plugin_id: &str,
    ) -> Result<Map<String, Value>, PluginServiceError> {
        let path = plugin_config_path(&self.config_dir, plugin_id);
        let (contents, should_migrate) = match fs::read_to_string(&path).await {
            Ok(contents) => (Some(contents), false),
            Err(error)
                if error.kind() == io::ErrorKind::NotFound && plugin_id == MEDIA_INFO_PLUGIN_ID =>
            {
                match fs::read_to_string(plugin_config_path(
                    &self.config_dir,
                    MEDIA_INFO_LEGACY_PLUGIN_ID,
                ))
                .await
                {
                    Ok(contents) => (Some(contents), true),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => (None, false),
                    Err(error) => return Err(PluginServiceError::ConfigIo(error)),
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => (None, false),
            Err(error) => return Err(PluginServiceError::ConfigIo(error)),
        };
        let Some(contents) = contents else {
            return Ok(Map::new());
        };
        let values =
            serde_json::from_str(&contents).map_err(|_| PluginServiceError::InvalidConfig)?;
        let values = normalize_plugin_config(plugin_id, values);
        if should_migrate {
            self.write_plugin_config(plugin_id, &values).await?;
        }
        Ok(values)
    }

    async fn write_plugin_config(
        &self,
        plugin_id: &str,
        values: &Map<String, Value>,
    ) -> Result<(), PluginServiceError> {
        let directory = self.config_dir.join(PLUGIN_CONFIG_DIR);
        fs::create_dir_all(&directory)
            .await
            .map_err(PluginServiceError::ConfigIo)?;
        let path = plugin_config_path(&self.config_dir, plugin_id);
        let temporary = directory.join(format!(".{plugin_id}.{}.tmp", Uuid::now_v7()));
        let contents =
            serde_json::to_vec_pretty(values).map_err(|_| PluginServiceError::InvalidConfig)?;
        let mut file = fs::File::create(&temporary)
            .await
            .map_err(PluginServiceError::ConfigIo)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = file
                .metadata()
                .await
                .map_err(PluginServiceError::ConfigIo)?
                .permissions();
            permissions.set_mode(0o600);
            fs::set_permissions(&temporary, permissions)
                .await
                .map_err(PluginServiceError::ConfigIo)?;
        }
        file.write_all(&contents)
            .await
            .map_err(PluginServiceError::ConfigIo)?;
        file.sync_all()
            .await
            .map_err(PluginServiceError::ConfigIo)?;
        drop(file);
        fs::rename(&temporary, &path)
            .await
            .map_err(PluginServiceError::ConfigIo)
    }

    pub async fn probe_media(&self, url: &str) -> Result<MediaProbeResult, PluginServiceError> {
        self.probe_media_with_options(url, true, false, DEFAULT_STRM_THUMBNAIL_POSITION_PERCENT)
            .await
            .map(|output| output.media)
    }

    pub async fn probe_media_with_options(
        &self,
        url: &str,
        include_media_info: bool,
        include_thumbnail: bool,
        thumbnail_position_percent: i64,
    ) -> Result<MediaProbeOutput, PluginServiceError> {
        let plugin = self
            .catalog
            .get(MEDIA_INFO_PLUGIN_ID)
            .ok_or_else(|| PluginServiceError::UnknownPlugin(MEDIA_INFO_PLUGIN_ID.to_owned()))?;
        if !plugin
            .manifest
            .capabilities
            .iter()
            .any(|capability| capability == MEDIA_PROBE_CAPABILITY)
            || !self
                .database
                .is_plugin_installed(MEDIA_INFO_PLUGIN_ID)
                .await?
        {
            return Err(PluginServiceError::Unavailable(
                MEDIA_INFO_PLUGIN_ID.to_owned(),
            ));
        }
        let value = self
            .supervisor
            .call_isolated(
                MEDIA_INFO_PLUGIN_ID,
                "media.probe",
                serde_json::json!({
                    "url": url,
                    "includeMediaInfo": include_media_info,
                    "includeThumbnail": include_thumbnail,
                    "thumbnailPositionPercent": thumbnail_position_percent,
                }),
            )
            .await
            .map_err(PluginServiceError::Runtime)?;
        let response: MediaProbeRpcResult =
            serde_json::from_value(value).map_err(|_| PluginServiceError::InvalidResponse)?;
        media_probe_output(response).ok_or(PluginServiceError::InvalidResponse)
    }

    pub async fn scraper_client(
        &self,
        scraper_id: &str,
    ) -> Result<crate::application::scraper::ScraperPluginClient, PluginServiceError> {
        let plugin_id = self.canonical_plugin_id(scraper_id);
        self.ensure_known_plugin(&plugin_id)?;
        self.ensure_builtin_plugins_installed().await?;
        if plugin_id == TMDB_PLUGIN_ID {
            return Err(PluginServiceError::Unavailable(plugin_id));
        }
        let (installed, enabled) = self.plugin_state(&plugin_id).await?;
        let view = self.view_for_id(&plugin_id, installed, enabled).await?;
        if view.category != PLUGIN_CATEGORY_SCRAPER || !view.available {
            return Err(PluginServiceError::Unavailable(plugin_id));
        }
        Ok(crate::application::scraper::ScraperPluginClient::new(
            self.clone(),
            plugin_id,
        ))
    }

    pub async fn restart(&self, plugin_id: &str) {
        let plugin_id = self.canonical_plugin_id(plugin_id);
        self.supervisor.stop(&plugin_id).await;
    }

    pub async fn stop_all(&self) {
        self.supervisor.stop_all().await;
    }

    async fn view_for_id(
        &self,
        plugin_id: &str,
        installed: bool,
        enabled: bool,
    ) -> Result<PluginView, PluginServiceError> {
        if plugin_id == TMDB_PLUGIN_ID {
            return Ok(legacy_tmdb_view(
                installed,
                enabled,
                self.tmdb_config_source().await,
                &self.config_dir,
            )
            .await);
        }
        let Some(plugin) = self.catalog.get(plugin_id) else {
            return Err(PluginServiceError::UnknownPlugin(plugin_id.to_owned()));
        };
        self.dynamic_view(plugin, installed, enabled).await
    }

    async fn plugin_state(&self, plugin_id: &str) -> Result<(bool, bool), PluginServiceError> {
        let status = self.database.plugin_installation_status(plugin_id).await?;
        Ok((status.is_some(), status == Some(true)))
    }

    async fn ensure_builtin_plugins_installed(&self) -> Result<(), PluginServiceError> {
        for plugin_id in [TMDB_DYNAMIC_PLUGIN_ID, IP138_PLUGIN_ID] {
            if self.catalog.get(plugin_id).is_some()
                && !self.database.has_plugin_installation(plugin_id).await?
            {
                self.database.install_plugin(plugin_id).await?;
            }
        }
        Ok(())
    }

    async fn installed_other_ip_location_plugins(&self) -> Result<Vec<String>, PluginServiceError> {
        let mut plugin_ids = Vec::new();
        for plugin_id in [IP_HIOFD_PLUGIN_ID] {
            if let Some(plugin) = self.catalog.get(plugin_id)
                && is_ip_location_plugin(plugin)
                && self.database.is_plugin_installed(plugin_id).await?
            {
                plugin_ids.push(plugin_id.to_owned());
            }
        }
        for plugin in &self.catalog.plugins {
            if plugin.manifest.id == IP138_PLUGIN_ID
                || plugin.manifest.id == IP_HIOFD_PLUGIN_ID
                || !is_ip_location_plugin(plugin)
                || !self
                    .database
                    .is_plugin_installed(&plugin.manifest.id)
                    .await?
            {
                continue;
            }
            plugin_ids.push(plugin.manifest.id.clone());
        }
        Ok(plugin_ids)
    }

    async fn dynamic_view(
        &self,
        plugin: &DiscoveredPlugin,
        installed: bool,
        enabled: bool,
    ) -> Result<PluginView, PluginServiceError> {
        let runtime = self.supervisor.status(&plugin.manifest.id).await;
        let disabled_by_other_ip_provider = plugin.manifest.id == IP138_PLUGIN_ID
            && !self.installed_other_ip_location_plugins().await?.is_empty();
        let config_source = if is_tmdb_plugin_id(&plugin.manifest.id) {
            self.tmdb_config_source().await.to_owned()
        } else if plugin.manifest.config_fields.is_empty() {
            CONFIG_SOURCE_NONE.to_owned()
        } else {
            CONFIG_SOURCE_PLUGIN.to_owned()
        };
        let config_fields = self.config_fields_for_plugin(plugin).await?;
        let stored_values = self.read_plugin_config(&plugin.manifest.id).await?;
        let config_values = merge_default_config_values(&config_fields, stored_values);
        let public_config_values = public_config_values(&config_fields, &config_values);
        let configured = if is_tmdb_plugin_id(&plugin.manifest.id) {
            config_source != CONFIG_SOURCE_NONE
        } else {
            config_fields.is_empty()
                || validate_config_values(&config_fields, &config_values).is_ok()
        };
        let enabled = installed && enabled && !disabled_by_other_ip_provider;
        let available = enabled && configured;
        Ok(PluginView {
            id: plugin.manifest.id.clone(),
            name: plugin.manifest.name.clone(),
            description: plugin.manifest.description.clone().unwrap_or_default(),
            category: plugin.manifest.category.clone(),
            version: Some(plugin.manifest.version.clone()),
            runtime: Some(plugin.manifest.runtime.kind.clone()),
            capabilities: plugin.manifest.capabilities.clone(),
            status: if disabled_by_other_ip_provider {
                "DISABLED".to_owned()
            } else if runtime.running {
                "RUNNING".to_owned()
            } else if runtime.last_error.is_some() {
                "ERROR".to_owned()
            } else if enabled {
                "READY".to_owned()
            } else if installed {
                "DISABLED".to_owned()
            } else {
                "AVAILABLE".to_owned()
            },
            running: runtime.running,
            last_error: runtime.last_error,
            installed,
            enabled,
            configured,
            available,
            unavailable_reason: if disabled_by_other_ip_provider {
                Some("OTHER_IP_LOCATION_PLUGIN_INSTALLED".to_owned())
            } else if !installed {
                Some("NOT_INSTALLED".to_owned())
            } else if !enabled {
                Some("DISABLED".to_owned())
            } else if !configured {
                Some("NOT_CONFIGURED".to_owned())
            } else {
                None
            },
            configurable: !config_fields.is_empty(),
            config_fields: if is_tmdb_plugin_id(&plugin.manifest.id) {
                tmdb_config_fields()
            } else {
                config_fields
            },
            config_source,
            config_values: if is_tmdb_plugin_id(&plugin.manifest.id) {
                tmdb_config_values(&self.config_dir).await
            } else {
                public_config_values
            },
        })
    }

    fn ensure_known_plugin(&self, plugin_id: &str) -> Result<(), PluginServiceError> {
        if plugin_id == TMDB_PLUGIN_ID || self.catalog.get(plugin_id).is_some() {
            Ok(())
        } else {
            Err(PluginServiceError::UnknownPlugin(plugin_id.to_owned()))
        }
    }

    fn canonical_plugin_id(&self, plugin_id: &str) -> String {
        let plugin_id = plugin_id.trim();
        if plugin_id == MEDIA_INFO_LEGACY_PLUGIN_ID {
            MEDIA_INFO_PLUGIN_ID.to_owned()
        } else if plugin_id == TMDB_PLUGIN_ID && self.catalog.get(TMDB_DYNAMIC_PLUGIN_ID).is_some()
        {
            TMDB_DYNAMIC_PLUGIN_ID.to_owned()
        } else {
            plugin_id.to_owned()
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

fn normalize_plugin_config(plugin_id: &str, mut values: Map<String, Value>) -> Map<String, Value> {
    if plugin_id != MEDIA_INFO_PLUGIN_ID {
        return values;
    }
    let legacy_include_ready = values.remove("includeReady");
    if !values.contains_key(MEDIA_INFO_EXISTING_INFO_POLICY_KEY) {
        if let Some(include_ready) = legacy_include_ready.and_then(|value| value.as_bool()) {
            let policy = if include_ready {
                MEDIA_INFO_EXISTING_INFO_POLICY_OVERWRITE
            } else {
                MEDIA_INFO_EXISTING_INFO_POLICY_SKIP
            };
            values.insert(
                MEDIA_INFO_EXISTING_INFO_POLICY_KEY.to_owned(),
                json!(policy),
            );
        }
    }
    values
}

fn media_info_schedule(values: &Map<String, Value>) -> Result<String, PluginServiceError> {
    let schedule = values
        .get("schedule")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_STRM_MEDIA_INFO_SCHEDULE)
        .trim();
    validate_cron(schedule).map_err(|_| PluginServiceError::InvalidConfig)?;
    Ok(schedule.to_owned())
}

fn optional_bool_config(
    values: &Map<String, Value>,
    key: &str,
    default: bool,
) -> Result<bool, PluginServiceError> {
    values
        .get(key)
        .map(Value::as_bool)
        .unwrap_or(Some(default))
        .ok_or(PluginServiceError::InvalidConfig)
}

fn optional_i64_config(
    values: &Map<String, Value>,
    key: &str,
    default: i64,
    minimum: i64,
    maximum: i64,
) -> Result<i64, PluginServiceError> {
    let value = match values.get(key) {
        Some(value) => value.as_i64().ok_or(PluginServiceError::InvalidConfig)?,
        None => default,
    };
    (minimum..=maximum)
        .contains(&value)
        .then_some(value)
        .ok_or(PluginServiceError::InvalidConfig)
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

fn plugin_config_path(config_dir: &Path, plugin_id: &str) -> PathBuf {
    config_dir
        .join(PLUGIN_CONFIG_DIR)
        .join(format!("{plugin_id}.json"))
}

fn merge_default_config_values(
    fields: &[PluginConfigField],
    stored: Map<String, Value>,
) -> Map<String, Value> {
    let mut values = Map::new();
    for field in fields {
        if let Some(default_value) = field.default_value.clone() {
            values.insert(field.key.clone(), default_value);
        }
    }
    values.extend(stored);
    values
}

fn public_config_values(
    fields: &[PluginConfigField],
    values: &Map<String, Value>,
) -> Map<String, Value> {
    fields
        .iter()
        .filter(|field| !field.sensitive)
        .filter_map(|field| {
            values
                .get(&field.key)
                .cloned()
                .map(|value| (field.key.clone(), value))
        })
        .collect()
}

fn validate_config_values(
    fields: &[PluginConfigField],
    values: &Map<String, Value>,
) -> Result<Map<String, Value>, PluginServiceError> {
    let allowed = fields
        .iter()
        .map(|field| field.key.as_str())
        .collect::<HashSet<_>>();
    if values.keys().any(|key| !allowed.contains(key.as_str())) {
        return Err(PluginServiceError::InvalidConfig);
    }
    let mut normalized = Map::new();
    for field in fields {
        let Some(value) = values.get(&field.key) else {
            if field.required {
                return Err(PluginServiceError::InvalidConfig);
            }
            continue;
        };
        match field.input_type.as_str() {
            "text" | "password" => {
                if value
                    .as_str()
                    .is_none_or(|value| value.chars().count() > 4096)
                {
                    return Err(PluginServiceError::InvalidConfig);
                }
            }
            "toggle" => {
                if !value.is_boolean() {
                    return Err(PluginServiceError::InvalidConfig);
                }
            }
            "number" => {
                let Some(value) = value.as_i64() else {
                    return Err(PluginServiceError::InvalidConfig);
                };
                if field.minimum.is_some_and(|minimum| value < minimum)
                    || field.maximum.is_some_and(|maximum| value > maximum)
                {
                    return Err(PluginServiceError::InvalidConfig);
                }
            }
            "select" => {
                if field.multiple {
                    let Some(values) = value.as_array() else {
                        return Err(PluginServiceError::InvalidConfig);
                    };
                    if field.required && values.is_empty() {
                        return Err(PluginServiceError::InvalidConfig);
                    }
                    if values
                        .iter()
                        .any(|value| !select_option_is_valid(field, value))
                    {
                        return Err(PluginServiceError::InvalidConfig);
                    }
                } else if !select_option_is_valid(field, value) {
                    return Err(PluginServiceError::InvalidConfig);
                }
            }
            _ => return Err(PluginServiceError::InvalidConfig),
        }
        normalized.insert(field.key.clone(), value.clone());
    }
    Ok(normalized)
}

fn select_option_is_valid(field: &PluginConfigField, value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|value| field.options.iter().any(|option| option.value == value))
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
    pub config_values: serde_json::Map<String, Value>,
}

#[derive(Debug)]
pub enum PluginServiceError {
    UnknownPlugin(String),
    Unavailable(String),
    InvalidConfig,
    InvalidResponse,
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
            Self::InvalidResponse => formatter.write_str("plugin returned an invalid response"),
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
            Self::UnknownPlugin(_)
            | Self::Unavailable(_)
            | Self::InvalidConfig
            | Self::InvalidResponse => None,
        }
    }
}

fn media_probe_output(response: MediaProbeRpcResult) -> Option<MediaProbeOutput> {
    if response
        .container
        .as_ref()
        .is_some_and(|value| value.len() > 128)
        || response.source_size.is_some_and(|value| value < 0)
        || response.duration_ticks.is_some_and(|value| value < 0)
        || response.bitrate.is_some_and(|value| value < 0)
        || response.streams.len() > 128
    {
        return None;
    }
    let mut indexes = HashSet::new();
    let streams = response
        .streams
        .into_iter()
        .map(|stream| {
            if stream.stream_index < 0
                || !indexes.insert(stream.stream_index)
                || stream.codec.as_ref().is_some_and(|value| value.len() > 256)
                || stream
                    .language
                    .as_ref()
                    .is_some_and(|value| value.len() > 256)
                || stream.title.as_ref().is_some_and(|value| value.len() > 512)
                || serde_json::to_vec(&stream.details)
                    .ok()
                    .is_none_or(|value| value.len() > 512 * 1024)
            {
                return None;
            }
            let stream_type = match stream.stream_type {
                MediaProbeRpcStreamType::Video => StreamType::Video,
                MediaProbeRpcStreamType::Audio => StreamType::Audio,
                MediaProbeRpcStreamType::Subtitle => StreamType::Subtitle,
            };
            Some(MediaStreamResult {
                stream_index: stream.stream_index,
                stream_type,
                codec: stream.codec,
                language: stream.language,
                title: stream.title,
                is_default: stream.is_default,
                is_forced: stream.is_forced,
                details: stream.details,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let thumbnail_jpeg = match response.thumbnail_jpeg_base64 {
        Some(value)
            if value.len() <= (MAX_MEDIA_PROBE_THUMBNAIL_BYTES * 4 / 3).saturating_add(4) =>
        {
            Some(BASE64.decode(value).ok()?)
        }
        Some(_) => return None,
        None => None,
    };
    if thumbnail_jpeg
        .as_ref()
        .is_some_and(|value| !is_valid_jpeg(value))
    {
        return None;
    }
    Some(MediaProbeOutput {
        media: MediaProbeResult {
            container: response.container,
            source_size: response.source_size,
            duration_ticks: response.duration_ticks,
            bitrate: response.bitrate,
            streams,
        },
        thumbnail_jpeg,
    })
}

fn is_valid_jpeg(bytes: &[u8]) -> bool {
    bytes.len() <= MAX_MEDIA_PROBE_THUMBNAIL_BYTES
        && bytes.len() >= 4
        && bytes.starts_with(&[0xff, 0xd8])
        && bytes.ends_with(&[0xff, 0xd9])
}

const MAX_IP_LOCATION_FIELD_CHARS: usize = 256;

fn normalize_ip_location_result(
    mut result: IpLocationRpcResult,
    query_ip: IpAddr,
) -> Option<IpLocationRpcResult> {
    if result.ip.trim().parse::<IpAddr>().ok() != Some(query_ip) {
        return None;
    }
    result.ip = query_ip.to_string();
    result.country = normalize_ip_location_field(result.country);
    result.province = normalize_ip_location_field(result.province);
    result.city = normalize_ip_location_field(result.city);
    result.district = normalize_ip_location_field(result.district);
    result.street = normalize_ip_location_field(result.street);
    result.isp = normalize_ip_location_field(result.isp);
    result.latitude = normalize_ip_location_field(result.latitude);
    result.longitude = normalize_ip_location_field(result.longitude);
    Some(result)
}

fn normalize_ip_location_field(value: Option<String>) -> Option<String> {
    let value = value?.trim().to_owned();
    (!value.is_empty()
        && value.chars().count() <= MAX_IP_LOCATION_FIELD_CHARS
        && !value.chars().any(char::is_control))
    .then_some(value)
}

pub fn validate_strm_resolver_url(value: &str) -> bool {
    if value.chars().count() > 8 * 1024
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return false;
    }
    let Some((_, authority_and_path)) = value.split_once("://") else {
        return false;
    };
    if authority_and_path.starts_with('/') {
        return false;
    }
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some_and(|host| !host.is_empty())
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
}

fn is_ip_location_plugin(plugin: &DiscoveredPlugin) -> bool {
    plugin.manifest.plugin_type == PLUGIN_TYPE_IP_LOCATION
        && plugin.manifest.category == PLUGIN_CATEGORY_NETWORK
        && plugin
            .manifest
            .capabilities
            .iter()
            .any(|capability| capability == IP_LOCATION_CAPABILITY)
}

fn is_strm_resolver_plugin(plugin: &DiscoveredPlugin) -> bool {
    plugin.manifest.plugin_type == PLUGIN_TYPE_STRM_RESOLVER
        && plugin.manifest.category == crate::application::plugin_protocol::PLUGIN_CATEGORY_MEDIA
        && plugin
            .manifest
            .capabilities
            .iter()
            .any(|capability| capability == STRM_RESOLVE_CAPABILITY)
}

impl From<StorageError> for PluginServiceError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

async fn legacy_tmdb_view(
    installed: bool,
    enabled: bool,
    config_source: &str,
    config_dir: &std::path::Path,
) -> PluginView {
    PluginView {
        id: TMDB_PLUGIN_ID.to_owned(),
        name: TMDB_PLUGIN_NAME.to_owned(),
        description: TMDB_PLUGIN_DESCRIPTION.to_owned(),
        category: PLUGIN_CATEGORY_SCRAPER.to_owned(),
        version: Some(TMDB_PLUGIN_VERSION.to_owned()),
        runtime: Some("built-in".to_owned()),
        capabilities: vec![
            "metadata.search".to_owned(),
            "metadata.get".to_owned(),
            "metadata.images".to_owned(),
            "metadata.credits".to_owned(),
            "metadata.externalIds".to_owned(),
            "metadata.trailers".to_owned(),
        ],
        status: if installed && enabled {
            "BUILT_IN_COMPATIBILITY".to_owned()
        } else if installed {
            "DISABLED".to_owned()
        } else {
            "BUILT_IN_COMPATIBILITY".to_owned()
        },
        running: true,
        last_error: None,
        installed,
        enabled,
        configured: config_source != CONFIG_SOURCE_NONE,
        available: enabled && config_source != CONFIG_SOURCE_NONE,
        unavailable_reason: if !installed {
            Some("NOT_INSTALLED".to_owned())
        } else if !enabled {
            Some("DISABLED".to_owned())
        } else if config_source == CONFIG_SOURCE_NONE {
            Some("NOT_CONFIGURED".to_owned())
        } else {
            None
        },
        configurable: true,
        config_fields: tmdb_config_fields(),
        config_source: config_source.to_owned(),
        config_values: tmdb_config_values(config_dir).await,
    }
}

async fn tmdb_config_values(config_dir: &std::path::Path) -> serde_json::Map<String, Value> {
    let settings = read_tmdb_settings(config_dir).await;
    serde_json::Map::from_iter([
        (
            "preferredLanguage".to_owned(),
            Value::String(settings.preferred_language),
        ),
        (
            "languageFallbackEnabled".to_owned(),
            Value::Bool(settings.language_fallback_enabled),
        ),
        (
            "fallbackLanguages".to_owned(),
            Value::Array(
                settings
                    .fallback_languages
                    .into_iter()
                    .map(Value::String)
                    .collect(),
            ),
        ),
        (
            "alternateApiEnabled".to_owned(),
            Value::Bool(settings.alternate_api_enabled),
        ),
        (
            "apiBaseUrl".to_owned(),
            Value::String(settings.api_base_url),
        ),
    ])
}
