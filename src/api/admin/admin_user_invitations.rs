use super::*;

use std::time::{SystemTime, UNIX_EPOCH};

use crate::{
    application::user_invitations::{
        CreateUserInvitationInput, UserInvitationError, create_user_invitation,
        list_user_invitations, queue_user_invitation_resend, revoke_user_invitation,
    },
    storage::UserInvitationStatus,
};

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AdminUserInvitationListQuery {
    page: Option<i64>,
    page_size: Option<i64>,
    status: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateAdminUserInvitationRequest {
    recipient_email: String,
    preset_id: String,
}

pub(super) fn api_routes() -> Router<ReadyAppState> {
    Router::new()
        .route(
            "/api/v1/admin/user-invitations",
            get(admin_list_user_invitations).post(admin_create_user_invitation),
        )
        .route(
            "/api/v1/admin/user-invitations/{invitation_id}/revoke",
            post(admin_revoke_user_invitation),
        )
        .route(
            "/api/v1/admin/user-invitations/{invitation_id}/resend",
            post(admin_resend_user_invitation),
        )
}

async fn admin_list_user_invitations(
    headers: HeaderMap,
    Query(query): Query<AdminUserInvitationListQuery>,
    State(state): State<ReadyAppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let database = state.database();
    let status = match query.status.as_deref() {
        None | Some("all") => None,
        Some("pending") => Some(UserInvitationStatus::Pending),
        Some("accepted") => Some(UserInvitationStatus::Accepted),
        Some("revoked") => Some(UserInvitationStatus::Revoked),
        Some("expired") => Some(UserInvitationStatus::Expired),
        Some(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "邀请状态筛选项无效",
            )
            .into_response();
        }
    };
    match list_user_invitations(
        database,
        status,
        query.page.unwrap_or(1),
        query.page_size.unwrap_or(20),
        current_unix_time(),
    )
    .await
    {
        Ok(page) => Json(json!({
            "invitations": page.entries.iter().map(invitation_json).collect::<Vec<_>>(),
            "page": page.page,
            "pageSize": page.page_size,
            "totalCount": page.total_count,
        }))
        .into_response(),
        Err(error) => invitation_error(&headers, error),
    }
}

async fn admin_create_user_invitation(
    headers: HeaderMap,
    State(state): State<ReadyAppState>,
    Json(request): Json<CreateAdminUserInvitationRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let database = state.database();
    let Some(config_dir) = state.config_dir.as_deref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let created_by_user_id = admin_invitation_creator(&headers, &state).await;
    match create_user_invitation(
        database,
        config_dir,
        CreateUserInvitationInput {
            recipient_email: &request.recipient_email,
            created_by_user_id: created_by_user_id.as_deref(),
            preset_id: &request.preset_id,
        },
        current_unix_time(),
    )
    .await
    {
        Ok(created) => {
            let invitation_id = created.invitation.id.clone();
            record_audit_event(
                &state,
                &headers,
                "USER_INVITATION_CREATED",
                Some("user_invitation"),
                Some(&invitation_id),
                "{}",
            )
            .await;
            (
                StatusCode::CREATED,
                Json(json!({
                    "invitation": invitation_json(&created.invitation),
                    "invitationUrl": created.invitation_url,
                    "emailQueued": true,
                })),
            )
                .into_response()
        }
        Err(error) => invitation_error(&headers, error),
    }
}

async fn admin_revoke_user_invitation(
    headers: HeaderMap,
    Path(invitation_id): Path<String>,
    State(state): State<ReadyAppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let database = state.database();
    match revoke_user_invitation(database, &invitation_id, current_unix_time()).await {
        Ok(()) => {
            record_audit_event(
                &state,
                &headers,
                "USER_INVITATION_REVOKED",
                Some("user_invitation"),
                Some(&invitation_id),
                "{}",
            )
            .await;
            Json(json!({ "revoked": true })).into_response()
        }
        Err(error) => invitation_error(&headers, error),
    }
}

async fn admin_resend_user_invitation(
    headers: HeaderMap,
    Path(invitation_id): Path<String>,
    State(state): State<ReadyAppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let database = state.database();
    let idempotency_key = headers
        .get("Idempotency-Key")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    match queue_user_invitation_resend(
        database,
        &invitation_id,
        idempotency_key,
        current_unix_time(),
    )
    .await
    {
        Ok(()) => {
            record_audit_event(
                &state,
                &headers,
                "USER_INVITATION_RESEND_QUEUED",
                Some("user_invitation"),
                Some(&invitation_id),
                "{}",
            )
            .await;
            (StatusCode::ACCEPTED, Json(json!({ "emailQueued": true }))).into_response()
        }
        Err(error) => invitation_error(&headers, error),
    }
}

async fn admin_invitation_creator(headers: &HeaderMap, state: &ReadyAppState) -> Option<String> {
    if lux_api_key_from_headers(headers).is_some() {
        return None;
    }
    let session_token = request_cookie(headers, "lux_session")?;
    state
        .auth()
        .resolve(&session_token)
        .await
        .ok()
        .flatten()
        .map(|session| session.user.id.to_string())
}

fn invitation_json(invitation: &crate::storage::StoredUserInvitation) -> Value {
    json!({
        "id": invitation.id,
        "recipientEmail": invitation.recipient_email,
        "createdByUserId": invitation.created_by_user_id,
        "acceptedUserId": invitation.accepted_user_id,
        "status": invitation_status_name(invitation.status),
        "presetId": invitation.preset_id,
        "presetName": invitation.preset_name,
        "presetVersion": invitation.preset_version,
        "expiresAt": invitation.expires_at,
        "acceptedAt": invitation.accepted_at,
        "revokedAt": invitation.revoked_at,
        "createdAt": invitation.created_at,
        "updatedAt": invitation.updated_at,
    })
}

fn invitation_status_name(status: UserInvitationStatus) -> &'static str {
    match status {
        UserInvitationStatus::Pending => "PENDING",
        UserInvitationStatus::Accepted => "ACCEPTED",
        UserInvitationStatus::Revoked => "REVOKED",
        UserInvitationStatus::Expired => "EXPIRED",
    }
}

fn invitation_error(headers: &HeaderMap, error: UserInvitationError) -> Response {
    match error {
        UserInvitationError::AlreadyPending | UserInvitationError::NotRevocable => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "邀请状态冲突",
        )
        .into_response(),
        UserInvitationError::InvalidEmail | UserInvitationError::InvalidInput => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "邀请请求无效",
        )
        .into_response(),
        UserInvitationError::IdempotencyConflict => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "幂等键已用于其他重发请求",
        )
        .into_response(),
        UserInvitationError::RateLimited => api_error(
            headers,
            StatusCode::TOO_MANY_REQUESTS,
            lux::ApiErrorCode::RateLimited,
            "邀请邮件发送过于频繁，请稍后重试",
        )
        .into_response(),
        UserInvitationError::InvalidPreset | UserInvitationError::NotFound => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "邀请或权限预设不存在",
        )
        .into_response(),
        UserInvitationError::SigningUnavailable
        | UserInvitationError::PresetUnavailable
        | UserInvitationError::Storage(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "邀请服务暂时不可用",
        )
        .into_response(),
    }
}

fn current_unix_time() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invitation_rate_limit_maps_to_http_429() {
        let headers = HeaderMap::new();
        assert_eq!(
            invitation_error(&headers, UserInvitationError::RateLimited).status(),
            StatusCode::TOO_MANY_REQUESTS
        );
    }
}
