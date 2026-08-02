pub mod lux;

use axum::{Json, Router, routing::get};
use serde_json::{Value, json};
use tower_http::{ServiceBuilderExt, request_id::MakeRequestUuid, trace::TraceLayer};

pub fn app() -> Router {
    Router::new().route("/health/live", get(live)).layer(
        tower::ServiceBuilder::new()
            .set_x_request_id(MakeRequestUuid)
            .layer(TraceLayer::new_for_http())
            .propagate_x_request_id(),
    )
}

async fn live() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}
