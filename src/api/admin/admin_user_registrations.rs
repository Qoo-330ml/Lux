use super::*;

use std::time::{SystemTime, UNIX_EPOCH};

use crate::application::{
    settings::NewUserPermissions,
    user_registration::{
        UserRegistrationReviewError, approve_pending_user_registration,
        list_pending_user_registrations, reject_pending_user_registration,
    },
};

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AdminUserRegistrationListQuery {
    page: Option<i64>,
    page_size: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ApproveUserRegistrationRequest {
    permissions: NewUserPermissions,
}

pub(super) fn api_routes() -> Router<ReadyAppState> {
    Router::new()
        .route(
            "/api/v1/admin/user-registrations",
            get(admin_list_user_registrations),
        )
        .route(
            "/api/v1/admin/user-registrations/{request_id}/approve",
            post(admin_approve_user_registration),
        )
        .route(
            "/api/v1/admin/user-registrations/{request_id}/reject",
            post(admin_reject_user_registration),
        )
}

async fn admin_list_user_registrations(
    headers: HeaderMap,
    Query(query): Query<AdminUserRegistrationListQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let database = require_service!(state.database);
    match list_pending_user_registrations(
        database,
        query.page.unwrap_or(1),
        query.page_size.unwrap_or(20),
        current_unix_time(),
    )
    .await
    {
        Ok(page) => Json(json!({
            "requests": page.requests.into_iter().map(registration_request_json).collect::<Vec<_>>(),
            "page": page.page,
            "pageSize": page.page_size,
            "totalCount": page.total_count,
        }))
        .into_response(),
        Err(error) => registration_error(&headers, error),
    }
}

async fn admin_approve_user_registration(
    headers: HeaderMap,
    Path(request_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<ApproveUserRegistrationRequest>,
) -> Response {
    let reviewer_user_id = match registration_reviewer_user_id(&headers, &state).await {
        Ok(user_id) => user_id,
        Err(response) => return response,
    };
    let database = require_service!(state.database);
    match approve_pending_user_registration(
        database,
        &request_id,
        &reviewer_user_id,
        request.permissions,
        current_unix_time(),
    )
    .await
    {
        Ok(user_id) => {
            record_audit_event(
                &state,
                &headers,
                "USER_REGISTRATION_APPROVED",
                Some("user_registration_request"),
                Some(&request_id),
                "{}",
            )
            .await;
            Json(json!({ "approved": true, "userId": user_id })).into_response()
        }
        Err(error) => registration_error(&headers, error),
    }
}

async fn admin_reject_user_registration(
    headers: HeaderMap,
    Path(request_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let reviewer_user_id = match registration_reviewer_user_id(&headers, &state).await {
        Ok(user_id) => user_id,
        Err(response) => return response,
    };
    let database = require_service!(state.database);
    match reject_pending_user_registration(
        database,
        &request_id,
        &reviewer_user_id,
        current_unix_time(),
    )
    .await
    {
        Ok(()) => {
            record_audit_event(
                &state,
                &headers,
                "USER_REGISTRATION_REJECTED",
                Some("user_registration_request"),
                Some(&request_id),
                "{}",
            )
            .await;
            Json(json!({ "rejected": true })).into_response()
        }
        Err(error) => registration_error(&headers, error),
    }
}

async fn registration_reviewer_user_id(
    headers: &HeaderMap,
    state: &AppState,
) -> Result<String, Response> {
    if lux_api_key_from_headers(headers).is_some() {
        return Err(api_error(
            headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::PermissionDenied,
            "请使用管理员网页会话审核注册申请",
        )
        .into_response());
    }
    require_admin_web_session(headers, state, true).await?;
    let Some(session_token) = request_cookie(headers, "lux_session") else {
        return Err(api_error(
            headers,
            StatusCode::UNAUTHORIZED,
            lux::ApiErrorCode::AuthenticationRequired,
            "需要登录",
        )
        .into_response());
    };
    let Some(auth) = state.auth() else {
        return Err(api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response());
    };
    match auth.resolve(&session_token).await {
        Ok(Some(session)) if session.user.can_manage_server => Ok(session.user.id.to_string()),
        Ok(Some(_)) => Err(api_error(
            headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::PermissionDenied,
            "没有服务器管理权限",
        )
        .into_response()),
        Ok(None) => Err(api_error(
            headers,
            StatusCode::UNAUTHORIZED,
            lux::ApiErrorCode::AuthenticationRequired,
            "需要登录",
        )
        .into_response()),
        Err(_) => Err(api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "认证暂时不可用",
        )
        .into_response()),
    }
}

fn registration_request_json(
    request: crate::application::user_registration::UserRegistrationReviewItem,
) -> Value {
    json!({
        "id": request.id,
        "recipientEmail": request.recipient_email,
        "status": request.status.as_str(),
        "createdAt": request.created_at,
        "emailVerifiedAt": request.email_verified_at,
        "permissions": request.permissions,
    })
}

fn registration_error(headers: &HeaderMap, error: UserRegistrationReviewError) -> Response {
    match error {
        UserRegistrationReviewError::InvalidRequest
        | UserRegistrationReviewError::InvalidPermissions => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "注册申请审核请求无效",
        )
        .into_response(),
        UserRegistrationReviewError::NotReviewable => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "注册申请不可审核",
        )
        .into_response(),
        UserRegistrationReviewError::StorageUnavailable => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "注册申请审核服务暂时不可用",
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
