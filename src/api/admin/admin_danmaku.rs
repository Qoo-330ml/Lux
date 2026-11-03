//! Administrator manual danmaku match (ADR-052 P5).
//!
//! Three endpoints, all behind the administrator Web session: search candidates through the
//! plugin, queue one persistent single-item match job, and remove a registered sidecar. The
//! handlers only parse, validate, call [`DanmakuService`] and map DTOs; every rule (query bounds,
//! identifier bounds, media-root checks, provider/confirmation rules) lives in
//! `application::danmaku::manual`.

use super::*;

use crate::application::plugin_protocol::DanmakuSearchCandidate;

pub(super) fn api_routes() -> Router<ReadyAppState> {
    Router::new()
        .route(
            "/api/v1/admin/items/{item_id}/danmaku/search",
            get(admin_search_item_danmaku),
        )
        .route(
            "/api/v1/admin/items/{item_id}/danmaku/match",
            post(admin_start_manual_danmaku_match),
        )
        .route(
            "/api/v1/admin/items/{item_id}/danmaku",
            delete(admin_delete_item_danmaku),
        )
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ManualDanmakuSearchQuery {
    pub(crate) query: Option<String>,
    pub(crate) source_id: Option<String>,
    pub(crate) limit: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ManualDanmakuMatchRequest {
    #[serde(default)]
    pub(crate) source_id: Option<String>,
    #[serde(default)]
    pub(crate) provider: Option<String>,
    #[serde(default)]
    pub(crate) anime_id: Option<String>,
    pub(crate) episode_id: String,
    #[serde(default)]
    pub(crate) overwrite: bool,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ManualDanmakuDeleteQuery {
    pub(crate) source_id: Option<String>,
    pub(crate) confirm_user_sidecar: Option<bool>,
}

// Response of `GET /api/v1/admin/items/{item_id}/danmaku/search`. A candidate mirrors the plugin
// contract, so an administrator picks exactly the values the manual match then submits.
#[derive(Serialize)]
#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "ts-bindings",
    ts(export, export_to = "web/src/lib/api/generated/")
)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ManualDanmakuSearchResponse {
    pub(crate) items: Vec<ManualDanmakuCandidate>,
    // Always `true` on a 2xx answer: an unsupported plugin is a `409`, never a `200` with no items.
    pub(crate) supported: bool,
}

#[derive(Serialize)]
#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "ts-bindings",
    ts(export, export_to = "web/src/lib/api/generated/")
)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ManualDanmakuCandidate {
    pub(crate) anime_id: String,
    pub(crate) anime_title: String,
    pub(crate) episodes: Vec<ManualDanmakuEpisode>,
    pub(crate) provider: Option<String>,
}

#[derive(Serialize)]
#[cfg_attr(feature = "ts-bindings", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "ts-bindings",
    ts(export, export_to = "web/src/lib/api/generated/")
)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ManualDanmakuEpisode {
    pub(crate) episode_id: String,
    pub(crate) title: String,
}

fn manual_candidate_json(candidate: DanmakuSearchCandidate) -> ManualDanmakuCandidate {
    ManualDanmakuCandidate {
        anime_id: candidate.anime_id,
        anime_title: candidate.anime_title,
        episodes: candidate
            .episodes
            .into_iter()
            .map(|episode| ManualDanmakuEpisode {
                episode_id: episode.episode_id,
                title: episode.title,
            })
            .collect(),
        provider: candidate.provider,
    }
}

/// Key that scopes the per-session search budget.
///
/// The raw session material only ever reaches the limiter, which hashes it before storing and never
/// writes it to a log.
fn manual_search_rate_key(headers: &HeaderMap) -> String {
    if let Some(session) = request_cookie(headers, "lux_session") {
        return format!("session:{session}");
    }
    if let Some(api_key) = lux_api_key_from_headers(headers) {
        return format!("api-key:{api_key}");
    }
    header_str(headers, "x-lux-peer-ip")
        .map(str::to_owned)
        .unwrap_or_else(|| "admin".to_owned())
}

pub(crate) async fn admin_search_item_danmaku(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<ManualDanmakuSearchQuery>,
    State(state): State<ReadyAppState>,
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
    // Every search reaches an upstream provider, so the administrator's own session is budgeted.
    if !state
        .danmaku_search_rate_limiter
        .allow(&manual_search_rate_key(&headers))
        .await
    {
        let mut response = api_error(
            &headers,
            StatusCode::TOO_MANY_REQUESTS,
            lux::ApiErrorCode::RateLimited,
            "弹幕搜索过于频繁，请稍后重试",
        )
        .into_response();
        response
            .headers_mut()
            .insert("retry-after", HeaderValue::from_static("1"));
        return response;
    }
    let limit = match query.limit {
        Some(limit) => match u32::try_from(limit) {
            Ok(limit) => Some(limit),
            Err(_) => {
                return api_error(
                    &headers,
                    StatusCode::BAD_REQUEST,
                    lux::ApiErrorCode::InvalidRequest,
                    "弹幕搜索分页参数无效",
                )
                .into_response();
            }
        },
        None => None,
    };
    let service = state.danmaku();
    match service
        .search_for_item(
            &item_id,
            query.query.as_deref(),
            query.source_id.as_deref(),
            limit,
        )
        .await
    {
        Ok(candidates) => Json(ManualDanmakuSearchResponse {
            items: candidates
                .into_iter()
                .map(manual_candidate_json)
                .collect::<Vec<_>>(),
            supported: true,
        })
        .into_response(),
        Err(error) => super::admin_jobs::danmaku_service_error(&headers, error),
    }
}

pub(crate) async fn admin_start_manual_danmaku_match(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<ReadyAppState>,
    Json(request): Json<ManualDanmakuMatchRequest>,
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
    let target = crate::application::danmaku::ManualMatchTarget {
        source_id: request.source_id,
        provider: request.provider,
        anime_id: request.anime_id,
        episode_id: request.episode_id,
        overwrite: request.overwrite,
    };
    let service = state.danmaku();
    let started = match crate::application::admin_job_workers::start_manual_danmaku_match_job(
        service, &item_id, target,
    )
    .await
    {
        Ok(started) => started,
        Err(error) => return super::admin_jobs::danmaku_service_error(&headers, error),
    };
    record_audit_event(
        &state,
        &headers,
        "DANMAKU_MANUAL_MATCH_STARTED",
        Some("danmaku_match_job"),
        Some(&started.job.id),
        &json!({
            "itemId": item_id,
            // The resolved source, not the (possibly omitted) request field.
            "sourceId": started.source_id,
            "overwrite": started.job.overwrite,
        })
        .to_string(),
    )
    .await;
    (StatusCode::ACCEPTED, Json(json!({ "job": started.job }))).into_response()
}

pub(crate) async fn admin_delete_item_danmaku(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<ManualDanmakuDeleteQuery>,
    State(state): State<ReadyAppState>,
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
    let service = state.danmaku();
    match service
        .delete_for_source(
            &item_id,
            query.source_id.as_deref(),
            query.confirm_user_sidecar.unwrap_or(false),
        )
        .await
    {
        Ok(source_id) => {
            // Audit the source the delete actually resolved (the item's default source when the
            // request omitted `sourceId`), never a `null` request parameter.
            record_audit_event(
                &state,
                &headers,
                "DANMAKU_SIDECAR_DELETED",
                Some("danmaku_track"),
                Some(&source_id),
                &json!({
                    "itemId": item_id,
                    "sourceId": source_id,
                    "confirmUserSidecar": query.confirm_user_sidecar.unwrap_or(false),
                })
                .to_string(),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => super::admin_jobs::danmaku_service_error(&headers, error),
    }
}
