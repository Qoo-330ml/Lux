use super::*;

use crate::auth::device_pairings::{DevicePairingError, PrismDeviceInfo};
use crate::{
    auth::sessions::AuthenticatedSession,
    security::{
        DEVICE_PAIRING_CREATE_LIMIT, DEVICE_PAIRING_REDEEM_LIMIT,
        DEVICE_PAIRING_RETRY_AFTER_SECONDS,
    },
};

pub(super) async fn live() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

pub(super) async fn ready(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
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

    let schema_version = match database.schema_version().await {
        Ok(schema_version) => schema_version,
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "status": "not_ready", "reason": "database_unavailable" })),
            );
        }
    };
    if database.probe_write().await.is_err() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "status": "not_ready",
                "reason": "database_write_unavailable",
                "schemaVersion": schema_version,
                "databaseWritable": false,
            })),
        );
    }
    (
        StatusCode::OK,
        Json(json!({
            "status": "ready",
            "schemaVersion": schema_version,
            "databaseWritable": true,
        })),
    )
}

pub(super) async fn version(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
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

pub(super) async fn setup_status(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    let Some(setup) = state.setup.as_ref() else {
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

pub(super) async fn setup_database_status(State(state): State<AppState>) -> Response {
    let Some(database_setup) = state.database_setup.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "not_ready", "reason": "database_unavailable" })),
        )
            .into_response();
    };

    match database_setup.status().await {
        Ok(status) => (
            StatusCode::OK,
            Json(json!({
                "configured": status.configured,
                "backend": status.backend,
                "currentBackend": status.current_backend,
                "restartRequired": status.restart_required
            })),
        )
            .into_response(),
        Err(error) => database_setup_error(&HeaderMap::new(), error),
    }
}

#[derive(Deserialize)]
#[serde(tag = "backend", rename_all = "SCREAMING_SNAKE_CASE")]
pub(super) enum SetupDatabaseRequest {
    #[serde(rename = "SQLITE")]
    Sqlite,
    #[serde(rename = "POSTGRESQL")]
    Postgres {
        host: String,
        port: u16,
        database: String,
        username: String,
        password: String,
        #[serde(default = "default_postgres_ssl_mode")]
        ssl_mode: String,
    },
}

fn default_postgres_ssl_mode() -> String {
    "prefer".to_owned()
}

impl SetupDatabaseRequest {
    fn into_configuration(self) -> DatabaseConfiguration {
        match self {
            Self::Sqlite => DatabaseConfiguration::Sqlite,
            Self::Postgres {
                host,
                port,
                database,
                username,
                password,
                ssl_mode,
            } => DatabaseConfiguration::Postgres(PostgresConnection {
                host,
                port,
                database,
                username,
                password,
                ssl_mode,
            }),
        }
    }
}

pub(super) async fn setup_database_test(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<SetupDatabaseRequest>,
) -> Response {
    let Some(setup) = state.setup.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    match setup.status().await {
        Ok(false) => {}
        Ok(true) => {
            return api_error(
                &headers,
                StatusCode::CONFLICT,
                lux::ApiErrorCode::SetupAlreadyCompleted,
                "初始设置已经完成",
            )
            .into_response();
        }
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "数据库不可用",
            )
            .into_response();
        }
    }
    let Some(database_setup) = state.database_setup.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let configuration = request.into_configuration();
    match database_setup.test(&configuration).await {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({ "ok": true, "backend": configuration.backend() })),
        )
            .into_response(),
        Err(error) => database_setup_error(&headers, error),
    }
}

pub(super) async fn setup_database_select(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<SetupDatabaseRequest>,
) -> Response {
    let Some(setup) = state.setup.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    match setup.status().await {
        Ok(true) => {
            return api_error(
                &headers,
                StatusCode::CONFLICT,
                lux::ApiErrorCode::SetupAlreadyCompleted,
                "初始设置已经完成",
            )
            .into_response();
        }
        Ok(false) => {}
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "数据库不可用",
            )
            .into_response();
        }
    }
    let Some(database_setup) = state.database_setup.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let configuration = request.into_configuration();
    match database_setup.select(&configuration).await {
        Ok(result) => (
            StatusCode::OK,
            Json(json!({
                "selected": true,
                "backend": result.backend,
                "restartRequired": result.restart_required
            })),
        )
            .into_response(),
        Err(error) => database_setup_error(&headers, error),
    }
}

fn database_setup_error(headers: &HeaderMap, error: DatabaseSetupError) -> Response {
    let (status, code, message) = match error {
        DatabaseSetupError::Configuration(_) => (
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "数据库配置无效",
        ),
        DatabaseSetupError::Storage(_) => (
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::DatabaseConnectionFailed,
            "无法连接数据库，请检查地址、端口、用户名和密码",
        ),
    };
    api_error(headers, status, code, message).into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SetupCompleteRequest {
    username: String,
    #[serde(default)]
    display_name: String,
    password: String,
    #[serde(default)]
    first_library: Option<SetupFirstLibraryRequest>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetupFirstLibraryRequest {
    name: String,
    #[serde(default = "default_setup_library_kind")]
    kind: String,
    #[serde(default = "default_realtime_watch_enabled")]
    realtime_watch_enabled: bool,
    #[serde(default)]
    root_path: Option<String>,
}

fn default_setup_library_kind() -> String {
    "MIXED".to_owned()
}

pub(super) async fn setup_complete(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<SetupCompleteRequest>,
) -> Response {
    let Some(setup) = state.setup.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };

    if state.database_selection_required {
        let Some(database_setup) = state.database_setup.as_ref() else {
            return api_error(
                &headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "服务尚未就绪",
            )
            .into_response();
        };
        match database_setup.status().await {
            Ok(status) if !status.configured => {
                return api_error(
                    &headers,
                    StatusCode::CONFLICT,
                    lux::ApiErrorCode::DatabaseConfigurationRequired,
                    "请先选择数据库",
                )
                .into_response();
            }
            Ok(status) if status.restart_required => {
                return api_error(
                    &headers,
                    StatusCode::CONFLICT,
                    lux::ApiErrorCode::DatabaseRestartRequired,
                    "数据库配置已保存，请重启 Lux 后继续",
                )
                .into_response();
            }
            Ok(_) => {}
            Err(_) => {
                return api_error(
                    &headers,
                    StatusCode::SERVICE_UNAVAILABLE,
                    lux::ApiErrorCode::DatabaseUnavailable,
                    "数据库不可用",
                )
                .into_response();
            }
        }
    }

    let setup_kind = match request
        .first_library
        .as_ref()
        .map(|library| library.kind.parse::<LibraryKind>())
        .transpose()
    {
        Ok(kind) => kind,
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "媒体库类型无效",
            )
            .into_response();
        }
    };
    if let Some(first_library) = request.first_library.as_ref() {
        if first_library.name.trim().is_empty() || first_library.name.chars().count() > 128 {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "媒体库名称无效",
            )
            .into_response();
        }
        if let Some(root_path) = first_library.root_path.as_deref() {
            if let Err(error) = crate::library::inspect_root_path(FsPath::new(root_path)).await {
                return library_error(&headers, LibraryServiceError::from(error));
            }
        }
    }

    match setup
        .complete(&request.username, &request.display_name, &request.password)
        .await
    {
        Ok(user) => {
            let mut library_json_value = None;
            if let (Some(first_library), Some(kind), Some(libraries)) =
                (request.first_library, setup_kind, state.libraries.as_ref())
            {
                let library = match libraries
                    .create_library_with_scraper(
                        &first_library.name,
                        kind,
                        first_library.realtime_watch_enabled,
                        None,
                        true,
                    )
                    .await
                {
                    Ok(library) => library,
                    Err(error) => return library_error(&headers, error),
                };
                let mut roots = Vec::new();
                let mut warnings = Vec::new();
                if let Some(root_path) = first_library.root_path {
                    match libraries.add_root(library.id, &root_path).await {
                        Ok(result) => {
                            roots.push(result.root);
                            warnings = result
                                .warnings
                                .iter()
                                .map(|warning| warning.as_str())
                                .collect::<Vec<_>>();
                        }
                        Err(error) => return library_error(&headers, error),
                    }
                }
                let scan_job = match spawn_library_scan(&state, library.id).await {
                    Ok(job) => job,
                    Err(error) => {
                        tracing::warn!(library_id = %library.id, %error, "initial library scan could not be started");
                        None
                    }
                };
                library_json_value = Some(json!({
                    "library": library_json(&library, &roots),
                    "warnings": warnings,
                    "scanJob": scan_job.as_ref().map(scan_job_json),
                }));
            }
            let mut response = json!({
                "initialized": true,
                "user": user_json(&user),
            });
            if let Some(library) = library_json_value {
                response["library"] = library["library"].clone();
                response["warnings"] = library["warnings"].clone();
                response["scanJob"] = library["scanJob"].clone();
            }
            (StatusCode::CREATED, Json(response)).into_response()
        }
        Err(SetupError::AlreadyCompleted) => api_error(
            &headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::SetupAlreadyCompleted,
            "初始化已完成",
        )
        .into_response(),
        Err(SetupError::UserStore(
            UserStoreError::InvalidUsername | UserStoreError::Password(_),
        )) => api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "用户名或密码无效",
        )
        .into_response(),
        Err(SetupError::UserStore(_)) => api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "初始化暂时不可用",
        )
        .into_response(),
    }
}

#[derive(Deserialize)]
pub(super) struct AuthLoginRequest {
    username: String,
    password: String,
}

pub(super) async fn auth_login(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<AuthLoginRequest>,
) -> Response {
    let Some(auth) = state.auth.clone() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };

    let login_key = login_attempt_key(&headers, &request.username);
    if !state.login_rate_limiter.is_allowed(&login_key).await {
        return api_error(
            &headers,
            StatusCode::UNAUTHORIZED,
            lux::ApiErrorCode::InvalidCredentials,
            "用户名或密码错误",
        )
        .into_response();
    }
    let mut session = match auth.login(&request.username, &request.password).await {
        Ok(session) => session,
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
    if session.is_none()
        && let Some(migration) = state.emby_migration.clone()
    {
        match migration
            .authenticate_pending_user(&request.username, &request.password)
            .await
        {
            Ok(true) => match auth.login(&request.username, &request.password).await {
                Ok(reauthenticated) => session = reauthenticated,
                Err(_) => {
                    return api_error(
                        &headers,
                        StatusCode::SERVICE_UNAVAILABLE,
                        lux::ApiErrorCode::DatabaseUnavailable,
                        "登录暂时不可用",
                    )
                    .into_response();
                }
            },
            Ok(false) => {}
            Err(_) => {}
        }
    }
    let Some(session) = session else {
        state.login_rate_limiter.record_failure(&login_key).await;
        return api_error(
            &headers,
            StatusCode::UNAUTHORIZED,
            lux::ApiErrorCode::InvalidCredentials,
            "用户名或密码错误",
        )
        .into_response();
    };
    state.login_rate_limiter.record_success(&login_key).await;
    if state.remote_access.is_remote(
        header_str(&headers, "x-lux-peer-ip"),
        header_str(&headers, "x-forwarded-for"),
    ) && !session.user.can_remote_access
    {
        let _ = auth.logout(&session.session_token).await;
        return api_error(
            &headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::PermissionDenied,
            "当前账户不允许远程访问",
        )
        .into_response();
    }

    let user_id = session.user.id.to_string();
    record_activity_event(
        state.database.as_ref(),
        &state.admin_events,
        &user_id,
        "AUTH_LOGIN",
        None,
        json!({
            "remoteIp": request_client_ip(&headers, &state.remote_access),
        }),
    )
    .await;

    let mut response_headers = HeaderMap::new();
    let secure_cookie = secure_cookie_for_request(&headers, &state.remote_access);
    let Some(session_cookie) = build_cookie(
        "lux_session",
        &session.session_token,
        true,
        None,
        secure_cookie,
    ) else {
        return api_error(
            &headers,
            StatusCode::INTERNAL_SERVER_ERROR,
            lux::ApiErrorCode::Internal,
            "无法创建会话",
        )
        .into_response();
    };
    let Some(csrf_cookie) =
        build_cookie("lux_csrf", &session.csrf_token, false, None, secure_cookie)
    else {
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

pub(super) async fn auth_me(headers: HeaderMap, State(state): State<AppState>) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let server_name = current_emby_server_name(&state).await;
    Json(json!({
        "user": user_json(&user),
        "serverName": server_name,
    }))
    .into_response()
}

pub(super) async fn auth_create_device_pairing(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    let session = match require_web_session_csrf(&headers, &state).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    let client_ip = pairing_rate_limit_client_ip(&headers, &state.remote_access);
    let rate_key = format!("create:{}:{client_ip}", session.user.id);
    if !state
        .device_pairing_rate_limiter
        .try_acquire(&rate_key, DEVICE_PAIRING_CREATE_LIMIT)
        .await
    {
        return rate_limited_response(&headers);
    }
    let Some(service) = state.device_pairings.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    match service.create(&session.user.id.to_string()).await {
        Ok(pairing) => (
            StatusCode::CREATED,
            [("Cache-Control", "no-store")],
            Json(json!({
                "pairingId": pairing.pairing_id,
                "secret": pairing.secret,
                "expiresAt": pairing.expires_at,
            })),
        )
            .into_response(),
        Err(_) => api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "设备配对暂时不可用",
        )
        .into_response(),
    }
}

pub(super) async fn auth_cancel_device_pairing(
    headers: HeaderMap,
    Path(pairing_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let session = match require_web_session_csrf(&headers, &state).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if uuid::Uuid::parse_str(&pairing_id).is_err() {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "配对 ID 无效",
        )
        .into_response();
    }
    let Some(service) = state.device_pairings.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    match service
        .cancel(&session.user.id.to_string(), &pairing_id)
        .await
    {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::DevicePairingNotFound,
            "设备配对不存在",
        )
        .into_response(),
        Err(_) => api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "设备配对暂时不可用",
        )
        .into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DevicePairingRedeemRequest {
    secret: String,
    device_id: String,
    device_name: String,
    platform: String,
    version: String,
}

pub(super) async fn redeem_device_pairing(
    headers: HeaderMap,
    Path(pairing_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<DevicePairingRedeemRequest>,
) -> Response {
    let client_ip = pairing_rate_limit_client_ip(&headers, &state.remote_access);
    if !state
        .device_pairing_rate_limiter
        .try_acquire(&format!("redeem:{client_ip}"), DEVICE_PAIRING_REDEEM_LIMIT)
        .await
    {
        return rate_limited_response(&headers);
    }
    if uuid::Uuid::parse_str(&pairing_id).is_err()
        || bounded_non_empty(&request.secret, 128).is_none()
        || bounded_non_empty(&request.device_id, 128).is_none()
        || bounded_non_empty(&request.device_name, 128).is_none()
        || bounded_non_empty(&request.platform, 64).is_none()
        || bounded_non_empty(&request.version, 64).is_none()
    {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "设备配对请求无效",
        )
        .into_response();
    }
    let Some(service) = state.device_pairings.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let device = PrismDeviceInfo {
        device_id: request.device_id.trim(),
        device_name: request.device_name.trim(),
        platform: request.platform.trim(),
        version: request.version.trim(),
    };
    match service
        .redeem(&pairing_id, request.secret.trim(), &device)
        .await
    {
        Ok(redemption) => (
            StatusCode::OK,
            [("Cache-Control", "no-store")],
            Json(json!({
                "accessToken": redemption.access_token,
                "userId": redemption.user_id,
                "serverId": state.server_id,
            })),
        )
            .into_response(),
        Err(error) => device_pairing_error(&headers, error),
    }
}

async fn require_web_session_csrf(
    headers: &HeaderMap,
    state: &AppState,
) -> Result<AuthenticatedSession, Response> {
    let Some(auth) = state.auth.as_ref() else {
        return Err(api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response());
    };
    let Some(session_token) = request_cookie(headers, "lux_session") else {
        return Err(api_error(
            headers,
            StatusCode::UNAUTHORIZED,
            lux::ApiErrorCode::AuthenticationRequired,
            "需要登录",
        )
        .into_response());
    };
    let session = match auth.resolve(&session_token).await {
        Ok(Some(session)) => session,
        Ok(None) => {
            return Err(api_error(
                headers,
                StatusCode::UNAUTHORIZED,
                lux::ApiErrorCode::AuthenticationRequired,
                "需要登录",
            )
            .into_response());
        }
        Err(_) => {
            return Err(api_error(
                headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "认证暂时不可用",
            )
            .into_response());
        }
    };
    if state.remote_access.is_remote(
        header_str(headers, "x-lux-peer-ip"),
        header_str(headers, "x-forwarded-for"),
    ) && !session.user.can_remote_access
    {
        return Err(api_error(
            headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::PermissionDenied,
            "当前账户不允许远程访问",
        )
        .into_response());
    }
    let Some(csrf_token) = headers
        .get("x-csrf-token")
        .and_then(|value| value.to_str().ok())
    else {
        return Err(api_error(
            headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::CsrfFailed,
            "CSRF 校验失败",
        )
        .into_response());
    };
    if !auth.verify_csrf(&session, csrf_token) {
        return Err(api_error(
            headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::CsrfFailed,
            "CSRF 校验失败",
        )
        .into_response());
    }
    Ok(session)
}

fn bounded_non_empty(value: &str, max_bytes: usize) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty() && value.len() <= max_bytes).then_some(value)
}

fn pairing_rate_limit_client_ip(headers: &HeaderMap, policy: &RemoteAccessPolicy) -> String {
    policy
        .client_ip(
            header_str(headers, "x-lux-peer-ip"),
            header_str(headers, "x-forwarded-for"),
        )
        .map(|address| address.to_string())
        .unwrap_or_else(|| "local".to_owned())
}

fn rate_limited_response(headers: &HeaderMap) -> Response {
    let (status, body) = api_error(
        headers,
        StatusCode::TOO_MANY_REQUESTS,
        lux::ApiErrorCode::RateLimited,
        "请求过于频繁，请稍后重试",
    );
    (
        status,
        [
            (
                "Retry-After",
                DEVICE_PAIRING_RETRY_AFTER_SECONDS.to_string(),
            ),
            ("Cache-Control", "no-store".to_owned()),
        ],
        body,
    )
        .into_response()
}

fn device_pairing_error(headers: &HeaderMap, error: DevicePairingError) -> Response {
    let (status, code, message) = match error {
        DevicePairingError::NotFound => (
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::DevicePairingNotFound,
            "设备配对不存在",
        ),
        DevicePairingError::InvalidSecret => (
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::DevicePairingInvalidSecret,
            "设备配对密钥无效",
        ),
        DevicePairingError::Expired => (
            StatusCode::GONE,
            lux::ApiErrorCode::DevicePairingExpired,
            "设备配对已过期",
        ),
        DevicePairingError::Cancelled => (
            StatusCode::GONE,
            lux::ApiErrorCode::DevicePairingCancelled,
            "设备配对已取消",
        ),
        DevicePairingError::Consumed => (
            StatusCode::CONFLICT,
            lux::ApiErrorCode::DevicePairingConsumed,
            "设备配对已使用",
        ),
        DevicePairingError::Storage(_) | DevicePairingError::TokenGeneration(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "设备配对暂时不可用",
        ),
    };
    api_error(headers, status, code, message).into_response()
}

pub(super) async fn auth_settings(headers: HeaderMap, State(state): State<AppState>) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database.user_played_percent(&user.id.to_string()).await {
        Ok(played_percent) => Json(json!({ "playedPercent": played_percent })).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AuthSettingsPatch {
    played_percent: i64,
}

pub(super) async fn auth_update_settings(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<AuthSettingsPatch>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_web_csrf(&headers, &state).await {
        return response;
    }
    if !(1..=100).contains(&request.played_percent) {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "自动标记已看百分比必须在 1 到 100 之间",
        )
        .into_response();
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database
        .set_user_played_percent(&user.id.to_string(), request.played_percent)
        .await
    {
        Ok(()) => Json(json!({ "playedPercent": request.played_percent })).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn auth_library_order(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let Some(libraries) = state.libraries.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let accessible_library_ids = match access.accessible_library_ids(principal).await {
        Ok(ids) => ids,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match libraries
        .saved_library_order_for_user(&user.id.to_string(), &accessible_library_ids)
        .await
    {
        Ok(library_order) => Json(json!({ "libraryOrder": library_order })).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LibraryOrderPatch {
    library_order: Vec<String>,
}

pub(super) async fn auth_update_library_order(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<LibraryOrderPatch>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_web_csrf(&headers, &state).await {
        return response;
    }
    if request.library_order.len() > 1024 {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "媒体库排序数量超出上限",
        )
        .into_response();
    }
    let Some(libraries) = state.libraries.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let accessible_library_ids = match access.accessible_library_ids(principal).await {
        Ok(ids) => ids,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match libraries
        .set_library_order(
            &user.id.to_string(),
            &request.library_order,
            &accessible_library_ids,
        )
        .await
    {
        Ok(library_order) => {
            if let Some(home) = state.home.as_ref() {
                home.invalidate();
            }
            Json(json!({ "libraryOrder": library_order })).into_response()
        }
        Err(LibraryServiceError::InvalidLibraryOrder(message)) => api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            &message,
        )
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn auth_avatar(headers: HeaderMap, State(state): State<AppState>) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let Some(avatars) = state.user_avatars.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match avatars.load(user.id).await {
        Ok(Some(avatar)) => match Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, avatar.content_type)
            .header(CACHE_CONTROL, "private, no-cache")
            .body(Body::from(avatar.bytes))
        {
            Ok(response) => response,
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        },
        Ok(None) => match Response::builder()
            .status(StatusCode::NO_CONTENT)
            .header(CACHE_CONTROL, "private, no-cache")
            .body(Body::empty())
        {
            Ok(response) => response,
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        },
        Err(error) => user_avatar_error(&headers, error),
    }
}

pub(super) async fn auth_update_avatar(
    headers: HeaderMap,
    State(state): State<AppState>,
    body: Bytes,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_web_csrf(&headers, &state).await {
        return response;
    }
    let Some(avatars) = state.user_avatars.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    match avatars.store(user.id, content_type, &body).await {
        Ok(()) => Json(json!({
            "avatarUrl": "/api/v1/auth/avatar",
        }))
        .into_response(),
        Err(error) => user_avatar_error(&headers, error),
    }
}

pub(super) async fn auth_sessions(headers: HeaderMap, State(state): State<AppState>) -> Response {
    let Some(auth) = state.auth.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
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
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match auth.list_sessions(&session.user.id, &session_token).await {
        Ok(sessions) => Json(json!({
            "sessions": sessions.iter().map(|session| json!({
                "id": session.id,
                "createdAt": session.created_at,
                "updatedAt": session.updated_at,
                "expiresAt": session.expires_at,
                "lastSeenAt": session.last_seen_at,
                "isCurrent": session.is_current,
            })).collect::<Vec<_>>()
        }))
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn auth_revoke_session(
    headers: HeaderMap,
    Path(session_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let Some(auth) = state.auth.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
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
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
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
    let sessions = match auth.list_sessions(&session.user.id, &session_token).await {
        Ok(sessions) => sessions,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    if sessions
        .iter()
        .any(|entry| entry.id == session_id && entry.is_current)
    {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "不能撤销当前会话，请使用退出登录",
        )
        .into_response();
    }
    match auth.revoke_session(&session.user.id, &session_id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "会话不存在",
        )
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn auth_logout(headers: HeaderMap, State(state): State<AppState>) -> Response {
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
    let secure_cookie = secure_cookie_for_request(&headers, &state.remote_access);
    if let Some(cookie) = build_cookie("lux_session", "", true, Some(0), secure_cookie) {
        response_headers.append(SET_COOKIE, cookie);
    }
    if let Some(cookie) = build_cookie("lux_csrf", "", false, Some(0), secure_cookie) {
        response_headers.append(SET_COOKIE, cookie);
    }
    (StatusCode::NO_CONTENT, response_headers).into_response()
}

pub(super) async fn require_web_user(
    headers: &HeaderMap,
    state: &AppState,
) -> Result<UserRecord, Response> {
    if lux_api_key_from_headers(headers).is_some() {
        let user = resolve_shared_admin_api_key(headers, state).await?;
        let Some(user) = user else {
            return Err(api_error(
                headers,
                StatusCode::UNAUTHORIZED,
                lux::ApiErrorCode::AuthenticationRequired,
                "需要有效的 API Key",
            )
            .into_response());
        };
        if state.remote_access.is_remote(
            header_str(headers, "x-lux-peer-ip"),
            header_str(headers, "x-forwarded-for"),
        ) && !user.can_remote_access
        {
            return Err(api_error(
                headers,
                StatusCode::FORBIDDEN,
                lux::ApiErrorCode::PermissionDenied,
                "当前管理员不允许远程访问",
            )
            .into_response());
        }
        return Ok(user);
    }
    let Some(auth) = state.auth.as_ref() else {
        return Err(api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response());
    };
    let Some(session_token) = request_cookie(headers, "lux_session") else {
        return Err(api_error(
            headers,
            StatusCode::UNAUTHORIZED,
            lux::ApiErrorCode::AuthenticationRequired,
            "需要登录",
        )
        .into_response());
    };
    match auth.resolve(&session_token).await {
        Ok(Some(session)) => {
            if state.remote_access.is_remote(
                header_str(headers, "x-lux-peer-ip"),
                header_str(headers, "x-forwarded-for"),
            ) && !session.user.can_remote_access
            {
                return Err(api_error(
                    headers,
                    StatusCode::FORBIDDEN,
                    lux::ApiErrorCode::PermissionDenied,
                    "当前账户不允许远程访问",
                )
                .into_response());
            }
            Ok(session.user)
        }
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

pub(super) async fn require_web_csrf(
    headers: &HeaderMap,
    state: &AppState,
) -> Result<(), Response> {
    if lux_api_key_from_headers(headers).is_some() {
        let user = resolve_shared_admin_api_key(headers, state).await?;
        if user.is_some() {
            return Ok(());
        }
        return Err(api_error(
            headers,
            StatusCode::UNAUTHORIZED,
            lux::ApiErrorCode::AuthenticationRequired,
            "需要有效的 API Key",
        )
        .into_response());
    }
    let Some(auth) = state.auth.as_ref() else {
        return Err(api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response());
    };
    let Some(session_token) = request_cookie(headers, "lux_session") else {
        return Err(api_error(
            headers,
            StatusCode::UNAUTHORIZED,
            lux::ApiErrorCode::AuthenticationRequired,
            "需要登录",
        )
        .into_response());
    };
    let session = match auth.resolve(&session_token).await {
        Ok(Some(session)) => session,
        Ok(None) => {
            return Err(api_error(
                headers,
                StatusCode::UNAUTHORIZED,
                lux::ApiErrorCode::AuthenticationRequired,
                "需要登录",
            )
            .into_response());
        }
        Err(_) => {
            return Err(api_error(
                headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "认证暂时不可用",
            )
            .into_response());
        }
    };
    let Some(csrf_token) = headers
        .get("x-csrf-token")
        .and_then(|value| value.to_str().ok())
    else {
        return Err(api_error(
            headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::CsrfFailed,
            "CSRF 校验失败",
        )
        .into_response());
    };
    if !auth.verify_csrf(&session, csrf_token) {
        return Err(api_error(
            headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::CsrfFailed,
            "CSRF 校验失败",
        )
        .into_response());
    }
    Ok(())
}

pub(super) fn api_routes() -> Router<AppState> {
    Router::new()
        .route("/health/live", get(users::live))
        .route("/health/ready", get(users::ready))
        .route("/api/v1/version", get(users::version))
        .route("/api/v1/setup/status", get(users::setup_status))
        .route("/api/v1/setup/database", get(users::setup_database_status))
        .route(
            "/api/v1/setup/database/test",
            post(users::setup_database_test),
        )
        .route(
            "/api/v1/setup/database/select",
            post(users::setup_database_select),
        )
        .route("/api/v1/setup/complete", post(users::setup_complete))
        .route("/api/v1/auth/login", post(users::auth_login))
        .route("/api/v1/auth/logout", post(users::auth_logout))
        .route("/api/v1/auth/me", get(users::auth_me))
        .route(
            "/api/v1/auth/device-pairings",
            post(users::auth_create_device_pairing),
        )
        .route(
            "/api/v1/auth/device-pairings/{pairing_id}",
            delete(users::auth_cancel_device_pairing),
        )
        .route(
            "/api/v1/device-pairings/{pairing_id}/redeem",
            post(users::redeem_device_pairing).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/v1/auth/settings",
            get(users::auth_settings).patch(users::auth_update_settings),
        )
        .route(
            "/api/v1/auth/library-order",
            get(users::auth_library_order).patch(users::auth_update_library_order),
        )
        .route(
            "/api/v1/auth/avatar",
            get(users::auth_avatar)
                .put(users::auth_update_avatar)
                .layer(DefaultBodyLimit::max(MAX_USER_AVATAR_BYTES as usize)),
        )
        .route("/api/v1/auth/sessions", get(users::auth_sessions))
        .route(
            "/api/v1/auth/sessions/{session_id}",
            delete(users::auth_revoke_session),
        )
}
