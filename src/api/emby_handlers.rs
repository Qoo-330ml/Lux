use super::*;

use quick_xml::{Reader, escape::unescape, events::Event};

use crate::domain::ids::UserId;

#[derive(Deserialize, Default)]
pub(super) struct DanmakuQuery {
    #[serde(
        rename = "api_key",
        alias = "apiKey",
        alias = "ApiKey",
        alias = "X-Emby-Token",
        alias = "x-emby-token",
        alias = "X-MediaBrowser-Token",
        alias = "x-media-browser-token"
    )]
    api_key: Option<String>,
    option: Option<String>,
}

pub(super) async fn emby_danmaku_info(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<DanmakuQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    match access.can_view_item(principal, &item_id).await {
        Ok(true) => {}
        Ok(false) => return StatusCode::FORBIDDEN.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(service) = state.danmaku.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.read_sidecar(&item_id).await {
        Ok(Some(_)) => Json(json!({
            "hasDanmaku": true,
            "format": "xml",
            "url": format!("/api/danmu/{item_id}/raw"),
            "rawUrl": format!("/api/danmu/{item_id}/raw"),
            "option": query.option,
        }))
        .into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn emby_danmaku_raw(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    Query(query): Query<DanmakuQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    match access.can_view_item(principal, &item_id).await {
        Ok(true) => {}
        Ok(false) => return StatusCode::FORBIDDEN.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(service) = state.danmaku.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.read_sidecar(&item_id).await {
        Ok(Some(bytes)) => Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/xml; charset=utf-8")
            .header("Cache-Control", "private, no-cache")
            .body(Body::from(bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn current_emby_server_name(state: &AppState) -> String {
    let Some(database) = state.database.as_ref() else {
        return DEFAULT_SERVER_NAME.to_owned();
    };
    match database.server_name().await {
        Ok(Some(name)) if !name.trim().is_empty() => name,
        Ok(_) | Err(_) => DEFAULT_SERVER_NAME.to_owned(),
    }
}

pub(super) async fn emby_public_system_info(State(state): State<AppState>) -> Json<Value> {
    let startup_wizard_completed = match state.setup.as_ref() {
        Some(setup) => setup.status().await.unwrap_or(false),
        None => false,
    };
    let server_name = current_emby_server_name(&state).await;
    Json(json!({
        "LocalAddress": "",
        "ServerName": server_name,
        "Version": VERSION,
        "Id": state.server_id,
        "StartupWizardCompleted": startup_wizard_completed
    }))
}

pub(super) async fn emby_system_info(
    headers: HeaderMap,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let Some(auth) = state.emby_auth.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if let Err(status) = require_emby_token(&headers, &query, auth, &state).await {
        return status.into_response();
    }
    let server_name = current_emby_server_name(&state).await;
    Json(json!({
        "LocalAddress": "",
        "ServerName": server_name,
        "Version": VERSION,
        "Id": state.server_id,
        "OperatingSystem": std::env::consts::OS,
        "OperatingSystemDisplayName": std::env::consts::OS,
        "SupportsLibraryMonitor": false,
        "SupportsHttps": false,
        "HasPendingRestart": false,
        "IsShuttingDown": false,
        "HttpServerPortNumber": 8097
    }))
    .into_response()
}

#[derive(Deserialize, Default)]
pub(super) struct EmbyDisplayPreferencesQuery {
    #[serde(flatten)]
    auth: EmbyTokenQuery,
    #[serde(rename = "UserId", alias = "userId", alias = "userid", default)]
    user_id: Option<String>,
    #[serde(rename = "Client", alias = "client", default)]
    client: Option<String>,
}

pub(super) async fn emby_display_preferences(
    headers: HeaderMap,
    Path(display_preferences_id): Path<String>,
    Query(query): Query<EmbyDisplayPreferencesQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user_with_query(&headers, &state, &query.auth).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let requested_user_id = query.user_id.unwrap_or_else(|| user.id.to_string());
    if let Err(status) = ensure_emby_user_scope(&user, &requested_user_id) {
        return status.into_response();
    }
    let Some(client) = query
        .client
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };

    Json(json!({
        "Id": display_preferences_id,
        "ViewType": "Poster",
        "SortBy": "SortName",
        "IndexBy": serde_json::Value::Null,
        "RememberIndexing": false,
        "PrimaryImageHeight": 250,
        "PrimaryImageWidth": 250,
        "CustomPrefs": {},
        "ScrollDirection": "Horizontal",
        "ShowBackdrop": true,
        "RememberSorting": false,
        "SortOrder": "Ascending",
        "ShowSidebar": false,
        "Client": client,
    }))
    .into_response()
}

pub(super) async fn emby_ping(
    _headers: HeaderMap,
    Query(_query): Query<EmbyTokenQuery>,
    State(_state): State<AppState>,
) -> Response {
    StatusCode::OK.into_response()
}

pub(super) async fn emby_public_users(State(state): State<AppState>) -> Json<Value> {
    let server_id = state.server_id.clone();
    let Some(auth) = state.emby_auth.as_ref() else {
        return Json(json!([]));
    };
    let server_name = current_emby_server_name(&state).await;
    let users = auth.public_users().await.unwrap_or_default();
    Json(Value::Array(
        users
            .iter()
            .map(|user| emby_user_json(user, &server_id, &server_name, &[]))
            .collect(),
    ))
}

pub(super) async fn emby_users(
    headers: HeaderMap,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if !user.can_manage_server {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(auth) = state.emby_auth.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let users = match auth.public_users().await {
        Ok(users) => users,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let server_name = current_emby_server_name(&state).await;
    Json(Value::Array(
        users
            .iter()
            .map(|user| emby_user_json(user, &state.server_id, &server_name, &[]))
            .collect(),
    ))
    .into_response()
}

pub(super) async fn emby_user(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if user_id.parse::<UserId>().is_err() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let user = if user.id.to_string() == user_id {
        user
    } else {
        if !user.can_manage_server {
            return StatusCode::FORBIDDEN.into_response();
        }
        let Some(auth) = state.emby_auth.as_ref() else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        match auth.user_by_id(&user_id).await {
            Ok(Some(user)) => user,
            Ok(None) => return StatusCode::NOT_FOUND.into_response(),
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        }
    };
    let server_name = current_emby_server_name(&state).await;
    let ordered_views = emby_ordered_views(&state, &user).await;
    Json(emby_user_json(
        &user,
        &state.server_id,
        &server_name,
        &ordered_views,
    ))
    .into_response()
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
pub(super) struct EmbyUserUpdateRequest {
    id: Option<String>,
    name: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
pub(super) struct EmbyUserPolicyRequest {
    is_administrator: Option<bool>,
    is_disabled: Option<bool>,
    enable_remote_access: Option<bool>,
    enable_content_downloading: Option<bool>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
pub(super) struct EmbyUpdateUserPasswordRequest {
    id: Option<String>,
    current_pw: Option<String>,
    new_pw: Option<String>,
    reset_password: Option<bool>,
}

#[derive(Deserialize, Default)]
pub(super) struct EmbyUserImageQuery {
    #[serde(flatten)]
    auth: EmbyTokenQuery,
    #[serde(rename = "Index", alias = "index")]
    index: Option<i32>,
}

fn emby_request_content_type(headers: &HeaderMap) -> &str {
    headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .unwrap_or_default()
}

fn parse_emby_xml_fields(body: &[u8]) -> Result<HashMap<String, String>, StatusCode> {
    let mut reader = Reader::from_reader(body);
    reader.config_mut().trim_text(true);
    let mut buffer = Vec::new();
    let mut stack = Vec::new();
    let mut fields = HashMap::new();

    loop {
        match reader.read_event_into(&mut buffer) {
            Ok(Event::Start(element)) => {
                stack.push(xml_element_name(element.name().as_ref())?);
            }
            Ok(Event::Empty(element)) => {
                fields.insert(xml_element_name(element.name().as_ref())?, String::new());
            }
            Ok(Event::Text(text)) => {
                if let Some(field) = stack.last() {
                    let decoded = text.decode().map_err(|_| StatusCode::BAD_REQUEST)?;
                    let value = unescape(decoded.as_ref())
                        .map_err(|_| StatusCode::BAD_REQUEST)?
                        .into_owned();
                    fields.insert(field.clone(), value);
                }
            }
            Ok(Event::CData(text)) => {
                if let Some(field) = stack.last() {
                    let value = text
                        .decode()
                        .map_err(|_| StatusCode::BAD_REQUEST)?
                        .into_owned();
                    fields.insert(field.clone(), value);
                }
            }
            Ok(Event::End(_)) => {
                if stack.pop().is_none() {
                    return Err(StatusCode::BAD_REQUEST);
                }
            }
            Ok(Event::Eof) => break,
            Ok(
                Event::Decl(_)
                | Event::Comment(_)
                | Event::PI(_)
                | Event::DocType(_)
                | Event::GeneralRef(_),
            ) => {}
            Err(_) => return Err(StatusCode::BAD_REQUEST),
        }
        buffer.clear();
    }

    if stack.is_empty() {
        Ok(fields)
    } else {
        Err(StatusCode::BAD_REQUEST)
    }
}

fn xml_element_name(name: &[u8]) -> Result<String, StatusCode> {
    let name = std::str::from_utf8(name).map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(name.rsplit(':').next().unwrap_or(name).to_owned())
}

fn xml_optional_bool(
    fields: &HashMap<String, String>,
    name: &str,
) -> Result<Option<bool>, StatusCode> {
    let Some(value) = fields.get(name) else {
        return Ok(None);
    };
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" => Ok(Some(true)),
        "false" | "0" => Ok(Some(false)),
        _ => Err(StatusCode::BAD_REQUEST),
    }
}

fn parse_emby_user_update_request(
    headers: &HeaderMap,
    body: &[u8],
) -> Result<EmbyUserUpdateRequest, StatusCode> {
    match emby_request_content_type(headers) {
        "application/json" => serde_json::from_slice(body).map_err(|_| StatusCode::BAD_REQUEST),
        "application/xml" | "text/xml" => {
            let fields = parse_emby_xml_fields(body)?;
            Ok(EmbyUserUpdateRequest {
                id: fields.get("Id").cloned(),
                name: fields.get("Name").cloned(),
            })
        }
        _ => Err(StatusCode::UNSUPPORTED_MEDIA_TYPE),
    }
}

fn parse_emby_user_policy_request(
    headers: &HeaderMap,
    body: &[u8],
) -> Result<EmbyUserPolicyRequest, StatusCode> {
    match emby_request_content_type(headers) {
        "application/json" => serde_json::from_slice(body).map_err(|_| StatusCode::BAD_REQUEST),
        "application/xml" | "text/xml" => {
            let fields = parse_emby_xml_fields(body)?;
            Ok(EmbyUserPolicyRequest {
                is_administrator: xml_optional_bool(&fields, "IsAdministrator")?,
                is_disabled: xml_optional_bool(&fields, "IsDisabled")?,
                enable_remote_access: xml_optional_bool(&fields, "EnableRemoteAccess")?,
                enable_content_downloading: xml_optional_bool(&fields, "EnableContentDownloading")?,
            })
        }
        _ => Err(StatusCode::UNSUPPORTED_MEDIA_TYPE),
    }
}

fn parse_emby_password_request(
    headers: &HeaderMap,
    body: &[u8],
) -> Result<EmbyUpdateUserPasswordRequest, StatusCode> {
    match emby_request_content_type(headers) {
        "application/json" => serde_json::from_slice(body).map_err(|_| StatusCode::BAD_REQUEST),
        "application/xml" | "text/xml" => {
            let fields = parse_emby_xml_fields(body)?;
            Ok(EmbyUpdateUserPasswordRequest {
                id: fields.get("Id").cloned(),
                current_pw: fields.get("CurrentPw").cloned(),
                new_pw: fields.get("NewPw").cloned(),
                reset_password: xml_optional_bool(&fields, "ResetPassword")?,
            })
        }
        _ => Err(StatusCode::UNSUPPORTED_MEDIA_TYPE),
    }
}

fn check_emby_target_id(body_id: Option<&str>, path_id: &str) -> Result<(), StatusCode> {
    if body_id.is_some_and(|body_id| body_id != path_id) || path_id.parse::<UserId>().is_err() {
        Err(StatusCode::BAD_REQUEST)
    } else {
        Ok(())
    }
}

fn check_emby_image_index(index: Option<i32>) -> Result<(), StatusCode> {
    match index {
        None | Some(0) => Ok(()),
        Some(index) if index < 0 => Err(StatusCode::BAD_REQUEST),
        Some(_) => Err(StatusCode::NOT_FOUND),
    }
}

fn check_primary_user_image(image_type: &str) -> Result<(), StatusCode> {
    if image_type.eq_ignore_ascii_case("Primary") {
        Ok(())
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

pub(super) async fn emby_update_user(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
    body: Bytes,
) -> Response {
    let request = match parse_emby_user_update_request(&headers, &body) {
        Ok(request) => request,
        Err(status) => return status.into_response(),
    };
    if let Err(status) = check_emby_target_id(request.id.as_deref(), &user_id) {
        return status.into_response();
    }
    let acting_user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if !acting_user.can_manage_server && acting_user.id.to_string() != user_id {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let users = match UserStore::new(database.clone()) {
        Ok(users) => users,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let Some(name) = request.name.as_deref() else {
        return StatusCode::OK.into_response();
    };
    if name.trim().is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    match users
        .update_user(
            &user_id,
            UserUpdate {
                display_name: Some(name),
                ..UserUpdate::default()
            },
        )
        .await
    {
        Ok(Some(_)) => StatusCode::OK.into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(UserStoreError::LastManager) => StatusCode::CONFLICT.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn emby_update_user_policy(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
    body: Bytes,
) -> Response {
    let request = match parse_emby_user_policy_request(&headers, &body) {
        Ok(request) => request,
        Err(status) => return status.into_response(),
    };
    if let Err(status) = check_emby_target_id(None, &user_id) {
        return status.into_response();
    }
    let acting_user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if !acting_user.can_manage_server {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let users = match UserStore::new(database.clone()) {
        Ok(users) => users,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match users
        .update_user(
            &user_id,
            UserUpdate {
                is_admin: request.is_administrator,
                can_manage_server: request.is_administrator,
                is_disabled: request.is_disabled,
                can_remote_access: request.enable_remote_access,
                can_download: request.enable_content_downloading,
                ..UserUpdate::default()
            },
        )
        .await
    {
        Ok(Some(_)) => StatusCode::OK.into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(UserStoreError::LastManager) => StatusCode::CONFLICT.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn emby_update_user_password(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
    body: Bytes,
) -> Response {
    let request = match parse_emby_password_request(&headers, &body) {
        Ok(request) => request,
        Err(status) => return status.into_response(),
    };
    if let Err(status) = check_emby_target_id(request.id.as_deref(), &user_id) {
        return status.into_response();
    }
    let acting_user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if !acting_user.can_manage_server && acting_user.id.to_string() != user_id {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(new_password) = request.new_pw.as_deref() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if new_password.is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let users = match UserStore::new(database.clone()) {
        Ok(users) => users,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    if let Some(current_password) = request.current_pw.as_deref() {
        match users
            .authenticate(&acting_user.username_normalized, current_password)
            .await
        {
            Ok(Some(_)) => {}
            Ok(None) => return StatusCode::FORBIDDEN.into_response(),
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        }
    }
    let _ = request.reset_password;
    match users
        .update_user(
            &user_id,
            UserUpdate {
                password: Some(new_password),
                ..UserUpdate::default()
            },
        )
        .await
    {
        Ok(Some(_)) => StatusCode::OK.into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(UserStoreError::LastManager) => StatusCode::CONFLICT.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn emby_user_avatar_response(
    image_type: &str,
    user_id: &str,
    index: Option<i32>,
    state: &AppState,
    head_only: bool,
) -> Response {
    if let Err(status) = check_primary_user_image(image_type)
        .and_then(|_| check_emby_target_id(None, user_id))
        .and_then(|_| check_emby_image_index(index))
    {
        return status.into_response();
    }
    let Some(avatars) = state.user_avatars.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(target_user_id) = user_id.parse::<UserId>().ok() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    match avatars.load(target_user_id).await {
        Ok(Some(avatar)) => {
            let builder = Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, avatar.content_type)
                .header(CACHE_CONTROL, "private, no-cache")
                .header("Content-Length", avatar.bytes.len().to_string());
            let body = if head_only {
                Body::empty()
            } else {
                Body::from(avatar.bytes)
            };
            builder
                .body(body)
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn emby_user_avatar(
    Path((user_id, image_type)): Path<(String, String)>,
    Query(query): Query<EmbyUserImageQuery>,
    State(state): State<AppState>,
) -> Response {
    let _ = query.auth;
    emby_user_avatar_response(&image_type, &user_id, query.index, &state, false).await
}

pub(super) async fn emby_user_avatar_head(
    Path((user_id, image_type)): Path<(String, String)>,
    Query(query): Query<EmbyUserImageQuery>,
    State(state): State<AppState>,
) -> Response {
    let _ = query.auth;
    emby_user_avatar_response(&image_type, &user_id, query.index, &state, true).await
}

async fn update_emby_user_avatar(
    headers: HeaderMap,
    user_id: String,
    image_type: String,
    query: EmbyUserImageQuery,
    state: AppState,
    body: Bytes,
    index: Option<i32>,
) -> Response {
    if let Err(status) = check_primary_user_image(&image_type)
        .and_then(|_| check_emby_target_id(None, &user_id))
        .and_then(|_| check_emby_image_index(index))
    {
        return status.into_response();
    }
    let user = match require_emby_user(&headers, &state, query.auth.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if !user.can_manage_server && user.id.to_string() != user_id {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(avatars) = state.user_avatars.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    match avatars
        .store(
            match user_id.parse::<UserId>() {
                Ok(user_id) => user_id,
                Err(_) => return StatusCode::BAD_REQUEST.into_response(),
            },
            content_type,
            &body,
        )
        .await
    {
        Ok(()) => StatusCode::OK.into_response(),
        Err(UserAvatarError::UnsupportedContentType | UserAvatarError::InvalidContent) => {
            StatusCode::BAD_REQUEST.into_response()
        }
        Err(UserAvatarError::TooLarge { .. }) => StatusCode::PAYLOAD_TOO_LARGE.into_response(),
        Err(UserAvatarError::InvalidPath(_) | UserAvatarError::Io { .. }) => {
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

pub(super) async fn emby_update_user_avatar(
    headers: HeaderMap,
    Path((user_id, image_type)): Path<(String, String)>,
    Query(query): Query<EmbyUserImageQuery>,
    State(state): State<AppState>,
    body: Bytes,
) -> Response {
    let index = query.index;
    update_emby_user_avatar(headers, user_id, image_type, query, state, body, index).await
}

pub(super) async fn emby_user_avatar_at_index(
    Path((user_id, image_type, image_index)): Path<(String, String, i32)>,
    Query(_query): Query<EmbyUserImageQuery>,
    State(state): State<AppState>,
) -> Response {
    emby_user_avatar_response(&image_type, &user_id, Some(image_index), &state, false).await
}

pub(super) async fn emby_user_avatar_at_index_head(
    Path((user_id, image_type, image_index)): Path<(String, String, i32)>,
    Query(_query): Query<EmbyUserImageQuery>,
    State(state): State<AppState>,
) -> Response {
    emby_user_avatar_response(&image_type, &user_id, Some(image_index), &state, true).await
}

pub(super) async fn emby_update_user_avatar_at_index(
    headers: HeaderMap,
    Path((user_id, image_type, image_index)): Path<(String, String, i32)>,
    Query(query): Query<EmbyUserImageQuery>,
    State(state): State<AppState>,
    body: Bytes,
) -> Response {
    update_emby_user_avatar(
        headers,
        user_id,
        image_type,
        query,
        state,
        body,
        Some(image_index),
    )
    .await
}

pub(super) async fn emby_delete_user_avatar_at_index(
    headers: HeaderMap,
    Path((user_id, image_type, image_index)): Path<(String, String, i32)>,
    Query(query): Query<EmbyUserImageQuery>,
    State(state): State<AppState>,
) -> Response {
    delete_emby_user_avatar(
        headers,
        user_id,
        image_type,
        query,
        state,
        Some(image_index),
    )
    .await
}

async fn delete_emby_user_avatar(
    headers: HeaderMap,
    user_id: String,
    image_type: String,
    query: EmbyUserImageQuery,
    state: AppState,
    index: Option<i32>,
) -> Response {
    if let Err(status) = check_primary_user_image(&image_type)
        .and_then(|_| check_emby_target_id(None, &user_id))
        .and_then(|_| check_emby_image_index(index))
    {
        return status.into_response();
    }
    let user = match require_emby_user(&headers, &state, query.auth.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if !user.can_manage_server && user.id.to_string() != user_id {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(avatars) = state.user_avatars.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(target_user_id) = user_id.parse::<UserId>().ok() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    match avatars.remove(target_user_id).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn emby_delete_user_avatar(
    headers: HeaderMap,
    Path((user_id, image_type)): Path<(String, String)>,
    Query(query): Query<EmbyUserImageQuery>,
    State(state): State<AppState>,
) -> Response {
    let index = query.index;
    delete_emby_user_avatar(headers, user_id, image_type, query, state, index).await
}

pub(super) async fn emby_ordered_views(state: &AppState, user: &UserRecord) -> Vec<String> {
    let (Some(libraries), Some(access)) = (state.libraries.as_ref(), state.access.as_ref()) else {
        return Vec::new();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let Ok(accessible_library_ids) = access.accessible_library_ids(principal).await else {
        return Vec::new();
    };
    libraries
        .saved_library_order_for_user(&user.id.to_string(), &accessible_library_ids)
        .await
        .unwrap_or_default()
}

#[derive(Deserialize)]
pub(super) struct EmbyAuthenticateRequest {
    #[serde(rename = "Username")]
    username: String,
    #[serde(rename = "Pw")]
    password: String,
}

pub(super) async fn emby_authenticate(
    headers: HeaderMap,
    State(state): State<AppState>,
    body: Bytes,
) -> Response {
    let request = match parse_emby_authenticate_request(&headers, &body) {
        Ok(request) => request,
        Err(status) => return status.into_response(),
    };
    let Some(auth) = state.emby_auth.clone() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let login_key = login_attempt_key(&headers, &request.username);
    if !state.login_rate_limiter.is_allowed(&login_key).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let device = emby_device_info_from_headers(&headers);
    match auth
        .authenticate(&request.username, &request.password, &device)
        .await
    {
        Ok(Some(result)) => {
            if state.remote_access.is_remote(
                header_str(&headers, "x-lux-peer-ip"),
                header_str(&headers, "x-forwarded-for"),
            ) && !result.user.can_remote_access
            {
                let _ = auth.logout(&result.token).await;
                return StatusCode::FORBIDDEN.into_response();
            }
            state.login_rate_limiter.record_success(&login_key).await;
            let user_id = result.user.id.to_string();
            record_activity_event(
                state.database.as_ref(),
                &state.admin_events,
                &user_id,
                "AUTH_LOGIN",
                None,
                json!({
                    "client": result.device.client,
                    "clientVersion": result.device.version,
                    "deviceName": result.device.device,
                    "deviceType": result.device.device,
                    "remoteIp": request_client_ip(&headers, &state.remote_access),
                }),
            )
            .await;
            let server_name = current_emby_server_name(&state).await;
            let ordered_views = emby_ordered_views(&state, &result.user).await;
            Json(json!({
                "User": emby_user_json(&result.user, &state.server_id, &server_name, &ordered_views),
                "SessionInfo": emby_login_session_json(&result, &state.server_id),
                "AccessToken": result.token,
                "ServerId": state.server_id
            }))
            .into_response()
        }
        Ok(None) => {
            state.login_rate_limiter.record_failure(&login_key).await;
            StatusCode::UNAUTHORIZED.into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub(super) fn parse_emby_authenticate_request(
    headers: &HeaderMap,
    body: &[u8],
) -> Result<EmbyAuthenticateRequest, StatusCode> {
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .unwrap_or_default();

    match content_type {
        "application/json" => serde_json::from_slice(body).map_err(|_| StatusCode::BAD_REQUEST),
        "application/x-www-form-urlencoded" => {
            let mut username = None;
            let mut password = None;
            for (key, value) in url::form_urlencoded::parse(body) {
                match key.as_ref() {
                    "Username" => username = Some(value.into_owned()),
                    "Pw" => password = Some(value.into_owned()),
                    _ => {}
                }
            }
            Ok(EmbyAuthenticateRequest {
                username: username.ok_or(StatusCode::BAD_REQUEST)?,
                password: password.ok_or(StatusCode::BAD_REQUEST)?,
            })
        }
        _ => Err(StatusCode::UNSUPPORTED_MEDIA_TYPE),
    }
}

#[derive(Deserialize, Default)]
pub(super) struct EmbyTokenQuery {
    #[serde(
        rename = "api_key",
        alias = "apiKey",
        alias = "ApiKey",
        alias = "X-Emby-Token",
        alias = "x-emby-token",
        alias = "X-MediaBrowser-Token",
        alias = "x-media-browser-token"
    )]
    pub(super) api_key: Option<String>,
    #[serde(rename = "tag", alias = "Tag")]
    pub(super) tag: Option<String>,
    #[serde(rename = "Fields", default)]
    pub(super) fields: Option<String>,
}

#[derive(Deserialize, Default)]
pub(super) struct EmbyPersonsQuery {
    #[serde(flatten)]
    pub(super) auth: EmbyTokenQuery,
    #[serde(rename = "UserId", alias = "userId", alias = "userid", default)]
    pub(super) user_id: Option<String>,
    #[serde(rename = "ParentId", alias = "parentId", default)]
    pub(super) parent_id: Option<String>,
    #[serde(rename = "PersonTypes", alias = "personTypes", default)]
    pub(super) person_types: Option<String>,
    #[serde(rename = "StartIndex", alias = "startIndex", default)]
    pub(super) start_index: Option<i64>,
    #[serde(rename = "Limit", alias = "limit", default)]
    pub(super) limit: Option<i64>,
    #[serde(
        rename = "Recursive",
        alias = "recursive",
        default,
        deserialize_with = "deserialize_optional_bool"
    )]
    pub(super) recursive: Option<bool>,
    #[serde(rename = "SortBy", alias = "sortBy", default)]
    pub(super) sort_by: Option<String>,
    #[serde(rename = "SortOrder", alias = "sortOrder", default)]
    pub(super) sort_order: Option<String>,
}

#[derive(Deserialize, Default)]
pub(super) struct EmbyPersonQuery {
    #[serde(flatten)]
    pub(super) auth: EmbyTokenQuery,
    #[serde(rename = "UserId", alias = "userId", alias = "userid", default)]
    pub(super) user_id: Option<String>,
}

pub(super) async fn require_emby_token(
    headers: &HeaderMap,
    query: &EmbyTokenQuery,
    auth: &EmbyAuthService,
    state: &AppState,
) -> Result<(), StatusCode> {
    let user = resolve_emby_user_with_auth(headers, query, auth, state).await?;
    if state.remote_access.is_remote(
        header_str(headers, "x-lux-peer-ip"),
        header_str(headers, "x-forwarded-for"),
    ) && !user.can_remote_access
    {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(())
}

pub(super) async fn resolve_emby_user_with_auth(
    headers: &HeaderMap,
    query: &EmbyTokenQuery,
    auth: &EmbyAuthService,
    state: &AppState,
) -> Result<UserRecord, StatusCode> {
    let token = emby_token_from_headers(headers)
        .or_else(|| query.api_key.clone())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    if let Some(service) = state.admin_api_key.as_ref() {
        match service.resolve(&token).await {
            Ok(Some(user)) => return Ok(user),
            Ok(None) => {}
            Err(_) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
        }
    }
    match auth.resolve_token(&token).await {
        Ok(Some(user)) => Ok(user),
        Ok(None) => Err(StatusCode::UNAUTHORIZED),
        Err(_) => Err(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

pub(super) fn emby_token_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get("X-Lux-Api-Key")
        .and_then(|value| value.to_str().ok())
        .and_then(emby_token_header_value)
        .or_else(|| {
            headers
                .get("X-Emby-Token")
                .or_else(|| headers.get("X-MediaBrowser-Token"))
                .and_then(|value| value.to_str().ok())
                .and_then(emby_token_header_value)
        })
        .or_else(|| {
            headers
                .get("X-Emby-Authorization")
                .and_then(|value| value.to_str().ok())
                .and_then(emby_authorization_token)
        })
        .or_else(|| {
            headers
                .get("X-Emby-Authentication")
                .and_then(|value| value.to_str().ok())
                .and_then(emby_authorization_token)
        })
        .or_else(|| {
            headers
                .get("Authorization")
                .and_then(|value| value.to_str().ok())
                .and_then(emby_token_header_value)
        })
}

pub(super) fn emby_device_info_from_headers(headers: &HeaderMap) -> EmbyDeviceInfo {
    let mut info = EmbyDeviceInfo::default();
    for name in [
        "X-Emby-Authorization",
        "X-Emby-Authentication",
        "Authorization",
    ] {
        let Some(candidate) = headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(EmbyDeviceInfo::parse)
        else {
            continue;
        };
        merge_emby_device_info(&mut info, candidate);
    }
    info
}

pub(super) fn merge_emby_device_info(target: &mut EmbyDeviceInfo, fallback: EmbyDeviceInfo) {
    if target.client.is_empty() {
        target.client = fallback.client;
    }
    if target.device.is_empty() {
        target.device = fallback.device;
    }
    if target.device_id.is_empty() {
        target.device_id = fallback.device_id;
    }
    if target.version.is_empty() {
        target.version = fallback.version;
    }
    if target.user_id.is_none() {
        target.user_id = fallback.user_id;
    }
}

pub(super) fn emby_token_header_value(value: &str) -> Option<String> {
    let value = value.trim();
    if let Some(token) = value.strip_prefix("Bearer ") {
        return (!token.is_empty()).then(|| token.to_owned());
    }
    emby_authorization_token(value).or_else(|| (!value.is_empty()).then(|| value.to_owned()))
}

pub(super) fn emby_authorization_token(value: &str) -> Option<String> {
    let parameters = value
        .split_once(' ')
        .map_or(value, |(_, parameters)| parameters);
    parameters.split(',').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        if !key.trim().eq_ignore_ascii_case("Token") {
            return None;
        }
        let token = value.trim().trim_matches('"');
        (!token.is_empty()).then(|| token.to_owned())
    })
}

pub(super) async fn require_emby_user(
    headers: &HeaderMap,
    state: &AppState,
    api_key: Option<&str>,
) -> Result<UserRecord, StatusCode> {
    let query = EmbyTokenQuery {
        api_key: api_key.map(str::to_owned),
        tag: None,
        fields: None,
    };
    require_emby_user_with_query(headers, state, &query).await
}

pub(super) async fn require_emby_user_with_query(
    headers: &HeaderMap,
    state: &AppState,
    query: &EmbyTokenQuery,
) -> Result<UserRecord, StatusCode> {
    let Some(auth) = state.emby_auth.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let user = resolve_emby_user_with_auth(headers, query, auth, state).await?;
    if state.remote_access.is_remote(
        header_str(headers, "x-lux-peer-ip"),
        header_str(headers, "x-forwarded-for"),
    ) && !user.can_remote_access
    {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(user)
}

pub(super) async fn emby_logout(
    headers: HeaderMap,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> StatusCode {
    let Some(auth) = state.emby_auth else {
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    let token = headers
        .get("X-Emby-Token")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .or(query.api_key);
    let Some(token) = token else {
        return StatusCode::UNAUTHORIZED;
    };
    match auth.logout(&token).await {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

fn emby_user_json(
    user: &UserRecord,
    server_id: &str,
    server_name: &str,
    ordered_views: &[String],
) -> Value {
    json!({
        "Id": user.id.to_string(),
        "ServerId": server_id,
        "ServerName": server_name,
        "Name": user.display_name,
        "HasPassword": true,
        "HasConfiguredPassword": true,
        "HasConfiguredEasyPassword": false,
        "EnableAutoLogin": false,
        "LastLoginDate": "1970-01-01T00:00:00.0000000Z",
        "LastActivityDate": "1970-01-01T00:00:00.0000000Z",
        "Configuration": emby_user_configuration_json(ordered_views),
        "Policy": emby_user_policy_json(user),
    })
}

fn emby_user_configuration_json(ordered_views: &[String]) -> Value {
    json!({
        "AudioLanguagePreference": "",
        "PlayDefaultAudioTrack": true,
        "SubtitleLanguagePreference": "",
        "DisplayMissingEpisodes": false,
        "GroupedFolders": [],
        "SubtitleMode": "Default",
        "DisplayCollectionsView": true,
        "EnableLocalPassword": false,
        "OrderedViews": ordered_views,
        "LatestItemsExcludes": [],
        "MyMediaExcludes": [],
        "HidePlayedInLatest": false,
        "RememberAudioSelections": true,
        "RememberSubtitleSelections": true,
        "EnableNextEpisodeAutoPlay": true,
    })
}

fn emby_user_policy_json(user: &UserRecord) -> Value {
    json!({
        "IsAdministrator": user.is_admin,
        "IsHidden": false,
        "IsHiddenRemotely": false,
        "IsDisabled": user.is_disabled,
        "MaxParentalRating": null,
        "BlockedTags": [],
        "EnableUserPreferenceAccess": true,
        "AccessSchedules": [],
        "BlockUnratedItems": [],
        "EnableRemoteControlOfOtherUsers": false,
        "EnableSharedDeviceControl": true,
        "EnableRemoteAccess": user.can_remote_access,
        "EnableLiveTvManagement": false,
        "EnableLiveTvAccess": false,
        "EnableMediaPlayback": true,
        "EnableAudioPlaybackTranscoding": false,
        "EnableVideoPlaybackTranscoding": false,
        "EnablePlaybackRemuxing": false,
        "EnableContentDeletion": false,
        "EnableContentDeletionFromFolders": [],
        "EnableContentDownloading": user.can_download,
        "EnableSubtitleDownloading": false,
        "EnableSubtitleManagement": false,
        "EnableSyncTranscoding": false,
        "EnableMediaConversion": false,
        "EnabledDevices": [],
        "EnableAllDevices": true,
        "EnabledChannels": [],
        "EnableAllChannels": false,
        "EnabledFolders": [],
        "EnableAllFolders": true,
        "InvalidLoginAttemptCount": 0,
        "EnablePublicSharing": false,
        "BlockedMediaFolders": [],
        "BlockedChannels": [],
        "RemoteClientBitrateLimit": 0,
        "AuthenticationProviderId": "Lux",
        "ExcludedSubFolders": [],
        "DisablePremiumFeatures": true,
    })
}

fn emby_login_session_json(result: &crate::auth::emby::EmbyAuthResult, server_id: &str) -> Value {
    json!({
        "Id": result.session_id,
        "ServerId": server_id,
        "UserId": result.user.id.to_string(),
        "UserName": result.user.display_name,
        "Client": result.device.client,
        "DeviceId": result.device.device_id,
        "DeviceName": result.device.device,
        "DeviceType": result.device.device,
        "ApplicationVersion": result.device.version,
        "AdditionalUsers": [],
        "PlayableMediaTypes": ["Audio", "Video"],
        "SupportedCommands": [],
        "SupportsRemoteControl": false,
        "RemoteEndPoint": "",
        "UserPrimaryImageTag": serde_json::Value::Null,
        "AppIconUrl": serde_json::Value::Null,
        "PlaylistItemId": serde_json::Value::Null,
        "PlayState": {},
        "Capabilities": {},
    })
}
