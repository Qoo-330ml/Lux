use std::{
    collections::HashSet,
    env, fmt, io,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde_json::{Map, Value, json};
use tokio::{fs, io::AsyncWriteExt, sync::RwLock};
use uuid::Uuid;

use crate::network::is_public_address;
use crate::{
    application::provider_cache::ProviderResponseCache,
    application::{
        plugin_protocol::{
            CHAPTER_DETECT_CAPABILITY, CHAPTER_DETECT_METHOD,
            CHAPTER_FINGERPRINT_POINT_DURATION_TICKS, CHAPTER_FINGERPRINT_SAMPLE_RATE,
            CHAPTER_LOOKUP_CAPABILITY, CHAPTER_LOOKUP_METHOD,
            CONFIG_OPTIONS_SOURCE_MEDIA_LIBRARIES, ChapterDetectRpcRequest, ChapterDetectRpcResult,
            ChapterLookupRpcRequest, ChapterLookupRpcResult, DANMAKU_MATCH_CAPABILITY,
            DANMAKU_MATCH_METHOD, DanmakuMatchRpcRequest, DanmakuMatchRpcResult,
            DanmakuMatchStatus, IP_LOCATION_CAPABILITY, IpLocationRpcRequest, IpLocationRpcResult,
            MEDIA_PROBE_CAPABILITY, MediaProbeRpcResult, MediaProbeRpcStreamType,
            NOTIFICATION_SEND_CAPABILITY, NOTIFICATION_SEND_METHOD, NotificationSendRpcResult,
            PLUGIN_CATEGORY_MEDIA, PLUGIN_CATEGORY_NETWORK, PLUGIN_CATEGORY_NOTIFICATION,
            PLUGIN_CATEGORY_SCRAPER, PLUGIN_TYPE_CHAPTER_DETECTOR, PLUGIN_TYPE_DANMAKU,
            PLUGIN_TYPE_IP_LOCATION, PLUGIN_TYPE_NOTIFICATION, PLUGIN_TYPE_STRM_RESOLVER,
            PluginConfigField, PluginConfigOption, STRM_RESOLVE_CAPABILITY, STRM_RESOLVE_METHOD,
            StrmResolveRpcRequest, StrmResolveRpcResult, StrmResolveStatus,
        },
        plugin_runtime::{DiscoveredPlugin, PluginCatalog, PluginRuntimeError, PluginSupervisor},
        plugin_store::{PluginStore, PluginStoreEntry, PluginStoreError, PluginStoreIndex},
        probe::{MediaProbeResult, MediaStreamResult, StreamType},
        schedule::{
            DEFAULT_CHAPTER_DETECTION_SCHEDULE, DEFAULT_ONLINE_CHAPTER_DETECTION_SCHEDULE,
            DEFAULT_STRM_MEDIA_INFO_SCHEDULE, validate_cron,
        },
        settings::{
            TMDB_API_KEY_FILE, TMDB_SETTINGS_FILE, TMDB_TOKEN_FILE, TmdbSettings,
            read_tmdb_settings, tmdb_api_base_url_options, tmdb_language_options,
            write_tmdb_api_key, write_tmdb_settings,
        },
        strm_target::{StrmTargetKind, classify_strm_target},
    },
    domain::ids::LibraryId,
    storage::{Database, StorageError},
};

pub const TMDB_PLUGIN_ID: &str = "tmdb";
pub const TMDB_DYNAMIC_PLUGIN_ID: &str = "org.lux.tmdb";
pub const MEDIA_INFO_PLUGIN_ID: &str = "org.lux.strm-media-info";
pub const CHAPTER_DETECTOR_PLUGIN_ID: &str = "org.lux.intro-outro-detector";
const THEINTRODB_CHAPTER_SOURCE_ID: &str = "org.lux.theintrodb-chapter-source";
pub const MEDIA_INFO_LEGACY_PLUGIN_ID: &str = "org.lux.media-info";
pub const IP_HIOFD_PLUGIN_ID: &str = "org.lux.ip-hiofd";
pub const IP138_PLUGIN_ID: &str = "org.lux.qoo-ip138";
pub const DANMAKU_PLUGIN_ID: &str = "org.lux.danmaku";
const CONFIG_SOURCE_CUSTOM: &str = "CUSTOM";
const CONFIG_SOURCE_ENVIRONMENT: &str = "ENVIRONMENT";
const CONFIG_SOURCE_READ_ACCESS_TOKEN: &str = "READ_ACCESS_TOKEN";
const CONFIG_SOURCE_PLUGIN_DEFAULT: &str = "PLUGIN_DEFAULT";
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
    pub title_alias_replacement_enabled: Option<bool>,
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
pub struct ChapterDetectorSettings {
    pub concurrency: i64,
    pub intro_window_seconds: i64,
    pub credits_window_seconds: i64,
    pub match_threshold: u32,
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
            description: Some("可选。留空时使用 TMDb 插件自己的默认凭据。".to_owned()),
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
        PluginConfigField {
            key: "titleAliasReplacementEnabled".to_owned(),
            label: "标题别名替换".to_owned(),
            input_type: "toggle".to_owned(),
            required: false,
            sensitive: false,
            description: Some("当tmdb语言检索不到中文名称时，尝试使用中文别名替换".to_owned()),
            multiple: false,
            options: Vec::new(),
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
    catalog: Arc<RwLock<PluginCatalog>>,
    supervisor: PluginSupervisor,
    store: Option<PluginStore>,
    provider_cache: ProviderResponseCache,
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
        let catalog = Arc::new(RwLock::new(PluginCatalog::discover(
            &config_dir.join("plugins"),
        )));
        let supervisor = PluginSupervisor::new_with_shared_catalog(catalog.clone())
            .with_config_dir(config_dir.clone())
            .with_network_proxy_url(proxy_url.clone());
        let store = PluginStore::new(config_dir.clone(), proxy_url).ok();
        Self {
            database,
            config_dir: config_dir.clone(),
            catalog,
            supervisor,
            store,
            provider_cache: ProviderResponseCache::new(Some(
                config_dir.join("metadata/provider-responses.json"),
            )),
        }
    }

    pub(crate) fn provider_cache(&self) -> ProviderResponseCache {
        self.provider_cache.clone()
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

    pub async fn list_notification_plugins(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<PluginPage, PluginServiceError> {
        let catalog = self.catalog_snapshot().await;
        let mut views = Vec::new();
        for plugin in &catalog.plugins {
            if !is_notification_plugin(plugin) {
                continue;
            }
            let (installed, enabled) = self.plugin_state(&plugin.manifest.id).await?;
            views.push(self.dynamic_view(plugin, installed, enabled).await?);
        }
        views.sort_by(|left, right| left.id.cmp(&right.id));
        let total = i64::try_from(views.len()).unwrap_or(i64::MAX);
        let start = offset.max(0).min(total) as usize;
        let end = offset.max(0).saturating_add(limit.max(0)).min(total) as usize;
        Ok(PluginPage {
            plugins: views[start..end].to_vec(),
            total,
            offset,
            limit,
        })
    }

    async fn list_filtered(
        &self,
        offset: i64,
        limit: i64,
        installed_only: bool,
    ) -> Result<PluginPage, PluginServiceError> {
        let catalog = self.catalog_snapshot().await;
        let store_index = self.store_index().await;
        let mut views = Vec::with_capacity(catalog.plugins.len() + store_index.plugins.len() + 1);
        let mut listed_ids = HashSet::new();
        for entry in &store_index.plugins {
            let local_plugin = catalog.get(&entry.id);
            let status_id = if is_tmdb_plugin_id(&entry.id) {
                TMDB_DYNAMIC_PLUGIN_ID
            } else {
                entry.id.as_str()
            };
            let status = self.database.plugin_installation_status(status_id).await?;
            let installed = local_plugin.is_some() && status.is_some();
            let enabled = installed && status == Some(true);
            if installed_only && !installed {
                continue;
            }
            if let Some(plugin) = local_plugin {
                let mut view = self.dynamic_view(plugin, installed, enabled).await?;
                view.latest_version = Some(entry.version.clone());
                view.update_available =
                    installed && plugin_version_is_newer(&plugin.manifest.version, &entry.version);
                views.push(view);
            } else {
                views.push(remote_plugin_view(entry, installed, enabled));
            }
            listed_ids.insert(entry.id.clone());
        }
        for plugin in &catalog.plugins {
            if listed_ids.contains(&plugin.manifest.id) || is_tmdb_plugin_id(&plugin.manifest.id) {
                continue;
            }
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
        let requested_id = plugin_id.trim().to_owned();
        let catalog = self.catalog_snapshot().await;
        let plugin_id = if requested_id == TMDB_PLUGIN_ID
            && catalog.get(TMDB_DYNAMIC_PLUGIN_ID).is_none()
            && self
                .store_index()
                .await
                .plugins
                .iter()
                .any(|entry| entry.id == TMDB_DYNAMIC_PLUGIN_ID)
        {
            TMDB_DYNAMIC_PLUGIN_ID.to_owned()
        } else {
            self.canonical_plugin_id(&requested_id, &catalog)
        };
        let was_installed = self.database.has_plugin_installation(&plugin_id).await?;
        if plugin_id != TMDB_PLUGIN_ID && catalog.get(&plugin_id).is_none() {
            let entry = self
                .store_index()
                .await
                .plugins
                .into_iter()
                .find(|entry| entry.id == plugin_id)
                .ok_or_else(|| PluginServiceError::UnknownPlugin(plugin_id.clone()))?;
            self.install_remote_package(&entry).await?;
        }
        self.database.install_plugin(&plugin_id).await?;
        let current_catalog = self.catalog_snapshot().await;
        if plugin_id == MEDIA_INFO_PLUGIN_ID {
            self.sync_media_info_scheduled_task().await?;
        } else if current_catalog
            .get(&plugin_id)
            .is_some_and(is_chapter_detector_plugin)
        {
            self.sync_chapter_detection_scheduled_tasks().await?;
        }
        let plugin = self.view_for_id(&plugin_id, true, true).await?;
        Ok(PluginInstall {
            plugin,
            was_installed,
        })
    }

    pub async fn uninstall(&self, plugin_id: &str) -> Result<(), PluginServiceError> {
        let catalog = self.catalog_snapshot().await;
        let plugin_id = self.canonical_plugin_id(plugin_id, &catalog);
        self.ensure_known_plugin(&plugin_id, &catalog)?;
        let plugin = catalog
            .get(&plugin_id)
            .ok_or_else(|| PluginServiceError::UnknownPlugin(plugin_id.clone()))?;
        if !self.database.has_plugin_installation(&plugin_id).await? {
            return Err(PluginServiceError::Unavailable(plugin_id));
        }

        remove_plugin_config(&self.config_dir, &plugin_id)
            .await
            .map_err(PluginServiceError::ConfigIo)?;
        self.supervisor.stop(&plugin_id).await;
        remove_plugin_files(plugin)
            .await
            .map_err(PluginServiceError::ConfigIo)?;
        self.database.uninstall_plugin(&plugin_id).await?;
        if plugin_id == MEDIA_INFO_PLUGIN_ID {
            self.database.disable_strm_media_info_task().await?;
        } else if is_chapter_detector_plugin(plugin) {
            self.sync_chapter_detection_scheduled_tasks().await?;
        }
        *self.catalog.write().await = PluginCatalog::discover(&self.config_dir.join("plugins"));
        Ok(())
    }

    pub async fn update(&self, plugin_id: &str) -> Result<PluginView, PluginServiceError> {
        let catalog = self.catalog_snapshot().await;
        let plugin_id = self.canonical_plugin_id(plugin_id, &catalog);
        self.ensure_known_plugin(&plugin_id, &catalog)?;
        if !self.database.has_plugin_installation(&plugin_id).await? {
            return Err(PluginServiceError::Unavailable(plugin_id));
        }
        let entry = self
            .store_index()
            .await
            .plugins
            .into_iter()
            .find(|entry| entry.id == plugin_id)
            .ok_or_else(|| PluginServiceError::UnknownPlugin(plugin_id.clone()))?;
        let plugin = catalog
            .get(&plugin_id)
            .ok_or_else(|| PluginServiceError::UnknownPlugin(plugin_id.clone()))?;
        if !plugin_version_is_newer(&plugin.manifest.version, &entry.version) {
            return Err(PluginServiceError::NoUpdate);
        }
        self.supervisor.stop(&plugin_id).await;
        self.install_remote_package(&entry).await?;
        let current_catalog = self.catalog_snapshot().await;
        if plugin_id == MEDIA_INFO_PLUGIN_ID {
            self.sync_media_info_scheduled_task().await?;
        } else if current_catalog
            .get(&plugin_id)
            .is_some_and(is_chapter_detector_plugin)
        {
            self.sync_chapter_detection_scheduled_tasks().await?;
        }
        let enabled = self
            .database
            .plugin_installation_status(&plugin_id)
            .await?
            .unwrap_or(false);
        self.view_for_id(&plugin_id, true, enabled).await
    }

    pub async fn set_enabled(
        &self,
        plugin_id: &str,
        enabled: bool,
    ) -> Result<PluginView, PluginServiceError> {
        let catalog = self.catalog_snapshot().await;
        let plugin_id = self.canonical_plugin_id(plugin_id, &catalog);
        self.ensure_known_plugin(&plugin_id, &catalog)?;
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
        } else if catalog
            .get(&plugin_id)
            .is_some_and(is_chapter_detector_plugin)
        {
            self.sync_chapter_detection_scheduled_tasks().await?;
        }
        let (installed, enabled) = self.plugin_state(&plugin_id).await?;
        self.view_for_id(&plugin_id, installed, enabled).await
    }

    pub async fn update_config(
        &self,
        plugin_id: &str,
        update: TmdbConfigUpdate<'_>,
    ) -> Result<PluginView, PluginServiceError> {
        let catalog = self.catalog_snapshot().await;
        let plugin_id = self.canonical_plugin_id(plugin_id, &catalog);
        self.ensure_known_plugin(&plugin_id, &catalog)?;
        if !is_tmdb_plugin_id(&plugin_id) {
            return Err(PluginServiceError::InvalidConfig);
        }
        if update
            .api_key
            .is_some_and(|value| value.trim().chars().count() > 4096)
        {
            return Err(PluginServiceError::InvalidConfig);
        }
        let current_settings = read_tmdb_settings(&self.config_dir).await;
        let settings = TmdbSettings::new_with_api_and_title_alias_config(
            update
                .preferred_language
                .map(str::to_owned)
                .unwrap_or(current_settings.preferred_language),
            update
                .language_fallback_enabled
                .unwrap_or(current_settings.language_fallback_enabled),
            update
                .title_alias_replacement_enabled
                .unwrap_or(current_settings.title_alias_replacement_enabled),
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
        let (installed, enabled) = self.plugin_state(&plugin_id).await?;
        self.view_for_id(&plugin_id, installed, enabled).await
    }

    pub async fn validate_selection(
        &self,
        scraper_id: Option<&str>,
    ) -> Result<(), PluginServiceError> {
        let Some(scraper_id) = scraper_id.map(str::trim).filter(|value| !value.is_empty()) else {
            return Ok(());
        };
        let catalog = self.catalog_snapshot().await;
        let scraper_id = self.canonical_plugin_id(scraper_id, &catalog);
        self.ensure_known_plugin(&scraper_id, &catalog)?;
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
        let catalog = self.catalog_snapshot().await;
        let plugin_id = self.canonical_plugin_id(plugin_id, &catalog);
        self.ensure_known_plugin(&plugin_id, &catalog)?;
        self.supervisor
            .call(&plugin_id, method, params)
            .await
            .map_err(PluginServiceError::Runtime)
    }

    pub async fn call_notification(
        &self,
        plugin_id: &str,
        params: Value,
    ) -> Result<NotificationSendRpcResult, PluginServiceError> {
        let catalog = self.catalog_snapshot().await;
        let plugin_id = self.canonical_plugin_id(plugin_id, &catalog);
        let plugin = catalog
            .get(&plugin_id)
            .ok_or_else(|| PluginServiceError::UnknownPlugin(plugin_id.clone()))?;
        if !is_notification_plugin(plugin) {
            return Err(PluginServiceError::Unavailable(plugin_id));
        }
        let (installed, enabled) = self.plugin_state(&plugin_id).await?;
        if !installed || !enabled {
            return Err(PluginServiceError::Unavailable(plugin_id));
        }
        let value = self
            .supervisor
            .call_without_config_access(&plugin_id, NOTIFICATION_SEND_METHOD, params)
            .await
            .map_err(PluginServiceError::Runtime)?;
        if serde_json::to_vec(&value)
            .ok()
            .is_none_or(|bytes| bytes.len() > 256 * 1024)
        {
            return Err(PluginServiceError::InvalidResponse);
        }
        serde_json::from_value(value).map_err(|_| PluginServiceError::InvalidResponse)
    }

    pub async fn validate_notification_provider(
        &self,
        plugin_id: &str,
    ) -> Result<(), PluginServiceError> {
        let catalog = self.catalog_snapshot().await;
        let plugin_id = self.canonical_plugin_id(plugin_id, &catalog);
        let plugin = catalog
            .get(&plugin_id)
            .ok_or_else(|| PluginServiceError::UnknownPlugin(plugin_id.clone()))?;
        if !is_notification_plugin(plugin) {
            return Err(PluginServiceError::Unavailable(plugin_id));
        }
        let (installed, enabled) = self.plugin_state(&plugin_id).await?;
        if !installed || !enabled {
            return Err(PluginServiceError::Unavailable(plugin_id));
        }
        Ok(())
    }

    pub async fn lookup_ip_location(
        &self,
        ip: IpAddr,
    ) -> Result<IpLocationRpcResult, PluginServiceError> {
        if !is_public_address(ip) {
            return Err(PluginServiceError::Unavailable("ip_location".to_owned()));
        }
        let query_ip = ip.to_string();
        let other_plugins = self.installed_other_ip_location_plugins().await?;
        let plugin_ids = if other_plugins.is_empty() {
            vec![IP138_PLUGIN_ID.to_owned()]
        } else {
            other_plugins
        };
        let catalog = self.catalog_snapshot().await;
        for plugin_id in plugin_ids {
            let Some(plugin) = catalog.get(&plugin_id) else {
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

    pub async fn match_danmaku(
        &self,
        file_name: &str,
    ) -> Result<DanmakuMatchRpcResult, PluginServiceError> {
        if file_name.trim().is_empty()
            || file_name.chars().count() > 1024
            || file_name.chars().any(char::is_control)
            || file_name.contains(['/', '\\'])
        {
            return Err(PluginServiceError::InvalidResponse);
        }
        let catalog = self.catalog_snapshot().await;
        let plugin = catalog
            .get(DANMAKU_PLUGIN_ID)
            .ok_or_else(|| PluginServiceError::UnknownPlugin(DANMAKU_PLUGIN_ID.to_owned()))?;
        if !is_danmaku_plugin(plugin) {
            return Err(PluginServiceError::Unavailable(
                DANMAKU_PLUGIN_ID.to_owned(),
            ));
        }
        let (installed, enabled) = self.plugin_state(DANMAKU_PLUGIN_ID).await?;
        if !installed || !enabled {
            return Err(PluginServiceError::Unavailable(
                DANMAKU_PLUGIN_ID.to_owned(),
            ));
        }
        let view = self.dynamic_view(plugin, installed, enabled).await?;
        if !view.available {
            return Err(PluginServiceError::Unavailable(
                DANMAKU_PLUGIN_ID.to_owned(),
            ));
        }
        let value = self
            .supervisor
            .call_isolated(
                DANMAKU_PLUGIN_ID,
                DANMAKU_MATCH_METHOD,
                serde_json::to_value(DanmakuMatchRpcRequest {
                    file_name: file_name.to_owned(),
                })
                .map_err(|_| PluginServiceError::InvalidResponse)?,
            )
            .await
            .map_err(PluginServiceError::Runtime)?;
        if serde_json::to_vec(&value)
            .ok()
            .is_none_or(|bytes| bytes.len() > 4 * 1024 * 1024)
        {
            return Err(PluginServiceError::InvalidResponse);
        }
        let result: DanmakuMatchRpcResult =
            serde_json::from_value(value).map_err(|_| PluginServiceError::InvalidResponse)?;
        match result.status {
            DanmakuMatchStatus::Matched
                if result.episode_id.is_some() && result.xml_base64.is_some() =>
            {
                Ok(result)
            }
            DanmakuMatchStatus::NoMatch
                if result.episode_id.is_none() && result.xml_base64.is_none() =>
            {
                Ok(result)
            }
            _ => Err(PluginServiceError::InvalidResponse),
        }
    }

    pub async fn has_available_danmaku(&self) -> Result<bool, PluginServiceError> {
        let catalog = self.catalog_snapshot().await;
        let Some(plugin) = catalog.get(DANMAKU_PLUGIN_ID) else {
            return Ok(false);
        };
        if !is_danmaku_plugin(plugin) {
            return Ok(false);
        }
        let (installed, enabled) = self.plugin_state(DANMAKU_PLUGIN_ID).await?;
        if !installed || !enabled {
            return Ok(false);
        }
        Ok(self
            .dynamic_view(plugin, installed, enabled)
            .await?
            .available)
    }

    pub async fn has_available_strm_resolver(&self) -> Result<bool, PluginServiceError> {
        Ok(!self.available_strm_resolver_ids().await?.is_empty())
    }

    pub async fn has_available_chapter_detector(
        &self,
        plugin_id: &str,
    ) -> Result<bool, PluginServiceError> {
        let catalog = self.catalog_snapshot().await;
        let Some(plugin) = catalog.get(plugin_id) else {
            return Ok(false);
        };
        if !is_chapter_detector_plugin(plugin) {
            return Ok(false);
        }
        let (installed, enabled) = self.plugin_state(plugin_id).await?;
        if !installed || !enabled {
            return Ok(false);
        }
        Ok(self
            .dynamic_view(plugin, installed, enabled)
            .await?
            .available)
    }

    pub async fn list_chapter_sources(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<ChapterSourcePage, PluginServiceError> {
        let catalog = self.catalog_snapshot().await;
        let mut sources = Vec::new();
        for plugin in &catalog.plugins {
            if !is_chapter_detector_plugin(plugin) {
                continue;
            }
            let (installed, enabled) = self.plugin_state(&plugin.manifest.id).await?;
            if !installed || !enabled {
                continue;
            }
            let view = self.dynamic_view(plugin, installed, enabled).await?;
            if view.available {
                sources.push(ChapterSourceView {
                    id: view.id,
                    name: view.name,
                    description: view.description,
                    version: view.version,
                    capabilities: view.capabilities,
                    lookup: is_chapter_lookup_plugin(plugin),
                });
            }
        }
        sources.sort_by(|left, right| left.id.cmp(&right.id));
        let total = i64::try_from(sources.len()).unwrap_or(i64::MAX);
        let start = offset.max(0).min(total) as usize;
        let end = (offset.max(0).saturating_add(limit.max(0))).min(total) as usize;
        Ok(ChapterSourcePage {
            sources: sources[start..end].to_vec(),
            total,
            offset,
            limit,
        })
    }

    pub async fn has_available_chapter_source(
        &self,
        plugin_id: &str,
    ) -> Result<bool, PluginServiceError> {
        self.has_available_chapter_detector(plugin_id).await
    }

    pub async fn is_chapter_lookup_plugin(
        &self,
        plugin_id: &str,
    ) -> Result<bool, PluginServiceError> {
        let catalog = self.catalog_snapshot().await;
        Ok(catalog.get(plugin_id).is_some_and(is_chapter_lookup_plugin))
    }

    pub async fn resolve_strm_target(
        &self,
        target: &str,
    ) -> Result<Option<String>, PluginServiceError> {
        if !matches!(
            classify_strm_target(target).kind,
            StrmTargetKind::Smb | StrmTargetKind::Ftp
        ) {
            return Ok(None);
        }
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
        let catalog = self.catalog_snapshot().await;
        for plugin in &catalog.plugins {
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
        let catalog = self.catalog_snapshot().await;
        let plugin_id = self.canonical_plugin_id(plugin_id, &catalog);
        self.ensure_known_plugin(&plugin_id, &catalog)?;
        if is_tmdb_plugin_id(&plugin_id) {
            return Err(PluginServiceError::InvalidConfig);
        }
        let plugin = catalog
            .get(&plugin_id)
            .ok_or_else(|| PluginServiceError::UnknownPlugin(plugin_id.clone()))?;
        let fields = self.config_fields_for_plugin(plugin).await?;
        let values = merge_default_config_values(&fields, values);
        let mut values = normalize_plugin_config(&plugin_id, values);
        if is_chapter_detector_plugin(plugin) {
            values.remove("libraryIds");
        }
        let values = validate_config_values(&fields, &values)?;
        validate_dynamic_plugin_config(&plugin_id, &values)?;
        if plugin_id == MEDIA_INFO_PLUGIN_ID {
            media_info_schedule(&values)?;
        } else if is_chapter_detector_plugin(plugin) {
            chapter_detector_settings_from_values(&plugin_id, &fields, &values)?;
        }
        self.write_plugin_config(&plugin_id, &values).await?;
        if plugin_id == MEDIA_INFO_PLUGIN_ID {
            self.sync_media_info_scheduled_task().await?;
        } else if is_chapter_detector_plugin(plugin) {
            self.sync_chapter_detection_scheduled_tasks().await?;
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
        let catalog = self.catalog_snapshot().await;
        let plugin = catalog
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

    pub async fn update_chapter_detector_schedule(
        &self,
        plugin_id: &str,
        schedule: &str,
    ) -> Result<(), PluginServiceError> {
        let schedule = schedule.trim();
        validate_cron(schedule).map_err(|_| PluginServiceError::InvalidConfig)?;
        let catalog = self.catalog_snapshot().await;
        let plugin_id = self.canonical_plugin_id(plugin_id, &catalog);
        let plugin = catalog
            .get(&plugin_id)
            .ok_or_else(|| PluginServiceError::UnknownPlugin(plugin_id.clone()))?;
        if !is_chapter_detector_plugin(plugin) {
            return Err(PluginServiceError::InvalidConfig);
        }
        let fields = self.config_fields_for_plugin(plugin).await?;
        let mut values = merge_default_config_values(
            &fields,
            normalize_plugin_config(&plugin_id, self.read_plugin_config(&plugin_id).await?),
        );
        values.insert("schedule".to_owned(), Value::String(schedule.to_owned()));
        let values = validate_config_values(&fields, &values)?;
        chapter_detector_settings_from_values(&plugin_id, &fields, &values)?;
        self.write_plugin_config(&plugin_id, &values).await?;
        self.sync_chapter_detection_scheduled_tasks().await
    }

    pub async fn media_info_settings(&self) -> Result<MediaInfoSettings, PluginServiceError> {
        let catalog = self.catalog_snapshot().await;
        let plugin = catalog
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

    pub async fn enabled_media_info_settings(
        &self,
    ) -> Result<Option<MediaInfoSettings>, PluginServiceError> {
        let (installed, enabled) = self.plugin_state(MEDIA_INFO_PLUGIN_ID).await?;
        if !installed || !enabled {
            return Ok(None);
        }
        self.media_info_settings().await.map(Some)
    }

    pub async fn chapter_detector_settings(
        &self,
        plugin_id: &str,
    ) -> Result<ChapterDetectorSettings, PluginServiceError> {
        let catalog = self.catalog_snapshot().await;
        let plugin = catalog
            .get(plugin_id)
            .ok_or_else(|| PluginServiceError::UnknownPlugin(plugin_id.to_owned()))?;
        if !is_chapter_detector_plugin(plugin) {
            return Err(PluginServiceError::Unavailable(plugin_id.to_owned()));
        }
        let fields = self.config_fields_for_plugin(plugin).await?;
        let mut stored_values = self.read_plugin_config(plugin_id).await?;
        stored_values.remove("libraryIds");
        let values =
            merge_default_config_values(&fields, normalize_plugin_config(plugin_id, stored_values));
        chapter_detector_settings_from_values(plugin_id, &fields, &values)
    }

    pub async fn enabled_chapter_detector_settings(
        &self,
        plugin_id: &str,
    ) -> Result<Option<ChapterDetectorSettings>, PluginServiceError> {
        let (installed, enabled) = self.plugin_state(plugin_id).await?;
        if !installed || !enabled {
            return Ok(None);
        }
        self.chapter_detector_settings(plugin_id).await.map(Some)
    }

    pub async fn sync_chapter_detection_scheduled_tasks(&self) -> Result<(), PluginServiceError> {
        self.migrate_legacy_chapter_source_selections().await?;
        self.database.disable_chapter_detection_tasks().await?;
        let catalog = self.catalog_snapshot().await;
        let mut selected = std::collections::HashMap::<String, String>::new();
        for plugin in &catalog.plugins {
            if !is_chapter_detector_plugin(plugin) {
                continue;
            }
            let (installed, enabled) = self.plugin_state(&plugin.manifest.id).await?;
            if !installed || !enabled {
                continue;
            }
            match self.chapter_detector_settings(&plugin.manifest.id).await {
                Ok(_) => {}
                Err(PluginServiceError::InvalidConfig) => continue,
                Err(error) => return Err(error),
            };
            let libraries = self.database.list_libraries().await?;
            for library in libraries {
                if !library.is_enabled
                    || library.kind == "MOVIE"
                    || library.chapter_source_id.as_deref() != Some(plugin.manifest.id.as_str())
                {
                    continue;
                }
                selected.insert(library.id, plugin.manifest.id.clone());
            }
        }
        for (library_id, plugin_id) in selected {
            let settings = self.chapter_detector_settings(&plugin_id).await?;
            self.database
                .upsert_chapter_detection_task(
                    &library_id,
                    &plugin_id,
                    &settings.schedule,
                    true,
                    settings.concurrency,
                    settings.intro_window_seconds,
                    settings.credits_window_seconds,
                    settings.match_threshold,
                )
                .await?;
        }
        Ok(())
    }

    async fn migrate_legacy_chapter_source_selections(&self) -> Result<(), PluginServiceError> {
        let catalog = self.catalog_snapshot().await;
        let mut plugins = catalog
            .plugins
            .iter()
            .filter(|plugin| is_chapter_detector_plugin(plugin))
            .collect::<Vec<_>>();
        plugins.sort_by(|left, right| {
            let left_priority = left.manifest.id != CHAPTER_DETECTOR_PLUGIN_ID;
            let right_priority = right.manifest.id != CHAPTER_DETECTOR_PLUGIN_ID;
            left_priority
                .cmp(&right_priority)
                .then_with(|| left.manifest.id.cmp(&right.manifest.id))
        });
        for plugin in plugins {
            let (installed, enabled) = self.plugin_state(&plugin.manifest.id).await?;
            if !installed || !enabled {
                continue;
            }
            let values = self.read_plugin_config(&plugin.manifest.id).await?;
            let Some(library_ids) = values.get("libraryIds").and_then(Value::as_array) else {
                continue;
            };
            for library_id in library_ids.iter().filter_map(Value::as_str) {
                let Some(library) = self.database.find_library(library_id).await? else {
                    continue;
                };
                if library.kind == "MOVIE" || library.chapter_source_id.is_some() {
                    continue;
                }
                self.database
                    .update_library_settings(
                        library_id,
                        crate::storage::LibrarySettingsUpdate {
                            name: None,
                            kind: None,
                            is_enabled: None,
                            realtime_watch_enabled: None,
                            realtime_metadata_auto_match_enabled: None,
                            reconciliation_schedule: None,
                            metadata_schedule: None,
                            scan_concurrency: None,
                            probe_concurrency: None,
                            scraper_id: None,
                            chapter_source_id: Some(Some(&plugin.manifest.id)),
                            media_strategy_json: None,
                        },
                    )
                    .await?;
            }
        }
        Ok(())
    }

    pub async fn sync_media_info_scheduled_task(&self) -> Result<(), PluginServiceError> {
        let (installed, enabled) = self.plugin_state(MEDIA_INFO_PLUGIN_ID).await?;
        if !installed {
            self.database.disable_strm_media_info_task().await?;
            return Ok(());
        }
        let catalog = self.catalog_snapshot().await;
        let plugin = catalog
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
        if plugin.manifest.id == MEDIA_INFO_PLUGIN_ID
            && !fields.iter().any(|field| field.key == "schedule")
        {
            // Older STRM media-info manifests predate host-managed scheduling. Keep their
            // persisted configuration editable from the task page and migrate the schedule
            // into the same plugin config file when it is first changed.
            fields.push(PluginConfigField {
                key: "schedule".to_owned(),
                label: "执行计划".to_owned(),
                input_type: "text".to_owned(),
                required: true,
                sensitive: false,
                description: None,
                multiple: false,
                options: Vec::new(),
                options_source: None,
                default_value: Some(Value::String(DEFAULT_STRM_MEDIA_INFO_SCHEDULE.to_owned())),
                minimum: None,
                maximum: None,
            });
        }
        if is_chapter_detector_plugin(plugin) {
            fields.retain(|field| field.key != "libraryIds");
        }
        if fields.iter().any(|field| {
            field.options_source.as_deref() == Some(CONFIG_OPTIONS_SOURCE_MEDIA_LIBRARIES)
        }) {
            let options = self
                .database
                .list_libraries()
                .await?
                .into_iter()
                .filter(|library| {
                    library.is_enabled
                        && (!is_chapter_detector_plugin(plugin) || library.kind != "MOVIE")
                })
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
        let catalog = self.catalog_snapshot().await;
        let plugin = catalog
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

    pub async fn detect_chapters(
        &self,
        plugin_id: &str,
        request: ChapterDetectRpcRequest,
    ) -> Result<ChapterDetectRpcResult, PluginServiceError> {
        validate_chapter_detect_request(&request)?;
        let catalog = self.catalog_snapshot().await;
        let plugin = catalog
            .get(plugin_id)
            .ok_or_else(|| PluginServiceError::UnknownPlugin(plugin_id.to_owned()))?;
        if !is_chapter_detector_plugin(plugin)
            || !plugin
                .manifest
                .capabilities
                .iter()
                .any(|capability| capability == CHAPTER_DETECT_CAPABILITY)
        {
            return Err(PluginServiceError::Unavailable(plugin_id.to_owned()));
        }
        let (installed, enabled) = self.plugin_state(plugin_id).await?;
        if !self
            .dynamic_view(plugin, installed, enabled)
            .await?
            .available
        {
            return Err(PluginServiceError::Unavailable(plugin_id.to_owned()));
        }
        let params =
            serde_json::to_value(&request).map_err(|_| PluginServiceError::InvalidResponse)?;
        let value = self
            .supervisor
            .call_isolated(plugin_id, CHAPTER_DETECT_METHOD, params)
            .await
            .map_err(PluginServiceError::Runtime)?;
        let result: ChapterDetectRpcResult =
            serde_json::from_value(value).map_err(|_| PluginServiceError::InvalidResponse)?;
        validate_chapter_detect_result(&request, &result)?;
        Ok(result)
    }

    pub async fn lookup_chapters(
        &self,
        plugin_id: &str,
        request: ChapterLookupRpcRequest,
    ) -> Result<ChapterLookupRpcResult, PluginServiceError> {
        validate_chapter_lookup_request(&request)?;
        let catalog = self.catalog_snapshot().await;
        let plugin = catalog
            .get(plugin_id)
            .ok_or_else(|| PluginServiceError::UnknownPlugin(plugin_id.to_owned()))?;
        if !is_chapter_lookup_plugin(plugin) {
            return Err(PluginServiceError::Unavailable(plugin_id.to_owned()));
        }
        let (installed, enabled) = self.plugin_state(plugin_id).await?;
        if !self
            .dynamic_view(plugin, installed, enabled)
            .await?
            .available
        {
            return Err(PluginServiceError::Unavailable(plugin_id.to_owned()));
        }
        let params =
            serde_json::to_value(&request).map_err(|_| PluginServiceError::InvalidResponse)?;
        let value = self
            .supervisor
            .call_isolated(plugin_id, CHAPTER_LOOKUP_METHOD, params)
            .await
            .map_err(PluginServiceError::Runtime)?;
        let result: ChapterLookupRpcResult =
            serde_json::from_value(value).map_err(|_| PluginServiceError::InvalidResponse)?;
        validate_chapter_lookup_result(&request, &result)?;
        Ok(result)
    }

    pub async fn scraper_client(
        &self,
        scraper_id: &str,
    ) -> Result<crate::application::scraper::ScraperPluginClient, PluginServiceError> {
        let catalog = self.catalog_snapshot().await;
        let plugin_id = self.canonical_plugin_id(scraper_id, &catalog);
        self.ensure_known_plugin(&plugin_id, &catalog)?;
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
            self.provider_cache(),
        ))
    }

    pub async fn restart(&self, plugin_id: &str) {
        let catalog = self.catalog_snapshot().await;
        let plugin_id = self.canonical_plugin_id(plugin_id, &catalog);
        self.supervisor.stop(&plugin_id).await;
    }

    pub async fn stop_all(&self) {
        self.supervisor.stop_all().await;
    }

    pub async fn plugin_store_source(&self) -> String {
        if let Some(store) = self.store.as_ref() {
            store.source().await
        } else {
            crate::application::plugin_store::DEFAULT_PLUGIN_STORE_URL.to_owned()
        }
    }

    pub async fn update_plugin_store_source(
        &self,
        source: &str,
    ) -> Result<String, PluginServiceError> {
        let Some(store) = self.store.as_ref() else {
            return Err(PluginServiceError::Store(PluginStoreError::InvalidSource));
        };
        store
            .save_source(source)
            .await
            .map_err(PluginServiceError::Store)
    }

    async fn catalog_snapshot(&self) -> PluginCatalog {
        self.catalog.read().await.clone()
    }

    async fn store_index(&self) -> PluginStoreIndex {
        let Some(store) = self.store.as_ref() else {
            return PluginStoreIndex {
                format_version: 1,
                plugins: Vec::new(),
            };
        };
        let source = store.source().await;
        match store.fetch_catalog().await {
            Ok(index) => index,
            Err(_) if source == crate::application::plugin_store::DEFAULT_PLUGIN_STORE_URL => {
                PluginStore::default_index().unwrap_or(PluginStoreIndex {
                    format_version: 1,
                    plugins: Vec::new(),
                })
            }
            Err(_) => PluginStoreIndex {
                format_version: 1,
                plugins: Vec::new(),
            },
        }
    }

    async fn install_remote_package(
        &self,
        entry: &PluginStoreEntry,
    ) -> Result<(), PluginServiceError> {
        let Some(store) = self.store.as_ref() else {
            return Err(PluginServiceError::Store(PluginStoreError::InvalidPackage));
        };
        let archive = store
            .download_package(entry)
            .await
            .map_err(PluginServiceError::Store)?;
        let plugin_dir = self.config_dir.join("plugins");
        if let Err(error) = fs::create_dir_all(&plugin_dir).await {
            let _ = fs::remove_file(&archive).await;
            return Err(PluginServiceError::ConfigIo(error));
        }
        let validation_dir = self
            .config_dir
            .join(format!(".lux-plugin-validation-{}", Uuid::now_v7()));
        if let Err(error) = fs::create_dir_all(&validation_dir).await {
            let _ = fs::remove_file(&archive).await;
            return Err(PluginServiceError::ConfigIo(error));
        }
        let validation_archive = validation_dir.join("package.zip");
        if let Err(error) = fs::rename(&archive, &validation_archive).await {
            let _ = fs::remove_dir_all(&validation_dir).await;
            return Err(PluginServiceError::ConfigIo(error));
        }
        let validation_catalog = PluginCatalog::discover(&validation_dir);
        if validation_catalog.get(&entry.id).is_none() {
            let _ = fs::remove_dir_all(&validation_dir).await;
            return Err(PluginServiceError::Store(PluginStoreError::InvalidPackage));
        }
        let destination = plugin_dir.join(format!("{}-{}.zip", entry.id, entry.version));
        let staged_destination =
            plugin_dir.join(format!(".lux-plugin-install-{}.zip", Uuid::now_v7()));
        if let Err(error) = fs::rename(&validation_archive, &staged_destination).await {
            let _ = fs::remove_dir_all(&validation_dir).await;
            return Err(PluginServiceError::ConfigIo(error));
        }
        if let Err(error) = fs::rename(&staged_destination, &destination).await {
            let _ = fs::remove_file(&staged_destination).await;
            let _ = fs::remove_dir_all(&validation_dir).await;
            return Err(PluginServiceError::ConfigIo(error));
        }
        // Keep the previous archives until the newly moved archive is visible in a
        // fresh catalog. This matters for updates: a discovery failure must leave
        // the old package available for the next attempt.
        let catalog = PluginCatalog::discover(&plugin_dir);
        if !catalog
            .get(&entry.id)
            .is_some_and(|plugin| plugin.manifest.version == entry.version)
        {
            let _ = fs::remove_file(&destination).await;
            let _ = fs::remove_dir_all(&validation_dir).await;
            return Err(PluginServiceError::Store(PluginStoreError::InvalidPackage));
        }
        let mut entries = fs::read_dir(&plugin_dir)
            .await
            .map_err(PluginServiceError::ConfigIo)?;
        while let Some(entry_result) = entries
            .next_entry()
            .await
            .map_err(PluginServiceError::ConfigIo)?
        {
            let path = entry_result.path();
            if path.is_file()
                && path.extension().is_some_and(|extension| extension == "zip")
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with(&format!("{}-", entry.id)) && path != destination
                    })
            {
                let _ = fs::remove_file(path).await;
            }
        }
        let _ = fs::remove_dir_all(&validation_dir).await;
        *self.catalog.write().await = catalog;
        Ok(())
    }

    async fn view_for_id(
        &self,
        plugin_id: &str,
        installed: bool,
        enabled: bool,
    ) -> Result<PluginView, PluginServiceError> {
        let catalog = self.catalog_snapshot().await;
        let Some(plugin) = catalog.get(plugin_id) else {
            return Err(PluginServiceError::UnknownPlugin(plugin_id.to_owned()));
        };
        self.dynamic_view(plugin, installed, enabled).await
    }

    async fn plugin_state(&self, plugin_id: &str) -> Result<(bool, bool), PluginServiceError> {
        let status = self.database.plugin_installation_status(plugin_id).await?;
        Ok((status.is_some(), status == Some(true)))
    }

    async fn installed_other_ip_location_plugins(&self) -> Result<Vec<String>, PluginServiceError> {
        let mut plugin_ids = Vec::new();
        let catalog = self.catalog_snapshot().await;
        for plugin_id in [IP_HIOFD_PLUGIN_ID] {
            if let Some(plugin) = catalog.get(plugin_id)
                && is_ip_location_plugin(plugin)
                && self.database.is_plugin_installed(plugin_id).await?
            {
                plugin_ids.push(plugin_id.to_owned());
            }
        }
        for plugin in &catalog.plugins {
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
        let mut stored_values = self.read_plugin_config(&plugin.manifest.id).await?;
        if is_chapter_detector_plugin(plugin) {
            stored_values.remove("libraryIds");
        }
        let config_values = merge_default_config_values(&config_fields, stored_values);
        let public_config_values = public_config_values(&config_fields, &config_values);
        let configured = if is_tmdb_plugin_id(&plugin.manifest.id) {
            config_source != CONFIG_SOURCE_NONE
        } else {
            config_fields.is_empty()
                || (validate_config_values(&config_fields, &config_values).is_ok()
                    && validate_dynamic_plugin_config(&plugin.manifest.id, &config_values).is_ok())
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
            latest_version: None,
            update_available: false,
        })
    }

    fn ensure_known_plugin(
        &self,
        plugin_id: &str,
        catalog: &PluginCatalog,
    ) -> Result<(), PluginServiceError> {
        if plugin_id == TMDB_PLUGIN_ID || catalog.get(plugin_id).is_some() {
            Ok(())
        } else {
            Err(PluginServiceError::UnknownPlugin(plugin_id.to_owned()))
        }
    }

    fn canonical_plugin_id(&self, plugin_id: &str, catalog: &PluginCatalog) -> String {
        let plugin_id = plugin_id.trim();
        if plugin_id == MEDIA_INFO_LEGACY_PLUGIN_ID {
            MEDIA_INFO_PLUGIN_ID.to_owned()
        } else if plugin_id == TMDB_PLUGIN_ID && catalog.get(TMDB_DYNAMIC_PLUGIN_ID).is_some() {
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
        } else {
            CONFIG_SOURCE_PLUGIN_DEFAULT
        }
    }
}

fn normalize_plugin_config(plugin_id: &str, mut values: Map<String, Value>) -> Map<String, Value> {
    if plugin_id == THEINTRODB_CHAPTER_SOURCE_ID
        && values.get("schedule").and_then(Value::as_str) == Some("0 5 * * 0")
    {
        values.insert(
            "schedule".to_owned(),
            Value::String(DEFAULT_ONLINE_CHAPTER_DETECTION_SCHEDULE.to_owned()),
        );
    }
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

fn validate_dynamic_plugin_config(
    plugin_id: &str,
    values: &Map<String, Value>,
) -> Result<(), PluginServiceError> {
    if plugin_id != DANMAKU_PLUGIN_ID {
        return Ok(());
    }
    let provider_url = values
        .get("providerBaseUrl")
        .and_then(Value::as_str)
        .ok_or(PluginServiceError::InvalidConfig)?;
    crate::application::danmaku::validate_provider_base_url(provider_url)
        .map(|_| ())
        .map_err(|_| PluginServiceError::InvalidConfig)
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

fn chapter_detector_settings_from_values(
    plugin_id: &str,
    fields: &[PluginConfigField],
    values: &Map<String, Value>,
) -> Result<ChapterDetectorSettings, PluginServiceError> {
    let values = validate_config_values(fields, values)?;
    let default_schedule = if plugin_id == THEINTRODB_CHAPTER_SOURCE_ID {
        DEFAULT_ONLINE_CHAPTER_DETECTION_SCHEDULE
    } else {
        DEFAULT_CHAPTER_DETECTION_SCHEDULE
    };
    let schedule = values
        .get("schedule")
        .and_then(Value::as_str)
        .unwrap_or(default_schedule)
        .trim();
    validate_cron(schedule).map_err(|_| PluginServiceError::InvalidConfig)?;
    let concurrency = values
        .get("concurrency")
        .and_then(Value::as_i64)
        .ok_or(PluginServiceError::InvalidConfig)?;
    let intro_window_seconds = values
        .get("introWindowSeconds")
        .and_then(Value::as_i64)
        .ok_or(PluginServiceError::InvalidConfig)?;
    let credits_window_seconds = values
        .get("creditsWindowSeconds")
        .and_then(Value::as_i64)
        .ok_or(PluginServiceError::InvalidConfig)?;
    let match_threshold = values
        .get("matchThreshold")
        .and_then(Value::as_i64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(PluginServiceError::InvalidConfig)?;
    Ok(ChapterDetectorSettings {
        concurrency,
        intro_window_seconds,
        credits_window_seconds,
        match_threshold,
        schedule: schedule.to_owned(),
    })
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

fn plugin_version_is_newer(current: &str, latest: &str) -> bool {
    if current == latest {
        return false;
    }
    let current_parts = current
        .split('.')
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>();
    let latest_parts = latest
        .split('.')
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>();
    match (current_parts, latest_parts) {
        (Ok(current_parts), Ok(latest_parts)) => {
            let length = current_parts.len().max(latest_parts.len());
            (0..length)
                .map(|index| {
                    (
                        current_parts.get(index).copied().unwrap_or(0),
                        latest_parts.get(index).copied().unwrap_or(0),
                    )
                })
                .find(|(current_part, latest_part)| current_part != latest_part)
                .is_some_and(|(current_part, latest_part)| latest_part > current_part)
        }
        _ => latest > current,
    }
}

fn remote_plugin_view(entry: &PluginStoreEntry, installed: bool, enabled: bool) -> PluginView {
    let enabled = installed && enabled;
    PluginView {
        id: entry.id.clone(),
        name: entry.name.clone(),
        description: entry.description.clone(),
        category: entry.category.clone(),
        version: Some(entry.version.clone()),
        runtime: (!entry.runtime.is_empty()).then(|| entry.runtime.clone()),
        capabilities: entry.capabilities.clone(),
        status: if enabled {
            "READY".to_owned()
        } else if installed {
            "DISABLED".to_owned()
        } else {
            "AVAILABLE".to_owned()
        },
        running: false,
        last_error: None,
        installed,
        enabled,
        configured: true,
        available: enabled,
        unavailable_reason: if !installed {
            Some("NOT_INSTALLED".to_owned())
        } else if !enabled {
            Some("DISABLED".to_owned())
        } else {
            None
        },
        configurable: false,
        config_fields: Vec::new(),
        config_source: CONFIG_SOURCE_NONE.to_owned(),
        config_values: Map::new(),
        latest_version: Some(entry.version.clone()),
        update_available: false,
    }
}

async fn remove_plugin_files(plugin: &DiscoveredPlugin) -> io::Result<()> {
    if plugin.is_archive {
        remove_directory_if_present(&plugin.root_path).await?;
        remove_file_if_present(&plugin.source_path).await?;
    } else {
        remove_directory_if_present(&plugin.source_path).await?;
    }
    Ok(())
}

async fn remove_plugin_config(config_dir: &Path, plugin_id: &str) -> io::Result<()> {
    remove_file_if_present(&plugin_config_path(config_dir, plugin_id)).await?;
    if plugin_id == MEDIA_INFO_PLUGIN_ID {
        remove_file_if_present(&plugin_config_path(config_dir, MEDIA_INFO_LEGACY_PLUGIN_ID))
            .await?;
    }
    if is_tmdb_plugin_id(plugin_id) {
        for file_name in [TMDB_API_KEY_FILE, TMDB_TOKEN_FILE, TMDB_SETTINGS_FILE] {
            remove_file_if_present(&config_dir.join(file_name)).await?;
        }
    }
    Ok(())
}

async fn remove_directory_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

async fn remove_file_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
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

#[derive(Clone, Debug)]
pub struct ChapterSourcePage {
    pub sources: Vec<ChapterSourceView>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
}

#[derive(Clone, Debug)]
pub struct ChapterSourceView {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: Option<String>,
    pub capabilities: Vec<String>,
    pub lookup: bool,
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
    pub latest_version: Option<String>,
    pub update_available: bool,
}

#[derive(Debug)]
pub enum PluginServiceError {
    UnknownPlugin(String),
    Unavailable(String),
    InvalidConfig,
    NoUpdate,
    InvalidResponse,
    ConfigIo(io::Error),
    Runtime(PluginRuntimeError),
    Store(PluginStoreError),
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
            Self::NoUpdate => formatter.write_str("plugin is already up to date"),
            Self::InvalidResponse => formatter.write_str("plugin returned an invalid response"),
            Self::ConfigIo(error) => write!(formatter, "plugin configuration IO error: {error}"),
            Self::Runtime(error) => error.fmt(formatter),
            Self::Store(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for PluginServiceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ConfigIo(error) => Some(error),
            Self::Runtime(error) => Some(error),
            Self::Store(error) => Some(error),
            Self::Storage(error) => Some(error),
            Self::UnknownPlugin(_)
            | Self::Unavailable(_)
            | Self::InvalidConfig
            | Self::NoUpdate
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

fn is_danmaku_plugin(plugin: &DiscoveredPlugin) -> bool {
    plugin.manifest.plugin_type == PLUGIN_TYPE_DANMAKU
        && plugin.manifest.category == PLUGIN_CATEGORY_MEDIA
        && plugin
            .manifest
            .capabilities
            .iter()
            .any(|capability| capability == DANMAKU_MATCH_CAPABILITY)
}

fn is_chapter_detector_plugin(plugin: &DiscoveredPlugin) -> bool {
    plugin.manifest.plugin_type == PLUGIN_TYPE_CHAPTER_DETECTOR
        && plugin.manifest.category == PLUGIN_CATEGORY_MEDIA
        && plugin.manifest.capabilities.iter().any(|capability| {
            capability == CHAPTER_DETECT_CAPABILITY || capability == CHAPTER_LOOKUP_CAPABILITY
        })
}

fn is_chapter_lookup_plugin(plugin: &DiscoveredPlugin) -> bool {
    is_chapter_detector_plugin(plugin)
        && plugin
            .manifest
            .capabilities
            .iter()
            .any(|capability| capability == CHAPTER_LOOKUP_CAPABILITY)
}

fn is_notification_plugin(plugin: &DiscoveredPlugin) -> bool {
    plugin.manifest.plugin_type == PLUGIN_TYPE_NOTIFICATION
        && plugin.manifest.category == PLUGIN_CATEGORY_NOTIFICATION
        && plugin
            .manifest
            .capabilities
            .iter()
            .any(|capability| capability == NOTIFICATION_SEND_CAPABILITY)
}

fn validate_chapter_detect_request(
    request: &ChapterDetectRpcRequest,
) -> Result<(), PluginServiceError> {
    if !(2..=64).contains(&request.episodes.len())
        || !(150_000_000..=3_000_000_000).contains(&request.intro_window_ticks)
        || !(150_000_000..=6_000_000_000).contains(&request.credits_window_ticks)
        || !(10_000_000..=1_200_000_000).contains(&request.minimum_match_duration_ticks)
        || !request.match_threshold.is_finite()
        || !(0.0..=1.0).contains(&request.match_threshold)
    {
        return Err(PluginServiceError::InvalidResponse);
    }
    let mut keys = HashSet::new();
    for episode in &request.episodes {
        let intro_bytes = BASE64.decode(&episode.intro_fingerprint_base64).ok();
        let credits_bytes = BASE64.decode(&episode.credits_fingerprint_base64).ok();
        if episode.key.is_empty()
            || episode.key.len() > 128
            || !keys.insert(episode.key.clone())
            || episode.sample_rate != CHAPTER_FINGERPRINT_SAMPLE_RATE
            || episode.fingerprint_point_duration_ticks != CHAPTER_FINGERPRINT_POINT_DURATION_TICKS
            || episode.intro_window_start_ticks < 0
            || episode.credits_window_start_ticks < 0
            || !(1..=request.intro_window_ticks).contains(&episode.intro_window_duration_ticks)
            || !(1..=request.credits_window_ticks).contains(&episode.credits_window_duration_ticks)
            || episode.intro_fingerprint_base64.len() > 512 * 1024
            || episode.credits_fingerprint_base64.len() > 512 * 1024
            || intro_bytes.as_ref().is_none_or(|bytes| {
                bytes.is_empty()
                    || bytes.len() > 384 * 1024
                    || bytes.len() % std::mem::size_of::<u32>() != 0
            })
            || credits_bytes.as_ref().is_none_or(|bytes| {
                bytes.is_empty()
                    || bytes.len() > 384 * 1024
                    || bytes.len() % std::mem::size_of::<u32>() != 0
            })
        {
            return Err(PluginServiceError::InvalidResponse);
        }
    }
    Ok(())
}

fn validate_chapter_lookup_request(
    request: &ChapterLookupRpcRequest,
) -> Result<(), PluginServiceError> {
    if !(1..=64).contains(&request.episodes.len()) {
        return Err(PluginServiceError::InvalidResponse);
    }
    let mut keys = HashSet::new();
    for episode in &request.episodes {
        let valid_imdb = episode.imdb_id.as_deref().is_none_or(|value| {
            !value.is_empty()
                && value.len() <= 32
                && value.starts_with("tt")
                && value[2..]
                    .chars()
                    .all(|character| character.is_ascii_digit())
        });
        if episode.key.is_empty()
            || episode.key.len() > 128
            || !keys.insert(episode.key.clone())
            || episode
                .tmdb_id
                .is_some_and(|value| !(1..=2_000_000_000).contains(&value))
            || episode
                .tvdb_id
                .is_some_and(|value| !(1..=2_000_000_000).contains(&value))
            || !valid_imdb
            || !(0..=1000).contains(&episode.season_number)
            || !(0..=10000).contains(&episode.episode_number)
            || episode
                .duration_ticks
                .is_some_and(|value| !(1..=3_600_000_000_000).contains(&value))
            || (episode.tmdb_id.is_none() && episode.tvdb_id.is_none() && episode.imdb_id.is_none())
        {
            return Err(PluginServiceError::InvalidResponse);
        }
    }
    Ok(())
}

fn validate_chapter_lookup_result(
    request: &ChapterLookupRpcRequest,
    result: &ChapterLookupRpcResult,
) -> Result<(), PluginServiceError> {
    if result.markers.len() > request.episodes.len().saturating_mul(3) {
        return Err(PluginServiceError::InvalidResponse);
    }
    let episodes = request
        .episodes
        .iter()
        .map(|episode| (episode.key.as_str(), episode))
        .collect::<std::collections::HashMap<_, _>>();
    let mut marker_keys = HashSet::new();
    for marker in &result.markers {
        let Some(episode) = episodes.get(marker.key.as_str()) else {
            return Err(PluginServiceError::InvalidResponse);
        };
        if marker.start_position_ticks < 0
            || !marker.confidence.is_finite()
            || !(0.0..=1.0).contains(&marker.confidence)
            || marker
                .name
                .as_ref()
                .is_some_and(|name| name.chars().count() > 256)
            || !marker_keys.insert((marker.key.as_str(), marker.marker_type))
            || episode
                .duration_ticks
                .is_some_and(|duration| marker.start_position_ticks > duration)
        {
            return Err(PluginServiceError::InvalidResponse);
        }
    }
    Ok(())
}

fn validate_chapter_detect_result(
    request: &ChapterDetectRpcRequest,
    result: &ChapterDetectRpcResult,
) -> Result<(), PluginServiceError> {
    if result.markers.len() > request.episodes.len().saturating_mul(3) {
        return Err(PluginServiceError::InvalidResponse);
    }
    let episodes = request
        .episodes
        .iter()
        .map(|episode| (episode.key.as_str(), episode))
        .collect::<std::collections::HashMap<_, _>>();
    let mut marker_keys = HashSet::new();
    for marker in &result.markers {
        let Some(episode) = episodes.get(marker.key.as_str()) else {
            return Err(PluginServiceError::InvalidResponse);
        };
        if marker.start_position_ticks < 0
            || !marker.confidence.is_finite()
            || !(0.0..=1.0).contains(&marker.confidence)
            || marker
                .name
                .as_ref()
                .is_some_and(|name| name.chars().count() > 256)
        {
            return Err(PluginServiceError::InvalidResponse);
        }
        if !marker_keys.insert((marker.key.as_str(), marker.marker_type)) {
            return Err(PluginServiceError::InvalidResponse);
        }
        let valid_range = match marker.marker_type {
            crate::application::plugin_protocol::ChapterDetectMarkerType::IntroStart
            | crate::application::plugin_protocol::ChapterDetectMarkerType::IntroEnd => {
                marker.start_position_ticks >= episode.intro_window_start_ticks
                    && marker.start_position_ticks
                        <= episode
                            .intro_window_start_ticks
                            .saturating_add(episode.intro_window_duration_ticks)
            }
            crate::application::plugin_protocol::ChapterDetectMarkerType::CreditsStart => {
                marker.start_position_ticks >= episode.credits_window_start_ticks
                    && marker.start_position_ticks
                        <= episode
                            .credits_window_start_ticks
                            .saturating_add(episode.credits_window_duration_ticks)
            }
        };
        if !valid_range {
            return Err(PluginServiceError::InvalidResponse);
        }
    }
    Ok(())
}

impl From<StorageError> for PluginServiceError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
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
            "titleAliasReplacementEnabled".to_owned(),
            Value::Bool(settings.title_alias_replacement_enabled),
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

#[cfg(test)]
mod plugin_update_tests {
    use super::plugin_version_is_newer;

    #[test]
    fn compares_numeric_plugin_versions_without_lexical_ordering() {
        assert!(plugin_version_is_newer("0.1.0", "0.2.0"));
        assert!(plugin_version_is_newer("0.9.0", "0.10.0"));
        assert!(!plugin_version_is_newer("1.0.1", "1.0.0"));
        assert!(!plugin_version_is_newer("1.0.0", "1.0.0"));
    }

    #[test]
    fn treats_a_changed_non_numeric_version_as_an_update_candidate() {
        assert!(plugin_version_is_newer("build-a", "build-b"));
        assert!(!plugin_version_is_newer("build-a", "build-a"));
    }
}
