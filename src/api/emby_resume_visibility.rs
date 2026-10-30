use super::*;

#[derive(Deserialize)]
pub(crate) struct ResumeVisibilityQuery {
    #[serde(flatten)]
    auth: EmbyTokenQuery,
    #[serde(rename = "Hide")]
    hide: Option<String>,
}

#[derive(Deserialize)]
struct ResumeVisibilityBody {
    #[serde(rename = "Hide")]
    hide: bool,
}

pub(in crate::api::legacy) async fn emby_hide_from_resume(
    headers: HeaderMap,
    Path((user_id, item_id)): Path<(String, String)>,
    Query(query): Query<ResumeVisibilityQuery>,
    State(state): State<ReadyAppState>,
    body: Bytes,
) -> Response {
    let auth_principal =
        match require_emby_principal(&headers, &state, query.auth.api_key.as_deref()).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    let principal =
        match emby_access_principal_for_target(&state, &auth_principal, Some(&user_id)).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    let query_hide = match query.hide.as_deref().map(str::parse::<bool>).transpose() {
        Ok(value) => value,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    // Emby's documented query form and iOS Filmly's JSON body are both valid.
    let body_hide = if body.is_empty() {
        None
    } else {
        match serde_json::from_slice::<ResumeVisibilityBody>(&body) {
            Ok(body) => Some(body.hide),
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        }
    };
    let hidden = match (query_hide, body_hide) {
        (Some(query), Some(body)) if query != body => {
            return StatusCode::BAD_REQUEST.into_response();
        }
        (Some(hidden), _) | (_, Some(hidden)) => hidden,
        (None, None) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let catalog = state.catalog();
    let item_id = emby_internal_id(&item_id);
    match catalog.set_resume_hidden(principal, &item_id, hidden).await {
        Ok(user_state) => {
            state.user_events.publish_home_coalesced_for(&user_id).await;
            Json(emby_user_item_data_json(&item_id, user_state.as_ref())).into_response()
        }
        Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => {
            StatusCode::NOT_FOUND.into_response()
        }
        Err(CatalogError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
