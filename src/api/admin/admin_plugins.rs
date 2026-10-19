use super::*;

pub(super) fn api_routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/admin/plugins", get(admin_list_plugins))
        .route(
            "/api/v1/admin/notification-providers",
            get(admin_list_notification_providers),
        )
        .route(
            "/api/v1/admin/chapter-sources",
            get(admin_list_chapter_sources),
        )
        .route(
            "/api/v1/admin/plugins/installed",
            get(admin_list_installed_plugins),
        )
        .route(
            "/api/v1/admin/plugins/{plugin_id}/install",
            post(admin_install_plugin),
        )
        .route(
            "/api/v1/admin/plugins/{plugin_id}/update",
            post(admin_update_plugin),
        )
        .route(
            "/api/v1/admin/plugins/{plugin_id}",
            delete(admin_uninstall_plugin),
        )
        .route(
            "/api/v1/admin/plugins/{plugin_id}/enabled",
            patch(admin_update_plugin_enabled),
        )
        .route(
            "/api/v1/admin/plugins/{plugin_id}/config",
            put(admin_update_plugin_config),
        )
        .route(
            "/api/v1/admin/plugins/{plugin_id}/config/image/{field_key}",
            put(admin_upload_plugin_config_image).layer(DefaultBodyLimit::max(
                crate::application::login_background_assets::MAX_LOGIN_BACKGROUND_ASSET_BYTES,
            )),
        )
        .route(
            "/api/v1/admin/plugins/{plugin_id}/run",
            post(admin_run_plugin),
        )
        .route(
            "/api/v1/admin/plugin-store",
            get(admin_plugin_store).put(admin_update_plugin_store),
        )
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
    if is_strm_media_info_plugin(&plugin_id)
        && let Some(service) = state.strm_probe.as_ref()
        && let Err(error) = service.cancel_active_jobs().await
    {
        tracing::error!(%error, "STRM probe jobs could not be cancelled before plugin uninstall");
    }
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
    if is_strm_media_info_plugin(&plugin_id)
        && !request.enabled
        && let Some(service) = state.strm_probe.as_ref()
        && let Err(error) = service.cancel_active_jobs().await
    {
        tracing::error!(%error, "STRM probe jobs could not be cancelled before plugin disable");
    }
    match plugins.set_enabled(&plugin_id, request.enabled).await {
        Ok(plugin) => {
            if is_strm_media_info_plugin(&plugin_id)
                && let Some(service) = state.strm_probe.as_ref()
            {
                if request.enabled {
                    service.schedule_configured_jobs_when_available();
                }
            }
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
    let is_strm_media_info = is_strm_media_info_plugin(&plugin_id);
    let previous_library_ids = if is_strm_media_info {
        configured_media_info_library_ids(plugins).await
    } else {
        None
    };
    let result = plugins
        .update_dynamic_config(&plugin_id, request.values)
        .await;
    match result {
        Ok(plugin) => {
            let selection_changed = is_strm_media_info
                && previous_library_ids != configured_media_info_library_ids(plugins).await;
            if selection_changed
                && let Some(service) = state.strm_probe.as_ref()
                && let Err(error) = service.cancel_active_jobs().await
            {
                tracing::error!(%error, "STRM probe jobs could not be cancelled after library selection changed");
            }
            plugins.restart(&plugin_id).await;
            if selection_changed
                && plugins
                    .enabled_media_info_settings()
                    .await
                    .is_ok_and(|settings| settings.is_some())
                && let Some(service) = state.strm_probe.as_ref()
            {
                service.schedule_configured_jobs_when_available();
            }
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

fn is_strm_media_info_plugin(plugin_id: &str) -> bool {
    matches!(
        plugin_id,
        crate::application::plugins::MEDIA_INFO_PLUGIN_ID | "org.lux.media-info"
    )
}

async fn configured_media_info_library_ids(plugins: &PluginService) -> Option<Vec<String>> {
    let mut library_ids = plugins
        .media_info_settings()
        .await
        .ok()?
        .library_ids
        .into_iter()
        .map(|library_id| library_id.to_string())
        .collect::<Vec<_>>();
    library_ids.sort();
    Some(library_ids)
}

pub(crate) async fn admin_upload_plugin_config_image(
    headers: HeaderMap,
    Path((plugin_id, field_key)): Path<(String, String)>,
    State(state): State<AppState>,
    body: Bytes,
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
    match plugins
        .upload_login_background_image(&plugin_id, &field_key, &body)
        .await
    {
        Ok(asset) => {
            record_audit_event(
                &state,
                &headers,
                "PLUGIN_CONFIG_UPDATED",
                Some("plugin"),
                Some(&plugin_id),
                "{\"kind\":\"image_upload\"}",
            )
            .await;
            (
                StatusCode::OK,
                Json(json!({
                    "assetId": asset.asset_id,
                    "contentType": asset.content_type,
                    "configured": true
                })),
            )
                .into_response()
        }
        Err(error) => plugin_error(&headers, error),
    }
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
        PluginServiceError::ImageAsset(error) => match error {
            crate::application::login_background_assets::LoginBackgroundAssetError::TooLarge => {
                api_error(
                    headers,
                    StatusCode::PAYLOAD_TOO_LARGE,
                    lux::ApiErrorCode::InvalidRequest,
                    "图片超过 5 MiB 大小限制",
                )
                .into_response()
            }
            crate::application::login_background_assets::LoginBackgroundAssetError::InvalidContent
            | crate::application::login_background_assets::LoginBackgroundAssetError::DimensionsTooLarge
            | crate::application::login_background_assets::LoginBackgroundAssetError::InvalidAssetId => {
                api_error(
                    headers,
                    StatusCode::BAD_REQUEST,
                    lux::ApiErrorCode::InvalidRequest,
                    "图片格式或尺寸无效",
                )
                .into_response()
            }
            crate::application::login_background_assets::LoginBackgroundAssetError::InvalidPath
            | crate::application::login_background_assets::LoginBackgroundAssetError::Io(_) => api_error(
                headers,
                StatusCode::INTERNAL_SERVER_ERROR,
                lux::ApiErrorCode::Internal,
                "图片保存失败",
            )
            .into_response(),
        },
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
    let jobs = match crate::application::admin_job_workers::start_configured_strm_probe_jobs(
        service,
    )
    .await
    {
        Ok(jobs) => jobs,
        Err(error) => return strm_probe_error(&headers, error),
    };
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
