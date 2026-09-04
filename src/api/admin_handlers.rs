use super::*;
use crate::application::scanner::BACKGROUND_SCAN_BATCH_SIZE;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateLibraryRequest {
    pub(crate) name: String,
    pub(crate) kind: String,
    #[serde(default = "default_realtime_watch_enabled")]
    pub(crate) realtime_watch_enabled: bool,
    #[serde(default = "default_realtime_metadata_auto_match_enabled")]
    pub(crate) realtime_metadata_auto_match_enabled: bool,
    pub(crate) scraper_id: Option<String>,
    pub(crate) scrapers: Option<Vec<LibraryScraperRequest>>,
    pub(crate) chapter_source_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LibraryScraperRequest {
    pub(crate) scraper_id: String,
    pub(crate) role: LibraryScraperRole,
}

pub(crate) fn default_realtime_watch_enabled() -> bool {
    true
}

pub(crate) fn default_realtime_metadata_auto_match_enabled() -> bool {
    true
}

#[derive(Deserialize)]
pub(crate) struct AddLibraryRootRequest {
    pub(crate) path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateLibraryRequest {
    pub(crate) name: Option<String>,
    pub(crate) kind: Option<String>,
    pub(crate) is_enabled: Option<bool>,
    pub(crate) realtime_watch_enabled: Option<bool>,
    pub(crate) realtime_metadata_auto_match_enabled: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_optional_optional")]
    #[allow(dead_code)]
    /// Accepted for legacy clients; realtime incremental scanning has no schedule.
    pub(crate) incremental_schedule: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_optional")]
    pub(crate) reconciliation_schedule: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_optional")]
    pub(crate) metadata_schedule: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_optional")]
    pub(crate) scraper_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_optional")]
    pub(crate) scrapers: Option<Option<Vec<LibraryScraperRequest>>>,
    #[serde(default, deserialize_with = "deserialize_optional_optional")]
    pub(crate) chapter_source_id: Option<Option<String>>,
    #[serde(default, deserialize_with = "deserialize_optional_optional")]
    pub(crate) media_strategy: Option<Option<MediaStrategySettings>>,
    pub(crate) scan_concurrency: Option<i64>,
    pub(crate) probe_concurrency: Option<i64>,
}

pub(crate) fn deserialize_optional_optional<'de, D, T>(
    deserializer: D,
) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Some(Option::<T>::deserialize(deserializer)?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetLibraryAccessRequest {
    pub(crate) can_view: bool,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MetadataCandidateQuery {
    pub(crate) page: Option<i64>,
    pub(crate) page_size: Option<i64>,
    #[serde(alias = "q")]
    pub(crate) search: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PersonMatchConfirmRequest {
    pub(crate) target_person_id: String,
    #[serde(default)]
    pub(crate) evidence: Value,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PersonMatchRejectRequest {
    #[serde(default)]
    pub(crate) evidence: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PersonIdentitySplitRequest {
    pub(crate) provider: String,
    pub(crate) provider_id: String,
    pub(crate) display_name: String,
    #[serde(default)]
    pub(crate) evidence: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PersonFieldLocksRequest {
    #[serde(default)]
    pub(crate) fields: Vec<String>,
    #[serde(default)]
    pub(crate) evidence: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MetadataCandidateSearchRequest {
    pub(crate) query: String,
    pub(crate) year: Option<i32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ItemImageSearchRequest {
    pub(crate) image_type: String,
    pub(crate) language: Option<String>,
    pub(crate) source: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ItemImageSelectRequest {
    pub(crate) image_type: String,
    pub(crate) url: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateExternalSubtitleRequest {
    pub(crate) source_id: String,
    pub(crate) title: Option<String>,
    pub(crate) language: Option<String>,
    pub(crate) is_default: bool,
    pub(crate) is_forced: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MetadataReidentifyRequest {
    #[serde(default)]
    pub(crate) item_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MetadataBatchConfirmationRequest {
    pub(crate) item_ids: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MetadataReidentifyListQuery {
    pub(crate) page: Option<i64>,
    pub(crate) page_size: Option<i64>,
    pub(crate) status: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum MetadataRefreshRequestMode {
    FillMissing,
    FullRefresh,
}

#[derive(Deserialize)]
pub(crate) struct MetadataRefreshRequest {
    pub(crate) mode: MetadataRefreshRequestMode,
}

impl MetadataRefreshRequestMode {
    const fn application_mode(&self) -> crate::application::reidentify::MetadataRefreshMode {
        match self {
            Self::FillMissing => crate::application::reidentify::MetadataRefreshMode::FillMissing,
            Self::FullRefresh => crate::application::reidentify::MetadataRefreshMode::FullRefresh,
        }
    }

    const fn as_str(&self) -> &'static str {
        match self {
            Self::FillMissing => "FILL_MISSING",
            Self::FullRefresh => "FULL_REFRESH",
        }
    }
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
    let current_server_name = match database.server_name().await {
        Ok(Some(name)) if !name.trim().is_empty() => name,
        Ok(_) => DEFAULT_SERVER_NAME.to_owned(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let percent = request.resume_played_percent.unwrap_or(current_percent);
    let minimum_ticks = request.resume_min_ticks.unwrap_or(current_ticks);
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
    if let Some(scraper_id) = media_strategy.scraper_id.as_deref() {
        if let Err(response) = validate_scraper_selection(&headers, &state, Some(scraper_id)).await
        {
            return response;
        }
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
    match database
        .set_server_settings(percent, minimum_ticks, &media_strategy_json)
        .await
    {
        Ok(()) => {
            if database.set_server_name(&server_name).await.is_err() {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            record_audit_event(
                &state,
                &headers,
                "SETTINGS_UPDATED",
                Some("settings"),
                None,
                &format!(r#"{{"resumePlayedPercent":{percent},"resumeMinTicks":{minimum_ticks}}}"#),
            )
            .await;
            Json(json!({
                "serverName": server_name,
                "resumePlayedPercent": percent,
                "resumeMinTicks": minimum_ticks,
                "mediaStrategy": media_strategy,
                "networkProxy": network_proxy_settings(&state).await,
            }))
            .into_response()
        }
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(crate) async fn admin_set_library_access(
    headers: HeaderMap,
    Path((user_id, library_id)): Path<(String, String)>,
    State(state): State<AppState>,
    Json(request): Json<SetLibraryAccessRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let user_id = match user_id.parse::<crate::domain::ids::UserId>() {
        Ok(id) => id.to_string(),
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "用户 ID 无效",
            )
            .into_response();
        }
    };
    let library_id = match library_id.parse::<crate::domain::ids::LibraryId>() {
        Ok(id) => id.to_string(),
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "媒体库 ID 无效",
            )
            .into_response();
        }
    };
    let Some(database) = state.database.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let user_exists = match database.user_exists(&user_id).await {
        Ok(exists) => exists,
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "数据库暂时不可用",
            )
            .into_response();
        }
    };
    if !user_exists {
        return api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "用户不存在",
        )
        .into_response();
    }
    let library_exists = match database.library_exists(&library_id).await {
        Ok(exists) => exists,
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "数据库暂时不可用",
            )
            .into_response();
        }
    };
    if !library_exists {
        return api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "媒体库不存在",
        )
        .into_response();
    }
    match database
        .set_user_library_access(&user_id, &library_id, request.can_view)
        .await
    {
        Ok(()) => {
            if let Some(home) = state.home.as_ref() {
                home.invalidate();
            }
            record_audit_event(
                &state,
                &headers,
                "LIBRARY_ACCESS_UPDATED",
                Some("user_library_access"),
                Some(&user_id),
                &format!(
                    r#"{{"libraryId":"{library_id}","canView":{}}}"#,
                    request.can_view
                ),
            )
            .await;
            Json(json!({
                "userId": user_id,
                "libraryId": library_id,
                "canView": request.can_view,
            }))
            .into_response()
        }
        Err(_) => api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "数据库暂时不可用",
        )
        .into_response(),
    }
}

pub(crate) async fn admin_start_scan(
    headers: HeaderMap,
    Path(library_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Ok(library_id) = library_id.parse::<crate::domain::ids::LibraryId>() else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体库 ID 无效",
        )
        .into_response();
    };
    let Some(scan_jobs) = state.scan_jobs.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let job = match scan_jobs.create_movie_scan_job(library_id).await {
        Ok(job) => job,
        Err(ScanJobError::LibraryNotFound) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "媒体库不存在",
            )
            .into_response();
        }
        Err(ScanJobError::AlreadyActive(_)) => {
            return api_error(
                &headers,
                StatusCode::CONFLICT,
                lux::ApiErrorCode::InvalidRequest,
                "媒体库已有扫描任务运行",
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let worker = scan_jobs.clone();
    let job_id = job.id.clone();
    let probe = state.probe.clone();
    let metadata = state.metadata_reidentify.clone();
    let thumbnails = state.thumbnails.clone();
    tokio::spawn(async move {
        let _ = worker
            .run_to_completion_with_metadata_and_thumbnails(
                &job_id,
                BACKGROUND_SCAN_BATCH_SIZE,
                probe,
                metadata,
                thumbnails,
            )
            .await;
    });
    let target_id = job.id.clone();
    record_audit_event(
        &state,
        &headers,
        "SCAN_STARTED",
        Some("scan_job"),
        Some(&target_id),
        "{}",
    )
    .await;
    (
        StatusCode::ACCEPTED,
        Json(json!({ "job": scan_job_json(&job) })),
    )
        .into_response()
}

pub(crate) async fn admin_start_library_reidentify(
    headers: HeaderMap,
    Path(library_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Ok(library_id) = library_id.parse::<crate::domain::ids::LibraryId>() else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体库 ID 无效",
        )
        .into_response();
    };
    let Some(reidentify) = state.metadata_reidentify.clone() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "元数据刮削器匹配服务尚未配置",
        )
        .into_response();
    };
    let job = match reidentify
        .create_library_refresh_job(
            &library_id.to_string(),
            crate::application::reidentify::MetadataRefreshMode::FillMissing,
        )
        .await
    {
        Ok(job) => job,
        Err(error) => return metadata_reidentify_error(&headers, error),
    };
    let job_id = job.id.clone();
    tokio::spawn(async move {
        reidentify.run(&job_id).await;
    });
    record_audit_event(
        &state,
        &headers,
        "METADATA_REIDENTIFY_STARTED",
        Some("library"),
        Some(&library_id.to_string()),
        &format!(
            r#"{{"itemCount":{},"jobId":"{}"}}"#,
            job.total_count, job.id
        ),
    )
    .await;
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "totalCount": job.total_count,
            "job": metadata_reidentify_job_json(&job),
        })),
    )
        .into_response()
}

pub(crate) async fn spawn_library_scan(
    state: &AppState,
    library_id: crate::domain::ids::LibraryId,
) -> Result<Option<ScanJob>, ScanJobError> {
    let Some(scan_jobs) = state.scan_jobs.as_ref() else {
        return Ok(None);
    };
    let job = match scan_jobs
        .create_movie_scan_job_with_metadata(library_id, false)
        .await
    {
        Ok(job) => job,
        Err(ScanJobError::AlreadyActive(_)) => return Ok(None),
        Err(error) => return Err(error),
    };
    let worker = scan_jobs.clone();
    let job_id = job.id.clone();
    let probe = state.probe.clone();
    let metadata = state.metadata_reidentify.clone();
    let thumbnails = state.thumbnails.clone();
    tokio::spawn(async move {
        let _ = worker
            .run_to_completion_with_metadata_and_thumbnails(
                &job_id,
                BACKGROUND_SCAN_BATCH_SIZE,
                probe,
                metadata,
                thumbnails,
            )
            .await;
    });
    Ok(Some(job))
}

pub(crate) async fn admin_start_library_metadata_refresh(
    headers: HeaderMap,
    Path(library_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<MetadataRefreshRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Ok(library_id) = library_id.parse::<crate::domain::ids::LibraryId>() else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体库 ID 无效",
        )
        .into_response();
    };
    let Some(reidentify) = state.metadata_reidentify.clone() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "元数据刷新服务尚未配置",
        )
        .into_response();
    };
    let mode = request.mode.application_mode();
    let job = match reidentify
        .create_library_refresh_job(&library_id.to_string(), mode)
        .await
    {
        Ok(job) => job,
        Err(error) => return metadata_reidentify_error(&headers, error),
    };
    let job_id = job.id.clone();
    tokio::spawn(async move {
        reidentify.run(&job_id).await;
    });
    record_audit_event(
        &state,
        &headers,
        "METADATA_REFRESH_STARTED",
        Some("library"),
        Some(&library_id.to_string()),
        &format!(
            r#"{{"itemCount":{},"jobId":"{}","mode":"{}"}}"#,
            job.total_count,
            job.id,
            request.mode.as_str()
        ),
    )
    .await;
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "totalCount": job.total_count,
            "mode": request.mode.as_str(),
            "job": metadata_reidentify_job_json(&job),
        })),
    )
        .into_response()
}

pub(crate) async fn admin_start_item_scan(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    if item_id.parse::<crate::domain::ids::ItemId>().is_err() {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体条目 ID 无效",
        )
        .into_response();
    }
    let Some(scan_jobs) = state.scan_jobs.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let job = match scan_jobs.create_item_folder_scan_job(&item_id).await {
        Ok(job) => job,
        Err(ScanJobError::ItemNotFound) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "媒体条目不存在",
            )
            .into_response();
        }
        Err(ScanJobError::LibraryNotFound) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "媒体条目不存在",
            )
            .into_response();
        }
        Err(ScanJobError::AlreadyActive(_)) => {
            return api_error(
                &headers,
                StatusCode::CONFLICT,
                lux::ApiErrorCode::InvalidRequest,
                "媒体库已有扫描任务运行",
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let worker = scan_jobs.clone();
    let job_id = job.id.clone();
    let probe = state.probe.clone();
    let metadata = state.metadata_reidentify.clone();
    let thumbnails = state.thumbnails.clone();
    tokio::spawn(async move {
        let _ = worker
            .run_to_completion_with_metadata_and_thumbnails(
                &job_id,
                BACKGROUND_SCAN_BATCH_SIZE,
                probe,
                metadata,
                thumbnails,
            )
            .await;
    });
    let target_id = job.id.clone();
    record_audit_event(
        &state,
        &headers,
        "SCAN_STARTED",
        Some("scan_job"),
        Some(&target_id),
        "{}",
    )
    .await;
    (
        StatusCode::ACCEPTED,
        Json(json!({ "job": scan_job_json(&job) })),
    )
        .into_response()
}

pub(crate) async fn admin_start_item_metadata_refresh(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<MetadataRefreshRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Ok(item_id) = item_id.parse::<crate::domain::ids::ItemId>() else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体条目 ID 无效",
        )
        .into_response();
    };
    let Some(reidentify) = state.metadata_reidentify.clone() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "元数据刷新服务尚未配置",
        )
        .into_response();
    };
    let mode = request.mode.application_mode();
    let job = match reidentify
        .create_item_refresh_job(&item_id.to_string(), mode)
        .await
    {
        Ok(job) => job,
        Err(error) => return metadata_reidentify_error(&headers, error),
    };
    let job_id = job.id.clone();
    tokio::spawn(async move {
        reidentify.run(&job_id).await;
    });
    record_audit_event(
        &state,
        &headers,
        "METADATA_REFRESH_STARTED",
        Some("item"),
        Some(&item_id.to_string()),
        &format!(
            r#"{{"jobId":"{}","mode":"{}"}}"#,
            job.id,
            request.mode.as_str()
        ),
    )
    .await;
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "totalCount": job.total_count,
            "mode": request.mode.as_str(),
            "job": metadata_reidentify_job_json(&job),
        })),
    )
        .into_response()
}

pub(crate) async fn admin_update_item_subtitle(
    headers: HeaderMap,
    Path((item_id, stream_index)): Path<(String, String)>,
    State(state): State<AppState>,
    Json(request): Json<UpdateExternalSubtitleRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    if item_id.parse::<crate::domain::ids::ItemId>().is_err() || request.source_id.trim().is_empty()
    {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "字幕或媒体条目参数无效",
        )
        .into_response();
    }
    let Ok(stream_index) = stream_index.parse::<i64>() else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "字幕轨道编号无效",
        )
        .into_response();
    };
    if stream_index < 0
        || request.source_id.chars().count() > 128
        || request
            .title
            .as_deref()
            .is_some_and(|value| value.chars().count() > 256)
        || request
            .language
            .as_deref()
            .is_some_and(|value| value.chars().count() > 32)
    {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "字幕属性长度无效",
        )
        .into_response();
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let title = request
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let language = request
        .language
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let updated = match database
        .update_external_subtitle(ExternalSubtitleUpdate {
            item_id: &item_id,
            media_source_id: request.source_id.trim(),
            stream_index,
            title,
            language,
            is_default: request.is_default,
            is_forced: request.is_forced,
        })
        .await
    {
        Ok(updated) => updated,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    if !updated {
        return api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "外挂字幕不存在",
        )
        .into_response();
    }
    record_audit_event(
        &state,
        &headers,
        "SUBTITLE_UPDATED",
        Some("media_stream"),
        Some(&format!("{}:{}", request.source_id.trim(), stream_index)),
        "{}",
    )
    .await;
    Json(json!({
        "sourceId": request.source_id.trim(),
        "streamIndex": stream_index,
        "title": title,
        "language": language,
        "isDefault": request.is_default,
        "isForced": request.is_forced,
    }))
    .into_response()
}

pub(crate) async fn admin_delete_item(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<LuxStreamQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    delete_media_item(&headers, &state, &item_id, query.source_id.as_deref()).await
}

pub(crate) async fn delete_media_item(
    headers: &HeaderMap,
    state: &AppState,
    item_id: &str,
    source_id: Option<&str>,
) -> Response {
    if item_id.parse::<crate::domain::ids::ItemId>().is_err() {
        return api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体条目 ID 无效",
        )
        .into_response();
    }
    let Some(deletion) = state.deletion.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let report = match deletion.delete(item_id, source_id).await {
        Ok(report) => report,
        Err(MediaDeleteError::ItemNotFound) => {
            return api_error(
                headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "媒体文件不存在",
            )
            .into_response();
        }
        Err(MediaDeleteError::PathOutsideRoot(_)) => {
            return api_error(
                headers,
                StatusCode::FORBIDDEN,
                lux::ApiErrorCode::PermissionDenied,
                "媒体路径不在媒体库根目录内",
            )
            .into_response();
        }
        Err(MediaDeleteError::InvalidFileName(_)) => {
            return api_error(
                headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "媒体文件名无效",
            )
            .into_response();
        }
        Err(MediaDeleteError::Io(_) | MediaDeleteError::Storage(_)) => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    };
    if let Some(home) = state.home.as_ref() {
        home.invalidate();
    }
    let audit_target = if report.source_ids.len() == 1 {
        ("media_source", report.source_ids[0].as_str())
    } else {
        ("media_item", report.item_id.as_str())
    };
    record_audit_event(
        state,
        headers,
        "MEDIA_DELETED",
        Some(audit_target.0),
        Some(audit_target.1),
        &format!(
            r#"{{"itemId":"{}","fileCount":{},"sourceCount":{}}}"#,
            report.item_id,
            report.deleted_file_count,
            report.source_ids.len()
        ),
    )
    .await;
    StatusCode::NO_CONTENT.into_response()
}

pub(crate) async fn admin_refresh_collection(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(collections) = state.collections.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "刮削器合集服务尚未配置",
        )
        .into_response();
    };
    match collections.refresh_for_item(&item_id).await {
        Ok(report) => {
            record_audit_event(
                &state,
                &headers,
                "COLLECTION_REFRESHED",
                Some("item"),
                Some(&item_id),
                "{}",
            )
            .await;
            (
                StatusCode::OK,
                Json(json!({
                    "sourceItemId": report.source_item_id,
                    "collectionItemId": report.collection_item_id,
                    "memberCount": report.member_count,
                })),
            )
                .into_response()
        }
        Err(CollectionError::MovieProviderIdMissing | CollectionError::NoCollection) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "电影没有可用的刮削器合集",
        )
        .into_response(),
        Err(CollectionError::InvalidProviderId) => api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "刮削器 provider ID 无效",
        )
        .into_response(),
        Err(
            CollectionError::Scraper(_)
            | CollectionError::Storage(_)
            | CollectionError::Metadata(_),
        ) => api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "刮削器合集刷新失败，可重试",
        )
        .into_response(),
    }
}

pub(crate) async fn admin_cancel_scan(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(scan_jobs) = state.scan_jobs.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match scan_jobs.cancel(&job_id).await {
        Ok(()) => {
            record_audit_event(
                &state,
                &headers,
                "SCAN_CANCELLED",
                Some("scan_job"),
                Some(&job_id),
                "{}",
            )
            .await;
            StatusCode::ACCEPTED.into_response()
        }
        Err(ScanJobError::JobNotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(crate) async fn admin_start_strm_probe(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.strm_probe.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let jobs = match service.create_configured_jobs().await {
        Ok(jobs) => jobs,
        Err(error) => return strm_probe_error(&headers, error),
    };
    for job in &jobs {
        let worker = service.clone();
        let job_id = job.id.clone();
        tokio::spawn(async move {
            if let Err(error) = worker.run(&job_id).await {
                tracing::error!(job_id = %job_id, %error, "STRM probe job stopped");
            }
        });
    }
    let operation_id = jobs
        .first()
        .map(|job| job.operation_id.clone())
        .unwrap_or_default();
    record_audit_event(
        &state,
        &headers,
        "STRM_PROBE_STARTED",
        Some("strm_probe_operation"),
        Some(&operation_id),
        &format!(r#"{{"jobCount":{}}}"#, jobs.len()),
    )
    .await;
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "operationId": operation_id,
            "jobs": jobs,
        })),
    )
        .into_response()
}

pub(crate) async fn admin_list_strm_probe_jobs(
    headers: HeaderMap,
    Query(query): Query<AdminJobsQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let (offset, limit) = match page_params(query.page, query.page_size) {
        Ok(params) => params,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    let status = query.status.as_deref().map(str::to_ascii_uppercase);
    if status.as_deref().is_some_and(|value| {
        !matches!(
            value,
            "PENDING" | "RUNNING" | "COMPLETED" | "CANCELLED" | "FAILED"
        )
    }) {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "任务状态无效",
        )
        .into_response();
    }
    let Some(service) = state.strm_probe.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.list(status.as_deref(), offset, limit).await {
        Ok(jobs) => Json(json!({
            "jobs": jobs,
            "page": offset / limit + 1,
            "pageSize": limit,
        }))
        .into_response(),
        Err(error) => strm_probe_error(&headers, error),
    }
}

pub(crate) async fn admin_get_strm_probe_job(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(service) = state.strm_probe.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.get(&job_id).await {
        Ok(job) => Json(json!({ "job": job })).into_response(),
        Err(error) => strm_probe_error(&headers, error),
    }
}

pub(crate) async fn admin_cancel_strm_probe(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.strm_probe.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.cancel(&job_id).await {
        Ok(()) => {
            record_audit_event(
                &state,
                &headers,
                "STRM_PROBE_CANCEL_REQUESTED",
                Some("strm_probe_job"),
                Some(&job_id),
                "{}",
            )
            .await;
            StatusCode::ACCEPTED.into_response()
        }
        Err(error) => strm_probe_error(&headers, error),
    }
}

pub(crate) async fn admin_retry_strm_probe(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.strm_probe.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let job = match service.retry(&job_id).await {
        Ok(job) => job,
        Err(error) => return strm_probe_error(&headers, error),
    };
    let worker = service.clone();
    let new_job_id = job.id.clone();
    tokio::spawn(async move {
        if let Err(error) = worker.run(&new_job_id).await {
            tracing::error!(job_id = %new_job_id, %error, "retried STRM probe job stopped");
        }
    });
    record_audit_event(
        &state,
        &headers,
        "STRM_PROBE_RETRIED",
        Some("strm_probe_job"),
        Some(&job_id),
        &format!(r#"{{"newJobId":"{}"}}"#, job.id),
    )
    .await;
    (StatusCode::ACCEPTED, Json(json!({ "job": job }))).into_response()
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChapterDetectionRequest {
    #[serde(default)]
    pub(crate) plugin_id: Option<String>,
    #[serde(default)]
    pub(crate) concurrency: Option<i64>,
    #[serde(default)]
    pub(crate) intro_window_seconds: Option<i64>,
    #[serde(default)]
    pub(crate) credits_window_seconds: Option<i64>,
    #[serde(default)]
    pub(crate) match_threshold: Option<u32>,
    #[serde(default)]
    pub(crate) force_refresh: bool,
}

pub(crate) async fn admin_start_chapter_detection(
    headers: HeaderMap,
    Path(library_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<ChapterDetectionRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Ok(library_id) = library_id.parse::<crate::domain::ids::LibraryId>() else {
        return chapter_detection_error(&headers, ChapterDetectionError::LibraryNotFound);
    };
    let Some(service) = state.chapter_detection.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let defaults = ChapterDetectionOptions::default();
    let plugin_id = request
        .plugin_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_CHAPTER_DETECTOR_PLUGIN_ID);
    let configured_options = if let Some(plugins) = state.plugins.as_ref() {
        match plugins.chapter_detector_settings(plugin_id).await {
            Ok(settings) => Some(ChapterDetectionOptions {
                concurrency: settings.concurrency,
                intro_window_seconds: settings.intro_window_seconds,
                credits_window_seconds: settings.credits_window_seconds,
                match_threshold: settings.match_threshold,
                force_refresh: false,
            }),
            Err(
                PluginServiceError::InvalidConfig
                | PluginServiceError::UnknownPlugin(_)
                | PluginServiceError::Unavailable(_),
            ) => None,
            Err(error) => {
                return chapter_detection_error(&headers, ChapterDetectionError::Plugin(error));
            }
        }
    } else {
        None
    };
    let configured_options = configured_options.unwrap_or(defaults);
    let options = ChapterDetectionOptions {
        concurrency: request
            .concurrency
            .unwrap_or(configured_options.concurrency),
        intro_window_seconds: request
            .intro_window_seconds
            .unwrap_or(configured_options.intro_window_seconds),
        credits_window_seconds: request
            .credits_window_seconds
            .unwrap_or(configured_options.credits_window_seconds),
        match_threshold: request
            .match_threshold
            .unwrap_or(configured_options.match_threshold),
        force_refresh: request.force_refresh,
    };
    let job = match service
        .create_library_job(library_id, plugin_id, options)
        .await
    {
        Ok(job) => job,
        Err(error) => return chapter_detection_error(&headers, error),
    };
    let worker = service.clone();
    let job_id = job.id.clone();
    tokio::spawn(async move {
        if let Err(error) = worker.run(&job_id).await {
            tracing::error!(job_id = %job_id, %error, "chapter detection job stopped");
        }
    });
    record_audit_event(
        &state,
        &headers,
        "CHAPTER_DETECTION_STARTED",
        Some("chapter_detection_job"),
        Some(&job.id),
        &format!(
            r#"{{"libraryId":"{}","pluginId":"{}"}}"#,
            job.library_id, job.plugin_id
        ),
    )
    .await;
    (StatusCode::ACCEPTED, Json(json!({ "job": job }))).into_response()
}

pub(crate) async fn admin_list_chapter_detection_jobs(
    headers: HeaderMap,
    Query(query): Query<AdminJobsQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let (offset, limit) = match page_params(query.page, query.page_size) {
        Ok(params) => params,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    let status = query.status.as_deref().map(str::to_ascii_uppercase);
    if status.as_deref().is_some_and(|value| {
        !matches!(
            value,
            "PENDING" | "RUNNING" | "COMPLETED" | "CANCELLED" | "FAILED"
        )
    }) {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "任务状态无效",
        )
        .into_response();
    }
    let Some(service) = state.chapter_detection.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.list(status.as_deref(), offset, limit).await {
        Ok(jobs) => Json(json!({
            "jobs": jobs,
            "page": offset / limit + 1,
            "pageSize": limit,
        }))
        .into_response(),
        Err(error) => chapter_detection_error(&headers, error),
    }
}

pub(crate) async fn admin_get_chapter_detection_job(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(service) = state.chapter_detection.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.get(&job_id).await {
        Ok(job) => Json(json!({ "job": job })).into_response(),
        Err(error) => chapter_detection_error(&headers, error),
    }
}

pub(crate) async fn admin_cancel_chapter_detection(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.chapter_detection.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.cancel(&job_id).await {
        Ok(()) => {
            record_audit_event(
                &state,
                &headers,
                "CHAPTER_DETECTION_CANCEL_REQUESTED",
                Some("chapter_detection_job"),
                Some(&job_id),
                "{}",
            )
            .await;
            StatusCode::ACCEPTED.into_response()
        }
        Err(error) => chapter_detection_error(&headers, error),
    }
}

pub(crate) async fn admin_retry_chapter_detection(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.chapter_detection.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let job = match service.retry(&job_id).await {
        Ok(job) => job,
        Err(error) => return chapter_detection_error(&headers, error),
    };
    let worker = service.clone();
    let new_job_id = job.id.clone();
    tokio::spawn(async move {
        if let Err(error) = worker.run(&new_job_id).await {
            tracing::error!(job_id = %new_job_id, %error, "retried chapter detection job stopped");
        }
    });
    record_audit_event(
        &state,
        &headers,
        "CHAPTER_DETECTION_RETRIED",
        Some("chapter_detection_job"),
        Some(&job_id),
        &format!(r#"{{"newJobId":"{}"}}"#, job.id),
    )
    .await;
    (StatusCode::ACCEPTED, Json(json!({ "job": job }))).into_response()
}

pub(crate) fn chapter_detection_error(
    headers: &HeaderMap,
    error: ChapterDetectionError,
) -> Response {
    let (status, code, message) = match error {
        ChapterDetectionError::InvalidOptions
        | ChapterDetectionError::InvalidPluginResult
        | ChapterDetectionError::SourceChanged
        | ChapterDetectionError::LibraryNotSupported => (
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "章节检测参数或插件结果无效",
        ),
        ChapterDetectionError::AlreadyActive => (
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "已有章节检测任务运行中",
        ),
        ChapterDetectionError::LibraryNotFound | ChapterDetectionError::JobNotFound => (
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "章节检测目标不存在",
        ),
        ChapterDetectionError::NotRetryable => (
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "章节检测任务不可重试",
        ),
        ChapterDetectionError::NotCancellable => (
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "章节检测任务不可取消",
        ),
        ChapterDetectionError::PluginUnavailable(_) => (
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "章节检测插件不可用",
        ),
        ChapterDetectionError::WorkerFailed
        | ChapterDetectionError::Plugin(_)
        | ChapterDetectionError::Storage(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "章节检测服务暂时不可用",
        ),
    };
    api_error(headers, status, code, message).into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DanmakuMatchRequest {
    #[serde(default = "default_danmaku_concurrency")]
    pub(crate) concurrency: i64,
    #[serde(default)]
    pub(crate) overwrite: bool,
}

const fn default_danmaku_concurrency() -> i64 {
    crate::application::danmaku::DEFAULT_DANMAKU_CONCURRENCY
}

pub(crate) async fn admin_start_danmaku_match(
    headers: HeaderMap,
    Path(library_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<DanmakuMatchRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let library_id = match library_id.parse::<crate::domain::ids::LibraryId>() {
        Ok(value) => value,
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "媒体库 ID 无效",
            )
            .into_response();
        }
    };
    let Some(service) = state.danmaku.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let job = match service
        .create_job(library_id, request.concurrency, request.overwrite)
        .await
    {
        Ok(job) => job,
        Err(error) => return danmaku_service_error(&headers, error),
    };
    let worker = service.clone();
    let job_id = job.id.clone();
    tokio::spawn(async move {
        if let Err(error) = worker.run(&job_id).await {
            tracing::error!(job_id = %job_id, %error, "danmaku match job stopped");
        }
    });
    record_audit_event(
        &state,
        &headers,
        "DANMAKU_MATCH_STARTED",
        Some("danmaku_match_job"),
        Some(&job.id),
        "{}",
    )
    .await;
    (StatusCode::ACCEPTED, Json(json!({ "job": job }))).into_response()
}

pub(crate) async fn admin_list_danmaku_match_jobs(
    headers: HeaderMap,
    Query(query): Query<AdminJobsQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let (offset, limit) = match page_params(query.page, query.page_size) {
        Ok(value) => value,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    let status = query.status.as_deref().map(str::to_ascii_uppercase);
    if status.as_deref().is_some_and(|value| {
        !matches!(
            value,
            "PENDING" | "RUNNING" | "COMPLETED" | "CANCELLED" | "FAILED"
        )
    }) {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "任务状态无效",
        )
        .into_response();
    }
    let Some(service) = state.danmaku.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.list(status.as_deref(), offset, limit).await {
        Ok(jobs) => Json(json!({
            "jobs": jobs,
            "page": offset / limit + 1,
            "pageSize": limit,
        }))
        .into_response(),
        Err(error) => danmaku_service_error(&headers, error),
    }
}

pub(crate) async fn admin_get_danmaku_match_job(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(service) = state.danmaku.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.get(&job_id).await {
        Ok(job) => Json(json!({ "job": job })).into_response(),
        Err(error) => danmaku_service_error(&headers, error),
    }
}

pub(crate) async fn admin_cancel_danmaku_match(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.danmaku.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.cancel(&job_id).await {
        Ok(()) => {
            record_audit_event(
                &state,
                &headers,
                "DANMAKU_MATCH_CANCEL_REQUESTED",
                Some("danmaku_match_job"),
                Some(&job_id),
                "{}",
            )
            .await;
            StatusCode::ACCEPTED.into_response()
        }
        Err(error) => danmaku_service_error(&headers, error),
    }
}

pub(crate) async fn admin_retry_danmaku_match(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.danmaku.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let job = match service.retry(&job_id).await {
        Ok(job) => job,
        Err(error) => return danmaku_service_error(&headers, error),
    };
    let worker = service.clone();
    let new_job_id = job.id.clone();
    tokio::spawn(async move {
        if let Err(error) = worker.run(&new_job_id).await {
            tracing::error!(job_id = %new_job_id, %error, "retried danmaku match job stopped");
        }
    });
    record_audit_event(
        &state,
        &headers,
        "DANMAKU_MATCH_RETRIED",
        Some("danmaku_match_job"),
        Some(&job_id),
        &format!(r#"{{"newJobId":"{}"}}"#, job.id),
    )
    .await;
    (StatusCode::ACCEPTED, Json(json!({ "job": job }))).into_response()
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AdminJobsQuery {
    pub(crate) page: Option<i64>,
    pub(crate) page_size: Option<i64>,
    pub(crate) status: Option<String>,
    pub(crate) search: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AdminJobEventsQuery {
    pub(crate) page: Option<i64>,
    pub(crate) page_size: Option<i64>,
    pub(crate) level: Option<String>,
    pub(crate) event_code: Option<String>,
}

#[derive(Deserialize, Default)]
pub(crate) struct AdminLogExportQuery {
    pub(crate) from: Option<String>,
    pub(crate) to: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AdminScheduledTasksQuery {
    pub(crate) page: Option<i64>,
    pub(crate) page_size: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AdminScheduledTaskRequest {
    pub(crate) owner_type: String,
    pub(crate) owner_id: String,
    pub(crate) task_type: String,
    pub(crate) schedule: Option<String>,
    pub(crate) is_enabled: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AdminScheduledTaskRunRequest {
    pub(crate) owner_type: String,
    pub(crate) owner_id: String,
    pub(crate) task_type: String,
}

const SCHEDULE_TASK_TYPES: [&str; 4] = [
    "RECONCILIATION_SCAN",
    "METADATA_PARSE",
    "CHAPTER_DETECTION",
    "AUTO_LIBRARY_COVER",
];

pub(crate) async fn admin_run_scheduled_task(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<AdminScheduledTaskRunRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(scheduled_tasks) = state.scheduled_tasks.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let owner_type = request.owner_type.trim();
    let owner_id = request.owner_id.trim();
    let task_type = request.task_type.trim();
    match scheduled_tasks
        .run_task(owner_type, owner_id, task_type)
        .await
    {
        Ok(run) => (
            StatusCode::ACCEPTED,
            Json(json!({
                "status": "ACCEPTED",
                "taskType": run.task_type(),
                "run": scheduled_task_run_json(&run),
            })),
        )
            .into_response(),
        Err(error) => scheduled_task_error(&headers, error),
    }
}

pub(crate) fn scheduled_task_run_json(run: &ScheduledTaskRun) -> Value {
    match run {
        ScheduledTaskRun::Reconciliation { job } => json!({ "jobId": job.id }),
        ScheduledTaskRun::Metadata { job } => json!({ "jobId": job.id }),
        ScheduledTaskRun::StrmMediaInfo {
            operation_id, jobs, ..
        } => json!({
            "operationId": operation_id,
            "jobIds": jobs.iter().map(|job| job.id.clone()).collect::<Vec<_>>(),
        }),
        ScheduledTaskRun::ChapterDetection { job } => json!({ "jobId": job.id }),
        ScheduledTaskRun::AutoLibraryCover { job } => {
            json!({ "jobId": job.id, "libraryId": job.library_id })
        }
        ScheduledTaskRun::DanmakuMatch { jobs } => {
            json!({ "jobIds": jobs.iter().map(|job| job.id.clone()).collect::<Vec<_>>() })
        }
    }
}

pub(crate) fn scheduled_task_error(headers: &HeaderMap, error: ScheduledTaskError) -> Response {
    let (status, code, message) = match error {
        ScheduledTaskError::InvalidOwner
        | ScheduledTaskError::UnsupportedTask
        | ScheduledTaskError::NotRegistered => (
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "注册任务不存在",
        ),
        ScheduledTaskError::Disabled => (
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "任务当前不可执行",
        ),
        ScheduledTaskError::Scan(ScanJobError::AlreadyActive(_))
        | ScheduledTaskError::Strm(StrmProbeError::AlreadyActive)
        | ScheduledTaskError::Chapter(ChapterDetectionError::AlreadyActive)
        | ScheduledTaskError::Cover(LibraryCoverError::AlreadyActive)
        | ScheduledTaskError::Danmaku(DanmakuServiceError::AlreadyActive) => (
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "同类任务已有运行中的作业",
        ),
        ScheduledTaskError::ServiceUnavailable
        | ScheduledTaskError::Scan(_)
        | ScheduledTaskError::Metadata(_)
        | ScheduledTaskError::Strm(_)
        | ScheduledTaskError::Chapter(_)
        | ScheduledTaskError::Cover(_)
        | ScheduledTaskError::Danmaku(_)
        | ScheduledTaskError::Storage(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "注册任务暂时无法执行",
        ),
    };
    api_error(headers, status, code, message).into_response()
}

pub(crate) async fn admin_list_task_activity(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let mut activities = Vec::new();
    let scan_jobs = match database.list_scan_jobs_for_activity(100).await {
        Ok(jobs) => jobs,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    activities.extend(scan_jobs.iter().map(|job| {
        json!({
            "id": job.id,
            "kind": "scan",
            "taskType": job.job_type,
            "libraryId": job.library_id,
            "status": job.status,
            "processedCount": job.processed_count,
            "totalCount": job.total_count,
            "cancelRequested": job.cancel_requested,
            "currentItem": job.current_item,
            "scanPhase": job.scan_phase,
        })
    }));
    for status in ["PENDING", "RUNNING"] {
        let metadata_status = if status == "PENDING" {
            "QUEUED"
        } else {
            status
        };
        let metadata_jobs = match database
            .list_metadata_reidentify_jobs(Some(metadata_status), 0, 100)
            .await
        {
            Ok(jobs) => jobs,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        activities.extend(metadata_jobs.iter().map(|job| {
            json!({
                "id": job.id,
                "kind": "metadata",
                "taskType": job.mode,
                "libraryId": job.library_id,
                "status": job.status,
                "processedCount": job.processed_count,
                "totalCount": job.total_count,
                "cancelRequested": job.cancel_requested,
            })
        }));

        let strm_jobs = match database.list_strm_probe_jobs(Some(status), 0, 100).await {
            Ok(jobs) => jobs,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        activities.extend(strm_jobs.iter().map(|job| {
            json!({
                "id": job.id,
                "kind": "strm",
                "taskType": "STRM_MEDIA_INFO",
                "libraryId": job.library_id,
                "status": job.status,
                "processedCount": job.processed_count,
                "totalCount": job.total_count,
                "cancelRequested": job.cancel_requested,
            })
        }));

        let chapter_jobs = match database
            .list_chapter_detection_jobs(Some(status), 0, 100)
            .await
        {
            Ok(jobs) => jobs,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        activities.extend(chapter_jobs.iter().map(|job| {
            json!({
                "id": job.id,
                "kind": "chapter",
                "taskType": "CHAPTER_DETECTION",
                "libraryId": job.library_id,
                "status": job.status,
                "processedCount": job.processed_count,
                "totalCount": job.total_count,
                "cancelRequested": job.cancel_requested,
            })
        }));

        let danmaku_jobs = match database.list_danmaku_match_jobs(Some(status), 0, 100).await {
            Ok(jobs) => jobs,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        activities.extend(danmaku_jobs.iter().map(|job| {
            json!({
                "id": job.id,
                "kind": "danmaku",
                "taskType": "DANMAKU_MATCH",
                "libraryId": job.library_id,
                "status": job.status,
                "processedCount": job.processed_count,
                "totalCount": job.total_count,
                "cancelRequested": job.cancel_requested,
            })
        }));

        let library_cover_jobs = match database.list_library_cover_jobs(Some(status), 0, 100).await
        {
            Ok(jobs) => jobs,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        activities.extend(library_cover_jobs.iter().map(|job| {
            json!({
                "id": job.id,
                "kind": "cover",
                "taskType": "AUTO_LIBRARY_COVER",
                "libraryId": job.library_id,
                "status": job.status,
                "processedCount": job.processed_count,
                "totalCount": job.total_count,
            })
        }));
    }
    activities.sort_by(|left, right| {
        right["status"]
            .as_str()
            .unwrap_or_default()
            .cmp(left["status"].as_str().unwrap_or_default())
            .then_with(|| {
                left["id"]
                    .as_str()
                    .unwrap_or_default()
                    .cmp(right["id"].as_str().unwrap_or_default())
            })
    });
    Json(json!({ "activities": activities })).into_response()
}

pub(crate) async fn admin_list_scheduled_tasks(
    headers: HeaderMap,
    Query(query): Query<AdminScheduledTasksQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let (offset, limit) = match page_params(query.page, query.page_size) {
        Ok(params) => params,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database.list_scheduled_task_configs(offset, limit).await {
        Ok((tasks, total)) => Json(json!({
            "scheduledTasks": tasks.iter().map(scheduled_task_json).collect::<Vec<_>>(),
            "total": total,
            "page": offset / limit + 1,
            "pageSize": limit,
        }))
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(crate) async fn admin_upsert_scheduled_task(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<AdminScheduledTaskRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let owner_type = request.owner_type.trim().to_ascii_uppercase();
    if owner_type != "GLOBAL" && owner_type != "LIBRARY" {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "计划归属必须是全局或媒体库",
        )
        .into_response();
    }
    let task_type = request.task_type.trim().to_ascii_uppercase();
    let is_global_strm = owner_type == "GLOBAL"
        && task_type == crate::application::schedule::STRM_MEDIA_INFO_TASK_TYPE;
    let is_global_danmaku = owner_type == "GLOBAL"
        && task_type == crate::application::schedule::DANMAKU_MATCH_TASK_TYPE;
    if !SCHEDULE_TASK_TYPES.contains(&task_type.as_str()) && !is_global_strm && !is_global_danmaku {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "任务类型无效",
        )
        .into_response();
    }
    if owner_type == "GLOBAL" {
        if !request.owner_id.trim().eq_ignore_ascii_case("global") {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "全局计划的 ownerId 必须是 global",
            )
            .into_response();
        }
        if is_global_strm || is_global_danmaku {
            let Some(database) = state.database.as_ref() else {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            };
            match database
                .find_scheduled_task_config("GLOBAL", "global", &task_type)
                .await
            {
                Ok(Some(_)) => {}
                Ok(None) => {
                    return api_error(
                        &headers,
                        StatusCode::NOT_FOUND,
                        lux::ApiErrorCode::NotFound,
                        "任务尚未注册",
                    )
                    .into_response();
                }
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            }
            let Some(schedule) = request
                .schedule
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                return api_error(
                    &headers,
                    StatusCode::BAD_REQUEST,
                    lux::ApiErrorCode::InvalidRequest,
                    if is_global_strm {
                        "STRM 媒体信息任务必须保留 Cron 执行计划"
                    } else {
                        "弹幕匹配任务必须保留 Cron 执行计划"
                    },
                )
                .into_response();
            };
            let Some(plugins) = state.plugins.as_ref() else {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            };
            let result = if is_global_strm {
                plugins.update_media_info_schedule(schedule).await
            } else {
                plugins.update_danmaku_schedule(schedule).await
            };
            if let Err(error) = result {
                return plugin_error(&headers, error);
            }
            let task = match database
                .find_scheduled_task_config("GLOBAL", "global", &task_type)
                .await
            {
                Ok(Some(task)) => task,
                Ok(None) => {
                    return api_error(
                        &headers,
                        StatusCode::NOT_FOUND,
                        lux::ApiErrorCode::NotFound,
                        "任务尚未注册",
                    )
                    .into_response();
                }
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            };
            record_audit_event(
                &state,
                &headers,
                "SCHEDULE_UPDATED",
                Some("scheduled_task"),
                Some(&format!("global:{task_type}")),
                "{}",
            )
            .await;
            return (
                StatusCode::OK,
                Json(json!({ "scheduledTask": scheduled_task_json(&task) })),
            )
                .into_response();
        }
        let enabled = request.is_enabled.unwrap_or(request.schedule.is_some());
        let schedule = if enabled {
            request.schedule.as_deref().map(str::trim)
        } else {
            None
        };
        let Some(database) = state.database.as_ref() else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        let task = match database
            .upsert_scheduled_task_config("GLOBAL", "global", &task_type, schedule, enabled)
            .await
        {
            Ok(Some(task)) => task,
            Ok(None) => {
                return api_error(
                    &headers,
                    StatusCode::NOT_FOUND,
                    lux::ApiErrorCode::NotFound,
                    "任务尚未注册",
                )
                .into_response();
            }
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        let target_id = format!("global:{task_type}");
        record_audit_event(
            &state,
            &headers,
            "SCHEDULE_UPDATED",
            Some("scheduled_task"),
            Some(&target_id),
            "{}",
        )
        .await;
        return (
            StatusCode::OK,
            Json(json!({ "scheduledTask": scheduled_task_json(&task) })),
        )
            .into_response();
    }
    let library_id = match request
        .owner_id
        .trim()
        .parse::<crate::domain::ids::LibraryId>()
    {
        Ok(id) => id,
        Err(error) => {
            return library_error(
                &headers,
                LibraryServiceError::InvalidLibraryId(error.to_string()),
            );
        }
    };
    let Some(libraries) = state.libraries.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database
        .find_scheduled_task_config("LIBRARY", &library_id.to_string(), &task_type)
        .await
    {
        Ok(Some(_)) => {}
        Ok(None) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "任务尚未注册",
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let enabled = request.is_enabled.unwrap_or(request.schedule.is_some());
    let schedule = if enabled {
        request.schedule.map(|value| value.trim().to_owned())
    } else {
        None
    };
    if let Some(schedule) = schedule.as_deref()
        && validate_cron(schedule).is_err()
    {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "Cron 执行计划无效",
        )
        .into_response();
    }
    let mut settings = LibrarySettingsPatch::default();
    match task_type.as_str() {
        "RECONCILIATION_SCAN" => settings.reconciliation_schedule = Some(schedule),
        "METADATA_PARSE" => settings.metadata_schedule = Some(schedule),
        "AUTO_LIBRARY_COVER" => {
            let task = match database
                .upsert_scheduled_task_config(
                    "LIBRARY",
                    &library_id.to_string(),
                    &task_type,
                    schedule.as_deref(),
                    enabled,
                )
                .await
            {
                Ok(Some(task)) => task,
                Ok(None) => {
                    return api_error(
                        &headers,
                        StatusCode::NOT_FOUND,
                        lux::ApiErrorCode::NotFound,
                        "任务尚未注册",
                    )
                    .into_response();
                }
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            };
            let target_id = format!("{}:{}", library_id, task_type);
            record_audit_event(
                &state,
                &headers,
                "SCHEDULE_UPDATED",
                Some("scheduled_task"),
                Some(&target_id),
                "{}",
            )
            .await;
            return (
                StatusCode::OK,
                Json(json!({ "scheduledTask": scheduled_task_json(&task) })),
            )
                .into_response();
        }
        "CHAPTER_DETECTION" => {
            let Some(schedule) = schedule.as_deref() else {
                return api_error(
                    &headers,
                    StatusCode::BAD_REQUEST,
                    lux::ApiErrorCode::InvalidRequest,
                    "片头片尾检测任务必须保留 Cron 执行计划",
                )
                .into_response();
            };
            let plugin_id = match database
                .find_scheduled_task_config("LIBRARY", &library_id.to_string(), &task_type)
                .await
            {
                Ok(Some(task)) => match task.plugin_id {
                    Some(plugin_id) => plugin_id,
                    None => {
                        return api_error(
                            &headers,
                            StatusCode::NOT_FOUND,
                            lux::ApiErrorCode::NotFound,
                            "片头片尾检测任务插件未注册",
                        )
                        .into_response();
                    }
                },
                Ok(None) => {
                    return api_error(
                        &headers,
                        StatusCode::NOT_FOUND,
                        lux::ApiErrorCode::NotFound,
                        "片头片尾检测任务插件未注册",
                    )
                    .into_response();
                }
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            };
            let Some(plugins) = state.plugins.as_ref() else {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            };
            if let Err(error) = plugins
                .update_chapter_detector_schedule(&plugin_id, schedule)
                .await
            {
                return plugin_error(&headers, error);
            }
            let task = match database
                .find_scheduled_task_config("LIBRARY", &library_id.to_string(), &task_type)
                .await
            {
                Ok(Some(task)) => task,
                Ok(None) => {
                    return api_error(
                        &headers,
                        StatusCode::NOT_FOUND,
                        lux::ApiErrorCode::NotFound,
                        "任务尚未注册",
                    )
                    .into_response();
                }
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            };
            let target_id = format!("{}:{}", library_id, task_type);
            record_audit_event(
                &state,
                &headers,
                "SCHEDULE_UPDATED",
                Some("scheduled_task"),
                Some(&target_id),
                "{}",
            )
            .await;
            return (
                StatusCode::OK,
                Json(json!({ "scheduledTask": scheduled_task_json(&task) })),
            )
                .into_response();
        }
        _ => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "任务类型无效",
            )
            .into_response();
        }
    }
    if let Err(error) = libraries.update_settings(library_id, settings).await {
        return library_error(&headers, error);
    }
    let task = match database
        .find_scheduled_task_config("LIBRARY", &library_id.to_string(), &task_type)
        .await
    {
        Ok(Some(task)) => task,
        Ok(None) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "任务尚未注册",
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let target_id = format!("{}:{}", library_id, task_type);
    record_audit_event(
        &state,
        &headers,
        "SCHEDULE_UPDATED",
        Some("scheduled_task"),
        Some(&target_id),
        "{}",
    )
    .await;
    (
        StatusCode::OK,
        Json(json!({ "scheduledTask": scheduled_task_json(&task) })),
    )
        .into_response()
}

pub(crate) async fn admin_list_jobs(
    headers: HeaderMap,
    Query(query): Query<AdminJobsQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let (offset, limit) = match page_params(query.page, query.page_size) {
        Ok(params) => params,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    let status = query.status.as_deref().map(str::to_ascii_uppercase);
    if status.as_deref().is_some_and(|value| {
        !matches!(
            value,
            "PENDING" | "RUNNING" | "COMPLETED" | "CANCELLED" | "FAILED"
        )
    }) {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "任务状态无效",
        )
        .into_response();
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database
        .list_scan_jobs(status.as_deref(), offset, limit)
        .await
    {
        Ok(jobs) => Json(json!({
            "jobs": jobs.iter().map(scan_job_json_from_storage).collect::<Vec<_>>(),
            "page": offset / limit + 1,
            "pageSize": limit,
        }))
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(crate) async fn admin_get_job(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database.find_scan_job(&job_id).await {
        Ok(Some(job)) => Json(json!({ "job": scan_job_json_from_storage(&job) })).into_response(),
        Ok(None) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "任务不存在",
        )
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(crate) async fn admin_list_job_events(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    Query(query): Query<AdminJobEventsQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let (offset, limit) = match page_params(query.page, query.page_size) {
        Ok(params) => params,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    let level = query.level.as_deref().map(str::to_ascii_uppercase);
    if level
        .as_deref()
        .is_some_and(|value| !matches!(value, "INFO" | "WARN" | "ERROR"))
    {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "日志级别无效",
        )
        .into_response();
    }
    let event_code = query
        .event_code
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_uppercase);
    if event_code.as_deref().is_some_and(|value| {
        value.chars().count() > 64
            || value.chars().any(|character| {
                !(character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_')
            })
    }) {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "事件代码无效",
        )
        .into_response();
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database.find_scan_job(&job_id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "任务不存在",
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let total = match database
        .count_scan_job_events(&job_id, level.as_deref(), event_code.as_deref())
        .await
    {
        Ok(total) => total,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match database
        .list_scan_job_events(
            &job_id,
            level.as_deref(),
            event_code.as_deref(),
            offset,
            limit,
        )
        .await
    {
        Ok(events) => Json(json!({
            "events": events.iter().map(scan_job_event_json).collect::<Vec<_>>(),
            "total": total,
            "page": offset / limit + 1,
            "pageSize": limit,
        }))
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(crate) async fn admin_retry_scan(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(scan_jobs) = state.scan_jobs.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let job = match scan_jobs.retry(&job_id).await {
        Ok(job) => job,
        Err(ScanJobError::JobNotFound) => return StatusCode::NOT_FOUND.into_response(),
        Err(ScanJobError::AlreadyActive(_)) => {
            return api_error(
                &headers,
                StatusCode::CONFLICT,
                lux::ApiErrorCode::InvalidRequest,
                "任务仍在运行或不可重试",
            )
            .into_response();
        }
        Err(ScanJobError::LibraryNotFound) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let worker = scan_jobs.clone();
    let new_job_id = job.id.clone();
    let probe = state.probe.clone();
    let metadata = state.metadata_reidentify.clone();
    let thumbnails = state.thumbnails.clone();
    tokio::spawn(async move {
        let _ = worker
            .run_to_completion_with_metadata_and_thumbnails(
                &new_job_id,
                BACKGROUND_SCAN_BATCH_SIZE,
                probe,
                metadata,
                thumbnails,
            )
            .await;
    });
    record_audit_event(
        &state,
        &headers,
        "SCAN_RETRIED",
        Some("scan_job"),
        Some(&job_id),
        &format!(r#"{{"newJobId":"{}"}}"#, job.id),
    )
    .await;
    (
        StatusCode::ACCEPTED,
        Json(json!({ "job": scan_job_json(&job) })),
    )
        .into_response()
}

pub(crate) fn scan_job_json_from_storage(job: &crate::storage::StoredScanJob) -> Value {
    json!({
        "id": job.id,
        "libraryId": job.library_id,
        "jobType": job.job_type,
        "status": job.status,
        "generation": job.generation,
        "cursor": job.cursor,
        "processedCount": job.processed_count,
        "totalCount": job.total_count,
        "discoveryCompleted": job.discovery_completed,
        "cancelRequested": job.cancel_requested,
        "error": job.error,
        "createdAt": job.created_at,
        "startedAt": job.started_at,
        "finishedAt": job.finished_at,
        "currentItem": job.current_item,
        "scanPhase": job.scan_phase,
    })
}

pub(crate) fn scheduled_task_json(task: &crate::storage::StoredScheduledTaskConfig) -> Value {
    let resource_limit =
        serde_json::from_str::<Value>(&task.resource_limit_json).unwrap_or_else(|_| json!({}));
    let owner_name = task
        .library_name
        .clone()
        .or_else(|| (task.owner_type == "GLOBAL").then(|| "全局".to_owned()));
    json!({
        "id": format!("{}:{}:{}", task.owner_type, task.owner_id, task.task_type),
        "ownerType": task.owner_type,
        "ownerId": task.owner_id,
        "ownerName": owner_name,
        "taskType": task.task_type,
        "name": task.task_name,
        "description": task.task_description,
        "sourceType": task.source_type,
        "pluginId": task.plugin_id,
        "schedule": task.cron_or_interval,
        "isEnabled": task.is_enabled,
        "resourceLimit": resource_limit,
        "createdAt": task.created_at,
        "updatedAt": task.updated_at,
    })
}

pub(crate) fn scan_job_event_json(event: &crate::storage::StoredScanJobEvent) -> Value {
    let details = serde_json::from_str::<Value>(&event.details_json)
        .unwrap_or_else(|_| json!({ "invalid": true }));
    json!({
        "id": event.id,
        "jobId": event.job_id,
        "level": event.level,
        "eventCode": event.event_code,
        "message": event.message,
        "details": details,
        "createdAt": event.created_at,
    })
}

pub(crate) fn scan_job_json(job: &crate::application::scanner::ScanJob) -> Value {
    json!({
        "id": job.id,
        "libraryId": job.library_id,
        "jobType": job.job_type,
        "status": job.status,
        "generation": job.generation,
        "cursor": job.cursor,
        "processedCount": job.processed_count,
        "totalCount": job.total_count,
        "discoveryCompleted": job.discovery_completed,
        "cancelRequested": job.cancel_requested,
        "error": job.error,
        "createdAt": job.created_at,
        "startedAt": job.started_at,
        "finishedAt": job.finished_at,
        "currentItem": job.current_item,
        "scanPhase": job.scan_phase,
    })
}

pub(crate) async fn require_admin(
    headers: &HeaderMap,
    state: &AppState,
    require_csrf: bool,
) -> Result<(), Response> {
    let user = require_web_user(headers, state).await?;
    if !user.can_manage_server {
        return Err(api_error(
            headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::PermissionDenied,
            "没有服务器管理权限",
        )
        .into_response());
    }
    if require_csrf && lux_api_key_from_headers(headers).is_none() {
        let Some(auth) = state.auth.as_ref() else {
            return Err(api_error(
                headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "服务尚未就绪",
            )
            .into_response());
        };
        let Some(session_token) = request_cookie(headers, "lux_session") else {
            return Err(api_error(
                headers,
                StatusCode::UNAUTHORIZED,
                lux::ApiErrorCode::AuthenticationRequired,
                "需要登录",
            )
            .into_response());
        };
        let session = match auth.resolve(&session_token).await {
            Ok(Some(session)) => session,
            Ok(None) => {
                return Err(api_error(
                    headers,
                    StatusCode::UNAUTHORIZED,
                    lux::ApiErrorCode::AuthenticationRequired,
                    "需要登录",
                )
                .into_response());
            }
            Err(_) => {
                return Err(api_error(
                    headers,
                    StatusCode::SERVICE_UNAVAILABLE,
                    lux::ApiErrorCode::DatabaseUnavailable,
                    "认证暂时不可用",
                )
                .into_response());
            }
        };
        let Some(csrf_token) = headers
            .get("x-csrf-token")
            .and_then(|value| value.to_str().ok())
        else {
            return Err(api_error(
                headers,
                StatusCode::FORBIDDEN,
                lux::ApiErrorCode::CsrfFailed,
                "CSRF 校验失败",
            )
            .into_response());
        };
        if !auth.verify_csrf(&session, csrf_token) {
            return Err(api_error(
                headers,
                StatusCode::FORBIDDEN,
                lux::ApiErrorCode::CsrfFailed,
                "CSRF 校验失败",
            )
            .into_response());
        }
    }
    Ok(())
}

pub(crate) async fn require_admin_web_session(
    headers: &HeaderMap,
    state: &AppState,
    require_csrf: bool,
) -> Result<(), Response> {
    if lux_api_key_from_headers(headers).is_some() {
        return Err(api_error(
            headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::PermissionDenied,
            "API Key 不能管理 API Key",
        )
        .into_response());
    }
    require_admin(headers, state, require_csrf).await
}

pub(crate) async fn resolve_shared_admin_api_key(
    headers: &HeaderMap,
    state: &AppState,
) -> Result<Option<UserRecord>, Response> {
    let Some(candidate) = lux_api_key_from_headers(headers) else {
        return Ok(None);
    };
    let Some(service) = state.admin_api_key.as_ref() else {
        return Err(api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "认证服务尚未就绪",
        )
        .into_response());
    };
    service.resolve(&candidate).await.map_err(|_| {
        api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "认证暂时不可用",
        )
        .into_response()
    })
}

pub(crate) fn lux_api_key_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get("X-Lux-Api-Key")
        .or_else(|| headers.get("X-Emby-Token"))
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            headers
                .get("Authorization")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().strip_prefix("Bearer "))
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        })
}

pub(crate) async fn normalize_lux_api_key_query(request: Request<Body>, next: Next) -> Response {
    let mut request = request;
    if request.uri().path().starts_with("/api/v1")
        && !request.headers().contains_key("X-Lux-Api-Key")
        && !request.headers().contains_key("X-Emby-Token")
        && !request.headers().contains_key("X-MediaBrowser-Token")
        && !request.headers().contains_key("Authorization")
        && let Some(query) = request.uri().query()
        && let Some(key) =
            url::form_urlencoded::parse(query.as_bytes()).find_map(|(name, value)| {
                name.eq_ignore_ascii_case("api_key")
                    .then_some(value.into_owned())
            })
        && let Ok(value) = HeaderValue::from_str(&key)
    {
        request.headers_mut().insert("X-Lux-Api-Key", value);
    }
    next.run(request).await
}

pub(crate) async fn record_audit_event(
    state: &AppState,
    headers: &HeaderMap,
    event_type: &str,
    target_type: Option<&str>,
    target_id: Option<&str>,
    metadata_json: &str,
) {
    let Some(database) = state.database.as_ref() else {
        return;
    };
    let (actor_user_id, metadata_json) = if let Some(candidate) = lux_api_key_from_headers(headers)
    {
        let Some(service) = state.admin_api_key.as_ref() else {
            return;
        };
        let Ok(Some(_)) = service.resolve(&candidate).await else {
            return;
        };
        (None, audit_metadata_for_shared_api_key(metadata_json))
    } else {
        let (Some(auth), Some(session_token)) =
            (state.auth.as_ref(), request_cookie(headers, "lux_session"))
        else {
            return;
        };
        let Ok(Some(session)) = auth.resolve(&session_token).await else {
            return;
        };
        (Some(session.user.id.to_string()), metadata_json.to_owned())
    };
    if database
        .insert_audit_event(crate::storage::NewAuditEvent {
            actor_user_id: actor_user_id.as_deref(),
            event_type,
            target_type,
            target_id,
            metadata_json: &metadata_json,
        })
        .await
        .is_ok()
    {
        state
            .admin_events
            .publish(admin_event_scope_for_audit(event_type));
    }
}

pub(crate) fn audit_metadata_for_shared_api_key(metadata_json: &str) -> String {
    let Ok(mut metadata) = serde_json::from_str::<Value>(metadata_json) else {
        return "{\"auth\":\"admin_api_key\"}".to_owned();
    };
    if let Value::Object(object) = &mut metadata {
        object.insert("auth".to_owned(), Value::String("admin_api_key".to_owned()));
    } else {
        metadata = json!({ "auth": "admin_api_key", "details": metadata });
    }
    serde_json::to_string(&metadata).unwrap_or_else(|_| "{\"auth\":\"admin_api_key\"}".to_owned())
}

pub(crate) fn admin_event_scope_for_audit(event_type: &str) -> AdminEventScope {
    if event_type == "SETTINGS_UPDATED" {
        return AdminEventScope::Settings;
    }
    if event_type.starts_with("PLUGIN_") {
        return AdminEventScope::Plugins;
    }
    if event_type.starts_with("USER_") || event_type == "LIBRARY_ACCESS_UPDATED" {
        return AdminEventScope::Users;
    }
    if event_type.starts_with("LIBRARY_") || event_type == "SCHEDULE_UPDATED" {
        return AdminEventScope::Libraries;
    }
    if event_type.starts_with("METADATA_") {
        return AdminEventScope::Metadata;
    }
    if event_type.starts_with("SCAN_")
        || event_type.starts_with("STRM_")
        || event_type.starts_with("DANMAKU_")
    {
        return AdminEventScope::Jobs;
    }
    AdminEventScope::All
}

pub(crate) async fn record_activity_event(
    database: Option<&Database>,
    admin_events: &AdminEventHub,
    user_id: &str,
    event_type: &str,
    target_id: Option<&str>,
    metadata: Value,
) {
    let Some(database) = database else {
        return;
    };
    let metadata_json = match serde_json::to_string(&metadata) {
        Ok(metadata_json) => metadata_json,
        Err(_) => return,
    };
    if database
        .insert_audit_event(crate::storage::NewAuditEvent {
            actor_user_id: Some(user_id),
            event_type,
            target_type: target_id.map(|_| "media_item"),
            target_id,
            metadata_json: &metadata_json,
        })
        .await
        .is_ok()
    {
        admin_events.publish(AdminEventScope::Dashboard);
    }
}

pub(crate) fn playback_activity_event_type(
    previous: Option<&StoredPlaybackSession>,
    state_name: &str,
) -> Option<&'static str> {
    if previous.is_some_and(|session| session.state == state_name)
        || (previous.is_none() && state_name == "STOPPED")
    {
        return None;
    }
    match state_name {
        "PLAYING" => Some("PLAYBACK_STARTED"),
        "PAUSED" => Some("PLAYBACK_PAUSED"),
        "STOPPED" => Some("PLAYBACK_STOPPED"),
        _ => None,
    }
}

pub(crate) async fn admin_list_libraries(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(libraries) = state.libraries.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    match libraries.list_libraries().await {
        Ok(views) => Json(json!({
            "libraries": views
                .iter()
                .map(|view| library_json(&view.library, &view.roots))
                .collect::<Vec<_>>()
        }))
        .into_response(),
        Err(error) => library_error(&headers, error),
    }
}

pub(crate) async fn admin_list_directories(
    headers: HeaderMap,
    Query(query): Query<DirectoryBrowseQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let (offset, limit) = match page_params(query.page, query.page_size) {
        Ok(params) if params.0 <= 10_000 => params,
        Ok(_) | Err(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "分页参数无效",
            )
            .into_response();
        }
    };
    let path = query.path.as_deref().unwrap_or("/");
    let page = query.page.unwrap_or(1);
    let page_size = query.page_size.unwrap_or(50);
    match list_directories(FsPath::new(path), offset as usize, limit as usize).await {
        Ok(result) => Json(json!({
            "path": result.path,
            "parentPath": result.parent_path,
            "directories": result.directories.iter().map(|entry| json!({
                "name": entry.name,
                "path": entry.path,
            })).collect::<Vec<_>>(),
            "page": page,
            "pageSize": page_size,
            "hasMore": result.has_more,
        }))
        .into_response(),
        Err(DirectoryBrowserError::InvalidPath | DirectoryBrowserError::NotDirectory) => api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "目录路径无效",
        )
        .into_response(),
        Err(DirectoryBrowserError::Unavailable) => api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "目录不可访问",
        )
        .into_response(),
    }
}

pub(crate) async fn admin_list_users(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let users = match UserStore::new(database.clone()) {
        Ok(users) => users,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match users.list_users().await {
        Ok(users) => Json(json!({
            "users": users.iter().map(user_json).collect::<Vec<_>>()
        }))
        .into_response(),
        Err(error) => user_store_error(&headers, error),
    }
}

pub(crate) async fn admin_list_user_library_access(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Ok(user_id) = user_id.parse::<crate::domain::ids::UserId>() else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "用户 ID 无效",
        )
        .into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database
        .list_accessible_library_ids(&user_id.to_string())
        .await
    {
        Ok(library_ids) => Json(json!({ "libraryIds": library_ids })).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(crate) async fn admin_list_audit(
    headers: HeaderMap,
    Query(query): Query<MetadataCandidateQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let (offset, limit) = match metadata_page_params(&query) {
        Ok(params) => params,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database.list_audit_events(offset, limit).await {
        Ok(events) => Json(json!({
            "events": events.iter().map(|event| json!({
                "id": event.id,
                "actorUserId": event.actor_user_id,
                "actorUsername": event.actor_username,
                "eventType": event.event_type,
                "targetType": event.target_type,
                "targetId": event.target_id,
                "metadata": serde_json::from_str::<Value>(&event.metadata_json)
                    .unwrap_or_else(|_| json!({})),
                "createdAt": event.created_at,
            })).collect::<Vec<_>>(),
            "page": offset / limit + 1,
            "pageSize": limit,
        }))
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(crate) async fn admin_list_people_index_rebuild(
    headers: HeaderMap,
    Query(query): Query<AdminJobsQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let (offset, limit) = match page_params(query.page, query.page_size) {
        Ok(params) => params,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    let Some(people) = state.people.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "人物索引服务尚未就绪",
        )
        .into_response();
    };
    let total = match people.count_person_index_rebuild_jobs().await {
        Ok(total) => total,
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "人物索引任务暂时不可用",
            )
            .into_response();
        }
    };
    match people.list_person_index_rebuild_jobs(offset, limit).await {
        Ok(jobs) => Json(json!({
            "jobs": jobs.iter().map(people_index_rebuild_job_json).collect::<Vec<_>>(),
            "total": total,
            "page": offset / limit + 1,
            "pageSize": limit,
        }))
        .into_response(),
        Err(_) => api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "人物索引任务暂时不可用",
        )
        .into_response(),
    }
}

pub(crate) async fn admin_queue_people_index_rebuild(
    headers: HeaderMap,
    Path(library_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Ok(library_id) = library_id.parse::<crate::domain::ids::LibraryId>() else {
        return api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "媒体库不存在",
        )
        .into_response();
    };
    let library_id = library_id.to_string();
    let Some(people) = state.people.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "人物索引服务尚未就绪",
        )
        .into_response();
    };
    match people.queue_person_index_rebuild(&library_id).await {
        Ok(true) => {}
        Ok(false) => {
            if let Ok(Some(job)) = people.get_person_index_rebuild_job(&library_id).await
                && job.status == "RUNNING"
            {
                return api_error(
                    &headers,
                    StatusCode::CONFLICT,
                    lux::ApiErrorCode::InvalidRequest,
                    "人物索引重建正在运行",
                )
                .into_response();
            }
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "媒体库不存在或未启用",
            )
            .into_response();
        }
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "人物索引任务暂时不可用",
            )
            .into_response();
        }
    }
    let job = match people.get_person_index_rebuild_job(&library_id).await {
        Ok(Some(job)) => job,
        Ok(None) | Err(_) => {
            return api_error(
                &headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "人物索引任务暂时不可用",
            )
            .into_response();
        }
    };
    record_audit_event(
        &state,
        &headers,
        "PEOPLE_INDEX_REBUILD_QUEUED",
        Some("library"),
        Some(&library_id),
        "{}",
    )
    .await;
    state.rebuild_people_index().await;
    (
        StatusCode::ACCEPTED,
        Json(json!({ "job": people_index_rebuild_job_json(&job) })),
    )
        .into_response()
}

pub(crate) async fn admin_cancel_people_index_rebuild(
    headers: HeaderMap,
    Path(library_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Ok(library_id) = library_id.parse::<crate::domain::ids::LibraryId>() else {
        return api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "媒体库不存在",
        )
        .into_response();
    };
    let library_id = library_id.to_string();
    let Some(people) = state.people.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "人物索引服务尚未就绪",
        )
        .into_response();
    };
    match people.cancel_person_index_rebuild(&library_id).await {
        Ok(true) => {}
        Ok(false) => {
            return api_error(
                &headers,
                StatusCode::CONFLICT,
                lux::ApiErrorCode::InvalidRequest,
                "没有可取消的人物索引重建任务",
            )
            .into_response();
        }
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "人物索引任务暂时不可用",
            )
            .into_response();
        }
    }
    let job = match people.get_person_index_rebuild_job(&library_id).await {
        Ok(Some(job)) => job,
        Ok(None) | Err(_) => {
            return api_error(
                &headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "人物索引任务暂时不可用",
            )
            .into_response();
        }
    };
    record_audit_event(
        &state,
        &headers,
        "PEOPLE_INDEX_REBUILD_CANCEL_REQUESTED",
        Some("library"),
        Some(&library_id),
        "{}",
    )
    .await;
    (
        StatusCode::ACCEPTED,
        Json(json!({ "job": people_index_rebuild_job_json(&job) })),
    )
        .into_response()
}

pub(crate) fn people_index_rebuild_job_json(
    job: &crate::storage::StoredPersonIndexRebuildJob,
) -> Value {
    json!({
        "libraryId": job.library_id,
        "status": job.status,
        "cursorId": job.cursor_id,
        "processedCount": job.processed_count,
        "totalCount": job.total_count,
        "cancelRequested": job.cancel_requested,
    })
}

pub(crate) async fn admin_list_logs(
    headers: HeaderMap,
    Query(query): Query<MetadataCandidateQuery>,
    State(state): State<AppState>,
) -> Response {
    admin_list_audit(headers, Query(query), State(state)).await
}

pub(crate) async fn admin_export_logs(
    headers: HeaderMap,
    Query(query): Query<AdminLogExportQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let range = match LogDateRange::from_query(query.from.as_deref(), query.to.as_deref()) {
        Ok(range) => range,
        Err(error) => return log_export_error_response(&headers, error),
    };
    let Some(config_dir) = state.config_dir.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match export_logs(config_dir, range).await {
        Ok(export) => {
            let (content_type, filename, contents) = match export {
                LogExport::Daily { contents, filename } => {
                    ("application/x-ndjson", filename, contents)
                }
                LogExport::Archive { contents, filename } => {
                    ("application/zip", filename, contents)
                }
            };
            Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", content_type)
                .header(
                    "Content-Disposition",
                    format!("attachment; filename=\"{filename}\""),
                )
                .header("Cache-Control", "no-store")
                .body(Body::from(contents))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        Err(error) => log_export_error_response(&headers, error),
    }
}

pub(crate) fn log_export_error_response(headers: &HeaderMap, error: LogExportError) -> Response {
    match error {
        LogExportError::InvalidDate
        | LogExportError::DateRangeReversed
        | LogExportError::DateRangeTooLarge => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            &error.to_string(),
        )
        .into_response(),
        LogExportError::ExportTooLarge => api_error(
            headers,
            StatusCode::PAYLOAD_TOO_LARGE,
            lux::ApiErrorCode::InvalidRequest,
            &error.to_string(),
        )
        .into_response(),
        LogExportError::NoLogs => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            &error.to_string(),
        )
        .into_response(),
        LogExportError::Io(_) | LogExportError::Archive(_) | LogExportError::Worker(_) => {
            tracing::warn!(%error, "log export failed");
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

pub(crate) async fn probe_directory_writable(path: &FsPath) -> bool {
    let probe_path = path.join(format!(".lux-health-probe-{}", uuid::Uuid::now_v7()));
    let payload = [0_u8; 4096];
    let result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe_path)
            .await?;
        file.write_all(&payload).await?;
        file.sync_all().await
    }
    .await;
    let _ = fs::remove_file(probe_path).await;
    result.is_ok()
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WebhookDestinationCreateRequest {
    pub(crate) name: String,
    pub(crate) url: String,
    #[serde(default = "default_enabled")]
    pub(crate) enabled: bool,
    #[serde(default)]
    pub(crate) allow_private_network: bool,
    #[serde(default)]
    pub(crate) event_types: Vec<String>,
    pub(crate) payload_format: Option<String>,
    pub(crate) secret: Option<String>,
    pub(crate) provider_plugin_id: Option<String>,
    pub(crate) provider_config: Option<Value>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WebhookDestinationUpdateRequest {
    pub(crate) name: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) enabled: Option<bool>,
    pub(crate) allow_private_network: Option<bool>,
    pub(crate) event_types: Option<Vec<String>>,
    pub(crate) payload_format: Option<String>,
    pub(crate) provider_plugin_id: Option<String>,
    pub(crate) provider_config: Option<Value>,
}

const fn default_enabled() -> bool {
    true
}

pub(crate) async fn admin_list_webhook_destinations(
    headers: HeaderMap,
    Query(query): Query<LuxPageQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let (offset, limit) = match lux_page_params(&query) {
        Ok(value) => value,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    let Some(service) = state.webhooks.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "Webhook 通知服务尚未就绪",
        )
        .into_response();
    };
    match service.list_destinations(offset, limit).await {
        Ok(destinations) => Json(json!({
            "destinations": destinations,
            "page": offset / limit + 1,
            "pageSize": limit,
        }))
        .into_response(),
        Err(error) => webhook_error_response(&headers, error),
    }
}

pub(crate) async fn admin_get_webhook_destination(
    headers: HeaderMap,
    Path(destination_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(service) = state.webhooks.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "Webhook 通知服务尚未就绪",
        )
        .into_response();
    };
    match service.get_destination(&destination_id).await {
        Ok(Some(destination)) => Json(json!({ "destination": destination })).into_response(),
        Ok(None) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "Webhook 目标不存在",
        )
        .into_response(),
        Err(error) => webhook_error_response(&headers, error),
    }
}

pub(crate) async fn admin_create_webhook_destination(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<WebhookDestinationCreateRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.webhooks.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "Webhook 通知服务尚未就绪",
        )
        .into_response();
    };
    let provider_plugin_id = request
        .provider_plugin_id
        .as_deref()
        .unwrap_or(BUILTIN_WEBHOOK_PROVIDER_ID);
    let provider_config = request
        .provider_config
        .as_ref()
        .cloned()
        .unwrap_or_else(|| json!({}));
    match service
        .create_destination_with_provider(
            &request.name,
            &request.url,
            request.enabled,
            request.allow_private_network,
            &request.event_types,
            request.secret.as_deref(),
            request.payload_format.as_deref().unwrap_or("LUX"),
            provider_plugin_id,
            &provider_config,
        )
        .await
    {
        Ok((destination, secret)) => (
            StatusCode::CREATED,
            Json(json!({ "destination": destination, "secret": secret })),
        )
            .into_response(),
        Err(error) => webhook_error_response(&headers, error),
    }
}

pub(crate) async fn admin_update_webhook_destination(
    headers: HeaderMap,
    Path(destination_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<WebhookDestinationUpdateRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.webhooks.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "Webhook 通知服务尚未就绪",
        )
        .into_response();
    };
    match service
        .update_destination_with_provider(
            &destination_id,
            request.name.as_deref(),
            request.url.as_deref(),
            request.enabled,
            request.allow_private_network,
            request.event_types.as_deref(),
            request.payload_format.as_deref(),
            request.provider_plugin_id.as_deref(),
            request.provider_config.as_ref(),
        )
        .await
    {
        Ok(destination) => Json(json!({ "destination": destination })).into_response(),
        Err(error) => webhook_error_response(&headers, error),
    }
}

pub(crate) async fn admin_delete_webhook_destination(
    headers: HeaderMap,
    Path(destination_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.webhooks.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "Webhook 通知服务尚未就绪",
        )
        .into_response();
    };
    match service.delete_destination(&destination_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => webhook_error_response(&headers, error),
    }
}

pub(crate) async fn admin_test_webhook_destination(
    headers: HeaderMap,
    Path(destination_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.webhooks.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "Webhook 通知服务尚未就绪",
        )
        .into_response();
    };
    match service.test_destination(&destination_id).await {
        Ok(status) => Json(json!({ "status": status })).into_response(),
        Err(error) => webhook_error_response(&headers, error),
    }
}

pub(crate) async fn admin_rotate_webhook_secret(
    headers: HeaderMap,
    Path(destination_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.webhooks.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "Webhook 通知服务尚未就绪",
        )
        .into_response();
    };
    match service.rotate_secret(&destination_id).await {
        Ok(secret) => Json(json!({ "secret": secret })).into_response(),
        Err(error) => webhook_error_response(&headers, error),
    }
}

pub(crate) async fn admin_list_webhook_deliveries(
    headers: HeaderMap,
    Query(query): Query<LuxPageQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let (offset, limit) = match lux_page_params(&query) {
        Ok(value) => value,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    let Some(service) = state.webhooks.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "Webhook 通知服务尚未就绪",
        )
        .into_response();
    };
    match service.list_deliveries(offset, limit).await {
        Ok(deliveries) => Json(json!({
            "deliveries": deliveries,
            "page": offset / limit + 1,
            "pageSize": limit,
        }))
        .into_response(),
        Err(error) => webhook_error_response(&headers, error),
    }
}

pub(crate) async fn admin_retry_webhook_delivery(
    headers: HeaderMap,
    Path(delivery_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.webhooks.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "Webhook 通知服务尚未就绪",
        )
        .into_response();
    };
    match service.retry_delivery(&delivery_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => webhook_error_response(&headers, error),
    }
}

pub(crate) fn webhook_error_response(headers: &HeaderMap, error: WebhookError) -> Response {
    match error {
        WebhookError::Invalid(message) => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            &message,
        )
        .into_response(),
        WebhookError::NotFound => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "Webhook 目标或投递记录不存在",
        )
        .into_response(),
        WebhookError::Storage(_)
        | WebhookError::Io(_)
        | WebhookError::Serialization(_)
        | WebhookError::HttpResponse { .. }
        | WebhookError::PluginRetryable { .. }
        | WebhookError::PluginFailed(_)
        | WebhookError::Plugin(_)
        | WebhookError::SecretUnavailable
        | WebhookError::RequestSetup(_) => {
            tracing::warn!("webhook operation failed");
            api_error(
                headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "Webhook 通知服务暂时不可用",
            )
            .into_response()
        }
    }
}

pub(crate) async fn admin_health(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    match admin_health_payload(&state).await {
        Ok(payload) => Json(payload).into_response(),
        Err(status) => status.into_response(),
    }
}

pub(crate) async fn admin_events(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }

    let mut receiver = state.admin_events.subscribe();
    let (mut writer, reader) = tokio::io::duplex(16 * 1024);
    tokio::spawn(async move {
        if writer
            .write_all(b"event: ready\ndata: {\"version\":1}\n\n")
            .await
            .is_err()
        {
            return;
        }

        let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
        heartbeat.tick().await;
        loop {
            tokio::select! {
                event = receiver.recv() => {
                    let scope = match event {
                        Ok(scope) => scope,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => AdminEventScope::All,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    };
                    let frame = format!(
                        "event: invalidate\ndata: {{\"scope\":\"{}\"}}\n\n",
                        scope.as_str(),
                    );
                    if writer.write_all(frame.as_bytes()).await.is_err() {
                        break;
                    }
                }
                _ = heartbeat.tick() => {
                    if writer.write_all(b": keep-alive\n\n").await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    event_stream_response(reader)
}

pub(crate) fn event_stream_response(reader: tokio::io::DuplexStream) -> Response {
    let mut response = Response::new(Body::from_stream(tokio_util::io::ReaderStream::new(reader)));
    response.headers_mut().insert(
        "Content-Type",
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    response.headers_mut().insert(
        "Cache-Control",
        HeaderValue::from_static("no-cache, no-store, must-revalidate"),
    );
    response
        .headers_mut()
        .insert("X-Accel-Buffering", HeaderValue::from_static("no"));
    response
}

pub(crate) async fn user_events(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if let Err(response) = require_web_user(&headers, &state).await {
        return response;
    }

    let mut receiver = state.user_events.subscribe();
    let (mut writer, reader) = tokio::io::duplex(16 * 1024);
    tokio::spawn(async move {
        if writer
            .write_all(b"event: ready\ndata: {\"version\":1}\n\n")
            .await
            .is_err()
        {
            return;
        }

        let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
        heartbeat.tick().await;
        loop {
            tokio::select! {
                event = receiver.recv() => {
                    let scope = match event {
                        Ok(scope) => scope,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => crate::application::admin_events::UserEventScope::Home,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    };
                    let frame = format!(
                        "event: invalidate\ndata: {{\"scope\":\"{}\"}}\n\n",
                        scope.as_str(),
                    );
                    if writer.write_all(frame.as_bytes()).await.is_err() {
                        break;
                    }
                }
                _ = heartbeat.tick() => {
                    if writer.write_all(b": keep-alive\n\n").await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    event_stream_response(reader)
}

pub(crate) async fn admin_health_payload(state: &AppState) -> Result<Value, StatusCode> {
    let Some(database) = state.database.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let schema_version = match database.schema_version().await {
        Ok(version) => version,
        Err(_) => return Err(StatusCode::SERVICE_UNAVAILABLE),
    };
    let resources = state.resources.snapshot().await;
    let database_writable = database.probe_write().await.is_ok();
    let database_pool = database.pool_snapshot();
    let config_available = match state.config_dir.as_deref() {
        Some(path) => fs::metadata(path)
            .await
            .map(|metadata| metadata.is_dir())
            .unwrap_or(false),
        None => false,
    };
    let config_writable = match state.config_dir.as_deref() {
        Some(path) if config_available => probe_directory_writable(path).await,
        _ => false,
    };
    let ffprobe_available = Command::new("ffprobe")
        .arg("-version")
        .output()
        .await
        .is_ok_and(|output| output.status.success());
    let libraries = match state.libraries.as_ref() {
        Some(libraries) => match libraries.list_libraries().await {
            Ok(views) => views
                .iter()
                .map(|view| {
                    json!({
                        "id": view.library.id.to_string(),
                        "name": view.library.name,
                        "isEnabled": view.library.is_enabled,
                        "rootCount": view.roots.len(),
                        "availableRootCount": view.roots.iter().filter(|root| root.is_available).count(),
                        "writableRootCount": view.roots.iter().filter(|root| root.is_writable).count(),
                    })
                })
                .collect::<Vec<_>>(),
            Err(_) => return Err(StatusCode::SERVICE_UNAVAILABLE),
        },
        None => Vec::new(),
    };
    let jobs = match database.count_scan_jobs_by_status().await {
        Ok(counts) => json!({
            "scanRunning": counts.running,
            "scanFailed": counts.failed,
        }),
        Err(_) => return Err(StatusCode::SERVICE_UNAVAILABLE),
    };
    let metadata_reidentify_running = match database.list_active_metadata_reidentify_job_ids().await
    {
        Ok(ids) => ids.len(),
        Err(_) => return Err(StatusCode::SERVICE_UNAVAILABLE),
    };
    let status = if database_writable && config_available && config_writable && ffprobe_available {
        "ok"
    } else {
        "degraded"
    };
    let (database_backend, journal_mode) = match database.backend() {
        DatabaseBackend::Sqlite => ("SQLITE", "wal"),
        DatabaseBackend::Postgres => ("POSTGRESQL", ""),
    };
    Ok(json!({
        "status": status,
        "schemaVersion": schema_version,
        "runtime": { "seconds": resources.runtime_seconds },
        "resources": resources,
        "database": {
            "status": if database_writable { "ok" } else { "degraded" },
            "backend": database_backend,
            "journalMode": journal_mode,
            "writable": database_writable,
            "pool": {
                "maxConnections": database_pool.max_connections,
                "size": database_pool.size,
                "idle": database_pool.idle,
                "inUse": database_pool.in_use,
                "saturated": database_pool.saturated,
            },
        },
        "config": { "available": config_available, "writable": config_writable },
        "ffprobe": { "available": ffprobe_available },
        "jobs": {
            "scanRunning": jobs["scanRunning"],
            "scanFailed": jobs["scanFailed"],
            "metadataReidentifyRunning": metadata_reidentify_running,
        },
        "libraries": libraries,
    }))
}

pub(crate) const DEFAULT_SERVER_NAME: &str = "Lux Server";
const DASHBOARD_ACTIVITY_LIMIT: i64 = 24;
const DASHBOARD_PLAYBACK_LIMIT: usize = 24;

pub(crate) async fn admin_dashboard(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let health = match admin_health_payload(&state).await {
        Ok(health) => health,
        Err(status) => return status.into_response(),
    };
    let server_name = match database.server_name().await {
        Ok(Some(name)) if !name.trim().is_empty() => name,
        Ok(_) => DEFAULT_SERVER_NAME.to_owned(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let stats = match database.dashboard_stats().await {
        Ok(stats) => stats,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let sessions = match database.list_playback_sessions(None, None).await {
        Ok(sessions) => sessions,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let users = match UserStore::new(database.clone()) {
        Ok(users) => match users.list_users().await {
            Ok(users) => users,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        },
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let activity = match database
        .list_activity_events(DASHBOARD_ACTIVITY_LIMIT)
        .await
    {
        Ok(events) => events
            .iter()
            .map(|event| dashboard_activity_json(event, state.ip_location.as_ref()))
            .collect::<Vec<_>>(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let now_playing = match dashboard_playback_json(&state, &sessions, &users).await {
        Ok(sessions) => sessions,
        Err(status) => return status.into_response(),
    };
    Json(json!({
        "server": {
            "name": server_name,
            "version": VERSION,
            "commit": COMMIT,
            "schemaVersion": health["schemaVersion"],
        },
        "stats": dashboard_stats_json(&stats),
        "health": health,
        "nowPlaying": now_playing,
        "activity": activity,
    }))
    .into_response()
}

pub(crate) fn dashboard_stats_json(stats: &DashboardStats) -> Value {
    json!({
        "movieCount": stats.movie_count,
        "seriesCount": stats.series_count,
        "userCount": stats.user_count,
    })
}

pub(crate) async fn dashboard_playback_json(
    state: &AppState,
    sessions: &[StoredPlaybackSession],
    users: &[UserRecord],
) -> Result<Vec<Value>, StatusCode> {
    let Some(catalog) = state.catalog.as_ref() else {
        return Ok(Vec::new());
    };
    let user_names = users
        .iter()
        .map(|user| (user.id.to_string(), user.display_name.clone()))
        .collect::<BTreeMap<_, _>>();
    let principal = AccessPrincipal::new(crate::domain::ids::UserId::new(), true);
    let sessions = sessions
        .iter()
        .take(DASHBOARD_PLAYBACK_LIMIT)
        .collect::<Vec<_>>();
    let item_ids = sessions
        .iter()
        .map(|session| session.item_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let items_by_id = catalog
        .find_items(principal, &item_ids)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let series_ids = items_by_id
        .values()
        .filter(|item| item.item_type == "EPISODE")
        .filter_map(|item| item.series_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let series_by_id = catalog
        .find_items(principal, &series_ids)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let mut values = Vec::new();
    for session in sessions {
        let Some(item) = items_by_id.get(&session.item_id) else {
            continue;
        };
        let series = item
            .series_id
            .as_deref()
            .and_then(|series_id| series_by_id.get(series_id));
        let remote_ip_location = session.remote_ip.as_deref().and_then(|remote_ip| {
            state
                .ip_location
                .as_ref()
                .and_then(|service| service.cached_or_schedule(remote_ip))
        });
        values.push(dashboard_playback_item_json(
            session,
            item,
            series,
            user_names
                .get(&session.user_id)
                .map(String::as_str)
                .unwrap_or("未知账户"),
            remote_ip_location.as_ref(),
        ));
    }
    Ok(values)
}

pub(crate) fn dashboard_playback_item_json(
    session: &StoredPlaybackSession,
    item: &CatalogItem,
    series: Option<&CatalogItem>,
    user_name: &str,
    remote_ip_location: Option<&IpLocation>,
) -> Value {
    let source = session
        .media_source_id
        .as_deref()
        .and_then(|source_id| {
            item.media_sources
                .iter()
                .find(|source| source.id == source_id)
        })
        .or_else(|| item.media_sources.iter().find(|source| source.is_default))
        .or_else(|| item.media_sources.first());
    json!({
        "id": session.id,
        "userId": session.user_id,
        "userName": user_name,
        "itemId": item.id,
        "title": item.title,
        "originalTitle": item.original_title,
        "itemType": item.item_type,
        "seriesId": item.series_id,
        "seriesTitle": series.map(|item| item.title.as_str()),
        "productionYear": item.production_year,
        "parentIndexNumber": item.season_number,
        "indexNumber": item.episode_number,
        "posterAvailable": item.poster_image_tag.is_some(),
        "positionTicks": session.position_ticks,
        "durationTicks": session.duration_ticks.or(item.runtime_ticks),
        "state": session.state,
        "isPaused": session.is_paused,
        "lastEventAt": session.last_event_at,
        "client": session.client,
        "clientVersion": session.client_version,
        "deviceId": session.device_id,
        "deviceName": session.device_name,
        "deviceType": session.device_type,
        "remoteIp": session.remote_ip,
        "remoteIpLocation": remote_ip_location.map(dashboard_ip_location_json),
        "playSessionId": session.play_session_id,
        "source": source.map(dashboard_source_json),
    })
}

pub(crate) fn dashboard_ip_location_json(location: &IpLocation) -> Value {
    json!({
        "location": location.formatted_location(),
        "district": location.district,
        "street": location.street,
        "isp": location.isp,
    })
}

pub(crate) fn dashboard_source_json(source: &CatalogSource) -> Value {
    let video = source
        .streams
        .iter()
        .find(|stream| stream.stream_type == "VIDEO");
    let audio = source
        .streams
        .iter()
        .find(|stream| stream.stream_type == "AUDIO");
    json!({
        "id": source.id,
        "qualityLabel": source.quality_label,
        "editionName": source.edition_name,
        "container": source.container,
        "bitrate": source.bitrate,
        "durationTicks": source.duration_ticks,
        "video": video.map(|stream| json!({
            "codec": stream.codec,
            "title": stream.title,
            "details": stream.details,
        })),
        "audio": audio.map(|stream| json!({
            "codec": stream.codec,
            "language": stream.language,
            "title": stream.title,
        })),
    })
}

pub(crate) fn dashboard_activity_json(
    event: &crate::storage::StoredActivityEvent,
    ip_location: Option<&IpLocationService>,
) -> Value {
    let metadata =
        serde_json::from_str::<Value>(&event.metadata_json).unwrap_or_else(|_| json!({}));
    let remote_ip = metadata.get("remoteIp").and_then(Value::as_str);
    let remote_ip_location = remote_ip.and_then(|remote_ip| {
        ip_location.and_then(|service| service.cached_or_schedule(remote_ip))
    });
    json!({
        "id": event.id,
        "userId": event.actor_user_id,
        "userName": event.actor_username,
        "eventType": event.event_type,
        "targetType": event.target_type,
        "targetId": event.target_id,
        "targetTitle": event.target_title,
        "metadata": metadata,
        "remoteIp": remote_ip,
        "remoteIpLocation": remote_ip_location
            .as_ref()
            .map(dashboard_ip_location_json),
        "createdAt": event.created_at,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateUserRequest {
    pub(crate) username: String,
    #[serde(default)]
    pub(crate) display_name: String,
    pub(crate) password: String,
    #[serde(default)]
    pub(crate) is_admin: bool,
}

pub(crate) async fn admin_create_user(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<CreateUserRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let users = match UserStore::new(database.clone()) {
        Ok(users) => users,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match users
        .create_user(
            &request.username,
            &request.display_name,
            &request.password,
            request.is_admin,
        )
        .await
    {
        Ok(user) => {
            let target_id = user.id.to_string();
            record_audit_event(
                &state,
                &headers,
                "USER_CREATED",
                Some("user"),
                Some(&target_id),
                "{}",
            )
            .await;
            (
                StatusCode::CREATED,
                Json(json!({ "user": user_json(&user) })),
            )
                .into_response()
        }
        Err(error) => user_store_error(&headers, error),
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateUserRequest {
    pub(crate) display_name: Option<String>,
    pub(crate) password: Option<String>,
    pub(crate) is_disabled: Option<bool>,
    pub(crate) is_admin: Option<bool>,
    pub(crate) can_manage_server: Option<bool>,
    pub(crate) can_remote_access: Option<bool>,
    pub(crate) can_download: Option<bool>,
}

pub(crate) async fn admin_update_user(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<UpdateUserRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let users = match UserStore::new(database.clone()) {
        Ok(users) => users,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match users
        .update_user(
            &user_id,
            UserUpdate {
                display_name: request.display_name.as_deref(),
                password: request.password.as_deref(),
                is_disabled: request.is_disabled,
                is_admin: request.is_admin,
                can_manage_server: request.can_manage_server,
                can_remote_access: request.can_remote_access,
                can_download: request.can_download,
            },
        )
        .await
    {
        Ok(Some(user)) => {
            record_audit_event(
                &state,
                &headers,
                "USER_UPDATED",
                Some("user"),
                Some(&user_id),
                "{}",
            )
            .await;
            Json(json!({ "user": user_json(&user) })).into_response()
        }
        Ok(None) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "用户不存在",
        )
        .into_response(),
        Err(error) => user_store_error(&headers, error),
    }
}

pub(crate) async fn admin_disable_user(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let users = match UserStore::new(database.clone()) {
        Ok(users) => users,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match users
        .update_user(
            &user_id,
            UserUpdate {
                is_disabled: Some(true),
                ..UserUpdate::default()
            },
        )
        .await
    {
        Ok(Some(user)) => {
            record_audit_event(
                &state,
                &headers,
                "USER_DISABLED",
                Some("user"),
                Some(&user_id),
                "{}",
            )
            .await;
            Json(json!({ "user": user_json(&user) })).into_response()
        }
        Ok(None) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "用户不存在",
        )
        .into_response(),
        Err(error) => user_store_error(&headers, error),
    }
}

pub(crate) fn user_store_error(headers: &HeaderMap, error: UserStoreError) -> Response {
    match error {
        UserStoreError::InvalidUsername | UserStoreError::Password(_) => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "用户请求无效",
        )
        .into_response(),
        UserStoreError::LastManager => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::PermissionDenied,
            "至少需要一个启用的服务器管理账户",
        )
        .into_response(),
        UserStoreError::InvalidUserId(_) => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "用户 ID 无效",
        )
        .into_response(),
        UserStoreError::Storage(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "用户数据暂时不可用",
        )
        .into_response(),
        UserStoreError::SetupAlreadyCompleted => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "初始化已完成",
        )
        .into_response(),
    }
}

pub(crate) async fn admin_list_pending_metadata(
    headers: HeaderMap,
    Query(query): Query<MetadataCandidateQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let (offset, limit) = match metadata_page_params(&query) {
        Ok(params) => params,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    let Some(candidates) = state.metadata_candidates.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    match candidates.list_pending(offset, limit).await {
        Ok(page) => Json(metadata_candidate_page_json(&page)).into_response(),
        Err(error) => metadata_candidate_error(&headers, error),
    }
}

pub(crate) async fn admin_list_pending_person_matches(
    headers: HeaderMap,
    Query(query): Query<MetadataCandidateQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let (offset, limit) = match metadata_page_params(&query) {
        Ok(params) => params,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    let Some(people) = state.people.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    match people
        .list_pending_person_match_candidates(offset, limit)
        .await
    {
        Ok((items, total)) => Json(json!({
            "items": items,
            "total": total,
            "offset": offset,
            "limit": limit,
        }))
        .into_response(),
        Err(error) => people_match_error(&headers, error),
    }
}

pub(crate) async fn admin_confirm_person_match(
    headers: HeaderMap,
    Path(candidate_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<PersonMatchConfirmRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(people) = state.people.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let evidence_json = match serde_json::to_string(&request.evidence) {
        Ok(value) if value.len() <= 16 * 1024 => value,
        Ok(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "匹配证据过大",
            )
            .into_response();
        }
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "匹配证据无效",
            )
            .into_response();
        }
    };
    match people
        .confirm_person_match_candidate(&candidate_id, &request.target_person_id, &evidence_json)
        .await
    {
        Ok(move_result) => {
            record_audit_event(
                &state,
                &headers,
                "PERSON_MATCH_CONFIRMED",
                Some("person_match_candidate"),
                Some(&candidate_id),
                &json!({
                    "targetPersonId": request.target_person_id,
                    "previousPersonId": move_result.previous_person_id,
                    "evidence": request.evidence,
                })
                .to_string(),
            )
            .await;
            Json(json!({
                "candidateId": candidate_id,
                "status": "CONFIRMED",
                "targetPersonId": request.target_person_id,
                "previousPersonId": move_result.previous_person_id,
            }))
            .into_response()
        }
        Err(error) => people_match_error(&headers, error),
    }
}

pub(crate) async fn admin_reject_person_match(
    headers: HeaderMap,
    Path(candidate_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<PersonMatchRejectRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(people) = state.people.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let evidence_json = match serde_json::to_string(&request.evidence) {
        Ok(value) if value.len() <= 16 * 1024 => value,
        Ok(_) | Err(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "匹配证据无效或过大",
            )
            .into_response();
        }
    };
    match people
        .reject_person_match_candidate(&candidate_id, &evidence_json)
        .await
    {
        Ok(()) => {
            record_audit_event(
                &state,
                &headers,
                "PERSON_MATCH_REJECTED",
                Some("person_match_candidate"),
                Some(&candidate_id),
                &evidence_json,
            )
            .await;
            Json(json!({
                "candidateId": candidate_id,
                "status": "REJECTED",
            }))
            .into_response()
        }
        Err(error) => people_match_error(&headers, error),
    }
}

pub(crate) async fn admin_undo_person_match(
    headers: HeaderMap,
    Path(candidate_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<PersonMatchRejectRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(people) = state.people.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let evidence_json = match serde_json::to_string(&request.evidence) {
        Ok(value) if value.len() <= 16 * 1024 => value,
        Ok(_) | Err(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "撤销证据无效或过大",
            )
            .into_response();
        }
    };
    match people
        .undo_person_match_candidate(&candidate_id, &evidence_json)
        .await
    {
        Ok(move_result) => {
            record_audit_event(
                &state,
                &headers,
                "PERSON_MATCH_UNDONE",
                Some("person_match_candidate"),
                Some(&candidate_id),
                &evidence_json,
            )
            .await;
            Json(json!({
                "candidateId": candidate_id,
                "status": "UNDONE",
                "previousPersonId": move_result.previous_person_id,
            }))
            .into_response()
        }
        Err(error) => people_match_error(&headers, error),
    }
}

pub(crate) async fn admin_split_person_identity(
    headers: HeaderMap,
    Path(person_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<PersonIdentitySplitRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(people) = state.people.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let evidence_json = match serde_json::to_string(&request.evidence) {
        Ok(value) if value.len() <= 16 * 1024 => value,
        Ok(_) | Err(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "拆分证据无效或过大",
            )
            .into_response();
        }
    };
    match people
        .split_person_identity(
            &person_id,
            &request.provider,
            &request.provider_id,
            &request.display_name,
            &evidence_json,
        )
        .await
    {
        Ok(new_person_id) => {
            record_audit_event(
                &state,
                &headers,
                "PERSON_IDENTITY_SPLIT",
                Some("person"),
                Some(&person_id),
                &json!({
                    "provider": request.provider,
                    "providerId": request.provider_id,
                    "newPersonId": new_person_id,
                    "displayName": request.display_name,
                    "evidence": request.evidence,
                })
                .to_string(),
            )
            .await;
            Json(json!({
                "sourcePersonId": person_id,
                "newPersonId": new_person_id,
                "status": "SPLIT",
            }))
            .into_response()
        }
        Err(error) => people_match_error(&headers, error),
    }
}

pub(crate) async fn admin_set_person_field_locks(
    headers: HeaderMap,
    Path(person_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<PersonFieldLocksRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(people) = state.people.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let evidence_json = match serde_json::to_string(&request.evidence) {
        Ok(value) if value.len() <= 16 * 1024 => value,
        Ok(_) | Err(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "人物字段锁定证据无效或过大",
            )
            .into_response();
        }
    };
    match people
        .set_person_field_locks(&person_id, &request.fields, &evidence_json)
        .await
    {
        Ok(fields) => {
            record_audit_event(
                &state,
                &headers,
                "PERSON_FIELD_LOCKS_UPDATED",
                Some("person"),
                Some(&person_id),
                &json!({"fields": fields, "evidence": request.evidence}).to_string(),
            )
            .await;
            Json(json!({
                "personId": person_id,
                "lockedFields": fields,
                "status": "UPDATED",
            }))
            .into_response()
        }
        Err(error) => people_match_error(&headers, error),
    }
}

pub(crate) fn people_match_error(headers: &HeaderMap, error: PeopleError) -> Response {
    match error {
        PeopleError::InvalidComponent(_) | PeopleError::Serialization(_) => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "人物匹配请求无效",
        )
        .into_response(),
        PeopleError::Storage(message) if message.contains("candidate") => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "人物匹配候选状态已变化或不存在",
        )
        .into_response(),
        PeopleError::Storage(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "人物索引暂时不可用",
        )
        .into_response(),
        _ => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "人物匹配处理失败",
        )
        .into_response(),
    }
}

pub(crate) async fn admin_list_metadata_reidentify(
    headers: HeaderMap,
    Query(query): Query<MetadataReidentifyListQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let (offset, limit) = match page_params(query.page, query.page_size) {
        Ok(params) => params,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    let status = query.status.as_deref().map(str::to_ascii_uppercase);
    if status.as_deref().is_some_and(|value| {
        !matches!(
            value,
            "QUEUED"
                | "RUNNING"
                | "COMPLETED"
                | "COMPLETED_WITH_ISSUES"
                | "DEFERRED"
                | "CANCELLED"
                | "FAILED"
        )
    }) {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "元数据任务状态无效",
        )
        .into_response();
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database
        .list_metadata_reidentify_jobs(status.as_deref(), offset, limit)
        .await
    {
        Ok(jobs) => Json(json!({
            "jobs": jobs.iter().map(metadata_reidentify_job_summary_json).collect::<Vec<_>>(),
            "page": offset / limit + 1,
            "pageSize": limit,
        }))
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(crate) async fn admin_start_metadata_reidentify(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<MetadataReidentifyRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    if request
        .item_ids
        .iter()
        .any(|item_id| item_id.parse::<crate::domain::ids::ItemId>().is_err())
    {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体条目 ID 无效",
        )
        .into_response();
    }
    let Some(reidentify) = state.metadata_reidentify.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "元数据刮削器匹配服务尚未配置",
        )
        .into_response();
    };
    let job = match reidentify.create_job(request.item_ids).await {
        Ok(job) => job,
        Err(error) => return metadata_reidentify_error(&headers, error),
    };
    let worker = reidentify.clone();
    let job_id = job.id.clone();
    tokio::spawn(async move {
        worker.run(&job_id).await;
    });
    record_audit_event(
        &state,
        &headers,
        "METADATA_REIDENTIFY_STARTED",
        Some("metadata_reidentify_job"),
        Some(&job.id),
        "{}",
    )
    .await;
    (
        StatusCode::ACCEPTED,
        Json(json!({ "job": metadata_reidentify_job_json(&job) })),
    )
        .into_response()
}

pub(crate) async fn admin_confirm_metadata(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<MetadataBatchConfirmationRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    if request.item_ids.is_empty() || request.item_ids.len() > 100 {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "批量确认条目数量必须在 1 到 100 之间",
        )
        .into_response();
    }
    let requested_item_count = request.item_ids.len();
    let item_ids: Vec<String> = request
        .item_ids
        .into_iter()
        .filter(|item_id| item_id.parse::<crate::domain::ids::ItemId>().is_ok())
        .collect();
    if item_ids.len() != item_ids.iter().collect::<HashSet<_>>().len() {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "批量确认条目不能重复",
        )
        .into_response();
    }
    if item_ids.len() != requested_item_count {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体条目 ID 无效",
        )
        .into_response();
    }
    let Some(selection) = state.metadata_selection.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "元数据写回服务尚未就绪",
        )
        .into_response();
    };
    let mut confirmed_count = 0_usize;
    let mut failed_item_ids = Vec::new();
    for item_id in &item_ids {
        match selection.confirm_best_pending(item_id).await {
            Ok(_) => confirmed_count += 1,
            Err(_) => failed_item_ids.push(item_id.clone()),
        }
    }
    record_audit_event(
        &state,
        &headers,
        "METADATA_BATCH_CONFIRMED",
        Some("metadata_items"),
        None,
        &json!({
            "requestedCount": item_ids.len(),
            "confirmedCount": confirmed_count,
            "failedCount": failed_item_ids.len(),
        })
        .to_string(),
    )
    .await;
    Json(json!({
        "confirmedCount": confirmed_count,
        "failedCount": failed_item_ids.len(),
        "failedItemIds": failed_item_ids,
    }))
    .into_response()
}

pub(crate) async fn admin_get_metadata_reidentify(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(reidentify) = state.metadata_reidentify.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "元数据刮削器匹配服务尚未配置",
        )
        .into_response();
    };
    match reidentify.get_job(&job_id).await {
        Ok(job) => Json(json!({ "job": metadata_reidentify_job_json(&job) })).into_response(),
        Err(error) => metadata_reidentify_error(&headers, error),
    }
}

pub(crate) async fn admin_retry_metadata_reidentify(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(reidentify) = state.metadata_reidentify.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "元数据刮削器匹配服务尚未配置",
        )
        .into_response();
    };
    let job = match reidentify.retry_job(&job_id).await {
        Ok(job) => job,
        Err(error) => return metadata_reidentify_error(&headers, error),
    };
    let worker = reidentify.clone();
    let worker_job_id = job.id.clone();
    tokio::spawn(async move {
        worker.run(&worker_job_id).await;
    });
    record_audit_event(
        &state,
        &headers,
        "METADATA_REIDENTIFY_RETRIED",
        Some("metadata_reidentify_job"),
        Some(&job.id),
        "{}",
    )
    .await;
    (
        StatusCode::ACCEPTED,
        Json(json!({ "job": metadata_reidentify_job_json(&job) })),
    )
        .into_response()
}

pub(crate) async fn admin_cancel_metadata_reidentify(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(reidentify) = state.metadata_reidentify.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "元数据刮削器匹配服务尚未配置",
        )
        .into_response();
    };
    match reidentify.cancel(&job_id).await {
        Ok(()) => {
            record_audit_event(
                &state,
                &headers,
                "METADATA_REIDENTIFY_CANCELLED",
                Some("metadata_reidentify_job"),
                Some(&job_id),
                "{}",
            )
            .await;
            StatusCode::ACCEPTED.into_response()
        }
        Err(error) => metadata_reidentify_error(&headers, error),
    }
}

pub(crate) async fn admin_list_item_candidates(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<MetadataCandidateQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    if item_id.parse::<crate::domain::ids::ItemId>().is_err() {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体条目 ID 无效",
        )
        .into_response();
    }
    let (offset, limit) = match metadata_page_params(&query) {
        Ok(params) => params,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    let Some(candidates) = state.metadata_candidates.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    match candidates
        .list_for_item(&item_id, query.search.as_deref(), offset, limit)
        .await
    {
        Ok(page) => Json(metadata_candidate_page_json(&page)).into_response(),
        Err(error) => metadata_candidate_error(&headers, error),
    }
}

pub(crate) async fn admin_search_item_candidates(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<MetadataCandidateSearchRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    if item_id.parse::<crate::domain::ids::ItemId>().is_err() {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体条目 ID 无效",
        )
        .into_response();
    }
    let Some(fallback_scraper) = state.scraper.as_ref().cloned() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "刮削器尚未配置",
        )
        .into_response();
    };
    let scraper = if let Some(resolver) = state.scraper_resolver.as_ref() {
        match resolver.for_item(&item_id).await {
            Ok(Some(scraper)) => ScraperProvider::from_scraper(scraper),
            Ok(None) => fallback_scraper,
            Err(error) => {
                return api_error(
                    &headers,
                    StatusCode::SERVICE_UNAVAILABLE,
                    lux::ApiErrorCode::DatabaseUnavailable,
                    &format!("刮削器不可用: {error}"),
                )
                .into_response();
            }
        }
    } else {
        fallback_scraper
    };
    let Some(candidates) = state.metadata_candidates.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "元数据候选服务尚未就绪",
        )
        .into_response();
    };
    match candidates
        .search_and_store(&item_id, &request.query, request.year, &scraper)
        .await
    {
        Ok(page) => {
            record_audit_event(
                &state,
                &headers,
                "METADATA_SEARCHED",
                Some("item"),
                Some(&item_id),
                "{}",
            )
            .await;
            Json(metadata_candidate_page_json(&page)).into_response()
        }
        Err(error) => metadata_candidate_error(&headers, error),
    }
}

pub(crate) async fn admin_list_item_images(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    if item_id.parse::<crate::domain::ids::ItemId>().is_err() {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体条目 ID 无效",
        )
        .into_response();
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database.find_media_item_metadata(&item_id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "媒体条目不存在",
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(images) = state.image_writes.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match images.list_item_images(&item_id).await {
        Ok(images) => Json(json!({
            "images": images.iter().map(|image| json!({
                "id": image.id,
                "itemId": image.item_id,
                "imageType": image.image_type,
                "imageIndex": image.image_index,
                "fileSize": image.file_size,
                "contentTag": image.content_tag,
                "source": image.source,
            })).collect::<Vec<_>>()
        }))
        .into_response(),
        Err(error) => image_write_error(&headers, error),
    }
}

pub(crate) async fn admin_delete_item_image(
    headers: HeaderMap,
    Path((item_id, image_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    if item_id.parse::<crate::domain::ids::ItemId>().is_err()
        || image_id.parse::<uuid::Uuid>().is_err()
    {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "图片或媒体条目 ID 无效",
        )
        .into_response();
    }
    let Some(images) = state.image_writes.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match images.delete_item_image(&item_id, &image_id).await {
        Ok(()) => {
            record_audit_event(
                &state,
                &headers,
                "IMAGE_DELETED",
                Some("item_image"),
                Some(&image_id),
                "{}",
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => image_write_error(&headers, error),
    }
}

#[derive(Deserialize)]
pub(crate) struct MetadataSelectionRequest {
    pub(crate) mode: MetadataSelectionMode,
}

pub(crate) async fn admin_select_candidate(
    headers: HeaderMap,
    Path((item_id, candidate_id)): Path<(String, String)>,
    State(state): State<AppState>,
    Json(request): Json<MetadataSelectionRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    if item_id.parse::<crate::domain::ids::ItemId>().is_err() {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体条目 ID 无效",
        )
        .into_response();
    }
    let Some(selection) = state.metadata_selection.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "元数据写回服务尚未就绪",
        )
        .into_response();
    };
    match selection
        .select(&item_id, &candidate_id, request.mode)
        .await
    {
        Ok(report) => {
            if let Some(reidentify) = state.metadata_reidentify.as_ref()
                && let Err(error) = reidentify
                    .enqueue_selected_actor_enrichment(&report.item_id, &report.candidate_id)
                    .await
            {
                tracing::warn!(
                    item_id = %report.item_id,
                    candidate_id = %report.candidate_id,
                    %error,
                    "selected actor metadata could not be queued"
                );
            }
            record_audit_event(
                &state,
                &headers,
                "METADATA_SELECTED",
                Some("item"),
                Some(&report.item_id),
                "{}",
            )
            .await;
            Json(json!({
                "itemId": report.item_id,
                "candidateId": report.candidate_id,
                "mode": report.mode.as_str(),
                "status": report.status,
                "imageTypes": report.image_types,
                "actorCount": report.actor_count,
            }))
            .into_response()
        }
        Err(error) => metadata_selection_error(&headers, error),
    }
}

pub(crate) fn metadata_selection_error(
    headers: &HeaderMap,
    error: MetadataSelectionError,
) -> Response {
    match error {
        MetadataSelectionError::ItemNotFound | MetadataSelectionError::CandidateNotFound => {
            api_error(
                headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "媒体条目或候选不存在",
            )
            .into_response()
        }
        MetadataSelectionError::CandidateNotPending(_) => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "候选已处理，不能重复选择",
        )
        .into_response(),
        MetadataSelectionError::InvalidCandidate(_) => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "候选数据无效",
        )
        .into_response(),
        MetadataSelectionError::Nfo(_)
        | MetadataSelectionError::Image(_)
        | MetadataSelectionError::People(_) => api_error(
            headers,
            StatusCode::INTERNAL_SERVER_ERROR,
            lux::ApiErrorCode::Internal,
            "元数据写回失败，可重试",
        )
        .into_response(),
        MetadataSelectionError::Storage(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "元数据保存暂时不可用，可重试",
        )
        .into_response(),
    }
}

pub(crate) fn image_write_error(headers: &HeaderMap, error: ImageWriteError) -> Response {
    match error {
        ImageWriteError::ItemNotFound => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "媒体条目或图片不存在",
        )
        .into_response(),
        ImageWriteError::PathOutsideRoot(_) | ImageWriteError::SymlinkTarget(_) => api_error(
            headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::PermissionDenied,
            "图片路径不在媒体根目录内",
        )
        .into_response(),
        ImageWriteError::Storage(_) | ImageWriteError::Io { .. } => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "图片操作暂时失败",
        )
        .into_response(),
        ImageWriteError::AttemptInProgress => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::Internal,
            "该图片正在下载，请稍后重试",
        )
        .into_response(),
        _ => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "图片请求无效",
        )
        .into_response(),
    }
}

pub(crate) fn item_image_json(item_id: &str, image: &crate::storage::StoredItemImage) -> Value {
    let image_type = image.image_type.to_ascii_lowercase();
    let index = if image.image_index > 0 {
        format!("/{}", image.image_index)
    } else {
        String::new()
    };
    json!({
        "id": image.id,
        "itemId": item_id,
        "imageType": image.image_type,
        "imageIndex": image.image_index,
        "fileSize": image.file_size,
        "contentTag": image.content_tag,
        "source": image.source,
        "language": Value::Null,
        "url": format!("/api/v1/items/{}/images/{}{}", encode_path_segment(item_id), image_type, index),
    })
}

pub(crate) fn image_candidate_json(image: &crate::application::images::ImageCandidate) -> Value {
    json!({
        "id": image.id,
        "imageType": image.image_type,
        "imageIndex": image.image_index,
        "language": image.language,
        "width": image.width,
        "height": image.height,
        "source": image.source,
        "url": image.url,
    })
}

pub(crate) fn encode_path_segment(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                vec![char::from(byte)]
            } else {
                format!("%{byte:02X}").chars().collect()
            }
        })
        .collect()
}

pub(crate) fn image_candidate_error(headers: &HeaderMap, error: ImageCandidateError) -> Response {
    match error {
        ImageCandidateError::ItemNotFound => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "媒体条目不存在",
        )
        .into_response(),
        ImageCandidateError::ItemNotIdentified => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "媒体条目尚未完成元数据匹配，暂时无法搜索图片",
        )
        .into_response(),
        ImageCandidateError::InvalidItem
        | ImageCandidateError::InvalidImageType(_)
        | ImageCandidateError::InvalidLanguage
        | ImageCandidateError::InvalidSource => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "图片搜索请求无效",
        )
        .into_response(),
        ImageCandidateError::Scraper(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::Internal,
            "刮削器暂时不可用",
        )
        .into_response(),
        ImageCandidateError::Storage(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "图片搜索暂时不可用",
        )
        .into_response(),
    }
}

pub(crate) fn metadata_write_error(headers: &HeaderMap, error: NfoWriteError) -> Response {
    match error {
        NfoWriteError::ItemNotFound => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "媒体条目不存在或没有本地媒体源",
        )
        .into_response(),
        NfoWriteError::InvalidMetadata(_) => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "元数据请求无效",
        )
        .into_response(),
        NfoWriteError::PathOutsideRoot(_) | NfoWriteError::SymlinkTarget(_) => api_error(
            headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::PermissionDenied,
            "元数据路径不在媒体根目录内",
        )
        .into_response(),
        NfoWriteError::Storage(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "元数据保存暂时不可用",
        )
        .into_response(),
        NfoWriteError::Nfo(_)
        | NfoWriteError::InvalidXml(_)
        | NfoWriteError::Io { .. }
        | NfoWriteError::ConcurrentModification(_) => api_error(
            headers,
            StatusCode::INTERNAL_SERVER_ERROR,
            lux::ApiErrorCode::Internal,
            "元数据写回失败，可重试",
        )
        .into_response(),
    }
}

pub(crate) fn metadata_candidate_page_json(page: &MetadataCandidatePage) -> Value {
    json!({
        "items": page.items.iter().map(|item| json!({
            "id": item.id,
            "itemId": item.item_id,
            "itemTitle": item.item_title,
            "provider": item.provider,
            "providerId": item.provider_id,
            "candidate": item.candidate,
            "score": item.score,
            "status": item.status,
            "expiresAt": item.expires_at,
            "fieldDiffs": item.field_diffs.iter().map(|diff| json!({
                "field": diff.field,
                "current": diff.current,
                "candidate": diff.candidate,
                "provenance": diff.provenance,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "total": page.total,
        "page": page.offset / page.limit + 1,
        "pageSize": page.limit,
    })
}

pub(crate) fn metadata_reidentify_job_json(
    job: &crate::application::reidentify::MetadataReidentifyJob,
) -> Value {
    json!({
        "id": job.id,
        "status": job.status,
        "mode": job.mode,
        "processedCount": job.processed_count,
        "totalCount": job.total_count,
        "error": job.error,
        "createdAt": job.created_at,
        "updatedAt": job.updated_at,
        "startedAt": job.started_at,
        "finishedAt": job.finished_at,
        "cancelRequested": job.cancel_requested,
        "libraryId": job.library_id,
        "jobScope": job.job_scope,
        "pendingCount": job.pending_count,
        "items": job.items.iter().map(|item| json!({
            "jobId": item.job_id,
            "itemId": item.item_id,
            "status": item.status,
            "candidateCount": item.candidate_count,
            "error": item.error,
            "updatedAt": item.updated_at,
        })).collect::<Vec<_>>(),
    })
}

pub(crate) fn metadata_reidentify_job_summary_json(
    job: &crate::storage::StoredMetadataReidentifyJob,
) -> Value {
    json!({
        "id": job.id,
        "status": job.status,
        "mode": job.mode,
        "processedCount": job.processed_count,
        "totalCount": job.total_count,
        "error": job.error,
        "createdAt": job.created_at,
        "updatedAt": job.updated_at,
        "startedAt": job.started_at,
        "finishedAt": job.finished_at,
        "cancelRequested": job.cancel_requested,
        "libraryId": job.library_id,
        "jobScope": job.job_scope,
        "pendingCount": job.pending_count,
    })
}

pub(crate) fn metadata_reidentify_error(
    headers: &HeaderMap,
    error: MetadataReidentifyError,
) -> Response {
    match error {
        MetadataReidentifyError::InvalidItemCount
        | MetadataReidentifyError::InvalidRefreshMode
        | MetadataReidentifyError::InvalidSearch
        | MetadataReidentifyError::Candidate(MetadataCandidateError::InvalidSearch) => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "批量元数据匹配请求无效",
        )
        .into_response(),
        MetadataReidentifyError::ItemNotFound(_)
        | MetadataReidentifyError::JobNotFound
        | MetadataReidentifyError::Candidate(MetadataCandidateError::ItemNotFound) => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "媒体条目或元数据匹配任务不存在",
        )
        .into_response(),
        MetadataReidentifyError::JobNotRetryable => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "该批量元数据匹配任务当前不可重试",
        )
        .into_response(),
        MetadataReidentifyError::JobNotCancelable => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "该批量元数据匹配任务当前不可取消",
        )
        .into_response(),
        MetadataReidentifyError::LibraryJobAlreadyActive(_) => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "已有整库元数据任务正在运行，请等待完成或先取消该任务",
        )
        .into_response(),
        MetadataReidentifyError::Candidate(MetadataCandidateError::InvalidCandidateJson(_)) => {
            api_error(
                headers,
                StatusCode::INTERNAL_SERVER_ERROR,
                lux::ApiErrorCode::Internal,
                "候选数据损坏",
            )
            .into_response()
        }
        MetadataReidentifyError::Candidate(MetadataCandidateError::Scraper(_))
        | MetadataReidentifyError::Scraper(_)
        | MetadataReidentifyError::Selection(_)
        | MetadataReidentifyError::SelectionUnavailable
        | MetadataReidentifyError::LowConfidence
        | MetadataReidentifyError::Candidate(MetadataCandidateError::Storage(_))
        | MetadataReidentifyError::Storage(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "批量元数据匹配暂时不可用",
        )
        .into_response(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MetadataCandidateFailureKind {
    ItemNotFound,
    InvalidSearch,
    InvalidCandidateJson,
    ScraperUnavailable,
    StorageUnavailable,
}

impl MetadataCandidateFailureKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ItemNotFound => "ITEM_NOT_FOUND",
            Self::InvalidSearch => "INVALID_SEARCH",
            Self::InvalidCandidateJson => "INVALID_CANDIDATE_JSON",
            Self::ScraperUnavailable => "SCRAPER_UNAVAILABLE",
            Self::StorageUnavailable => "STORAGE_UNAVAILABLE",
        }
    }
}

pub(crate) fn metadata_candidate_failure_kind(
    error: &MetadataCandidateError,
) -> MetadataCandidateFailureKind {
    match error {
        MetadataCandidateError::ItemNotFound => MetadataCandidateFailureKind::ItemNotFound,
        MetadataCandidateError::InvalidSearch => MetadataCandidateFailureKind::InvalidSearch,
        MetadataCandidateError::InvalidCandidateJson(_) => {
            MetadataCandidateFailureKind::InvalidCandidateJson
        }
        MetadataCandidateError::Scraper(_) => MetadataCandidateFailureKind::ScraperUnavailable,
        MetadataCandidateError::Storage(_) => MetadataCandidateFailureKind::StorageUnavailable,
    }
}

pub(crate) fn metadata_candidate_error(
    headers: &HeaderMap,
    error: MetadataCandidateError,
) -> Response {
    let failure_kind = metadata_candidate_failure_kind(&error);
    let request_id = header_str(headers, "x-request-id").unwrap_or("unknown");
    tracing::warn!(
        event = "metadata_candidate_request_failed",
        error_kind = failure_kind.as_str(),
        request_id = %request_id,
        "metadata candidate request failed"
    );

    match error {
        MetadataCandidateError::ItemNotFound => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "媒体条目不存在",
        )
        .into_response(),
        MetadataCandidateError::InvalidSearch => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "候选搜索条件无效",
        )
        .into_response(),
        MetadataCandidateError::InvalidCandidateJson(_) => api_error(
            headers,
            StatusCode::INTERNAL_SERVER_ERROR,
            lux::ApiErrorCode::Internal,
            "候选数据损坏",
        )
        .into_response(),
        MetadataCandidateError::Scraper(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "刮削器暂时不可用，请稍后重试",
        )
        .into_response(),
        MetadataCandidateError::Storage(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "数据库暂时不可用",
        )
        .into_response(),
    }
}

pub(crate) async fn admin_list_plugins(
    headers: HeaderMap,
    Query(query): Query<LuxPageQuery>,
    State(state): State<AppState>,
) -> Response {
    admin_list_plugins_with_scope(headers, query, state, false).await
}

pub(crate) async fn admin_list_notification_providers(
    headers: HeaderMap,
    Query(query): Query<LuxPageQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let (offset, limit) = match lux_page_params(&query) {
        Ok(params) => params,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    let Some(plugins) = state.plugins.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match plugins.list_notification_plugins(offset, limit).await {
        Ok(page) => Json(plugin_page_json(&page)).into_response(),
        Err(error) => plugin_error(&headers, error),
    }
}

pub(crate) async fn admin_list_chapter_sources(
    headers: HeaderMap,
    Query(query): Query<LuxPageQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let (offset, limit) = match lux_page_params(&query) {
        Ok(params) => params,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    let Some(plugins) = state.plugins.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match plugins.list_chapter_sources(offset, limit).await {
        Ok(page) => Json(json!({
            "sources": page.sources.iter().map(|source| json!({
                "id": source.id,
                "name": source.name,
                "description": source.description,
                "version": source.version,
                "capabilities": source.capabilities,
                "lookup": source.lookup,
                "supportedMediaSourceKinds": source.supported_media_source_kinds,
            })).collect::<Vec<_>>(),
            "total": page.total,
            "page": page.offset / page.limit + 1,
            "pageSize": page.limit,
        }))
        .into_response(),
        Err(error) => plugin_error(&headers, error),
    }
}

pub(crate) async fn admin_plugin_store(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(plugins) = state.plugins.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    Json(json!({
        "url": plugins.plugin_store_source().await,
        "defaultUrl": crate::application::plugin_store::DEFAULT_PLUGIN_STORE_URL,
    }))
    .into_response()
}

pub(crate) async fn admin_test_emby_migration(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.emby_migration.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.test_connection().await {
        Ok(info) => Json(info).into_response(),
        Err(error) => emby_migration_error(&headers, error),
    }
}

pub(crate) async fn admin_list_emby_migration_source_users(
    headers: HeaderMap,
    Query(query): Query<AdminJobsQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let (offset, limit) = match page_params(query.page, query.page_size) {
        Ok(params) => params,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    let Some(service) = state.emby_migration.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service
        .list_source_users_filtered(offset, limit, query.search.as_deref())
        .await
    {
        Ok(page) => Json(json!({
            "users": page.users,
            "total": page.total,
            "page": offset / limit + 1,
            "pageSize": limit,
        }))
        .into_response(),
        Err(error) => emby_migration_error(&headers, error),
    }
}

pub(crate) async fn admin_create_emby_migration(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<CreateMigrationRequest>,
) -> Response {
    let actor = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.emby_migration.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.create_job(&actor.id.to_string(), request).await {
        Ok(job) => {
            let job_id = job.id.clone();
            service.clone().spawn(job_id.clone());
            record_audit_event(
                &state,
                &headers,
                "EMBY_MIGRATION_CREATED",
                Some("emby_migration_job"),
                Some(&job_id),
                "{}",
            )
            .await;
            (StatusCode::ACCEPTED, Json(json!({ "job": job }))).into_response()
        }
        Err(error) => emby_migration_error(&headers, error),
    }
}

pub(crate) async fn admin_list_emby_migrations(
    headers: HeaderMap,
    Query(query): Query<AdminJobsQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let (offset, limit) = match page_params(query.page, query.page_size) {
        Ok(params) => params,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    let Some(service) = state.emby_migration.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let jobs = match service.list_jobs(offset, limit).await {
        Ok(jobs) => jobs,
        Err(error) => return emby_migration_error(&headers, error),
    };
    let total = match service.count_jobs().await {
        Ok(total) => total,
        Err(error) => return emby_migration_error(&headers, error),
    };
    Json(json!({
        "jobs": jobs,
        "total": total,
        "page": offset / limit + 1,
        "pageSize": limit,
    }))
    .into_response()
}

pub(crate) async fn admin_get_emby_migration(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(service) = state.emby_migration.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.get_job(&job_id).await {
        Ok(job) => Json(json!({ "job": job })).into_response(),
        Err(error) => emby_migration_error(&headers, error),
    }
}

pub(crate) async fn admin_cancel_emby_migration(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.emby_migration.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.cancel_job(&job_id).await {
        Ok(true) => Json(json!({ "cancelRequested": true })).into_response(),
        Ok(false) => api_error(
            &headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "迁移任务不存在或已经结束",
        )
        .into_response(),
        Err(error) => emby_migration_error(&headers, error),
    }
}

pub(crate) async fn admin_retry_emby_migration(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(service) = state.emby_migration.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.resume_job(&job_id).await {
        Ok(true) => {
            service.clone().spawn(job_id.clone());
            (
                StatusCode::ACCEPTED,
                Json(json!({ "jobId": job_id, "status": "ACCEPTED" })),
            )
                .into_response()
        }
        Ok(false) => api_error(
            &headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "迁移任务不能恢复",
        )
        .into_response(),
        Err(error) => emby_migration_error(&headers, error),
    }
}

pub(crate) fn emby_migration_error(
    headers: &HeaderMap,
    error: EmbyMigrationServiceError,
) -> Response {
    let (status, message) = match error {
        EmbyMigrationServiceError::InvalidInput(_) => {
            (StatusCode::BAD_REQUEST, "Emby 迁移参数无效")
        }
        EmbyMigrationServiceError::NotFound => (StatusCode::NOT_FOUND, "迁移任务不存在"),
        EmbyMigrationServiceError::InvalidState => {
            (StatusCode::CONFLICT, "迁移任务状态不允许此操作")
        }
        EmbyMigrationServiceError::AlreadyActive => {
            (StatusCode::CONFLICT, "已有 Emby 迁移正在执行或等待执行")
        }
        EmbyMigrationServiceError::Plugin(
            crate::application::plugins::PluginServiceError::InvalidConfig,
        ) => (StatusCode::BAD_REQUEST, "Emby 迁移插件配置无效"),
        EmbyMigrationServiceError::Plugin(_) => {
            (StatusCode::SERVICE_UNAVAILABLE, "Emby 迁移插件暂时不可用")
        }
        EmbyMigrationServiceError::Storage(_)
        | EmbyMigrationServiceError::User(_)
        | EmbyMigrationServiceError::Io(_) => {
            (StatusCode::SERVICE_UNAVAILABLE, "Emby 迁移服务暂时不可用")
        }
    };
    api_error(headers, status, lux::ApiErrorCode::InvalidRequest, message).into_response()
}

pub(crate) async fn admin_list_emby_migration_users(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    Query(query): Query<AdminJobsQuery>,
    State(state): State<AppState>,
) -> Response {
    admin_list_emby_migration_report(
        headers,
        job_id,
        query,
        state,
        EmbyMigrationReportKind::Users,
    )
    .await
}

pub(crate) async fn admin_list_emby_migration_matches(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    Query(query): Query<AdminJobsQuery>,
    State(state): State<AppState>,
) -> Response {
    admin_list_emby_migration_report(
        headers,
        job_id,
        query,
        state,
        EmbyMigrationReportKind::Matches,
    )
    .await
}

pub(crate) async fn admin_list_emby_migration_imports(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    Query(query): Query<AdminJobsQuery>,
    State(state): State<AppState>,
) -> Response {
    admin_list_emby_migration_report(
        headers,
        job_id,
        query,
        state,
        EmbyMigrationReportKind::Imports,
    )
    .await
}

pub(crate) async fn admin_list_emby_migration_person_favorites(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    Query(query): Query<AdminJobsQuery>,
    State(state): State<AppState>,
) -> Response {
    admin_list_emby_migration_report(
        headers,
        job_id,
        query,
        state,
        EmbyMigrationReportKind::PersonFavorites,
    )
    .await
}

#[derive(Clone, Copy)]
pub(crate) enum EmbyMigrationReportKind {
    Users,
    Matches,
    Imports,
    PersonFavorites,
}

pub(crate) async fn admin_list_emby_migration_report(
    headers: HeaderMap,
    job_id: String,
    query: AdminJobsQuery,
    state: AppState,
    kind: EmbyMigrationReportKind,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let (offset, limit) = match page_params(query.page, query.page_size) {
        Ok(params) => params,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    let Some(service) = state.emby_migration.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let result = match kind {
        EmbyMigrationReportKind::Users => service
            .list_user_links(&job_id, offset, limit)
            .await
            .map(|items| json!({ "users": items })),
        EmbyMigrationReportKind::Matches => service
            .list_item_matches(&job_id, offset, limit)
            .await
            .map(|items| json!({ "matches": items })),
        EmbyMigrationReportKind::Imports => service
            .list_import_records(&job_id, offset, limit)
            .await
            .map(|items| json!({ "imports": items })),
        EmbyMigrationReportKind::PersonFavorites => service
            .list_person_favorite_records(&job_id, offset, limit)
            .await
            .map(|items| json!({ "personFavorites": items })),
    };
    match result {
        Ok(mut response) => {
            response["page"] = json!(offset / limit + 1);
            response["pageSize"] = json!(limit);
            Json(response).into_response()
        }
        Err(error) => emby_migration_error(&headers, error),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PluginStoreUpdateRequest {
    pub(crate) url: String,
}

pub(crate) async fn admin_update_plugin_store(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<PluginStoreUpdateRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(plugins) = state.plugins.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match plugins.update_plugin_store_source(&request.url).await {
        Ok(url) => {
            record_audit_event(
                &state,
                &headers,
                "PLUGIN_STORE_UPDATED",
                Some("plugin_store"),
                None,
                "{}",
            )
            .await;
            Json(json!({
                "url": url,
                "defaultUrl": crate::application::plugin_store::DEFAULT_PLUGIN_STORE_URL,
            }))
            .into_response()
        }
        Err(crate::application::plugins::PluginServiceError::Store(
            crate::application::plugin_store::PluginStoreError::InvalidSource,
        )) => api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "插件商店地址无效",
        )
        .into_response(),
        Err(error) => plugin_error(&headers, error),
    }
}

pub(crate) async fn admin_list_installed_plugins(
    headers: HeaderMap,
    Query(query): Query<LuxPageQuery>,
    State(state): State<AppState>,
) -> Response {
    admin_list_plugins_with_scope(headers, query, state, true).await
}

pub(crate) async fn admin_list_plugins_with_scope(
    headers: HeaderMap,
    query: LuxPageQuery,
    state: AppState,
    installed_only: bool,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let (offset, limit) = match lux_page_params(&query) {
        Ok(params) => params,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    let Some(plugins) = state.plugins.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let page = if installed_only {
        plugins.list_installed(offset, limit).await
    } else {
        plugins.list(offset, limit).await
    };
    match page {
        Ok(page) => Json(plugin_page_json(&page)).into_response(),
        Err(error) => plugin_error(&headers, error),
    }
}

pub(crate) async fn admin_install_plugin(
    headers: HeaderMap,
    Path(plugin_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(plugins) = state.plugins.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    match plugins.install(&plugin_id).await {
        Ok(result) => {
            record_audit_event(
                &state,
                &headers,
                "PLUGIN_INSTALLED",
                Some("plugin"),
                Some(&plugin_id),
                "{}",
            )
            .await;
            let status = if result.was_installed {
                StatusCode::OK
            } else {
                StatusCode::CREATED
            };
            (
                status,
                Json(json!({ "plugin": plugin_json(&result.plugin) })),
            )
                .into_response()
        }
        Err(error) => plugin_error(&headers, error),
    }
}

pub(crate) async fn admin_update_plugin(
    headers: HeaderMap,
    Path(plugin_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(plugins) = state.plugins.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    match plugins.update(&plugin_id).await {
        Ok(plugin) => {
            record_audit_event(
                &state,
                &headers,
                "PLUGIN_UPDATED",
                Some("plugin"),
                Some(&plugin_id),
                "{}",
            )
            .await;
            Json(json!({ "plugin": plugin_json(&plugin) })).into_response()
        }
        Err(error) => plugin_error(&headers, error),
    }
}

pub(crate) async fn admin_uninstall_plugin(
    headers: HeaderMap,
    Path(plugin_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(plugins) = state.plugins.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    match plugins.uninstall(&plugin_id).await {
        Ok(()) => {
            record_audit_event(
                &state,
                &headers,
                "PLUGIN_UNINSTALLED",
                Some("plugin"),
                Some(&plugin_id),
                "{}",
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => plugin_error(&headers, error),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PluginEnabledRequest {
    pub(crate) enabled: bool,
}

pub(crate) async fn admin_update_plugin_enabled(
    headers: HeaderMap,
    Path(plugin_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<PluginEnabledRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(plugins) = state.plugins.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    match plugins.set_enabled(&plugin_id, request.enabled).await {
        Ok(plugin) => {
            record_audit_event(
                &state,
                &headers,
                if request.enabled {
                    "PLUGIN_ENABLED"
                } else {
                    "PLUGIN_DISABLED"
                },
                Some("plugin"),
                Some(&plugin_id),
                "{}",
            )
            .await;
            (
                StatusCode::OK,
                Json(json!({ "plugin": plugin_json(&plugin) })),
            )
                .into_response()
        }
        Err(error) => plugin_error(&headers, error),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PluginConfigRequest {
    #[serde(flatten)]
    pub(crate) values: serde_json::Map<String, Value>,
}

pub(crate) async fn admin_update_plugin_config(
    headers: HeaderMap,
    Path(plugin_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<PluginConfigRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(plugins) = state.plugins.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let result = plugins
        .update_dynamic_config(&plugin_id, request.values)
        .await;
    match result {
        Ok(plugin) => {
            plugins.restart(&plugin_id).await;
            record_audit_event(
                &state,
                &headers,
                "PLUGIN_CONFIG_UPDATED",
                Some("plugin"),
                Some(&plugin_id),
                "{}",
            )
            .await;
            (
                StatusCode::OK,
                Json(json!({ "plugin": plugin_json(&plugin) })),
            )
                .into_response()
        }
        Err(error) => plugin_error(&headers, error),
    }
}

pub(crate) async fn validate_scraper_selection(
    headers: &HeaderMap,
    state: &AppState,
    scraper_id: Option<&str>,
) -> Result<(), Response> {
    let Some(plugins) = state.plugins.as_ref() else {
        return Err(api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response());
    };
    plugins
        .validate_selection(scraper_id)
        .await
        .map_err(|error| plugin_error(headers, error).into_response())
}

pub(crate) fn create_library_scrapers(
    request: &CreateLibraryRequest,
) -> Result<Vec<LibraryScraper>, &'static str> {
    if request.scrapers.is_some() && request.scraper_id.is_some() {
        return Err("scraperId 和 scrapers 不能同时提交");
    }
    if let Some(scrapers) = request.scrapers.as_deref() {
        return Ok(scrapers
            .iter()
            .enumerate()
            .map(|(position, scraper)| LibraryScraper {
                scraper_id: scraper.scraper_id.clone(),
                position: i64::try_from(position).unwrap_or(i64::MAX),
                role: scraper.role,
            })
            .collect());
    }
    Ok(request
        .scraper_id
        .as_deref()
        .map(|scraper_id| LibraryScraper {
            scraper_id: scraper_id.to_owned(),
            position: 0,
            role: LibraryScraperRole::Primary,
        })
        .into_iter()
        .collect())
}

type LibraryScraperUpdate = (Option<Option<String>>, Option<Option<Vec<LibraryScraper>>>);

pub(crate) fn update_library_scrapers(
    request: &UpdateLibraryRequest,
) -> Result<LibraryScraperUpdate, &'static str> {
    if request.scrapers.is_some() && request.scraper_id.is_some() {
        return Err("scraperId 和 scrapers 不能同时提交");
    }
    if let Some(scrapers) = request.scrapers.as_ref() {
        return Ok((
            None,
            Some(scrapers.as_ref().map(|scrapers| {
                scrapers
                    .iter()
                    .enumerate()
                    .map(|(position, scraper)| LibraryScraper {
                        scraper_id: scraper.scraper_id.clone(),
                        position: i64::try_from(position).unwrap_or(i64::MAX),
                        role: scraper.role,
                    })
                    .collect()
            })),
        ));
    }
    Ok((request.scraper_id.clone(), None))
}

pub(crate) async fn validate_scraper_selections(
    headers: &HeaderMap,
    state: &AppState,
    scrapers: &[LibraryScraper],
) -> Result<(), Response> {
    for scraper in scrapers {
        validate_scraper_selection(headers, state, Some(&scraper.scraper_id)).await?;
    }
    Ok(())
}

pub(crate) async fn validate_chapter_source_selection(
    headers: &HeaderMap,
    state: &AppState,
    kind: LibraryKind,
    chapter_source_id: Option<&str>,
) -> Result<(), Response> {
    let Some(chapter_source_id) = chapter_source_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(());
    };
    if !kind.supports_chapter_source() {
        return Err(api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "片头片尾数据源只能用于剧集或混合媒体库",
        )
        .into_response());
    }
    let Some(plugins) = state.plugins.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE.into_response());
    };
    match plugins
        .has_available_chapter_source(chapter_source_id)
        .await
    {
        Ok(true) => Ok(()),
        Ok(false) => Err(plugin_error(
            headers,
            PluginServiceError::Unavailable(chapter_source_id.to_owned()),
        )),
        Err(error) => Err(plugin_error(headers, error)),
    }
}

pub(crate) async fn admin_create_library(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<CreateLibraryRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(libraries) = state.libraries.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let scrapers = match create_library_scrapers(&request) {
        Ok(scrapers) => scrapers,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    if let Err(response) = validate_scraper_selections(&headers, &state, &scrapers).await {
        return response;
    }
    let kind = match request.kind.parse::<LibraryKind>() {
        Ok(kind) => kind,
        Err(_error) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "媒体库类型无效",
            )
            .into_response();
        }
    };
    if let Err(response) = validate_chapter_source_selection(
        &headers,
        &state,
        kind,
        request.chapter_source_id.as_deref(),
    )
    .await
    {
        return response;
    }
    match libraries
        .create_library_with_scrapers_and_chapter_source(
            &request.name,
            kind,
            request.realtime_watch_enabled,
            &scrapers,
            request.chapter_source_id.as_deref(),
            request.realtime_metadata_auto_match_enabled,
        )
        .await
    {
        Ok(library) => {
            if let Some(home) = state.home.as_ref() {
                home.invalidate();
            }
            if let Some(plugins) = state.plugins.as_ref()
                && let Err(error) = plugins.sync_chapter_detection_scheduled_tasks().await
            {
                return plugin_error(&headers, error);
            }
            let library_id = library.id.to_string();
            record_audit_event(
                &state,
                &headers,
                "LIBRARY_CREATED",
                Some("library"),
                Some(&library_id),
                "{}",
            )
            .await;
            (
                StatusCode::CREATED,
                Json(json!({
                    "library": library_json(&library, &[]),
                    "warnings": []
                })),
            )
                .into_response()
        }
        Err(error) => library_error(&headers, error),
    }
}

pub(crate) async fn admin_update_library(
    headers: HeaderMap,
    Path(library_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<UpdateLibraryRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let library_id = match library_id.parse::<crate::domain::ids::LibraryId>() {
        Ok(id) => id,
        Err(error) => {
            return library_error(
                &headers,
                LibraryServiceError::InvalidLibraryId(error.to_string()),
            );
        }
    };
    let Some(libraries) = state.libraries.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let kind = match request.kind.as_deref() {
        Some(value) => match value.parse::<LibraryKind>() {
            Ok(kind) => Some(kind),
            Err(_error) => {
                return api_error(
                    &headers,
                    StatusCode::BAD_REQUEST,
                    lux::ApiErrorCode::InvalidRequest,
                    "媒体库类型无效",
                )
                .into_response();
            }
        },
        None => None,
    };
    let current_library = match libraries.get_library(library_id).await {
        Ok(library) => library,
        Err(error) => return library_error(&headers, error),
    };
    if let Err(response) = validate_chapter_source_selection(
        &headers,
        &state,
        kind.unwrap_or(current_library.kind),
        request
            .chapter_source_id
            .as_ref()
            .and_then(|value| value.as_deref()),
    )
    .await
    {
        return response;
    }
    let effective_kind = kind.unwrap_or(current_library.kind);
    let chapter_source_id = request.chapter_source_id.clone().or_else(|| {
        (!effective_kind.supports_chapter_source() && current_library.chapter_source_id.is_some())
            .then_some(None)
    });
    let media_strategy_json = match request.media_strategy.as_ref() {
        None => None,
        Some(None) => Some(None),
        Some(Some(strategy)) => {
            if !validate_media_strategy(strategy) {
                return api_error(
                    &headers,
                    StatusCode::BAD_REQUEST,
                    lux::ApiErrorCode::InvalidRequest,
                    "媒体库策略无效",
                )
                .into_response();
            }
            if let Some(scraper_id) = strategy.scraper_id.as_deref() {
                if let Err(response) =
                    validate_scraper_selection(&headers, &state, Some(scraper_id)).await
                {
                    return response;
                }
            }
            match serde_json::to_string(strategy) {
                Ok(value) => Some(Some(value)),
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            }
        }
    };
    let (scraper_id, scrapers) = match update_library_scrapers(&request) {
        Ok(value) => value,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    if let Some(scrapers) = scrapers.as_ref().and_then(|value| value.as_ref())
        && let Err(response) = validate_scraper_selections(&headers, &state, scrapers).await
    {
        return response;
    }
    let settings = LibrarySettingsPatch {
        name: request.name,
        kind,
        is_enabled: request.is_enabled,
        realtime_watch_enabled: request.realtime_watch_enabled,
        realtime_metadata_auto_match_enabled: request.realtime_metadata_auto_match_enabled,
        reconciliation_schedule: request.reconciliation_schedule,
        metadata_schedule: request.metadata_schedule,
        scraper_id,
        scrapers: scrapers.map(|value| value.unwrap_or_default()),
        chapter_source_id,
        media_strategy_json,
        scan_concurrency: request.scan_concurrency,
        probe_concurrency: request.probe_concurrency,
    };
    match libraries.update_settings(library_id, settings).await {
        Ok(view) => {
            if let Some(home) = state.home.as_ref() {
                home.invalidate();
            }
            if let Some(plugins) = state.plugins.as_ref()
                && let Err(error) = plugins.sync_chapter_detection_scheduled_tasks().await
            {
                return plugin_error(&headers, error);
            }
            let target_id = library_id.to_string();
            record_audit_event(
                &state,
                &headers,
                "LIBRARY_UPDATED",
                Some("library"),
                Some(&target_id),
                "{}",
            )
            .await;
            (
                StatusCode::OK,
                Json(json!({
                    "library": library_json(&view.library, &view.roots)
                })),
            )
                .into_response()
        }
        Err(error) => library_error(&headers, error),
    }
}

pub(crate) async fn admin_update_library_cover(
    headers: HeaderMap,
    Path(library_id): Path<String>,
    State(state): State<AppState>,
    body: Bytes,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let library_id = match library_id.parse::<crate::domain::ids::LibraryId>() {
        Ok(id) => id,
        Err(error) => {
            return library_error(
                &headers,
                LibraryServiceError::InvalidLibraryId(error.to_string()),
            );
        }
    };
    let Some(covers) = state.library_covers.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let content_type = headers
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    match covers.store(library_id, content_type, &body).await {
        Ok(cover) => {
            let target_id = library_id.to_string();
            let cover_url =
                library_cover_url_for_tag(&target_id, Some(cover.etag.trim_matches('"')));
            record_audit_event(
                &state,
                &headers,
                "LIBRARY_COVER_UPDATED",
                Some("library"),
                Some(&target_id),
                "{}",
            )
            .await;
            (
                StatusCode::OK,
                Json(json!({
                    "library": {
                        "id": target_id,
                        "coverImageUrl": cover_url,
                        "contentType": cover.content_type,
                        "contentLength": cover.content_length,
                    }
                })),
            )
                .into_response()
        }
        Err(error) => library_cover_error(&headers, error),
    }
}

pub(crate) async fn admin_list_library_cover_jobs(
    headers: HeaderMap,
    Query(query): Query<AdminJobsQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let (offset, limit) = match page_params(query.page, query.page_size) {
        Ok(value) => value,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    let status = query.status.as_deref().map(str::to_ascii_uppercase);
    if status.as_deref().is_some_and(|value| {
        !matches!(
            value,
            "PENDING" | "RUNNING" | "COMPLETED" | "CANCELLED" | "FAILED"
        )
    }) {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "任务状态无效",
        )
        .into_response();
    }
    let Some(service) = state.library_covers.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.list(status.as_deref(), offset, limit).await {
        Ok(jobs) => Json(json!({
            "jobs": jobs,
            "page": offset / limit + 1,
            "pageSize": limit,
        }))
        .into_response(),
        Err(error) => library_cover_error(&headers, error),
    }
}

pub(crate) async fn admin_get_library_cover_job(
    headers: HeaderMap,
    Path(job_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(service) = state.library_covers.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.get(&job_id).await {
        Ok(job) => Json(json!({ "job": job })).into_response(),
        Err(error) => library_cover_error(&headers, error),
    }
}

pub(crate) async fn admin_run_auto_library_cover(
    headers: HeaderMap,
    Path(library_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let library_id = match library_id.parse::<crate::domain::ids::LibraryId>() {
        Ok(id) => id,
        Err(error) => {
            return library_error(
                &headers,
                LibraryServiceError::InvalidLibraryId(error.to_string()),
            );
        }
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let library_id_text = library_id.to_string();
    match database
        .find_scheduled_task_config(
            "LIBRARY",
            &library_id_text,
            crate::application::library_covers::AUTO_LIBRARY_COVER_TASK_TYPE,
        )
        .await
    {
        Ok(Some(_)) => {}
        Ok(None) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "自动媒体库封面任务尚未注册",
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(covers) = state.library_covers.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let job = match covers.create_manual_job(library_id).await {
        Ok(job) => job,
        Err(error) => return library_cover_error(&headers, error),
    };
    let job_id = job.id.clone();
    tokio::spawn(async move {
        match covers.run_job(&job_id).await {
            Ok(crate::application::library_covers::AutoLibraryCoverResult::Generated) => {
                tracing::info!(job_id = %job_id, "manual automatic library cover generation completed");
            }
            Ok(
                crate::application::library_covers::AutoLibraryCoverResult::ExistingCover
                | crate::application::library_covers::AutoLibraryCoverResult::BelowThreshold
                | crate::application::library_covers::AutoLibraryCoverResult::TaskNotRegistered
                | crate::application::library_covers::AutoLibraryCoverResult::AlreadyHandled,
            ) => {
                tracing::info!(job_id = %job_id, "manual automatic library cover generation skipped");
            }
            Err(error) => {
                tracing::warn!(job_id = %job_id, %error, "manual automatic library cover generation failed");
            }
        }
    });
    record_audit_event(
        &state,
        &headers,
        "LIBRARY_COVER_GENERATION_STARTED",
        Some("library"),
        Some(&library_id_text),
        "{\"mode\":\"manual\"}",
    )
    .await;
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "status": "QUEUED",
            "taskType": crate::application::library_covers::AUTO_LIBRARY_COVER_TASK_TYPE,
            "job": job,
        })),
    )
        .into_response()
}

pub(crate) async fn admin_delete_library_root(
    headers: HeaderMap,
    Path((library_id, root_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let library_id = match library_id.parse::<crate::domain::ids::LibraryId>() {
        Ok(id) => id,
        Err(error) => {
            return library_error(
                &headers,
                LibraryServiceError::InvalidLibraryId(error.to_string()),
            );
        }
    };
    let root_id = match root_id.parse::<crate::domain::ids::LibraryRootId>() {
        Ok(id) => id,
        Err(error) => {
            return library_error(
                &headers,
                LibraryServiceError::InvalidRootId(error.to_string()),
            );
        }
    };
    let Some(libraries) = state.libraries.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match libraries.delete_root(library_id, root_id).await {
        Ok(()) => {
            if let Some(home) = state.home.as_ref() {
                home.invalidate();
            }
            let target_id = root_id.to_string();
            record_audit_event(
                &state,
                &headers,
                "LIBRARY_ROOT_DELETED",
                Some("library_root"),
                Some(&target_id),
                "{}",
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => library_error(&headers, error),
    }
}

pub(crate) async fn admin_delete_library(
    headers: HeaderMap,
    Path(library_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let library_id = match library_id.parse::<crate::domain::ids::LibraryId>() {
        Ok(id) => id,
        Err(error) => {
            return library_error(
                &headers,
                LibraryServiceError::InvalidLibraryId(error.to_string()),
            );
        }
    };
    let Some(libraries) = state.libraries.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if let Some(scan_jobs) = state.scan_jobs.as_ref()
        && let Err(error) = scan_jobs.prepare_library_deletion(library_id).await
    {
        match error {
            ScanJobError::Storage(error) => {
                return library_error(&headers, LibraryServiceError::Storage(error));
            }
            error => {
                tracing::error!(library_id = %library_id, %error, "failed to prepare library deletion");
                return api_error(
                    &headers,
                    StatusCode::SERVICE_UNAVAILABLE,
                    lux::ApiErrorCode::DatabaseUnavailable,
                    "媒体库删除准备失败",
                )
                .into_response();
            }
        }
    }
    match libraries.delete_library(library_id).await {
        Ok(()) => {
            if let Some(plugins) = state.plugins.as_ref()
                && let Err(error) = plugins.prune_media_info_library_ids().await
            {
                tracing::error!(
                    library_id = %library_id,
                    %error,
                    "library deleted but STRM media-info plugin configuration could not be pruned"
                );
            }
            if let Some(plugins) = state.plugins.as_ref()
                && let Err(error) = plugins.prune_danmaku_library_ids().await
            {
                tracing::error!(
                    library_id = %library_id,
                    %error,
                    "library deleted but danmaku plugin configuration could not be pruned"
                );
            }
            if let Some(home) = state.home.as_ref() {
                home.invalidate();
            }
            let target_id = library_id.to_string();
            record_audit_event(
                &state,
                &headers,
                "LIBRARY_DELETED",
                Some("library"),
                Some(&target_id),
                "{}",
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => library_error(&headers, error),
    }
}

pub(crate) async fn admin_add_library_root(
    headers: HeaderMap,
    Path(library_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<AddLibraryRootRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let library_id = match library_id.parse::<crate::domain::ids::LibraryId>() {
        Ok(id) => id,
        Err(error) => {
            return library_error(
                &headers,
                LibraryServiceError::InvalidLibraryId(error.to_string()),
            );
        }
    };
    let Some(libraries) = state.libraries.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    match libraries.add_root(library_id, &request.path).await {
        Ok(result) => {
            if let Some(home) = state.home.as_ref() {
                home.invalidate();
            }
            let target_id = library_id.to_string();
            record_audit_event(
                &state,
                &headers,
                "LIBRARY_ROOT_ADDED",
                Some("library"),
                Some(&target_id),
                "{}",
            )
            .await;
            let scan_job = match spawn_library_scan(&state, library_id).await {
                Ok(job) => job,
                Err(error) => {
                    tracing::warn!(library_id = %target_id, %error, "library root added but automatic scan could not be started");
                    None
                }
            };
            (
                StatusCode::CREATED,
                Json(json!({
                    "root": root_json(&result.root),
                    "warnings": result.warnings.iter().map(|warning| warning.as_str()).collect::<Vec<_>>(),
                    "scanJob": scan_job.as_ref().map(scan_job_json),
                })),
            )
                .into_response()
        }
        Err(error) => library_error(&headers, error),
    }
}

pub(crate) fn library_cover_error(headers: &HeaderMap, error: LibraryCoverError) -> Response {
    match error {
        LibraryCoverError::UnsupportedContentType(_) | LibraryCoverError::InvalidContent { .. } => {
            api_error(
                headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "封面图格式无效，仅支持 JPEG、PNG 或 WebP",
            )
            .into_response()
        }
        LibraryCoverError::TooLarge { .. } => api_error(
            headers,
            StatusCode::PAYLOAD_TOO_LARGE,
            lux::ApiErrorCode::InvalidRequest,
            "封面图不能超过 5 MiB",
        )
        .into_response(),
        LibraryCoverError::LibraryNotFound => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "媒体库不存在",
        )
        .into_response(),
        LibraryCoverError::TaskNotRegistered | LibraryCoverError::JobNotFound => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "自动媒体库封面任务不存在",
        )
        .into_response(),
        LibraryCoverError::AlreadyActive => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "自动媒体库封面任务已有运行中的作业",
        )
        .into_response(),
        LibraryCoverError::InvalidPath => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "媒体库封面路径无效",
        )
        .into_response(),
        LibraryCoverError::Io { .. }
        | LibraryCoverError::ImageWrite(_)
        | LibraryCoverError::Storage(_)
        | LibraryCoverError::FontNotFound
        | LibraryCoverError::Render(_)
        | LibraryCoverError::RenderPanicked
        | LibraryCoverError::GeneratedCoverRace
        | LibraryCoverError::GenerationUnavailable => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "媒体库封面保存失败",
        )
        .into_response(),
    }
}

pub(crate) fn library_error(headers: &HeaderMap, error: LibraryServiceError) -> Response {
    let (status, code, message) = match error {
        LibraryServiceError::InvalidName
        | LibraryServiceError::InvalidSchedule
        | LibraryServiceError::InvalidConcurrency
        | LibraryServiceError::InvalidLibraryId(_)
        | LibraryServiceError::InvalidRootId(_)
        | LibraryServiceError::InvalidKind(_)
        | LibraryServiceError::InvalidScraperId
        | LibraryServiceError::InvalidScraperRole(_)
        | LibraryServiceError::InvalidScraperOrder(_)
        | LibraryServiceError::InvalidChapterSourceId
        | LibraryServiceError::InvalidLibraryOrder(_) => (
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体库请求无效",
        ),
        LibraryServiceError::LibraryNotFound => (
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "媒体库不存在",
        ),
        LibraryServiceError::RootNotFound => (
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "媒体根路径不存在",
        ),
        LibraryServiceError::DuplicateRoot => (
            StatusCode::CONFLICT,
            lux::ApiErrorCode::LibraryRootDuplicate,
            "根路径已存在",
        ),
        LibraryServiceError::OverlappingRoot => (
            StatusCode::UNPROCESSABLE_ENTITY,
            lux::ApiErrorCode::LibraryRootOverlap,
            "根路径与同一媒体库的其他路径重叠",
        ),
        LibraryServiceError::Path(error) => (
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::LibraryPathUnavailable,
            if error.is_unavailable() {
                "媒体目录不可用"
            } else {
                "媒体目录无效"
            },
        ),
        LibraryServiceError::RootNotFoundAfterInsert => (
            StatusCode::INTERNAL_SERVER_ERROR,
            lux::ApiErrorCode::Internal,
            "媒体根路径保存失败",
        ),
        LibraryServiceError::Storage(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "数据库暂时不可用",
        ),
    };
    api_error(headers, status, code, message).into_response()
}

pub(crate) fn plugin_error(headers: &HeaderMap, error: PluginServiceError) -> Response {
    match error {
        PluginServiceError::UnknownPlugin(_) => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "插件不存在",
        )
        .into_response(),
        PluginServiceError::Unavailable(_) => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::PluginUnavailable,
            "插件尚未安装或配置完成",
        )
        .into_response(),
        PluginServiceError::InvalidConfig => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "插件配置无效",
        )
        .into_response(),
        PluginServiceError::NoUpdate => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::PluginNoUpdate,
            "插件已经是最新版本",
        )
        .into_response(),
        PluginServiceError::ConfigIo(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "插件配置保存失败",
        )
        .into_response(),
        PluginServiceError::Runtime(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::PluginUnavailable,
            "插件进程暂时不可用",
        )
        .into_response(),
        PluginServiceError::Store(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::PluginUnavailable,
            "插件商店暂时不可用",
        )
        .into_response(),
        PluginServiceError::InvalidResponse => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::PluginUnavailable,
            "插件返回的数据无效",
        )
        .into_response(),
        PluginServiceError::Storage(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "数据库暂时不可用",
        )
        .into_response(),
    }
}

pub(crate) fn danmaku_service_error(headers: &HeaderMap, error: DanmakuServiceError) -> Response {
    match error {
        DanmakuServiceError::InvalidConcurrency | DanmakuServiceError::ProviderNotConfigured => {
            api_error(
                headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "弹幕匹配配置无效或尚未配置",
            )
            .into_response()
        }
        DanmakuServiceError::AlreadyActive => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "已有弹幕匹配任务运行",
        )
        .into_response(),
        DanmakuServiceError::LibraryNotFound
        | DanmakuServiceError::SourceNotFound
        | DanmakuServiceError::JobNotFound => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "弹幕匹配对象不存在",
        )
        .into_response(),
        DanmakuServiceError::LibraryNotSelected => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "该媒体库未在弹幕插件配置中启用",
        )
        .into_response(),
        DanmakuServiceError::NotRetryable => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "弹幕匹配任务当前不可重试",
        )
        .into_response(),
        DanmakuServiceError::WorkerFailed | DanmakuServiceError::Storage(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "弹幕匹配服务暂时不可用",
        )
        .into_response(),
    }
}

pub(crate) fn strm_probe_error(headers: &HeaderMap, error: StrmProbeError) -> Response {
    match error {
        StrmProbeError::InvalidLibraryCount
        | StrmProbeError::InvalidConcurrency
        | StrmProbeError::InvalidThumbnailPosition => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "STRM 探测参数无效",
        )
        .into_response(),
        StrmProbeError::AlreadyActive => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "已有 STRM 探测任务运行",
        )
        .into_response(),
        StrmProbeError::NotRetryable => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "任务当前不可重试",
        )
        .into_response(),
        StrmProbeError::LibraryNotFound | StrmProbeError::JobNotFound => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "STRM 探测对象不存在",
        )
        .into_response(),
        StrmProbeError::WorkerFailed | StrmProbeError::Storage(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "STRM 探测服务暂时不可用",
        )
        .into_response(),
        StrmProbeError::Plugin(PluginServiceError::InvalidConfig) => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "STRM 插件配置无效",
        )
        .into_response(),
        StrmProbeError::Plugin(error) => plugin_error(headers, error),
    }
}

pub(crate) fn plugin_page_json(page: &PluginPage) -> Value {
    json!({
        "plugins": page.plugins.iter().map(plugin_json).collect::<Vec<_>>(),
        "total": page.total,
        "page": page.offset / page.limit + 1,
        "pageSize": page.limit,
    })
}

pub(crate) async fn admin_run_plugin(
    headers: HeaderMap,
    Path(plugin_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    if plugin_id != crate::application::plugins::MEDIA_INFO_PLUGIN_ID {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "该插件不支持后台运行",
        )
        .into_response();
    }
    let Some(service) = state.strm_probe.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let jobs = match service.create_configured_jobs().await {
        Ok(jobs) => jobs,
        Err(error) => return strm_probe_error(&headers, error),
    };
    for job in &jobs {
        let worker = service.clone();
        let job_id = job.id.clone();
        tokio::spawn(async move {
            if let Err(error) = worker.run(&job_id).await {
                tracing::error!(job_id = %job_id, %error, "configured STRM probe job stopped");
            }
        });
    }
    let operation_id = jobs
        .first()
        .map(|job| job.operation_id.clone())
        .unwrap_or_default();
    record_audit_event(
        &state,
        &headers,
        "STRM_PROBE_STARTED",
        Some("strm_probe_operation"),
        Some(&operation_id),
        &format!(r#"{{"jobCount":{}}}"#, jobs.len()),
    )
    .await;
    (
        StatusCode::ACCEPTED,
        Json(json!({
            "operationId": operation_id,
            "jobs": jobs,
        })),
    )
        .into_response()
}

pub(crate) fn plugin_json(plugin: &crate::application::plugins::PluginView) -> Value {
    json!({
        "id": plugin.id,
        "name": plugin.name,
        "description": plugin.description,
        "category": plugin.category,
        "version": plugin.version,
        "runtime": plugin.runtime,
        "providerKey": plugin.provider_key,
        "capabilities": plugin.capabilities,
        "status": plugin.status,
        "running": plugin.running,
        "lastError": plugin.last_error,
        "installed": plugin.installed,
        "enabled": plugin.enabled,
        "configured": plugin.configured,
        "available": plugin.available,
        "unavailableReason": plugin.unavailable_reason,
        "configurable": plugin.configurable,
        "configFields": plugin.config_fields.iter().map(|field| json!({
            "key": field.key,
            "label": field.label,
            "type": field.input_type,
            "required": field.required,
            "sensitive": field.sensitive,
            "description": field.description,
            "multiple": field.multiple,
            "optionsSource": field.options_source,
            "defaultValue": field.default_value,
            "minimum": field.minimum,
            "maximum": field.maximum,
            "options": field.options.iter().map(|option| json!({
                "value": option.value,
                "label": option.label,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
        "configValues": plugin.config_values,
        "configSource": plugin.config_source,
        "latestVersion": plugin.latest_version,
        "updateAvailable": plugin.update_available,
    })
}

pub(crate) fn library_json(library: &LibraryRecord, roots: &[LibraryRootRecord]) -> Value {
    json!({
        "id": library.id.to_string(),
        "name": library.name,
        "kind": library.kind.as_str(),
        "scraperId": library.scraper_id,
        "scrapers": library.scrapers.iter().map(|scraper| json!({
            "scraperId": scraper.scraper_id,
            "position": scraper.position,
            "role": scraper.role.as_str(),
        })).collect::<Vec<_>>(),
        "chapterSourceId": library.chapter_source_id,
        "coverImageUrl": library_cover_url(library),
        "isEnabled": library.is_enabled,
        "realtimeWatchEnabled": library.realtime_watch_enabled,
        "realtimeMetadataAutoMatchEnabled": library.realtime_metadata_auto_match_enabled,
        "incrementalSchedule": library.incremental_schedule,
        "reconciliationSchedule": library.reconciliation_schedule,
        "metadataSchedule": library.metadata_schedule,
        "mediaStrategy": library_media_strategy_json(library.media_strategy_json.as_deref()),
        "scanConcurrency": library.scan_concurrency,
        "probeConcurrency": library.probe_concurrency,
        "lastScanAt": library.last_scan_at,
        "roots": roots.iter().map(root_json).collect::<Vec<_>>(),
    })
}

pub(crate) fn library_media_strategy_json(value: Option<&str>) -> Option<Value> {
    let strategy = serde_json::from_str::<MediaStrategySettings>(value?).ok()?;
    serde_json::to_value(strategy).ok()
}

pub(crate) fn library_cover_url(library: &LibraryRecord) -> Option<String> {
    library.cover_image_path.as_ref().map(|_| {
        library_cover_url_for_tag(&library.id.to_string(), library.cover_image_tag.as_deref())
    })
}

pub(crate) fn library_cover_url_for_tag(library_id: &str, tag: Option<&str>) -> String {
    let base = format!("/api/v1/libraries/{library_id}/cover");
    tag.map(str::trim)
        .filter(|tag| !tag.is_empty())
        .map_or(base.clone(), |tag| format!("{base}?v={tag}"))
}

pub(crate) fn root_json(root: &LibraryRootRecord) -> Value {
    json!({
        "id": root.id.to_string(),
        "libraryId": root.library_id.to_string(),
        "canonicalPath": root.canonical_path,
        "displayPath": root.display_path,
        "isAvailable": root.is_available,
        "isWritable": root.is_writable,
        "lastCheckedAt": root.last_checked_at,
        "unavailableSince": root.unavailable_since,
        "scanCursor": root.scan_cursor,
    })
}

pub(crate) fn user_json(user: &UserRecord) -> Value {
    json!({
        "id": user.id.to_string(),
        "usernameNormalized": user.username_normalized,
        "displayName": user.display_name,
        "isDisabled": user.is_disabled,
        "isAdmin": user.is_admin,
        "canManageServer": user.can_manage_server,
        "canRemoteAccess": user.can_remote_access,
        "canDownload": user.can_download
    })
}

pub(crate) fn api_error(
    headers: &HeaderMap,
    status: StatusCode,
    code: lux::ApiErrorCode,
    message: &str,
) -> (StatusCode, Json<Value>) {
    let request_id = headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown");
    tracing::Span::current().record("errorCode", code.as_str());
    let body = lux::ApiError::new(code, message, request_id);
    (
        status,
        Json(json!({
            "error": {
                "code": body.code,
                "message": body.message,
                "requestId": body.request_id
            }
        })),
    )
}

pub(crate) fn user_avatar_error(headers: &HeaderMap, error: UserAvatarError) -> Response {
    match error {
        UserAvatarError::UnsupportedContentType | UserAvatarError::InvalidContent => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "头像格式无效，仅支持 JPEG、PNG 或 WebP",
        )
        .into_response(),
        UserAvatarError::TooLarge { .. } => api_error(
            headers,
            StatusCode::PAYLOAD_TOO_LARGE,
            lux::ApiErrorCode::InvalidRequest,
            "头像不能超过 5 MiB",
        )
        .into_response(),
        UserAvatarError::InvalidPath(_) | UserAvatarError::Io { .. } => api_error(
            headers,
            StatusCode::INTERNAL_SERVER_ERROR,
            lux::ApiErrorCode::Internal,
            "头像暂时无法保存",
        )
        .into_response(),
    }
}

pub(crate) fn request_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value.split(';').find_map(|part| {
                let (cookie_name, cookie_value) = part.trim().split_once('=')?;
                (cookie_name == name).then(|| cookie_value.to_owned())
            })
        })
}

pub(crate) fn secure_cookie_for_request(headers: &HeaderMap, policy: &RemoteAccessPolicy) -> bool {
    policy.is_secure_request(
        header_str(headers, "x-lux-peer-ip"),
        header_str(headers, "x-forwarded-proto"),
    )
}

pub(crate) fn build_cookie(
    name: &str,
    value: &str,
    http_only: bool,
    max_age: Option<i64>,
    secure: bool,
) -> Option<HeaderValue> {
    let mut cookie = format!("{name}={value}; Path=/;");
    if secure {
        cookie.push_str(" Secure;");
    }
    cookie.push_str(" SameSite=Lax");
    if http_only {
        cookie.push_str("; HttpOnly");
    }
    if let Some(max_age) = max_age {
        cookie.push_str(&format!("; Max-Age={max_age}"));
    }
    HeaderValue::from_str(&cookie).ok()
}
