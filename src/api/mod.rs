pub mod lux;

use std::path::PathBuf;

use axum::{
    Json, Router,
    extract::State,
    http::{
        HeaderMap, HeaderValue, StatusCode,
        header::{COOKIE, SET_COOKIE},
    },
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::fs;
use tower_http::{ServiceBuilderExt, request_id::MakeRequestUuid, trace::TraceLayer};

use crate::{
    COMMIT, VERSION,
    application::setup::{SetupError, SetupService},
    auth::sessions::WebAuthService,
    auth::users::{UserRecord, UserStoreError},
    config::Config,
    storage::Database,
};

#[derive(Clone, Default)]
pub struct AppState {
    database: Option<Database>,
    config_dir: Option<PathBuf>,
    setup: Option<SetupService>,
    auth: Option<WebAuthService>,
}

impl AppState {
    pub fn ready(
        config: Config,
        database: Database,
        setup: SetupService,
        auth: WebAuthService,
    ) -> Self {
        Self {
            database: Some(database),
            config_dir: Some(config.config_dir),
            setup: Some(setup),
            auth: Some(auth),
        }
    }
}

pub fn app() -> Router {
    app_with_state(AppState::default())
}

pub fn app_with_state(state: AppState) -> Router {
    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/api/v1/version", get(version))
        .route("/api/v1/setup/status", get(setup_status))
        .route("/api/v1/setup/complete", post(setup_complete))
        .route("/api/v1/auth/login", post(auth_login))
        .route("/api/v1/auth/logout", post(auth_logout))
        .route("/api/v1/auth/me", get(auth_me))
        .with_state(state)
        .layer(
            tower::ServiceBuilder::new()
                .set_x_request_id(MakeRequestUuid)
                .layer(TraceLayer::new_for_http())
                .propagate_x_request_id(),
        )
}

async fn live() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

async fn ready(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    let Some(database) = state.database else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "not_ready", "reason": "database_unavailable" })),
        );
    };
    let config_available = match state.config_dir.as_deref() {
        Some(path) => fs::metadata(path)
            .await
            .map(|metadata| metadata.is_dir())
            .unwrap_or(false),
        None => false,
    };

    if !config_available {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "not_ready", "reason": "config_unavailable" })),
        );
    }

    match database.schema_version().await {
        Ok(schema_version) => (
            StatusCode::OK,
            Json(json!({ "status": "ready", "schemaVersion": schema_version })),
        ),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "not_ready", "reason": "database_unavailable" })),
        ),
    }
}

async fn version(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    let Some(database) = state.database else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "not_ready", "reason": "database_unavailable" })),
        );
    };

    match database.schema_version().await {
        Ok(schema_version) => (
            StatusCode::OK,
            Json(json!({
                "luxVersion": VERSION,
                "commit": COMMIT,
                "schemaVersion": schema_version
            })),
        ),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "not_ready", "reason": "database_unavailable" })),
        ),
    }
}

async fn setup_status(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    let Some(setup) = state.setup else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "not_ready", "reason": "database_unavailable" })),
        );
    };

    match setup.status().await {
        Ok(initialized) => (StatusCode::OK, Json(json!({ "initialized": initialized }))),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "not_ready", "reason": "database_unavailable" })),
        ),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetupCompleteRequest {
    username: String,
    #[serde(default)]
    display_name: String,
    password: String,
}

async fn setup_complete(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<SetupCompleteRequest>,
) -> (StatusCode, Json<Value>) {
    let Some(setup) = state.setup else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        );
    };

    match setup
        .complete(&request.username, &request.display_name, &request.password)
        .await
    {
        Ok(user) => (
            StatusCode::CREATED,
            Json(json!({ "initialized": true, "user": user_json(&user) })),
        ),
        Err(SetupError::AlreadyCompleted) => api_error(
            &headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::SetupAlreadyCompleted,
            "初始化已完成",
        ),
        Err(SetupError::UserStore(
            UserStoreError::InvalidUsername | UserStoreError::Password(_),
        )) => api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "用户名或密码无效",
        ),
        Err(SetupError::UserStore(_)) => api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "初始化暂时不可用",
        ),
    }
}

#[derive(Deserialize)]
struct AuthLoginRequest {
    username: String,
    password: String,
}

async fn auth_login(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<AuthLoginRequest>,
) -> Response {
    let Some(auth) = state.auth else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };

    let session = match auth.login(&request.username, &request.password).await {
        Ok(Some(session)) => session,
        Ok(None) => {
            return api_error(
                &headers,
                StatusCode::UNAUTHORIZED,
                lux::ApiErrorCode::InvalidCredentials,
                "用户名或密码错误",
            )
            .into_response();
        }
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "登录暂时不可用",
            )
            .into_response();
        }
    };

    let mut response_headers = HeaderMap::new();
    let Some(session_cookie) = build_cookie("lux_session", &session.session_token, true, None)
    else {
        return api_error(
            &headers,
            StatusCode::INTERNAL_SERVER_ERROR,
            lux::ApiErrorCode::Internal,
            "无法创建会话",
        )
        .into_response();
    };
    let Some(csrf_cookie) = build_cookie("lux_csrf", &session.csrf_token, false, None) else {
        return api_error(
            &headers,
            StatusCode::INTERNAL_SERVER_ERROR,
            lux::ApiErrorCode::Internal,
            "无法创建会话",
        )
        .into_response();
    };
    response_headers.append(SET_COOKIE, session_cookie);
    response_headers.append(SET_COOKIE, csrf_cookie);
    (
        StatusCode::OK,
        response_headers,
        Json(json!({ "user": user_json(&session.user), "csrfToken": session.csrf_token })),
    )
        .into_response()
}

async fn auth_me(headers: HeaderMap, State(state): State<AppState>) -> Response {
    let Some(auth) = state.auth else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let Some(session_token) = request_cookie(&headers, "lux_session") else {
        return api_error(
            &headers,
            StatusCode::UNAUTHORIZED,
            lux::ApiErrorCode::AuthenticationRequired,
            "需要登录",
        )
        .into_response();
    };
    match auth.resolve(&session_token).await {
        Ok(Some(session)) => Json(json!({ "user": user_json(&session.user) })).into_response(),
        Ok(None) => api_error(
            &headers,
            StatusCode::UNAUTHORIZED,
            lux::ApiErrorCode::AuthenticationRequired,
            "需要登录",
        )
        .into_response(),
        Err(_) => api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "认证暂时不可用",
        )
        .into_response(),
    }
}

async fn auth_logout(headers: HeaderMap, State(state): State<AppState>) -> Response {
    let Some(auth) = state.auth else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let Some(session_token) = request_cookie(&headers, "lux_session") else {
        return api_error(
            &headers,
            StatusCode::UNAUTHORIZED,
            lux::ApiErrorCode::AuthenticationRequired,
            "需要登录",
        )
        .into_response();
    };
    let session = match auth.resolve(&session_token).await {
        Ok(Some(session)) => session,
        Ok(None) => {
            return api_error(
                &headers,
                StatusCode::UNAUTHORIZED,
                lux::ApiErrorCode::AuthenticationRequired,
                "需要登录",
            )
            .into_response();
        }
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "认证暂时不可用",
            )
            .into_response();
        }
    };
    let Some(csrf_token) = headers
        .get("x-csrf-token")
        .and_then(|value| value.to_str().ok())
    else {
        return api_error(
            &headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::CsrfFailed,
            "CSRF 校验失败",
        )
        .into_response();
    };
    if !auth.verify_csrf(&session, csrf_token) {
        return api_error(
            &headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::CsrfFailed,
            "CSRF 校验失败",
        )
        .into_response();
    }
    if auth.logout(&session_token).await.is_err() {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "退出登录暂时不可用",
        )
        .into_response();
    }

    let mut response_headers = HeaderMap::new();
    if let Some(cookie) = build_cookie("lux_session", "", true, Some(0)) {
        response_headers.append(SET_COOKIE, cookie);
    }
    if let Some(cookie) = build_cookie("lux_csrf", "", false, Some(0)) {
        response_headers.append(SET_COOKIE, cookie);
    }
    (StatusCode::NO_CONTENT, response_headers).into_response()
}

fn user_json(user: &UserRecord) -> Value {
    json!({
        "id": user.id.to_string(),
        "usernameNormalized": user.username_normalized,
        "displayName": user.display_name,
        "isAdmin": user.is_admin,
        "canManageServer": user.can_manage_server,
        "canRemoteAccess": user.can_remote_access,
        "canDownload": user.can_download
    })
}

fn api_error(
    headers: &HeaderMap,
    status: StatusCode,
    code: lux::ApiErrorCode,
    message: &str,
) -> (StatusCode, Json<Value>) {
    let request_id = headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown");
    let body = lux::ApiError::new(code, message, request_id);
    (
        status,
        Json(json!({
            "error": {
                "code": body.code,
                "message": body.message,
                "requestId": body.request_id
            }
        })),
    )
}

fn request_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value.split(';').find_map(|part| {
                let (cookie_name, cookie_value) = part.trim().split_once('=')?;
                (cookie_name == name).then(|| cookie_value.to_owned())
            })
        })
}

fn build_cookie(
    name: &str,
    value: &str,
    http_only: bool,
    max_age: Option<i64>,
) -> Option<HeaderValue> {
    let mut cookie = format!("{name}={value}; Path=/; Secure; SameSite=Lax");
    if http_only {
        cookie.push_str("; HttpOnly");
    }
    if let Some(max_age) = max_age {
        cookie.push_str(&format!("; Max-Age={max_age}"));
    }
    HeaderValue::from_str(&cookie).ok()
}
