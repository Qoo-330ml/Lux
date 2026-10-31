use super::*;

pub(super) fn api_routes() -> Router<ReadyAppState> {
    Router::new().route(
        "/api/v1/admin/performance-metrics",
        get(admin_performance_metrics),
    )
}

/// Read-only in-process request metrics grouped by route template.
///
/// The snapshot never includes request paths with identifiers, query strings, headers, user
/// details, or media names; see `observability::request_metrics`.
pub(crate) async fn admin_performance_metrics(
    headers: HeaderMap,
    State(state): State<ReadyAppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let mut response = Json(state.request_metrics.snapshot()).into_response();
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("no-store, max-age=0"),
    );
    response
}
