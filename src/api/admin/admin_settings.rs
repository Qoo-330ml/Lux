use super::*;

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
    let settings = match crate::application::settings::read_admin_settings(
        database,
        state.plugins.as_ref(),
        state.config_dir.as_deref(),
    )
    .await
    {
        Ok(settings) => settings,
        Err(()) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    Json(json!({
        "serverName": settings.server_name,
        "resumePlayedPercent": settings.resume_played_percent,
        "resumeMinTicks": settings.resume_min_ticks,
        "forceAdminLibraryOrder": settings.force_admin_library_order,
        "userDeletionGraceDays": settings.user_deletion_grace_days,
        "loginBackgroundSource": settings.login_background_source,
        "loginBackgroundSourceStatus": settings.login_background_source_status,
        "mediaStrategy": settings.media_strategy,
        "networkProxy": settings.network_proxy,
    }))
    .into_response()
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
    let (proxy_source, diagnostics) = match crate::application::settings::test_admin_network_proxy(
        state.config_dir.as_deref(),
        request.network_proxy_url,
    )
    .await
    {
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
    Json(network_proxy_diagnostics_response(
        proxy_source,
        diagnostics,
    ))
    .into_response()
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
    pub(crate) user_deletion_grace_days: Option<i64>,
    pub(crate) login_background_source: Option<String>,
    pub(crate) media_strategy: Option<MediaStrategySettings>,
    #[serde(flatten)]
    pub(crate) extra: BTreeMap<String, Value>,
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
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let network_proxy = match request.extra.get("networkProxyUrl") {
        None => crate::application::settings::NetworkProxyChange::Unchanged,
        Some(Value::Null) => crate::application::settings::NetworkProxyChange::Set(None),
        Some(Value::String(value)) => {
            crate::application::settings::NetworkProxyChange::Set(Some(value.clone()))
        }
        Some(_) => crate::application::settings::NetworkProxyChange::Invalid,
    };
    let settings = match crate::application::settings::update_admin_settings(
        database,
        state.home.as_ref(),
        state.plugins.as_ref(),
        state.config_dir.as_deref(),
        crate::application::settings::AdminSettingsUpdate {
            server_name: request.server_name,
            resume_played_percent: request.resume_played_percent,
            resume_min_ticks: request.resume_min_ticks,
            force_admin_library_order: request.force_admin_library_order,
            user_deletion_grace_days: request.user_deletion_grace_days,
            login_background_source: request.login_background_source,
            media_strategy: request.media_strategy,
            network_proxy,
        },
    )
    .await
    {
        Ok(settings) => settings,
        Err(crate::application::settings::AdminSettingsError::Unavailable) => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Err(crate::application::settings::AdminSettingsError::InvalidLoginBackgroundSource) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "登录页背景来源无效",
            )
            .into_response();
        }
        Err(crate::application::settings::AdminSettingsError::LoginBackgroundPluginUnavailable) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "登录页背景插件不可用",
            )
            .into_response();
        }
        Err(crate::application::settings::AdminSettingsError::InvalidServerName) => {
            return StatusCode::BAD_REQUEST.into_response();
        }
        Err(crate::application::settings::AdminSettingsError::InvalidGlobalSettings) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "全局媒体策略无效",
            )
            .into_response();
        }
        Err(crate::application::settings::AdminSettingsError::InvalidUserDeletionGraceDays) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "用户删除保留期限必须在 1 到 3650 天之间",
            )
            .into_response();
        }
        Err(crate::application::settings::AdminSettingsError::InvalidNetworkProxy) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "网络代理地址无效",
            )
            .into_response();
        }
        Err(crate::application::settings::AdminSettingsError::PluginServiceUnavailable) => {
            return api_error(
                &headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "服务尚未就绪",
            )
            .into_response();
        }
        Err(crate::application::settings::AdminSettingsError::Plugin(error)) => {
            return plugin_error(&headers, error);
        }
    };
    record_audit_event(
        &state,
        &headers,
        "SETTINGS_UPDATED",
        Some("settings"),
        None,
        &format!(r#"{{"resumePlayedPercent":{},"resumeMinTicks":{},"forceAdminLibraryOrder":{},"userDeletionGraceDays":{},"loginBackgroundSource":"{}"}}"#, settings.resume_played_percent, settings.resume_min_ticks, settings.force_admin_library_order, settings.user_deletion_grace_days, settings.login_background_source),
    )
    .await;
    Json(json!({
        "serverName": settings.server_name,
        "resumePlayedPercent": settings.resume_played_percent,
        "resumeMinTicks": settings.resume_min_ticks,
        "forceAdminLibraryOrder": settings.force_admin_library_order,
        "userDeletionGraceDays": settings.user_deletion_grace_days,
        "loginBackgroundSource": settings.login_background_source,
        "loginBackgroundSourceStatus": settings.login_background_source_status,
        "mediaStrategy": settings.media_strategy,
        "networkProxy": settings.network_proxy,
    }))
    .into_response()
}
