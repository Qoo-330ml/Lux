use super::*;

pub(super) fn api_routes() -> Router<ReadyAppState> {
    Router::new().route("/api/v1/admin/health", get(admin_health))
}

pub(crate) async fn admin_health(
    headers: HeaderMap,
    State(state): State<ReadyAppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    match admin_health_payload(&state).await {
        Ok(payload) => Json(payload).into_response(),
        Err(status) => status.into_response(),
    }
}

pub(crate) async fn admin_health_payload(state: &ReadyAppState) -> Result<Value, StatusCode> {
    let database = state.database();
    let report = admin_health_report(
        database,
        state.config_dir.as_deref(),
        Some(state.libraries()),
        &state.resources,
        &state.admin_health_probe_cache,
    )
    .await
    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    serde_json::to_value(report).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)
}
