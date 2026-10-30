use super::*;

use crate::auth::device_pairings::{DevicePairingError, PrismDeviceInfo};
use crate::{
    application::settings::{
        DEFAULT_LOGIN_BACKGROUND_SOURCE, RECENTLY_ADDED_LOGIN_BACKGROUND_SOURCE,
        is_valid_login_background_source, login_background_plugin_id,
    },
    application::user_account_preferences::{
        AccountPreferencesError, UpdateAccountPreferences, get_account_preferences,
        update_account_preferences,
    },
    application::user_email_verification::{
        UserEmailVerificationError, clear_user_email_change, pending_user_email_change,
        request_user_email_change, resend_user_email_change, verify_user_email_change,
    },
    application::user_invitations::{
        AcceptUserInvitationInput, UserInvitationAcceptanceError, accept_user_invitation,
    },
    application::user_registration::{
        SubmitUserRegistrationInput, UserRegistrationError, resend_public_user_registration_email,
        submit_public_user_registration, verify_public_user_registration_email,
    },
    auth::sessions::AuthenticatedSession,
    security::{
        DEVICE_PAIRING_CREATE_LIMIT, DEVICE_PAIRING_REDEEM_LIMIT,
        DEVICE_PAIRING_RETRY_AFTER_SECONDS,
    },
};

const MAX_LOGIN_BACKGROUND_IMAGES: i64 = 40;

pub(super) async fn live() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

pub(super) async fn login_background(State(state): State<ReadyAppState>) -> Response {
    let database = state.database();
    let source = match database.login_background_source().await {
        Ok(Some(source)) if is_valid_login_background_source(&source) => source,
        Ok(_) => DEFAULT_LOGIN_BACKGROUND_SOURCE.to_owned(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    if let Some(plugin_id) = login_background_plugin_id(&source) {
        let result = match state
            .plugins()
            .cached_login_background_result(plugin_id)
            .await
        {
            Ok(Some(result)) => result,
            Ok(None) | Err(_) => {
                return Json(json!({ "source": DEFAULT_LOGIN_BACKGROUND_SOURCE, "images": [] }))
                    .into_response();
            }
        };
        let mut result = match serde_json::to_value(result) {
            Ok(Value::Object(result)) => result,
            _ => {
                return Json(json!({ "source": DEFAULT_LOGIN_BACKGROUND_SOURCE, "images": [] }))
                    .into_response();
            }
        };
        result.insert("source".to_owned(), Value::String(source));
        return Json(Value::Object(result)).into_response();
    }
    if source != RECENTLY_ADDED_LOGIN_BACKGROUND_SOURCE {
        return Json(json!({ "source": source, "images": [] })).into_response();
    }

    let libraries = state.libraries();
    let enabled_library_ids = match libraries.list_libraries().await {
        Ok(views) => views
            .into_iter()
            .filter(|view| view.library.is_enabled)
            .map(|view| view.library.id.to_string())
            .collect::<Vec<_>>(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let catalog = state.catalog();
    let page = match catalog
        .list_recently_added_movies_and_series_for_library_ids(
            &enabled_library_ids,
            0,
            MAX_LOGIN_BACKGROUND_IMAGES,
        )
        .await
    {
        Ok(page) => page,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let images = page
        .items
        .into_iter()
        .filter_map(|item| {
            let tag = item.poster_image_tag.as_deref()?;
            Some(format!(
                "/emby/Items/{}/Images/Primary?tag={}",
                emby_public_id(&item.id),
                percent_encode_filename(tag),
            ))
        })
        .take(MAX_LOGIN_BACKGROUND_IMAGES as usize)
        .collect::<Vec<_>>();

    Json(json!({ "source": source, "images": images })).into_response()
}

pub(super) async fn login_background_custom_image(
    headers: HeaderMap,
    State(state): State<ReadyAppState>,
) -> Response {
    let plugins = state.plugins();
    let asset = match plugins.selected_custom_login_background_asset().await {
        Ok(Some(asset)) => asset,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(
            crate::application::plugins::PluginServiceError::InvalidConfig
            | crate::application::plugins::PluginServiceError::UnknownPlugin(_)
            | crate::application::plugins::PluginServiceError::Unavailable(_),
        ) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let not_modified = headers
        .get("If-None-Match")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value.split(',').any(|candidate| {
                let candidate = candidate.trim();
                candidate == "*" || candidate.strip_prefix("W/").unwrap_or(candidate) == asset.etag
            })
        });
    let (status, body) = if not_modified {
        (StatusCode::NOT_MODIFIED, Body::empty())
    } else {
        (StatusCode::OK, Body::from(asset.bytes))
    };
    match Response::builder()
        .status(status)
        .header(CONTENT_TYPE, asset.content_type)
        .header(CACHE_CONTROL, "no-cache, must-revalidate")
        .header("X-Content-Type-Options", "nosniff")
        .header("ETag", &asset.etag)
        .body(body)
    {
        Ok(response) => response,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub(super) async fn ready(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    let Some(database) = state.database() else {
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
            "fileLogging": crate::observability::file_logging_available(),
        })),
    )
}

pub(super) async fn version(State(state): State<AppState>) -> (StatusCode, Json<Value>) {
    let Some(database) = state.database() else {
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
    let Some(setup) = state.setup() else {
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
    let Some(database_setup) = state.database_setup() else {
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

/// The pre-setup database probe must not be reachable from the public internet: it lets the
/// caller make the server connect to an arbitrary PostgreSQL endpoint.
fn setup_endpoint_allows_peer(headers: &HeaderMap) -> bool {
    header_str(headers, "x-lux-peer-ip")
        .and_then(|value| value.parse::<std::net::IpAddr>().ok())
        .is_some_and(crate::application::user_access_rules::is_private_or_loopback)
}

fn setup_remote_peer_rejected(headers: &HeaderMap) -> Response {
    api_error(
        headers,
        StatusCode::FORBIDDEN,
        lux::ApiErrorCode::PermissionDenied,
        "只能在局域网内完成数据库初始化",
    )
    .into_response()
}

pub(super) async fn setup_database_test(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<SetupDatabaseRequest>,
) -> Response {
    if !setup_endpoint_allows_peer(&headers) {
        return setup_remote_peer_rejected(&headers);
    }
    let Some(setup) = state.setup() else {
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
    let Some(database_setup) = state.database_setup() else {
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
    if !setup_endpoint_allows_peer(&headers) {
        return setup_remote_peer_rejected(&headers);
    }
    let Some(setup) = state.setup() else {
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
    let Some(database_setup) = state.database_setup() else {
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

pub(super) async fn setup_database_restart(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    if !state.database_selection_required {
        return api_error(
            &headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::SetupAlreadyCompleted,
            "初始设置已经完成",
        )
        .into_response();
    }

    let Some(setup) = state.setup() else {
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

    let Some(database_setup) = state.database_setup() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    let status = match database_setup.status().await {
        Ok(status) => status,
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "数据库配置不可用",
            )
            .into_response();
        }
    };
    if !status.configured {
        return api_error(
            &headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::DatabaseConfigurationRequired,
            "请先选择数据库",
        )
        .into_response();
    }
    if !status.restart_required {
        return api_error(
            &headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "当前不需要重启 Lux",
        )
        .into_response();
    }

    let Some(restart_handle) = state.restart_handle.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务不支持在线重启",
        )
        .into_response();
    };
    if !restart_handle.request_restart() {
        return api_error(
            &headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "Lux 已经在准备重启",
        )
        .into_response();
    }
    (StatusCode::ACCEPTED, Json(json!({ "restarting": true }))).into_response()
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
    let Some(setup) = state.setup() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };

    if state.database_selection_required {
        let Some(database_setup) = state.database_setup() else {
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
            let library_json_value = match (request.first_library, setup_kind) {
                (Some(first_library), Some(kind)) => {
                    match setup_first_library(&state, &headers, first_library, kind).await {
                        Ok(library) => library,
                        Err(response) => {
                            // The administrator is stored, so setup can never run again: ask for the
                            // restart even though this request answered 4xx. Without it the process
                            // would stay in setup mode and the owner could not create a library.
                            schedule_setup_restart(&state);
                            return response;
                        }
                    }
                }
                _ => None,
            };
            let mut response = json!({
                "initialized": true,
                "user": user_json(&user),
            });
            if let Some(library) = library_json_value {
                response["library"] = library["library"].clone();
                response["warnings"] = library["warnings"].clone();
                response["scanJob"] = library["scanJob"].clone();
            }
            response["restarting"] = json!(schedule_setup_restart(&state));
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

/// Creates the optional first library of a completed setup, with its root and its first scan.
///
/// The caller has already stored the administrator, so every error here is a partial success: the
/// caller schedules the restart and answers the 4xx as before.
async fn setup_first_library(
    state: &AppState,
    headers: &HeaderMap,
    first_library: SetupFirstLibraryRequest,
    kind: LibraryKind,
) -> Result<Option<serde_json::Value>, Response> {
    let Some(libraries) = state.libraries() else {
        return Ok(None);
    };
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
        Err(error) => return Err(library_error(headers, error)),
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
            Err(error) => return Err(library_error(headers, error)),
        }
    }
    let scan_job = match state.as_ready() {
        Some(ready) => spawn_library_scan(&ready, library.id).await,
        None => Ok(None),
    };
    let scan_job = match scan_job {
        Ok(job) => job,
        Err(error) => {
            tracing::warn!(library_id = %library.id, %error, "initial library scan could not be started");
            None
        }
    };
    Ok(Some(json!({
        "library": library_json(&library, &roots),
        "warnings": warnings,
        "scanJob": scan_job.as_ref().map(scan_job_json),
    })))
}

/// Requests the process restart that leaves the setup mode once the response has been written.
///
/// Returns whether the restart was scheduled, which the `201` body reports as `restarting`. A
/// missing handle is not an error: setup already succeeded, so the request only degrades to "no
/// restart" (embedded states and older test fixtures inject no handle).
fn schedule_setup_restart(state: &AppState) -> bool {
    let Some(handle) = state.restart_handle.clone() else {
        tracing::warn!("setup completed without a restart handle");
        return false;
    };
    let delay = state.setup_restart_delay();
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        if !handle.request_restart() {
            // The handle is one-shot, so `false` means another path (database switch) already
            // asked for the same restart.
            tracing::info!("restart was already requested before setup completed");
        }
    });
    true
}

#[derive(Deserialize)]
pub(super) struct AuthLoginRequest {
    username: String,
    password: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct AuthInvitationAcceptRequest {
    token: String,
    username: String,
    #[serde(default)]
    display_name: String,
    password: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct AuthUserRegistrationRequest {
    email: String,
    username: String,
    #[serde(default)]
    display_name: String,
    password: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AuthUserRegistrationTokenRequest {
    token: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AuthUserRegistrationResendRequest {
    email: String,
}

fn web_login_device_id(headers: &HeaderMap) -> Option<String> {
    let header_id = header_str(headers, "x-lux-device-id")?;
    let device_id = uuid::Uuid::parse_str(header_id).ok()?;
    (device_id.get_version_num() == 4).then_some(device_id.to_string())
}

async fn record_web_device_observation(
    state: &ReadyAppState,
    user_id: &str,
    device_id: Option<&str>,
    remote_ip: Option<String>,
) {
    let Some(device_id) = device_id else {
        return;
    };
    let database = state.database();
    if let Err(error) = database
        .upsert_user_device(NewUserDeviceObservation {
            user_id,
            device_id,
            device_name: None,
            device_type: Some("WEB"),
            client_name: "Lux Web",
            client_version: None,
            last_remote_ip: remote_ip.as_deref(),
            seen_at: super::playback::current_unix_timestamp(),
        })
        .await
    {
        tracing::warn!(%error, "failed to update web user device inventory");
    }
}

pub(super) async fn auth_login(
    headers: HeaderMap,
    State(state): State<ReadyAppState>,
    Json(request): Json<AuthLoginRequest>,
) -> Response {
    let auth = state.auth().clone();

    let login_key = login_attempt_key(&headers, &request.username);
    let login_device_id = web_login_device_id(&headers);
    if !state.login_rate_limiter.is_allowed(&login_key).await {
        return api_error(
            &headers,
            StatusCode::UNAUTHORIZED,
            lux::ApiErrorCode::InvalidCredentials,
            "用户名或密码错误",
        )
        .into_response();
    }
    let client_ip = match super::request_access_ip(&headers, &state).await {
        Ok(client_ip) => client_ip,
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
    let mut session = match auth
        .login_with_device_and_ip(
            &request.username,
            &request.password,
            login_device_id.as_deref(),
            client_ip,
        )
        .await
    {
        Ok(session) => session,
        Err(crate::auth::sessions::WebAuthError::AccessDenied) => {
            return api_error(
                &headers,
                StatusCode::FORBIDDEN,
                lux::ApiErrorCode::PermissionDenied,
                "当前设备、客户端或 IP 已被禁止访问",
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
    let mut migration_auth_failed = false;
    if session.is_none()
        && let Some(migration) = Some(state.emby_migration().clone())
    {
        match migration
            .authenticate_pending_user(&request.username, &request.password)
            .await
        {
            Ok(true) => match auth
                .login_with_device_and_ip(
                    &request.username,
                    &request.password,
                    login_device_id.as_deref(),
                    client_ip,
                )
                .await
            {
                Ok(reauthenticated) => session = reauthenticated,
                Err(crate::auth::sessions::WebAuthError::AccessDenied) => {
                    return api_error(
                        &headers,
                        StatusCode::FORBIDDEN,
                        lux::ApiErrorCode::PermissionDenied,
                        "当前设备、客户端或 IP 已被禁止访问",
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
            },
            Ok(false) => {}
            Err(_) => migration_auth_failed = true,
        }
    }
    let Some(session) = session else {
        state.login_rate_limiter.record_failure(&login_key).await;
        if !migration_auth_failed
            && let Ok(Some(user_id)) = auth.find_user_id_by_username(&request.username).await
        {
            record_failed_authentication_event(
                Some(state.database()),
                &state.admin_events,
                &user_id,
                "LUX_WEB",
                request_client_ip(&headers, &state).await,
            )
            .await;
        }
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
    let login_remote_ip = request_client_ip(&headers, &state).await;
    record_web_device_observation(
        &state,
        &user_id,
        login_device_id.as_deref(),
        login_remote_ip,
    )
    .await;
    record_activity_event(
        Some(state.database()),
        &state.admin_events,
        &user_id,
        "AUTH_LOGIN",
        None,
        json!({
            "remoteIp": request_client_ip(&headers, &state).await,
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

pub(super) async fn auth_accept_user_invitation(
    headers: HeaderMap,
    State(state): State<ReadyAppState>,
    request: Result<Json<AuthInvitationAcceptRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(request) = match request {
        Ok(request) => request,
        Err(rejection) if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE => {
            return api_error(
                &headers,
                StatusCode::PAYLOAD_TOO_LARGE,
                lux::ApiErrorCode::InvalidRequest,
                "邀请请求内容过大",
            )
            .into_response();
        }
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "邀请请求无效",
            )
            .into_response();
        }
    };
    let database = state.database();
    let Some(config_dir) = state.config_dir.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "邀请暂时无法处理",
        )
        .into_response();
    };

    match accept_user_invitation(
        database,
        config_dir,
        AcceptUserInvitationInput {
            token: &request.token,
            username: &request.username,
            display_name: &request.display_name,
            password: &request.password,
        },
        super::playback::current_unix_timestamp(),
    )
    .await
    {
        Ok(()) => (StatusCode::CREATED, Json(json!({ "created": true }))).into_response(),
        Err(UserInvitationAcceptanceError::UsernameTaken)
        | Err(UserInvitationAcceptanceError::EmailAlreadyRegistered) => api_error(
            &headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "用户名或邮箱不可用",
        )
        .into_response(),
        Err(UserInvitationAcceptanceError::InvalidInput)
        | Err(UserInvitationAcceptanceError::NotAcceptable) => api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "邀请请求无效或已失效",
        )
        .into_response(),
        Err(UserInvitationAcceptanceError::SigningUnavailable)
        | Err(UserInvitationAcceptanceError::PasswordUnavailable) => api_error(
            &headers,
            StatusCode::INTERNAL_SERVER_ERROR,
            lux::ApiErrorCode::Internal,
            "邀请暂时无法处理",
        )
        .into_response(),
        Err(UserInvitationAcceptanceError::Storage(_)) => api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "邀请暂时无法处理",
        )
        .into_response(),
    }
}

fn user_registration_error(headers: &HeaderMap, error: UserRegistrationError) -> Response {
    match error {
        UserRegistrationError::InvalidEmail
        | UserRegistrationError::InvalidInput
        | UserRegistrationError::NotAcceptable => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "注册请求无效或已失效",
        )
        .into_response(),
        UserRegistrationError::RegistrationClosed => api_error(
            headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::PermissionDenied,
            "公开注册当前不可用",
        )
        .into_response(),
        UserRegistrationError::RateLimited => api_error(
            headers,
            StatusCode::TOO_MANY_REQUESTS,
            lux::ApiErrorCode::RateLimited,
            "注册请求过于频繁，请稍后重试",
        )
        .into_response(),
        UserRegistrationError::PasswordUnavailable | UserRegistrationError::SigningUnavailable => {
            api_error(
                headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::Internal,
                "注册服务暂时不可用",
            )
            .into_response()
        }
        UserRegistrationError::SettingsUnavailable | UserRegistrationError::StorageUnavailable => {
            api_error(
                headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "注册服务暂时不可用",
            )
            .into_response()
        }
    }
}

fn user_email_verification_error(
    headers: &HeaderMap,
    error: UserEmailVerificationError,
) -> Response {
    match error {
        UserEmailVerificationError::InvalidEmail
        | UserEmailVerificationError::InvalidInput
        | UserEmailVerificationError::NotAcceptable => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "邮箱验证请求无效或已失效",
        )
        .into_response(),
        UserEmailVerificationError::RateLimited => api_error(
            headers,
            StatusCode::TOO_MANY_REQUESTS,
            lux::ApiErrorCode::RateLimited,
            "邮箱验证请求过于频繁，请稍后重试",
        )
        .into_response(),
        UserEmailVerificationError::SigningUnavailable => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::Internal,
            "邮箱验证服务暂时不可用",
        )
        .into_response(),
        UserEmailVerificationError::StorageUnavailable => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "邮箱验证服务暂时不可用",
        )
        .into_response(),
    }
}

fn registration_request_body_error(headers: &HeaderMap, too_large: bool) -> Response {
    if too_large {
        api_error(
            headers,
            StatusCode::PAYLOAD_TOO_LARGE,
            lux::ApiErrorCode::InvalidRequest,
            "注册请求内容过大",
        )
        .into_response()
    } else {
        api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "注册请求无效",
        )
        .into_response()
    }
}

fn registration_connection_peer(headers: &HeaderMap) -> Option<std::net::IpAddr> {
    header_str(headers, "x-lux-peer-ip")?.parse().ok()
}

pub(super) async fn auth_submit_public_user_registration(
    headers: HeaderMap,
    State(state): State<ReadyAppState>,
    request: Result<Json<AuthUserRegistrationRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Json(request) = match request {
        Ok(request) => request,
        Err(rejection) => {
            return registration_request_body_error(
                &headers,
                rejection.status() == StatusCode::PAYLOAD_TOO_LARGE,
            );
        }
    };
    let (Some(database), Some(config_dir), Some(connection_peer)) = (
        Some(state.database()),
        state.config_dir.as_deref(),
        registration_connection_peer(&headers),
    ) else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::Internal,
            "注册服务暂时不可用",
        )
        .into_response();
    };

    match submit_public_user_registration(
        database,
        config_dir,
        &state.device_pairing_rate_limiter,
        connection_peer,
        SubmitUserRegistrationInput {
            email: &request.email,
            username: &request.username,
            display_name: &request.display_name,
            password: &request.password,
        },
        super::playback::current_unix_timestamp(),
    )
    .await
    {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(json!({
                "accepted": true,
                "message": "若申请符合条件，我们会发送邮件"
            })),
        )
            .into_response(),
        Err(error) => user_registration_error(&headers, error),
    }
}

pub(super) async fn auth_verify_public_user_registration_email(
    headers: HeaderMap,
    State(state): State<ReadyAppState>,
    request: Result<
        Json<AuthUserRegistrationTokenRequest>,
        axum::extract::rejection::JsonRejection,
    >,
) -> Response {
    let Json(request) = match request {
        Ok(request) => request,
        Err(rejection) => {
            return registration_request_body_error(
                &headers,
                rejection.status() == StatusCode::PAYLOAD_TOO_LARGE,
            );
        }
    };
    let (Some(database), Some(config_dir), Some(connection_peer)) = (
        Some(state.database()),
        state.config_dir.as_deref(),
        registration_connection_peer(&headers),
    ) else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::Internal,
            "邮箱验证暂时无法处理",
        )
        .into_response();
    };
    match verify_public_user_registration_email(
        database,
        config_dir,
        &state.device_pairing_rate_limiter,
        connection_peer,
        &request.token,
        super::playback::current_unix_timestamp(),
    )
    .await
    {
        Ok(()) => (
            StatusCode::OK,
            Json(json!({ "verified": true, "status": "PENDING_APPROVAL" })),
        )
            .into_response(),
        Err(error) => user_registration_error(&headers, error),
    }
}

pub(super) async fn auth_resend_public_user_registration_email(
    headers: HeaderMap,
    State(state): State<ReadyAppState>,
    request: Result<
        Json<AuthUserRegistrationResendRequest>,
        axum::extract::rejection::JsonRejection,
    >,
) -> Response {
    let Json(request) = match request {
        Ok(request) => request,
        Err(rejection) => {
            return registration_request_body_error(
                &headers,
                rejection.status() == StatusCode::PAYLOAD_TOO_LARGE,
            );
        }
    };
    let Some(idempotency_key) = header_str(&headers, "idempotency-key") else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "重发请求缺少幂等键",
        )
        .into_response();
    };
    let (Some(database), Some(connection_peer)) = (
        Some(state.database()),
        registration_connection_peer(&headers),
    ) else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::Internal,
            "注册服务暂时不可用",
        )
        .into_response();
    };

    match resend_public_user_registration_email(
        database,
        &state.device_pairing_rate_limiter,
        connection_peer,
        &request.email,
        idempotency_key,
        super::playback::current_unix_timestamp(),
    )
    .await
    {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(json!({
                "accepted": true,
                "message": "若申请符合条件，我们会发送邮件"
            })),
        )
            .into_response(),
        Err(error) => user_registration_error(&headers, error),
    }
}

pub(super) async fn public_registration_settings(State(state): State<ReadyAppState>) -> Response {
    let database = state.database();
    match crate::application::settings::read_registration_mode(database).await {
        Ok(settings) => Json(json!({ "registrationMode": settings.mode })).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn auth_me(headers: HeaderMap, State(state): State<ReadyAppState>) -> Response {
    if let Some(candidate) = lux_api_key_from_headers(&headers) {
        let service = state.admin_api_key();
        match service.resolve_principal(&candidate).await {
            Ok(Some(principal)) => {
                if state.remote_access.is_remote(
                    header_str(&headers, "x-lux-peer-ip"),
                    header_str(&headers, "x-forwarded-for"),
                ) && !principal.can_remote_access()
                {
                    return api_error(
                        &headers,
                        StatusCode::FORBIDDEN,
                        lux::ApiErrorCode::PermissionDenied,
                        "当前主体不允许远程访问",
                    )
                    .into_response();
                }
                let server_name = current_emby_server_name(&state).await;
                return Json(json!({
                    "principal": {
                        "type": "shared_admin_api_key",
                        "permissions": {
                            "canManageServer": principal.can_manage_server(),
                            "canRemoteAccess": principal.can_remote_access(),
                        },
                    },
                    "serverName": server_name,
                    "imageVariantsEnabled": false,
                }))
                .into_response();
            }
            Ok(None) => {}
            Err(_) => {
                return api_error(
                    &headers,
                    StatusCode::SERVICE_UNAVAILABLE,
                    lux::ApiErrorCode::DatabaseUnavailable,
                    "认证暂时不可用",
                )
                .into_response();
            }
        }
    }
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let server_name = current_emby_server_name(&state).await;
    let image_variants_available = state.image_variants().enabled();
    Json(json!({
        "user": user_json(&user),
        "serverName": server_name,
        "imageVariantsEnabled": image_variants_available,
    }))
    .into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AuthProfilePatch {
    display_name: Option<String>,
    email: Option<String>,
}

fn account_profile_json(
    profile: &crate::auth::users::AdminUserProfile,
    pending_email: Option<&str>,
) -> Value {
    let mut user = user_json(&profile.user);
    user["email"] = json!(profile.email);
    user["emailVerifiedAt"] = json!(profile.email_verified_at);
    user["pendingEmail"] = json!(pending_email);
    json!({ "user": user })
}

pub(super) async fn auth_account_profile(
    headers: HeaderMap,
    State(state): State<ReadyAppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let database = state.database();
    let users = match UserStore::new(database.clone()) {
        Ok(users) => users,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match users.find_admin_profile_by_id(&user.id.to_string()).await {
        Ok(Some(profile)) => {
            let pending_email = match pending_user_email_change(
                database,
                &user.id.to_string(),
                super::playback::current_unix_timestamp(),
            )
            .await
            {
                Ok(pending) => pending.map(|pending| pending.pending_email),
                Err(error) => return user_email_verification_error(&headers, error),
            };
            (
                [("Cache-Control", "no-store")],
                Json(account_profile_json(&profile, pending_email.as_deref())),
            )
                .into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn auth_update_account_profile(
    headers: HeaderMap,
    State(state): State<ReadyAppState>,
    Json(request): Json<AuthProfilePatch>,
) -> Response {
    let session = match require_web_session_csrf(&headers, &state).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    let database = state.database();
    let users = match UserStore::new(database.clone()) {
        Ok(users) => users,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let user_id = session.user.id.to_string();
    let current_profile = match users.find_admin_profile_by_id(&user_id).await {
        Ok(Some(profile)) => profile,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    if let Some(email) = request.email.as_deref().map(str::trim) {
        if email.is_empty() {
            if let Err(error) = clear_user_email_change(
                database,
                &user_id,
                super::playback::current_unix_timestamp(),
            )
            .await
            {
                return user_email_verification_error(&headers, error);
            }
        } else {
            let is_unchanged_verified_email =
                current_profile.email.as_deref().is_some_and(|current| {
                    current_profile.email_verified_at.is_some()
                        && current.eq_ignore_ascii_case(email)
                });
            if !is_unchanged_verified_email {
                let Some(config_dir) = state.config_dir.as_deref() else {
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                };
                if let Err(error) = request_user_email_change(
                    database,
                    config_dir,
                    &state.device_pairing_rate_limiter,
                    &user_id,
                    email,
                    super::playback::current_unix_timestamp(),
                )
                .await
                {
                    return user_email_verification_error(&headers, error);
                }
            }
        }
    }
    let profile = if let Some(display_name) = request.display_name.as_deref() {
        match users
            .update_profile(&user_id, Some(display_name), None)
            .await
        {
            Ok(Some(profile)) => profile,
            Ok(None) => return StatusCode::NOT_FOUND.into_response(),
            Err(_) => {
                return api_error(
                    &headers,
                    StatusCode::SERVICE_UNAVAILABLE,
                    lux::ApiErrorCode::DatabaseUnavailable,
                    "账户资料暂时无法保存",
                )
                .into_response();
            }
        }
    } else {
        match users.find_admin_profile_by_id(&user_id).await {
            Ok(Some(profile)) => profile,
            Ok(None) => return StatusCode::NOT_FOUND.into_response(),
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        }
    };
    let pending_email = match pending_user_email_change(
        database,
        &user_id,
        super::playback::current_unix_timestamp(),
    )
    .await
    {
        Ok(pending) => pending.map(|pending| pending.pending_email),
        Err(error) => return user_email_verification_error(&headers, error),
    };
    (
        [("Cache-Control", "no-store")],
        Json(account_profile_json(&profile, pending_email.as_deref())),
    )
        .into_response()
}

pub(super) async fn auth_resend_account_email_change(
    headers: HeaderMap,
    State(state): State<ReadyAppState>,
) -> Response {
    let session = match require_web_session_csrf(&headers, &state).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    let database = state.database();
    let user_id = session.user.id.to_string();
    let pending = match pending_user_email_change(
        database,
        &user_id,
        super::playback::current_unix_timestamp(),
    )
    .await
    {
        Ok(pending) => pending,
        Err(error) => return user_email_verification_error(&headers, error),
    };
    let Some(pending) = pending else {
        return (
            StatusCode::ACCEPTED,
            [("Cache-Control", "no-store")],
            Json(json!({ "accepted": true })),
        )
            .into_response();
    };
    let Some(config_dir) = state.config_dir.as_deref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match resend_user_email_change(
        database,
        config_dir,
        &state.device_pairing_rate_limiter,
        &user_id,
        &pending,
        super::playback::current_unix_timestamp(),
    )
    .await
    {
        Ok(_) => (
            StatusCode::ACCEPTED,
            [("Cache-Control", "no-store")],
            Json(json!({ "accepted": true })),
        )
            .into_response(),
        Err(error) => user_email_verification_error(&headers, error),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AuthAccountEmailVerificationRequest {
    token: String,
}

pub(super) async fn auth_verify_account_email_change(
    headers: HeaderMap,
    State(state): State<ReadyAppState>,
    Json(request): Json<AuthAccountEmailVerificationRequest>,
) -> Response {
    let session = match require_web_session_csrf(&headers, &state).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    let database = state.database();
    let Some(config_dir) = state.config_dir.as_deref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match verify_user_email_change(
        database,
        config_dir,
        &session.user.id.to_string(),
        &request.token,
        super::playback::current_unix_timestamp(),
    )
    .await
    {
        Ok(()) => (
            StatusCode::OK,
            [("Cache-Control", "no-store")],
            Json(json!({ "verified": true })),
        )
            .into_response(),
        Err(error) => user_email_verification_error(&headers, error),
    }
}

pub(super) async fn auth_create_device_pairing(
    headers: HeaderMap,
    State(state): State<ReadyAppState>,
) -> Response {
    let session = match require_web_session_csrf(&headers, &state).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    let client_ip = pairing_rate_limit_client_ip(&headers, &state).await;
    let rate_key = format!("create:{}:{client_ip}", session.user.id);
    if !state
        .device_pairing_rate_limiter
        .try_acquire(&rate_key, DEVICE_PAIRING_CREATE_LIMIT)
        .await
    {
        return rate_limited_response(&headers);
    }
    let service = state.device_pairings();
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
    State(state): State<ReadyAppState>,
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
    let service = state.device_pairings();
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
    State(state): State<ReadyAppState>,
    Json(request): Json<DevicePairingRedeemRequest>,
) -> Response {
    let client_ip = pairing_rate_limit_client_ip(&headers, &state).await;
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
    let service = state.device_pairings();
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
    state: &ReadyAppState,
) -> Result<AuthenticatedSession, Response> {
    let auth = state.auth();
    let Some(session_token) = request_cookie(headers, "lux_session") else {
        return Err(api_error(
            headers,
            StatusCode::UNAUTHORIZED,
            lux::ApiErrorCode::AuthenticationRequired,
            "需要登录",
        )
        .into_response());
    };
    let session = match resolve_web_session_for_request(headers, state, auth, &session_token).await
    {
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
    let user_id = session.user.id.to_string();
    let device_id = session
        .device_id
        .clone()
        .or_else(|| web_login_device_id(headers));
    let remote_ip = request_client_ip(headers, state).await;
    record_web_device_observation(state, &user_id, device_id.as_deref(), remote_ip).await;
    Ok(session)
}

fn bounded_non_empty(value: &str, max_bytes: usize) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty() && value.len() <= max_bytes).then_some(value)
}

async fn pairing_rate_limit_client_ip(headers: &HeaderMap, state: &ReadyAppState) -> String {
    request_client_ip(headers, state)
        .await
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

pub(super) async fn auth_settings(
    headers: HeaderMap,
    State(state): State<ReadyAppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let database = state.database();
    let libraries = state.libraries();
    let preferences = match get_account_preferences(database, &user.id.to_string()).await {
        Ok(preferences) => preferences,
        Err(error) => return account_preferences_error(&headers, error),
    };
    let (use_admin_library_order, library_order_forced) = match libraries
        .uses_admin_library_order(&user.id.to_string(), user.is_admin)
        .await
    {
        Ok(settings) => settings,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    Json(json!({
        "playedPercent": preferences.played_percent,
        "preferencesVersion": preferences.version,
        "useAdminLibraryOrder": use_admin_library_order,
        "libraryOrderForced": library_order_forced,
    }))
    .into_response()
}

pub(super) async fn auth_account_preferences(
    headers: HeaderMap,
    State(state): State<ReadyAppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let database = state.database();
    let mut preferences = match get_account_preferences(database, &user.id.to_string()).await {
        Ok(preferences) => preferences,
        Err(error) => return account_preferences_error(&headers, error),
    };
    let accessible_library_ids = match library_order_access_scope(
        &state,
        AccessPrincipal::new(user.id, user.is_admin),
    )
    .await
    {
        Ok(ids) => ids,
        Err(status) => return status.into_response(),
    };
    preferences
        .library_order
        .retain(|library_id| accessible_library_ids.contains(library_id));
    Json(preferences).into_response()
}

pub(super) async fn auth_update_account_preferences(
    headers: HeaderMap,
    State(state): State<ReadyAppState>,
    Json(mut request): Json<UpdateAccountPreferences>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_web_csrf(&headers, &state).await {
        return response;
    }
    let database = state.database();
    let accessible_library_ids = match library_order_access_scope(
        &state,
        AccessPrincipal::new(user.id, user.is_admin),
    )
    .await
    {
        Ok(ids) => ids,
        Err(status) => return status.into_response(),
    };
    if let Some(requested_order) = request.library_order.as_deref() {
        let libraries = state.libraries();
        match libraries
            .normalize_library_order(requested_order, &accessible_library_ids)
            .await
        {
            Ok(order) => request.library_order = Some(order),
            Err(LibraryServiceError::InvalidLibraryOrder(message)) => {
                return api_error(
                    &headers,
                    StatusCode::BAD_REQUEST,
                    lux::ApiErrorCode::InvalidRequest,
                    &message,
                )
                .into_response();
            }
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        }
    }
    match update_account_preferences(database, &user.id.to_string(), request).await {
        Ok(mut preferences) => {
            preferences
                .library_order
                .retain(|library_id| accessible_library_ids.contains(library_id));
            {
                let home = state.home();
                home.invalidate();
            }
            state
                .user_events
                .publish_home_coalesced_for(&user.id.to_string())
                .await;
            Json(preferences).into_response()
        }
        Err(error) => account_preferences_error(&headers, error),
    }
}

fn account_preferences_error(headers: &HeaderMap, error: AccountPreferencesError) -> Response {
    let (status, code, message) = match error {
        AccountPreferencesError::InvalidPreferences => (
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "账户偏好设置无效",
        ),
        AccountPreferencesError::VersionConflict => (
            StatusCode::CONFLICT,
            lux::ApiErrorCode::VersionConflict,
            "账户偏好已在其他页面更新，请重新加载后重试",
        ),
        AccountPreferencesError::UserUnavailable => (
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "账户不可用",
        ),
        AccountPreferencesError::Storage(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "账户偏好暂时不可用",
        ),
    };
    api_error(headers, status, code, message).into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AuthSettingsPatch {
    played_percent: Option<i64>,
    use_admin_library_order: Option<bool>,
    expected_version: Option<i64>,
}

pub(super) async fn auth_update_settings(
    headers: HeaderMap,
    State(state): State<ReadyAppState>,
    Json(request): Json<AuthSettingsPatch>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_web_csrf(&headers, &state).await {
        return response;
    }
    if request
        .played_percent
        .is_some_and(|played_percent| !(1..=100).contains(&played_percent))
    {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "自动标记已看百分比必须在 1 到 100 之间",
        )
        .into_response();
    }
    let database = state.database();
    let mut preferences = match get_account_preferences(database, &user.id.to_string()).await {
        Ok(preferences) => preferences,
        Err(error) => return account_preferences_error(&headers, error),
    };
    if request
        .expected_version
        .is_some_and(|expected| expected != preferences.version)
    {
        return account_preferences_error(&headers, AccountPreferencesError::VersionConflict);
    }
    if let Some(played_percent) = request.played_percent {
        match update_account_preferences(
            database,
            &user.id.to_string(),
            UpdateAccountPreferences {
                expected_version: request.expected_version.unwrap_or(preferences.version),
                theme: preferences.theme.clone(),
                accent_color: preferences.accent_color.clone(),
                show_media_libraries: preferences.show_media_libraries,
                show_continue_watching: preferences.show_continue_watching,
                audio_language: preferences.audio_language.clone(),
                subtitle_language: preferences.subtitle_language.clone(),
                auto_play_next_episode: preferences.auto_play_next_episode,
                library_order: None,
                played_percent: Some(played_percent),
            },
        )
        .await
        {
            Ok(updated) => preferences = updated,
            Err(error) => return account_preferences_error(&headers, error),
        }
    }
    if let Some(use_admin_library_order) = request.use_admin_library_order {
        if database
            .set_user_uses_admin_library_order(&user.id.to_string(), use_admin_library_order)
            .await
            .is_err()
        {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        {
            let home = state.home();
            home.invalidate();
        }
        state
            .user_events
            .publish_home_coalesced_for(&user.id.to_string())
            .await;
    }
    let libraries = state.libraries();
    let (use_admin_library_order, library_order_forced) = match libraries
        .uses_admin_library_order(&user.id.to_string(), user.is_admin)
        .await
    {
        Ok(settings) => settings,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    Json(json!({
        "playedPercent": preferences.played_percent,
        "preferencesVersion": preferences.version,
        "useAdminLibraryOrder": use_admin_library_order,
        "libraryOrderForced": library_order_forced,
    }))
    .into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AuthPasswordPatch {
    current_password: String,
    new_password: String,
}

pub(super) async fn auth_update_password(
    headers: HeaderMap,
    State(state): State<ReadyAppState>,
    Json(request): Json<AuthPasswordPatch>,
) -> Response {
    let session = match require_web_session_csrf(&headers, &state).await {
        Ok(session) => session,
        Err(response) => return response,
    };
    if request.current_password.is_empty() || request.new_password.is_empty() {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "当前密码和新密码不能为空",
        )
        .into_response();
    }
    let auth = state.auth();
    // Keep the caller's own session usable after the change; every other credential is revoked.
    let keep_session_token_hash = request_cookie(&headers, "lux_session")
        .map(|token| crate::auth::sessions::WebAuthService::session_token_hash(&token));
    match auth
        .change_password(
            &session.user.id,
            &request.current_password,
            &request.new_password,
            keep_session_token_hash.as_deref(),
        )
        .await
    {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => api_error(
            &headers,
            StatusCode::UNAUTHORIZED,
            lux::ApiErrorCode::InvalidCredentials,
            "当前密码错误",
        )
        .into_response(),
        Err(_) => api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "密码修改暂时不可用",
        )
        .into_response(),
    }
}

fn user_library_order_response(library_order: Vec<String>, preferences_version: i64) -> Response {
    Json(json!({
        "libraryOrder": library_order,
        "preferencesVersion": preferences_version,
    }))
    .into_response()
}

async fn effective_library_order(
    libraries: &crate::application::libraries::LibraryService,
    user_id: &str,
    is_admin: bool,
    accessible_library_ids: &[String],
) -> Result<Vec<String>, LibraryServiceError> {
    libraries
        .saved_library_order_for_user(user_id, is_admin, accessible_library_ids)
        .await
}

async fn library_order_access_scope(
    state: &ReadyAppState,
    principal: AccessPrincipal,
) -> Result<Vec<String>, StatusCode> {
    let access = state.access();
    let mut library_ids = access
        .accessible_library_ids(principal)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let catalog = state.catalog();
    let collections = catalog
        .list_global_collection_cards(principal, 0, 1)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    if collections.total > 0 {
        library_ids.push(crate::storage::GLOBAL_COLLECTION_LIBRARY_ID.to_owned());
    }
    Ok(library_ids)
}

pub(super) async fn auth_library_order(
    headers: HeaderMap,
    State(state): State<ReadyAppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let libraries = state.libraries();
    let database = state.database();
    let preferences = match get_account_preferences(database, &user.id.to_string()).await {
        Ok(preferences) => preferences,
        Err(error) => return account_preferences_error(&headers, error),
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let accessible_library_ids = match library_order_access_scope(&state, principal).await {
        Ok(ids) => ids,
        Err(status) => return status.into_response(),
    };
    match effective_library_order(
        libraries,
        &user.id.to_string(),
        user.is_admin,
        &accessible_library_ids,
    )
    .await
    {
        Ok(library_order) => user_library_order_response(library_order, preferences.version),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LibraryOrderPatch {
    library_order: Vec<String>,
    expected_version: Option<i64>,
}

pub(super) async fn auth_update_library_order(
    headers: HeaderMap,
    State(state): State<ReadyAppState>,
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
    let libraries = state.libraries();
    let database = state.database();
    let preferences = match get_account_preferences(database, &user.id.to_string()).await {
        Ok(preferences) => preferences,
        Err(error) => return account_preferences_error(&headers, error),
    };
    if request
        .expected_version
        .is_some_and(|expected| expected != preferences.version)
    {
        return account_preferences_error(&headers, AccountPreferencesError::VersionConflict);
    }
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let accessible_library_ids = match library_order_access_scope(&state, principal).await {
        Ok(ids) => ids,
        Err(status) => return status.into_response(),
    };
    let (use_admin_library_order, _) = match libraries
        .uses_admin_library_order(&user.id.to_string(), user.is_admin)
        .await
    {
        Ok(settings) => settings,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    if use_admin_library_order && !user.is_admin {
        return match effective_library_order(
            libraries,
            &user.id.to_string(),
            user.is_admin,
            &accessible_library_ids,
        )
        .await
        {
            Ok(library_order) => user_library_order_response(library_order, preferences.version),
            Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
    }
    let library_order = match libraries
        .normalize_library_order(&request.library_order, &accessible_library_ids)
        .await
    {
        Ok(order) => order,
        Err(LibraryServiceError::InvalidLibraryOrder(message)) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                &message,
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match update_account_preferences(
        database,
        &user.id.to_string(),
        UpdateAccountPreferences {
            expected_version: request.expected_version.unwrap_or(preferences.version),
            theme: preferences.theme,
            accent_color: preferences.accent_color,
            show_media_libraries: preferences.show_media_libraries,
            show_continue_watching: preferences.show_continue_watching,
            audio_language: preferences.audio_language,
            subtitle_language: preferences.subtitle_language,
            auto_play_next_episode: preferences.auto_play_next_episode,
            library_order: Some(library_order),
            played_percent: None,
        },
    )
    .await
    {
        Ok(updated) => {
            let library_order = updated.library_order;
            let preferences_version = updated.version;
            {
                let home = state.home();
                home.invalidate();
            }
            state
                .user_events
                .publish_home_coalesced_for(&user.id.to_string())
                .await;
            user_library_order_response(library_order, preferences_version)
        }
        Err(error) => account_preferences_error(&headers, error),
    }
}

pub(super) async fn auth_avatar(
    headers: HeaderMap,
    State(state): State<ReadyAppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let avatars = state.user_avatars();
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
    State(state): State<ReadyAppState>,
    body: Bytes,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_web_csrf(&headers, &state).await {
        return response;
    }
    let avatars = state.user_avatars();
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

pub(super) async fn auth_sessions(
    headers: HeaderMap,
    State(state): State<ReadyAppState>,
) -> Response {
    let auth = state.auth();
    let Some(session_token) = request_cookie(&headers, "lux_session") else {
        return api_error(
            &headers,
            StatusCode::UNAUTHORIZED,
            lux::ApiErrorCode::AuthenticationRequired,
            "需要登录",
        )
        .into_response();
    };
    let session =
        match resolve_web_session_for_request(&headers, &state, auth, &session_token).await {
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
    State(state): State<ReadyAppState>,
) -> Response {
    let auth = state.auth();
    let Some(session_token) = request_cookie(&headers, "lux_session") else {
        return api_error(
            &headers,
            StatusCode::UNAUTHORIZED,
            lux::ApiErrorCode::AuthenticationRequired,
            "需要登录",
        )
        .into_response();
    };
    let session =
        match resolve_web_session_for_request(&headers, &state, auth, &session_token).await {
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

pub(super) async fn auth_logout(
    headers: HeaderMap,
    State(state): State<ReadyAppState>,
) -> Response {
    let auth = state.auth();
    let Some(session_token) = request_cookie(&headers, "lux_session") else {
        return api_error(
            &headers,
            StatusCode::UNAUTHORIZED,
            lux::ApiErrorCode::AuthenticationRequired,
            "需要登录",
        )
        .into_response();
    };
    let session =
        match resolve_web_session_for_request(&headers, &state, auth, &session_token).await {
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
    state: &ReadyAppState,
) -> Result<UserRecord, Response> {
    match require_web_principal(headers, state).await? {
        crate::auth::users::AuthenticationPrincipal::User(user) => Ok(user),
        crate::auth::users::AuthenticationPrincipal::SharedAdminApiKey => Err(api_error(
            headers,
            StatusCode::UNAUTHORIZED,
            lux::ApiErrorCode::AuthenticationRequired,
            "需要用户身份",
        )
        .into_response()),
    }
}

async fn resolve_web_session_for_request(
    headers: &HeaderMap,
    state: &ReadyAppState,
    auth: &crate::auth::sessions::WebAuthService,
    session_token: &str,
) -> Result<Option<AuthenticatedSession>, crate::auth::sessions::WebAuthError> {
    Ok(
        resolve_web_session_with_ip(headers, state, auth, session_token)
            .await?
            .0,
    )
}

async fn resolve_web_session_with_ip(
    headers: &HeaderMap,
    state: &ReadyAppState,
    auth: &crate::auth::sessions::WebAuthService,
    session_token: &str,
) -> Result<
    (Option<AuthenticatedSession>, Option<std::net::IpAddr>),
    crate::auth::sessions::WebAuthError,
> {
    let client_ip = super::request_access_ip(headers, state)
        .await
        .map_err(|_| crate::auth::sessions::WebAuthError::AccessRulesUnavailable)?;
    let session = auth.resolve_with_ip(session_token, client_ip).await?;
    Ok((session, client_ip))
}

pub(super) async fn require_web_principal(
    headers: &HeaderMap,
    state: &ReadyAppState,
) -> Result<crate::auth::users::AuthenticationPrincipal, Response> {
    if has_client_token(headers) {
        if let Some(principal) = resolve_shared_admin_api_key(headers, state).await? {
            if state.remote_access.is_remote(
                header_str(headers, "x-lux-peer-ip"),
                header_str(headers, "x-forwarded-for"),
            ) && !principal.can_remote_access()
            {
                return Err(api_error(
                    headers,
                    StatusCode::FORBIDDEN,
                    lux::ApiErrorCode::PermissionDenied,
                    "当前主体不允许远程访问",
                )
                .into_response());
            }
            return Ok(principal);
        }
        let Some(token) = lux_user_token_from_headers(headers) else {
            return Err(api_error(
                headers,
                StatusCode::UNAUTHORIZED,
                lux::ApiErrorCode::AuthenticationRequired,
                "需要有效的客户端令牌",
            )
            .into_response());
        };
        let auth = state.emby_auth();
        let client_ip = match super::request_access_ip(headers, state).await {
            Ok(client_ip) => client_ip,
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
        return match auth.resolve_token_with_ip(&token, client_ip).await {
            Ok(Some(user)) => {
                if state.remote_access.is_remote(
                    header_str(headers, "x-lux-peer-ip"),
                    header_str(headers, "x-forwarded-for"),
                ) && !user.can_remote_access
                {
                    Err(api_error(
                        headers,
                        StatusCode::FORBIDDEN,
                        lux::ApiErrorCode::PermissionDenied,
                        "当前管理员不允许远程访问",
                    )
                    .into_response())
                } else {
                    Ok(crate::auth::users::AuthenticationPrincipal::User(user))
                }
            }
            Ok(None) => Err(api_error(
                headers,
                StatusCode::UNAUTHORIZED,
                lux::ApiErrorCode::AuthenticationRequired,
                "需要有效的客户端令牌",
            )
            .into_response()),
            Err(crate::auth::emby::EmbyAuthError::AccessRulesUnavailable) => Err(api_error(
                headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "认证暂时不可用",
            )
            .into_response()),
            Err(_) => Err(api_error(
                headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "认证暂时不可用",
            )
            .into_response()),
        };
    }
    let auth = state.auth();
    let Some(session_token) = request_cookie(headers, "lux_session") else {
        return Err(api_error(
            headers,
            StatusCode::UNAUTHORIZED,
            lux::ApiErrorCode::AuthenticationRequired,
            "需要登录",
        )
        .into_response());
    };
    // One rules query covers the IP, device and client checks the request would otherwise run
    // as two separate statements.
    let client_ip = match super::request_access_ip(headers, state).await {
        Ok(client_ip) => client_ip,
        Err(_) => {
            return Err(api_error(
                headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "访问规则暂时无法读取",
            )
            .into_response());
        }
    };
    match auth.resolve_principal(&session_token, client_ip).await {
        Ok(crate::auth::sessions::PrincipalResolution::Session(session)) => {
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
            let user_id = session.user.id.to_string();
            let device_id = session
                .device_id
                .clone()
                .or_else(|| web_login_device_id(headers));
            record_web_device_observation(
                state,
                &user_id,
                device_id.as_deref(),
                client_ip.map(|address| address.to_string()),
            )
            .await;
            Ok(crate::auth::users::AuthenticationPrincipal::User(
                session.user,
            ))
        }
        Ok(crate::auth::sessions::PrincipalResolution::AccessDenied) => {
            if auth.logout(&session_token).await.is_err() {
                return Err(api_error(
                    headers,
                    StatusCode::SERVICE_UNAVAILABLE,
                    lux::ApiErrorCode::DatabaseUnavailable,
                    "登录状态暂时无法更新",
                )
                .into_response());
            }
            Err(api_error(
                headers,
                StatusCode::FORBIDDEN,
                lux::ApiErrorCode::PermissionDenied,
                "当前设备或客户端已被禁止访问",
            )
            .into_response())
        }
        Ok(crate::auth::sessions::PrincipalResolution::Unauthenticated) => Err(api_error(
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
    state: &ReadyAppState,
) -> Result<(), Response> {
    if has_client_token(headers) {
        require_web_user(headers, state).await.map(|_| ())?;
        return Ok(());
    }
    let auth = state.auth();
    let Some(session_token) = request_cookie(headers, "lux_session") else {
        return Err(api_error(
            headers,
            StatusCode::UNAUTHORIZED,
            lux::ApiErrorCode::AuthenticationRequired,
            "需要登录",
        )
        .into_response());
    };
    let session = match resolve_web_session_for_request(headers, state, auth, &session_token).await
    {
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

pub(super) fn has_client_token(headers: &HeaderMap) -> bool {
    lux_api_key_from_headers(headers).is_some() || lux_user_token_from_headers(headers).is_some()
}

pub(super) fn lux_user_token_from_headers(headers: &HeaderMap) -> Option<String> {
    ["X-Lux-Token", "X-Emby-Token", "X-MediaBrowser-Token"]
        .into_iter()
        .find_map(|name| headers.get(name).and_then(|value| value.to_str().ok()))
        .and_then(parse_lux_user_token)
        .or_else(|| {
            headers
                .get("Authorization")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().strip_prefix("Bearer "))
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        })
}

fn parse_lux_user_token(value: &str) -> Option<String> {
    let value = value.trim();
    let value = value.strip_prefix("Bearer ").unwrap_or(value).trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// The setup wizard routes, mounted by both the ready tree and the setup tree.
///
/// Their handlers answer from the shared `AppState` façade, which both trees can resolve.
pub(super) fn setup_routes<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    AppState: FromRef<S>,
{
    Router::new()
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
        .route(
            "/api/v1/setup/database/restart",
            post(users::setup_database_restart),
        )
        .route("/api/v1/setup/complete", post(users::setup_complete))
}

/// The fixed answer of `GET /api/v1/auth/me` while no administrator exists: no session can exist.
pub(super) async fn setup_auth_me(headers: HeaderMap) -> Response {
    api_error(
        &headers,
        StatusCode::UNAUTHORIZED,
        lux::ApiErrorCode::AuthenticationRequired,
        "需要登录",
    )
    .into_response()
}

pub(super) fn api_routes() -> Router<ReadyAppState> {
    // `/health/live`, `/health/ready` and `/api/v1/version` live in
    // `routes::core_routes()` now: both router trees mount them.
    Router::new()
        .route(
            "/api/v1/auth/login-background",
            get(users::login_background),
        )
        .route(
            "/api/v1/auth/login-background/custom-image",
            get(users::login_background_custom_image),
        )
        .route(
            "/api/v1/auth/login",
            post(users::auth_login).layer(DefaultBodyLimit::max(8 * 1024)),
        )
        .route(
            "/api/v1/auth/invitations/accept",
            post(users::auth_accept_user_invitation).layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/v1/auth/registrations/settings",
            get(users::public_registration_settings),
        )
        .route(
            "/api/v1/auth/registrations",
            post(users::auth_submit_public_user_registration)
                .layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/v1/auth/registrations/verify-email",
            post(users::auth_verify_public_user_registration_email)
                .layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route(
            "/api/v1/auth/registrations/resend",
            post(users::auth_resend_public_user_registration_email)
                .layer(DefaultBodyLimit::max(16 * 1024)),
        )
        .route("/api/v1/auth/logout", post(users::auth_logout))
        .route("/api/v1/auth/me", get(users::auth_me))
        .route(
            "/api/v1/auth/profile",
            get(users::auth_account_profile).patch(users::auth_update_account_profile),
        )
        .route(
            "/api/v1/auth/profile/email-verification/resend",
            post(users::auth_resend_account_email_change),
        )
        .route(
            "/api/v1/auth/profile/email-verification/verify",
            post(users::auth_verify_account_email_change).layer(DefaultBodyLimit::max(16 * 1024)),
        )
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
            "/api/v1/auth/account-preferences",
            get(users::auth_account_preferences).patch(users::auth_update_account_preferences),
        )
        .route("/api/v1/auth/password", patch(users::auth_update_password))
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

#[cfg(test)]
mod setup_peer_tests {
    use super::setup_endpoint_allows_peer;
    use axum::http::{HeaderMap, HeaderValue};

    #[test]
    fn setup_endpoints_only_accept_loopback_or_private_peers() {
        let mut headers = HeaderMap::new();
        assert!(
            !setup_endpoint_allows_peer(&headers),
            "a missing peer must fail closed"
        );
        for peer in ["127.0.0.1", "10.1.2.3", "192.168.1.20", "::1", "fd00::1"] {
            headers.insert("x-lux-peer-ip", HeaderValue::from_str(peer).unwrap());
            assert!(setup_endpoint_allows_peer(&headers), "{peer}");
        }
        for peer in ["203.0.113.5", "2001:db8::1"] {
            headers.insert("x-lux-peer-ip", HeaderValue::from_str(peer).unwrap());
            assert!(!setup_endpoint_allows_peer(&headers), "{peer}");
        }
    }
}
