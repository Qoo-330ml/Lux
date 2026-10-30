use super::*;

pub(super) fn api_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/admin/notification-destinations",
            get(admin_list_webhook_destinations).post(admin_create_webhook_destination),
        )
        .route(
            "/api/v1/admin/notification-destinations/{destination_id}",
            get(admin_get_webhook_destination)
                .patch(admin_update_webhook_destination)
                .delete(admin_delete_webhook_destination),
        )
        .route(
            "/api/v1/admin/notification-destinations/{destination_id}/test",
            post(admin_test_webhook_destination),
        )
        .route(
            "/api/v1/admin/notification-destinations/{destination_id}/rotate-secret",
            post(admin_rotate_webhook_secret),
        )
        .route(
            "/api/v1/admin/notification-deliveries",
            get(admin_list_webhook_deliveries),
        )
        .route(
            "/api/v1/admin/notification-deliveries/{delivery_id}/retry",
            post(admin_retry_webhook_delivery),
        )
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
    let Some(service) = state.webhooks() else {
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
    let Some(service) = state.webhooks() else {
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
    let Some(service) = state.webhooks() else {
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
    let Some(service) = state.webhooks() else {
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
    let Some(service) = state.webhooks() else {
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
    let Some(service) = state.webhooks() else {
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
    let Some(service) = state.webhooks() else {
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
    let Some(service) = state.webhooks() else {
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
    let Some(service) = state.webhooks() else {
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
