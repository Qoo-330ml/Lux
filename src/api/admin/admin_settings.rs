use super::*;
use crate::application::settings::{
    DEFAULT_LOGIN_BACKGROUND_SOURCE, is_valid_login_background_source, login_background_plugin_id,
};

pub(super) fn api_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/admin/settings",
            get(admin_settings).patch(admin_update_settings),
        )
        .route(
            "/api/v1/admin/api-key",
            get(admin_get_api_key).delete(admin_revoke_api_key),
        )
        .route("/api/v1/admin/api-key/rotate", post(admin_rotate_api_key))
        .route(
            "/api/v1/admin/settings/network-proxy/test",
            post(admin_test_network_proxy),
        )
}

pub(crate) async fn admin_settings(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let (played_percent, minimum_ticks) = match database.resume_settings().await {
        Ok(settings) => settings,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let media_strategy = match read_media_strategy_settings(database).await {
        Ok(settings) => settings,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let force_admin_library_order = match database.force_admin_library_order().await {
        Ok(enabled) => enabled,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let login_background_source = match database.login_background_source().await {
        Ok(Some(source)) if is_valid_login_background_source(&source) => source,
        Ok(_) => DEFAULT_LOGIN_BACKGROUND_SOURCE.to_owned(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let login_background_source_status = if let Some(plugins) = state.plugins.as_ref() {
        plugins
            .login_background_source_status(&login_background_source)
            .await
    } else if login_background_plugin_id(&login_background_source).is_some() {
        "PLUGIN_UNAVAILABLE".to_owned()
    } else {
        "READY".to_owned()
    };
    let server_name = match database.server_name().await {
        Ok(Some(name)) if !name.trim().is_empty() => name,
        Ok(_) => DEFAULT_SERVER_NAME.to_owned(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let network_proxy = network_proxy_settings(&state).await;
    Json(json!({
        "serverName": server_name,
        "resumePlayedPercent": played_percent,
        "resumeMinTicks": minimum_ticks,
        "forceAdminLibraryOrder": force_admin_library_order,
        "loginBackgroundSource": login_background_source,
        "loginBackgroundSourceStatus": login_background_source_status,
        "mediaStrategy": media_strategy,
        "networkProxy": network_proxy,
    }))
    .into_response()
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NetworkProxySettingsResponse {
    pub(crate) configured: bool,
    pub(crate) url: Option<String>,
    pub(crate) has_credentials: bool,
    pub(crate) source: &'static str,
    pub(crate) restart_required: bool,
}

pub(crate) async fn network_proxy_settings(state: &AppState) -> NetworkProxySettingsResponse {
    if let Some(config_dir) = state.config_dir.as_deref()
        && let Some(proxy_url) = read_network_proxy_url_async(config_dir).await
    {
        let url = redact_proxy_url(&proxy_url).ok();
        let has_credentials = proxy_url_has_credentials(&proxy_url).unwrap_or(false);
        return NetworkProxySettingsResponse {
            configured: true,
            url,
            has_credentials,
            source: "settings",
            restart_required: true,
        };
    }
    if let Ok(Some(proxy_url)) = proxy_url_from_env() {
        return NetworkProxySettingsResponse {
            configured: true,
            url: redact_proxy_url(&proxy_url).ok(),
            has_credentials: proxy_url_has_credentials(&proxy_url).unwrap_or(false),
            source: "environment",
            restart_required: true,
        };
    }
    if standard_environment_proxy_configured() {
        return NetworkProxySettingsResponse {
            configured: true,
            url: None,
            has_credentials: false,
            source: "environment",
            restart_required: true,
        };
    }
    NetworkProxySettingsResponse {
        configured: false,
        url: None,
        has_credentials: false,
        source: "none",
        restart_required: true,
    }
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

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NetworkProxyTestRequest {
    pub(crate) network_proxy_url: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NetworkProxyDiagnosticsResponse {
    pub(crate) proxy_source: &'static str,
    pub(crate) probes: Vec<NetworkProxyProbeResponse>,
    pub(crate) egress_ip: Option<String>,
    pub(crate) egress_country: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct NetworkProxyProbeResponse {
    pub(crate) id: &'static str,
    pub(crate) label: &'static str,
    pub(crate) latency_ms: Option<u64>,
    pub(crate) status: Option<u16>,
    pub(crate) reachable: bool,
    pub(crate) error: Option<&'static str>,
}

pub(crate) async fn admin_test_network_proxy(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<NetworkProxyTestRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let (proxy_url, proxy_source) =
        match network_proxy_for_test(&state, request.network_proxy_url).await {
            Ok(value) => value,
            Err(()) => {
                return api_error(
                    &headers,
                    StatusCode::BAD_REQUEST,
                    lux::ApiErrorCode::InvalidRequest,
                    "网络代理地址无效",
                )
                .into_response();
            }
        };
    let diagnostics = test_network(proxy_url.as_deref()).await;
    Json(network_proxy_diagnostics_response(
        proxy_source,
        diagnostics,
    ))
    .into_response()
}

pub(crate) async fn network_proxy_for_test(
    state: &AppState,
    requested_proxy: Option<String>,
) -> Result<(Option<String>, &'static str), ()> {
    let current = match state.config_dir.as_deref() {
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

pub(crate) fn network_proxy_diagnostics_response(
    proxy_source: &'static str,
    diagnostics: NetworkDiagnostics,
) -> NetworkProxyDiagnosticsResponse {
    NetworkProxyDiagnosticsResponse {
        proxy_source,
        probes: diagnostics
            .probes
            .into_iter()
            .map(network_proxy_probe_response)
            .collect(),
        egress_ip: diagnostics.egress_ip,
        egress_country: diagnostics.egress_country,
    }
}

pub(crate) fn network_proxy_probe_response(
    result: NetworkProbeResult,
) -> NetworkProxyProbeResponse {
    NetworkProxyProbeResponse {
        id: result.id,
        label: result.label,
        latency_ms: result.latency_ms,
        status: result.status,
        reachable: result.reachable,
        error: result.error,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdatePlaybackSettingsRequest {
    pub(crate) server_name: Option<String>,
    pub(crate) resume_played_percent: Option<i64>,
    pub(crate) resume_min_ticks: Option<i64>,
    pub(crate) force_admin_library_order: Option<bool>,
    pub(crate) login_background_source: Option<String>,
    pub(crate) media_strategy: Option<MediaStrategySettings>,
    #[serde(flatten)]
    pub(crate) extra: BTreeMap<String, Value>,
}

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

pub(crate) async fn admin_get_api_key(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin_web_session(&headers, &state, false).await {
        return response;
    }
    let Some(service) = state.admin_api_key.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.current().await {
        Ok(api_key) => Json(json!({
            "configured": api_key.is_some(),
            "apiKey": api_key,
        }))
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(crate) async fn admin_rotate_api_key(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin_web_session(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.admin_api_key.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.rotate().await {
        Ok(api_key) => {
            record_audit_event(
                &state,
                &headers,
                "ADMIN_API_KEY_ROTATED",
                Some("admin_api_key"),
                None,
                "{}",
            )
            .await;
            Json(json!({
                "configured": true,
                "apiKey": api_key,
            }))
            .into_response()
        }
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(crate) async fn admin_revoke_api_key(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin_web_session(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.admin_api_key.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.revoke().await {
        Ok(()) => {
            record_audit_event(
                &state,
                &headers,
                "ADMIN_API_KEY_REVOKED",
                Some("admin_api_key"),
                None,
                "{}",
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(crate) async fn admin_update_settings(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<UpdatePlaybackSettingsRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let requested_proxy = request.extra.get("networkProxyUrl").cloned();
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let (current_percent, current_ticks) = match database.resume_settings().await {
        Ok(settings) => settings,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let current_media_strategy = match read_media_strategy_settings(database).await {
        Ok(settings) => settings,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let current_force_admin_library_order = match database.force_admin_library_order().await {
        Ok(enabled) => enabled,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let current_login_background_source = match database.login_background_source().await {
        Ok(Some(source)) if is_valid_login_background_source(&source) => source,
        Ok(_) => DEFAULT_LOGIN_BACKGROUND_SOURCE.to_owned(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let current_server_name = match database.server_name().await {
        Ok(Some(name)) if !name.trim().is_empty() => name,
        Ok(_) => DEFAULT_SERVER_NAME.to_owned(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let percent = request.resume_played_percent.unwrap_or(current_percent);
    let minimum_ticks = request.resume_min_ticks.unwrap_or(current_ticks);
    let force_admin_library_order = request
        .force_admin_library_order
        .unwrap_or(current_force_admin_library_order);
    let login_background_source_was_requested = request.login_background_source.is_some();
    let login_background_source = match request.login_background_source {
        Some(source) if is_valid_login_background_source(&source) => source,
        Some(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "登录页背景来源无效",
            )
            .into_response();
        }
        None => current_login_background_source.clone(),
    };
    let login_background_source_changed =
        login_background_source != current_login_background_source;
    if login_background_source_was_requested
        && login_background_source_changed
        && let Some(plugin_id) = login_background_plugin_id(&login_background_source)
    {
        let provider_available = if let Some(plugins) = state.plugins.as_ref() {
            plugins
                .validate_login_background_provider(plugin_id)
                .await
                .is_ok()
        } else {
            false
        };
        if !provider_available {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "登录页背景插件不可用",
            )
            .into_response();
        }
    }
    let media_strategy = request.media_strategy.unwrap_or(current_media_strategy);
    let server_name = match request.server_name {
        Some(name) => match normalize_server_name(&name) {
            Some(name) => name,
            None => return StatusCode::BAD_REQUEST.into_response(),
        },
        None => current_server_name,
    };
    if !(1..=100).contains(&percent)
        || minimum_ticks < 0
        || !validate_media_strategy(&media_strategy)
    {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "全局媒体策略无效",
        )
        .into_response();
    }
    if let Some(scraper_id) = media_strategy.scraper_id.as_deref()
        && let Err(response) = validate_scraper_selection(&headers, &state, Some(scraper_id)).await
    {
        return response;
    }
    let media_strategy_json = match serde_json::to_string(&media_strategy) {
        Ok(value) => value,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    if let Some(requested_proxy) = requested_proxy {
        let Some(config_dir) = state.config_dir.as_deref() else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        let proxy_url = match requested_proxy {
            Value::Null => None,
            Value::String(value) => {
                let normalized = match normalize_proxy_url(&value) {
                    Ok(value) => value,
                    Err(_) => {
                        return api_error(
                            &headers,
                            StatusCode::BAD_REQUEST,
                            lux::ApiErrorCode::InvalidRequest,
                            "网络代理地址无效",
                        )
                        .into_response();
                    }
                };
                let current = read_network_proxy_url_async(config_dir).await;
                let keep_current_credentials = current.as_deref().is_some_and(|current| {
                    !proxy_url_has_credentials(&normalized).unwrap_or(true)
                        && redact_proxy_url(current).ok() == redact_proxy_url(&normalized).ok()
                });
                Some(if keep_current_credentials {
                    current.unwrap_or(normalized)
                } else {
                    normalized
                })
            }
            _ => {
                return api_error(
                    &headers,
                    StatusCode::BAD_REQUEST,
                    lux::ApiErrorCode::InvalidRequest,
                    "网络代理地址无效",
                )
                .into_response();
            }
        };
        if write_network_proxy_url(config_dir, proxy_url.as_deref())
            .await
            .is_err()
        {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    }
    if crate::application::settings::write_admin_settings(
        database,
        state.home.as_ref(),
        state.plugins.as_ref(),
        crate::application::settings::AdminSettingsWrite {
            resume_played_percent: percent,
            resume_min_ticks: minimum_ticks,
            media_strategy_json: &media_strategy_json,
            force_admin_library_order,
            login_background_source: &login_background_source,
            server_name: &server_name,
            refresh_login_background: login_background_source_changed
                && login_background_plugin_id(&login_background_source).is_some(),
        },
    )
    .await
    .is_err()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let login_background_source_status = if let Some(plugins) = state.plugins.as_ref() {
        plugins
            .login_background_source_status(&login_background_source)
            .await
    } else if login_background_plugin_id(&login_background_source).is_some() {
        "PLUGIN_UNAVAILABLE".to_owned()
    } else {
        "READY".to_owned()
    };
    record_audit_event(
        &state,
        &headers,
        "SETTINGS_UPDATED",
        Some("settings"),
        None,
        &format!(r#"{{"resumePlayedPercent":{percent},"resumeMinTicks":{minimum_ticks},"forceAdminLibraryOrder":{force_admin_library_order},"loginBackgroundSource":"{login_background_source}"}}"#),
    )
    .await;
    Json(json!({
        "serverName": server_name,
        "resumePlayedPercent": percent,
        "resumeMinTicks": minimum_ticks,
        "forceAdminLibraryOrder": force_admin_library_order,
        "loginBackgroundSource": login_background_source,
        "loginBackgroundSourceStatus": login_background_source_status,
        "mediaStrategy": media_strategy,
        "networkProxy": network_proxy_settings(&state).await,
    }))
    .into_response()
}
