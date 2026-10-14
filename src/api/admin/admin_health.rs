use super::*;

pub(super) fn api_routes() -> Router<AppState> {
    Router::new().route("/api/v1/admin/health", get(admin_health))
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

pub(crate) async fn admin_health_payload(state: &AppState) -> Result<Value, StatusCode> {
    let Some(database) = state.database.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let report = admin_health_report(
        database,
        state.config_dir.as_deref(),
        state.libraries.as_ref(),
        &state.resources,
        &state.admin_health_probe_cache,
    )
    .await
    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    serde_json::to_value(report).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)
}
