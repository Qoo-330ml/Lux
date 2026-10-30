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
    // Publish with the canonical user id: the path segment is a public Emby id
    // and must not be used as the SSE routing key.
    let notification_user_id = principal.user_id.map(|user_id| user_id.to_string());
    match catalog.set_resume_hidden(principal, &item_id, hidden).await {
        Ok(user_state) => {
            // Hiding an item from resume changes only this user's continue-watching rail.
            if let Some(user_id) = notification_user_id.as_deref() {
                state.home().invalidate_user(user_id);
            }
            match notification_user_id {
                Some(user_id) => state.user_events.publish_home_coalesced_for(&user_id).await,
                // No user to attribute the change to; keep the old broadcast.
                None => state.user_events.publish_home_coalesced().await,
            }
            Json(emby_user_item_data_json(&item_id, user_state.as_ref())).into_response()
        }
        Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => {
            StatusCode::NOT_FOUND.into_response()
        }
        Err(CatalogError::Storage(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
