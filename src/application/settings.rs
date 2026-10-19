use std::{collections::HashSet, io::Write, path::Path};

use serde::{Deserialize, Serialize};

use crate::{
    application::{
        admin_dashboard::DEFAULT_SERVER_NAME,
        home::HomeService,
        network_diagnostics::{NetworkDiagnostics, test_network},
        plugins::{PluginService, PluginServiceError},
    },
    network::{
        normalize_proxy_url, proxy_url_from_env, proxy_url_has_credentials, redact_proxy_url,
    },
    storage::Database,
};
use tokio::fs;

pub const NETWORK_PROXY_URL_FILE: &str = "network_proxy_url";
const MAX_TRUSTED_PROXY_CIDRS: usize = 100;
pub const DEFAULT_LOGIN_BACKGROUND_SOURCE: &str = "STATIC";
pub const RECENTLY_ADDED_LOGIN_BACKGROUND_SOURCE: &str = "RECENTLY_ADDED";
pub const LOGIN_BACKGROUND_PLUGIN_SOURCE_PREFIX: &str = "PLUGIN:";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MediaStrategySettings {
    pub(crate) metadata_language: String,
    pub(crate) image_language: String,
    pub(crate) region: String,
    pub(crate) scraper_id: Option<String>,
    #[serde(default = "default_metadata_refresh_mode")]
    pub(crate) metadata_refresh_mode: String,
    #[serde(default = "default_show_metadata_pending")]
    pub(crate) show_metadata_pending: bool,
    pub(crate) apply_scope: String,
    pub(crate) images: MediaImageStrategySettings,
    pub(crate) subtitles: MediaSubtitleStrategySettings,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MediaImageStrategySettings {
    pub(crate) poster: bool,
    pub(crate) artwork: bool,
    pub(crate) banner: bool,
    pub(crate) logo: bool,
    pub(crate) thumbnail: bool,
    #[serde(default = "default_thumbnail_scraping_mode")]
    pub(crate) thumbnail_scraping_mode: String,
    #[serde(default)]
    pub(crate) disc: bool,
    #[serde(default)]
    pub(crate) wallpaper: bool,
    #[serde(default)]
    pub(crate) write_to_metadata: bool,
    pub(crate) max_backdrop_count: i64,
    pub(crate) min_download_width: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MediaSubtitleStrategySettings {
    pub(crate) auto_download: bool,
    pub(crate) languages: Vec<String>,
    pub(crate) forced_only: bool,
    pub(crate) hearing_impaired: bool,
}

impl Default for MediaStrategySettings {
    fn default() -> Self {
        Self {
            metadata_language: "zh-CN".to_owned(),
            image_language: "zh-CN".to_owned(),
            region: "CN".to_owned(),
            scraper_id: None,
            metadata_refresh_mode: default_metadata_refresh_mode(),
            show_metadata_pending: true,
            apply_scope: "NEW_CONTENT".to_owned(),
            images: MediaImageStrategySettings {
                poster: true,
                artwork: false,
                banner: false,
                logo: true,
                thumbnail: true,
                thumbnail_scraping_mode:
                    crate::application::thumbnail_policy::DEFAULT_THUMBNAIL_SCRAPING_MODE.to_owned(),
                disc: false,
                wallpaper: false,
                write_to_metadata: false,
                max_backdrop_count: 1,
                min_download_width: 1280,
            },
            subtitles: MediaSubtitleStrategySettings {
                auto_download: false,
                languages: vec!["zh-CN".to_owned()],
                forced_only: false,
                hearing_impaired: false,
            },
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct NewUserPermissions {
    pub(crate) library_access_mode: String,
    pub(crate) library_ids: Vec<String>,
    pub(crate) can_download: bool,
    pub(crate) can_play: bool,
    pub(crate) max_simultaneous_playbacks: i64,
    pub(crate) allow_remux: bool,
    pub(crate) allow_audio_transcode: bool,
    pub(crate) allow_video_transcode: bool,
}

impl Default for NewUserPermissions {
    fn default() -> Self {
        Self {
            library_access_mode: "NONE".to_owned(),
            library_ids: Vec::new(),
            can_download: false,
            can_play: true,
            max_simultaneous_playbacks: 2,
            allow_remux: true,
            allow_audio_transcode: true,
            allow_video_transcode: true,
        }
    }
}

impl NewUserPermissions {
    pub(crate) fn has_valid_shape(&self) -> bool {
        self.max_simultaneous_playbacks >= 0
            && self.library_ids.len() <= 100
            && match self.library_access_mode.as_str() {
                "ALL" | "NONE" => self.library_ids.is_empty(),
                "SELECTED" => !self
                    .library_ids
                    .iter()
                    .enumerate()
                    .any(|(index, id)| self.library_ids[..index].contains(id)),
                _ => false,
            }
    }

    pub(crate) fn normalize_empty_selection(&mut self) {
        if self.library_access_mode == "SELECTED" && self.library_ids.is_empty() {
            self.library_access_mode = "NONE".to_owned();
        }
    }

    pub(crate) fn stored_snapshot(&self) -> crate::storage::NewStoredUserPermissions<'_> {
        crate::storage::NewStoredUserPermissions {
            library_access_mode: &self.library_access_mode,
            library_ids: &self.library_ids,
            can_download: self.can_download,
            can_play: self.can_play,
            max_simultaneous_playbacks: self.max_simultaneous_playbacks,
            allow_remux: self.allow_remux,
            allow_audio_transcode: self.allow_audio_transcode,
            allow_video_transcode: self.allow_video_transcode,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NetworkProxySettings {
    pub(crate) configured: bool,
    pub(crate) url: Option<String>,
    pub(crate) has_credentials: bool,
    pub(crate) source: &'static str,
    pub(crate) restart_required: bool,
}

pub(crate) struct AdminSettingsSnapshot {
    pub(crate) server_name: String,
    pub(crate) resume_played_percent: i64,
    pub(crate) resume_min_ticks: i64,
    pub(crate) force_admin_library_order: bool,
    pub(crate) user_deletion_grace_days: i64,
    pub(crate) user_activity_retention_days: i64,
    pub(crate) trusted_proxy_cidrs: Vec<String>,
    pub(crate) new_user_permissions: NewUserPermissions,
    pub(crate) login_background_source: String,
    pub(crate) login_background_source_status: String,
    pub(crate) media_strategy: MediaStrategySettings,
    pub(crate) network_proxy: NetworkProxySettings,
}

pub(crate) enum NetworkProxyChange {
    Unchanged,
    Set(Option<String>),
    Invalid,
}

pub(crate) struct AdminSettingsUpdate {
    pub(crate) server_name: Option<String>,
    pub(crate) resume_played_percent: Option<i64>,
    pub(crate) resume_min_ticks: Option<i64>,
    pub(crate) force_admin_library_order: Option<bool>,
    pub(crate) user_deletion_grace_days: Option<i64>,
    pub(crate) user_activity_retention_days: Option<i64>,
    pub(crate) trusted_proxy_cidrs: Option<Vec<String>>,
    pub(crate) new_user_permissions: Option<NewUserPermissions>,
    pub(crate) login_background_source: Option<String>,
    pub(crate) media_strategy: Option<MediaStrategySettings>,
    pub(crate) network_proxy: NetworkProxyChange,
}

#[derive(Debug)]
pub(crate) enum AdminSettingsError {
    Unavailable,
    InvalidLoginBackgroundSource,
    LoginBackgroundPluginUnavailable,
    InvalidServerName,
    InvalidGlobalSettings,
    InvalidUserDeletionGraceDays,
    InvalidUserActivityRetentionDays,
    InvalidTrustedProxyCidrs,
    InvalidNewUserPermissions,
    InvalidNetworkProxy,
    PluginServiceUnavailable,
    Plugin(PluginServiceError),
}

struct StoredAdminSettings {
    server_name: String,
    resume_played_percent: i64,
    resume_min_ticks: i64,
    force_admin_library_order: bool,
    user_deletion_grace_days: i64,
    user_activity_retention_days: i64,
    trusted_proxy_cidrs: Vec<String>,
    new_user_permissions: NewUserPermissions,
    login_background_source: String,
    media_strategy: MediaStrategySettings,
}

pub(crate) async fn read_admin_settings(
    database: &Database,
    plugins: Option<&PluginService>,
    config_dir: Option<&Path>,
) -> Result<AdminSettingsSnapshot, ()> {
    let (resume_played_percent, resume_min_ticks) =
        database.resume_settings().await.map_err(|_| ())?;
    let media_strategy = read_media_strategy_settings(database).await?;
    let force_admin_library_order = database.force_admin_library_order().await.map_err(|_| ())?;
    let user_deletion_grace_days = database.user_deletion_grace_days().await.map_err(|_| ())?;
    let user_activity_retention_days = database
        .user_activity_retention_days()
        .await
        .map_err(|_| ())?;
    let trusted_proxy_cidrs = read_trusted_proxy_cidrs(database).await?;
    let new_user_permissions = read_new_user_permissions(database).await?;
    let login_background_source = match database.login_background_source().await.map_err(|_| ())? {
        Some(source) if is_valid_login_background_source(&source) => source,
        _ => DEFAULT_LOGIN_BACKGROUND_SOURCE.to_owned(),
    };
    let login_background_source_status =
        login_background_source_status(plugins, &login_background_source).await;
    let server_name = match database.server_name().await.map_err(|_| ())? {
        Some(name) if !name.trim().is_empty() => name,
        _ => DEFAULT_SERVER_NAME.to_owned(),
    };
    let network_proxy = network_proxy_settings(config_dir).await;
    Ok(AdminSettingsSnapshot {
        server_name,
        resume_played_percent,
        resume_min_ticks,
        force_admin_library_order,
        user_deletion_grace_days,
        user_activity_retention_days,
        trusted_proxy_cidrs,
        new_user_permissions,
        login_background_source,
        login_background_source_status,
        media_strategy,
        network_proxy,
    })
}

pub(crate) async fn update_admin_settings(
    database: &Database,
    home: Option<&HomeService>,
    plugins: Option<&PluginService>,
    config_dir: Option<&Path>,
    update: AdminSettingsUpdate,
) -> Result<AdminSettingsSnapshot, AdminSettingsError> {
    let current = read_stored_admin_settings(database)
        .await
        .map_err(|_| AdminSettingsError::Unavailable)?;
    let login_background_source_was_requested = update.login_background_source.is_some();
    let login_background_source = match update.login_background_source {
        Some(source) if is_valid_login_background_source(&source) => source,
        Some(_) => return Err(AdminSettingsError::InvalidLoginBackgroundSource),
        None => current.login_background_source.clone(),
    };
    let login_background_source_changed =
        login_background_source != current.login_background_source;
    if login_background_source_was_requested
        && login_background_source_changed
        && let Some(plugin_id) = login_background_plugin_id(&login_background_source)
    {
        let provider_available = if let Some(plugins) = plugins {
            plugins
                .validate_login_background_provider(plugin_id)
                .await
                .is_ok()
        } else {
            false
        };
        if !provider_available {
            return Err(AdminSettingsError::LoginBackgroundPluginUnavailable);
        }
    }
    let media_strategy = update.media_strategy.unwrap_or(current.media_strategy);
    let server_name = match update.server_name {
        Some(name) => normalize_server_name(&name).ok_or(AdminSettingsError::InvalidServerName)?,
        None => current.server_name,
    };
    let resume_played_percent = update
        .resume_played_percent
        .unwrap_or(current.resume_played_percent);
    let resume_min_ticks = update.resume_min_ticks.unwrap_or(current.resume_min_ticks);
    let force_admin_library_order = update
        .force_admin_library_order
        .unwrap_or(current.force_admin_library_order);
    let user_deletion_grace_days = update
        .user_deletion_grace_days
        .unwrap_or(current.user_deletion_grace_days);
    if !(1..=3_650).contains(&user_deletion_grace_days) {
        return Err(AdminSettingsError::InvalidUserDeletionGraceDays);
    }
    let user_activity_retention_days = update
        .user_activity_retention_days
        .unwrap_or(current.user_activity_retention_days);
    if !(1..=3_650).contains(&user_activity_retention_days) {
        return Err(AdminSettingsError::InvalidUserActivityRetentionDays);
    }
    let trusted_proxy_cidrs = match update.trusted_proxy_cidrs {
        Some(values) => normalize_trusted_proxy_cidrs(values)?,
        None => current.trusted_proxy_cidrs,
    };
    let trusted_proxy_cidrs_json =
        serde_json::to_string(&trusted_proxy_cidrs).map_err(|_| AdminSettingsError::Unavailable)?;
    let mut new_user_permissions = update
        .new_user_permissions
        .unwrap_or(current.new_user_permissions);
    new_user_permissions.normalize_empty_selection();
    if !new_user_permissions.has_valid_shape()
        || (new_user_permissions.library_access_mode == "SELECTED"
            && !database
                .are_enabled_regular_libraries(&new_user_permissions.library_ids)
                .await
                .map_err(|_| AdminSettingsError::Unavailable)?)
    {
        return Err(AdminSettingsError::InvalidNewUserPermissions);
    }
    let new_user_permissions_json = serde_json::to_string(&new_user_permissions)
        .map_err(|_| AdminSettingsError::Unavailable)?;
    if !(1..=100).contains(&resume_played_percent)
        || resume_min_ticks < 0
        || !validate_media_strategy(&media_strategy)
    {
        return Err(AdminSettingsError::InvalidGlobalSettings);
    }
    if let Some(scraper_id) = media_strategy.scraper_id.as_deref() {
        let Some(plugins) = plugins else {
            return Err(AdminSettingsError::PluginServiceUnavailable);
        };
        plugins
            .validate_selection(Some(scraper_id))
            .await
            .map_err(AdminSettingsError::Plugin)?;
    }
    let media_strategy_json =
        serde_json::to_string(&media_strategy).map_err(|_| AdminSettingsError::Unavailable)?;
    match update.network_proxy {
        NetworkProxyChange::Unchanged => {}
        NetworkProxyChange::Invalid => {
            config_dir.ok_or(AdminSettingsError::Unavailable)?;
            return Err(AdminSettingsError::InvalidNetworkProxy);
        }
        NetworkProxyChange::Set(requested_proxy) => {
            let config_dir = config_dir.ok_or(AdminSettingsError::Unavailable)?;
            let proxy_url = match requested_proxy {
                None => None,
                Some(value) => {
                    let normalized = normalize_proxy_url(&value)
                        .map_err(|_| AdminSettingsError::InvalidNetworkProxy)?;
                    let current_proxy = read_network_proxy_url_async(config_dir).await;
                    let keep_current_credentials =
                        current_proxy.as_deref().is_some_and(|current| {
                            !proxy_url_has_credentials(&normalized).unwrap_or(true)
                                && redact_proxy_url(current).ok()
                                    == redact_proxy_url(&normalized).ok()
                        });
                    Some(if keep_current_credentials {
                        current_proxy.unwrap_or(normalized)
                    } else {
                        normalized
                    })
                }
            };
            write_network_proxy_url(config_dir, proxy_url.as_deref())
                .await
                .map_err(|_| AdminSettingsError::Unavailable)?;
        }
    }
    write_admin_settings(
        database,
        home,
        plugins,
        AdminSettingsWrite {
            resume_played_percent,
            resume_min_ticks,
            media_strategy_json: &media_strategy_json,
            force_admin_library_order,
            user_deletion_grace_days,
            user_activity_retention_days,
            trusted_proxy_cidrs_json: &trusted_proxy_cidrs_json,
            new_user_permissions_json: &new_user_permissions_json,
            login_background_source: &login_background_source,
            server_name: &server_name,
            refresh_login_background: login_background_source_changed
                && login_background_plugin_id(&login_background_source).is_some(),
        },
    )
    .await
    .map_err(|_| AdminSettingsError::Unavailable)?;
    let login_background_source_status =
        login_background_source_status(plugins, &login_background_source).await;
    let network_proxy = network_proxy_settings(config_dir).await;
    Ok(AdminSettingsSnapshot {
        server_name,
        resume_played_percent,
        resume_min_ticks,
        force_admin_library_order,
        user_deletion_grace_days,
        user_activity_retention_days,
        trusted_proxy_cidrs,
        new_user_permissions,
        login_background_source,
        login_background_source_status,
        media_strategy,
        network_proxy,
    })
}

async fn read_stored_admin_settings(database: &Database) -> Result<StoredAdminSettings, ()> {
    let (resume_played_percent, resume_min_ticks) =
        database.resume_settings().await.map_err(|_| ())?;
    let media_strategy = read_media_strategy_settings(database).await?;
    let force_admin_library_order = database.force_admin_library_order().await.map_err(|_| ())?;
    let user_deletion_grace_days = database.user_deletion_grace_days().await.map_err(|_| ())?;
    let user_activity_retention_days = database
        .user_activity_retention_days()
        .await
        .map_err(|_| ())?;
    let trusted_proxy_cidrs = read_trusted_proxy_cidrs(database).await?;
    let new_user_permissions = read_new_user_permissions(database).await?;
    let login_background_source = match database.login_background_source().await.map_err(|_| ())? {
        Some(source) if is_valid_login_background_source(&source) => source,
        _ => DEFAULT_LOGIN_BACKGROUND_SOURCE.to_owned(),
    };
    let server_name = match database.server_name().await.map_err(|_| ())? {
        Some(name) if !name.trim().is_empty() => name,
        _ => DEFAULT_SERVER_NAME.to_owned(),
    };
    Ok(StoredAdminSettings {
        server_name,
        resume_played_percent,
        resume_min_ticks,
        force_admin_library_order,
        user_deletion_grace_days,
        user_activity_retention_days,
        trusted_proxy_cidrs,
        new_user_permissions,
        login_background_source,
        media_strategy,
    })
}

pub(crate) async fn read_trusted_proxy_cidrs(database: &Database) -> Result<Vec<String>, ()> {
    let stored = database.trusted_proxy_cidrs_json().await.map_err(|_| ())?;
    let Some(stored) = stored else {
        return Ok(Vec::new());
    };
    let Ok(values) = serde_json::from_str::<Vec<String>>(&stored) else {
        return Ok(Vec::new());
    };
    Ok(normalize_trusted_proxy_cidrs(values).unwrap_or_default())
}

fn normalize_trusted_proxy_cidrs(values: Vec<String>) -> Result<Vec<String>, AdminSettingsError> {
    if values.len() > MAX_TRUSTED_PROXY_CIDRS {
        return Err(AdminSettingsError::InvalidTrustedProxyCidrs);
    }
    let mut seen = HashSet::with_capacity(values.len());
    let mut normalized = Vec::with_capacity(values.len());
    for value in values {
        let value = crate::application::user_access_rules::normalize_user_access_blacklist_value(
            crate::application::user_access_rules::UserAccessBlacklistRuleType::IpCidr,
            &value,
        )
        .map_err(|_| AdminSettingsError::InvalidTrustedProxyCidrs)?;
        if !seen.insert(value.clone()) {
            return Err(AdminSettingsError::InvalidTrustedProxyCidrs);
        }
        normalized.push(value);
    }
    Ok(normalized)
}

pub(crate) async fn read_new_user_permissions(
    database: &Database,
) -> Result<NewUserPermissions, ()> {
    let stored = database.new_user_permissions_json().await.map_err(|_| ())?;
    let Some(stored) = stored else {
        return Ok(NewUserPermissions::default());
    };
    let Ok(mut permissions) = serde_json::from_str::<NewUserPermissions>(&stored) else {
        return Ok(NewUserPermissions::default());
    };
    permissions.normalize_empty_selection();
    if permissions.has_valid_shape() {
        Ok(permissions)
    } else {
        Ok(NewUserPermissions::default())
    }
}

async fn login_background_source_status(plugins: Option<&PluginService>, source: &str) -> String {
    if let Some(plugins) = plugins {
        plugins.login_background_source_status(source).await
    } else if login_background_plugin_id(source).is_some() {
        "PLUGIN_UNAVAILABLE".to_owned()
    } else {
        "READY".to_owned()
    }
}

pub(crate) async fn network_proxy_settings(config_dir: Option<&Path>) -> NetworkProxySettings {
    if let Some(config_dir) = config_dir
        && let Some(proxy_url) = read_network_proxy_url_async(config_dir).await
    {
        let url = redact_proxy_url(&proxy_url).ok();
        let has_credentials = proxy_url_has_credentials(&proxy_url).unwrap_or(false);
        return NetworkProxySettings {
            configured: true,
            url,
            has_credentials,
            source: "settings",
            restart_required: true,
        };
    }
    if let Ok(Some(proxy_url)) = proxy_url_from_env() {
        return NetworkProxySettings {
            configured: true,
            url: redact_proxy_url(&proxy_url).ok(),
            has_credentials: proxy_url_has_credentials(&proxy_url).unwrap_or(false),
            source: "environment",
            restart_required: true,
        };
    }
    if standard_environment_proxy_configured() {
        return NetworkProxySettings {
            configured: true,
            url: None,
            has_credentials: false,
            source: "environment",
            restart_required: true,
        };
    }
    NetworkProxySettings {
        configured: false,
        url: None,
        has_credentials: false,
        source: "none",
        restart_required: true,
    }
}

pub(crate) async fn network_proxy_for_test(
    config_dir: Option<&Path>,
    requested_proxy: Option<String>,
) -> Result<(Option<String>, &'static str), ()> {
    let current = match config_dir {
        Some(config_dir) => read_network_proxy_url_async(config_dir).await,
        None => None,
    };
    if let Some(requested_proxy) = requested_proxy {
        let normalized = normalize_proxy_url(&requested_proxy).map_err(|_| ())?;
        let keep_current_credentials = current.as_deref().is_some_and(|current| {
            !proxy_url_has_credentials(&normalized).unwrap_or(true)
                && redact_proxy_url(current).ok() == redact_proxy_url(&normalized).ok()
        });
        return Ok((
            Some(if keep_current_credentials {
                current.unwrap_or(normalized)
            } else {
                normalized
            }),
            if keep_current_credentials {
                "settings"
            } else {
                "input"
            },
        ));
    }
    if let Some(current) = current {
        return Ok((Some(current), "settings"));
    }
    if let Ok(Some(proxy_url)) = proxy_url_from_env() {
        return Ok((Some(proxy_url), "environment"));
    }
    Ok((
        None,
        if standard_environment_proxy_configured() {
            "environment"
        } else {
            "none"
        },
    ))
}

pub(crate) async fn test_admin_network_proxy(
    config_dir: Option<&Path>,
    requested_proxy: Option<String>,
) -> Result<(&'static str, NetworkDiagnostics), ()> {
    let (proxy_url, proxy_source) = network_proxy_for_test(config_dir, requested_proxy).await?;
    Ok((proxy_source, test_network(proxy_url.as_deref()).await))
}

pub(crate) fn standard_environment_proxy_configured() -> bool {
    [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ]
    .into_iter()
    .any(|name| {
        std::env::var(name)
            .ok()
            .is_some_and(|value| !value.trim().is_empty())
    })
}

pub(crate) fn default_metadata_refresh_mode() -> String {
    "FILL_MISSING".to_owned()
}

pub(crate) fn default_show_metadata_pending() -> bool {
    true
}

pub(crate) fn default_thumbnail_scraping_mode() -> String {
    crate::application::thumbnail_policy::DEFAULT_THUMBNAIL_SCRAPING_MODE.to_owned()
}

pub(crate) async fn read_media_strategy_settings(
    database: &Database,
) -> Result<MediaStrategySettings, ()> {
    let stored = database.media_strategy_settings().await.map_err(|_| ())?;
    match stored {
        Some(value) => serde_json::from_str(&value).map_err(|_| ()),
        None => Ok(MediaStrategySettings::default()),
    }
}

pub(crate) fn valid_strategy_code(value: &str, max_length: usize) -> bool {
    !value.is_empty()
        && value.chars().count() <= max_length
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
}

pub(crate) fn valid_optional_strategy_code(value: &str, max_length: usize) -> bool {
    value.is_empty() || valid_strategy_code(value, max_length)
}

pub(crate) fn valid_plugin_id(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= 64
        && value
            .split('.')
            .all(|segment| valid_strategy_code(segment, 32))
}

pub(crate) fn normalize_server_name(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 80 || value.chars().any(char::is_control) {
        return None;
    }
    Some(value.to_owned())
}

pub(crate) fn validate_media_strategy(settings: &MediaStrategySettings) -> bool {
    valid_strategy_code(&settings.metadata_language, 32)
        && valid_optional_strategy_code(&settings.image_language, 32)
        && valid_strategy_code(&settings.region, 16)
        && matches!(
            settings.apply_scope.as_str(),
            "NEW_CONTENT" | "SELECTED_CONTENT" | "ALL_CONTENT"
        )
        && matches!(
            settings.metadata_refresh_mode.as_str(),
            "FILL_MISSING" | "FULL_REFRESH"
        )
        && settings
            .scraper_id
            .as_deref()
            .map(valid_plugin_id)
            .unwrap_or(true)
        && matches!(
            settings.images.thumbnail_scraping_mode.as_str(),
            "NONE" | "SCREENSHOT_FIRST" | "SCRAPER_FIRST"
        )
        && (0..=20).contains(&settings.images.max_backdrop_count)
        && (0..=8192).contains(&settings.images.min_download_width)
        && (1..=8).contains(&settings.subtitles.languages.len())
        && settings
            .subtitles
            .languages
            .iter()
            .all(|value| valid_strategy_code(value, 32))
}

pub(crate) struct AdminSettingsWrite<'a> {
    pub(crate) resume_played_percent: i64,
    pub(crate) resume_min_ticks: i64,
    pub(crate) media_strategy_json: &'a str,
    pub(crate) force_admin_library_order: bool,
    pub(crate) user_deletion_grace_days: i64,
    pub(crate) user_activity_retention_days: i64,
    pub(crate) trusted_proxy_cidrs_json: &'a str,
    pub(crate) new_user_permissions_json: &'a str,
    pub(crate) login_background_source: &'a str,
    pub(crate) server_name: &'a str,
    pub(crate) refresh_login_background: bool,
}

pub(crate) async fn write_admin_settings(
    database: &Database,
    home: Option<&HomeService>,
    plugins: Option<&PluginService>,
    settings: AdminSettingsWrite<'_>,
) -> Result<(), ()> {
    database
        .set_server_settings(
            settings.resume_played_percent,
            settings.resume_min_ticks,
            settings.media_strategy_json,
            settings.force_admin_library_order,
            settings.login_background_source,
        )
        .await
        .map_err(|_| ())?;
    database
        .set_user_deletion_grace_days(settings.user_deletion_grace_days)
        .await
        .map_err(|_| ())?;
    database
        .set_user_activity_retention_days(settings.user_activity_retention_days)
        .await
        .map_err(|_| ())?;
    database
        .set_trusted_proxy_cidrs_json(settings.trusted_proxy_cidrs_json)
        .await
        .map_err(|_| ())?;
    database
        .set_new_user_permissions_json(settings.new_user_permissions_json)
        .await
        .map_err(|_| ())?;
    if settings.refresh_login_background
        && let Some(plugins) = plugins
    {
        plugins.request_login_background_refresh();
    }
    database
        .set_server_name(settings.server_name)
        .await
        .map_err(|_| ())?;
    if let Some(home) = home {
        home.invalidate();
    }
    Ok(())
}

pub fn is_valid_login_background_source(value: &str) -> bool {
    matches!(
        value,
        DEFAULT_LOGIN_BACKGROUND_SOURCE | RECENTLY_ADDED_LOGIN_BACKGROUND_SOURCE
    ) || login_background_plugin_id(value).is_some()
}

pub fn login_background_plugin_id(source: &str) -> Option<&str> {
    let plugin_id = source.strip_prefix(LOGIN_BACKGROUND_PLUGIN_SOURCE_PREFIX)?;
    if plugin_id.is_empty()
        || plugin_id.len() > 128
        || !plugin_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return None;
    }
    Some(plugin_id)
}

pub fn read_network_proxy_url(config_dir: &Path) -> Option<String> {
    read_secret(config_dir.join(NETWORK_PROXY_URL_FILE))
}

fn read_secret(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

pub async fn read_network_proxy_url_async(config_dir: &Path) -> Option<String> {
    fs::read_to_string(config_dir.join(NETWORK_PROXY_URL_FILE))
        .await
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

pub async fn write_network_proxy_url(
    config_dir: &Path,
    proxy_url: Option<&str>,
) -> std::io::Result<()> {
    write_secret_file(config_dir, NETWORK_PROXY_URL_FILE, proxy_url).await
}

async fn write_secret_file(
    config_dir: &Path,
    file_name: &str,
    value: Option<&str>,
) -> std::io::Result<()> {
    fs::create_dir_all(config_dir).await?;
    let path = config_dir.join(file_name);
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        if let Err(error) = fs::remove_file(path).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(error);
            }
        }
        return Ok(());
    };
    let value = format!("{value}\n");
    tokio::task::spawn_blocking(move || {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&path)?;
        #[cfg(unix)]
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        file.write_all(value.as_bytes())?;
        file.sync_all()
    })
    .await
    .map_err(|error| std::io::Error::other(error.to_string()))?
}
