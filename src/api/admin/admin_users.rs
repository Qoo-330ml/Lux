use super::*;

async fn admin_content_restriction_index_status(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database.media_content_restriction_index_status().await {
        Ok((total_items, indexed_items)) => Json(json!({
            "totalItems": total_items,
            "indexedItems": indexed_items,
            "pendingItems": total_items.saturating_sub(indexed_items),
        }))
        .into_response(),
        Err(_) => api_error(
            &headers,
            StatusCode::INTERNAL_SERVER_ERROR,
            lux::ApiErrorCode::Internal,
            "无法读取内容分类索引状态",
        )
        .into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetLibraryAccessRequest {
    pub(crate) can_view: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReplaceLibraryAccessRequest {
    pub(crate) library_access_mode: String,
    #[serde(default)]
    pub(crate) library_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct UpdateUserPermissionPresetRequest {
    version: u64,
    name: String,
    description: Option<String>,
    is_admin: bool,
    can_manage_server: bool,
    permissions: crate::application::settings::NewUserPermissions,
}

impl UpdateUserPermissionPresetRequest {
    fn into_input(self) -> crate::application::user_presets::NewUserPermissionPresetInput {
        crate::application::user_presets::NewUserPermissionPresetInput {
            name: self.name,
            description: self.description,
            is_admin: self.is_admin,
            can_manage_server: self.can_manage_server,
            permissions: self.permissions,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DeleteUserPermissionPresetQuery {
    version: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ApplyUserPermissionPresetRequest {
    preset_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BatchApplyUserPermissionPresetRequest {
    user_ids: Vec<String>,
    preset_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BatchUserStatusRequest {
    user_ids: Vec<String>,
    is_disabled: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AdminUserSessionsQuery {
    page: Option<u32>,
    page_size: Option<u32>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AdminUserAccessBlacklistRulesQuery {
    page: Option<u32>,
    page_size: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateUserAccessBlacklistRuleRequest {
    rule_type: String,
    match_value: String,
    is_enabled: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SetUserAccessBlacklistRuleEnabledRequest {
    is_enabled: bool,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AdminUserActivityQuery {
    page: Option<i64>,
    page_size: Option<i64>,
    event_type: Option<String>,
    #[serde(rename = "from")]
    from_at: Option<i64>,
    #[serde(rename = "to")]
    to_at: Option<i64>,
}

async fn admin_list_user_permission_presets(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match crate::application::user_presets::list_user_permission_presets(database).await {
        Ok(presets) => Json(json!({ "presets": presets })).into_response(),
        Err(error) => user_permission_preset_error(&headers, error),
    }
}

async fn admin_create_user_permission_preset(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<crate::application::user_presets::NewUserPermissionPresetInput>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match crate::application::user_presets::create_user_permission_preset(database, request).await {
        Ok(preset) => {
            record_audit_event(
                &state,
                &headers,
                "USER_PERMISSION_PRESET_CREATED",
                Some("user_permission_preset"),
                Some(&preset.id),
                "{}",
            )
            .await;
            (StatusCode::CREATED, Json(json!({ "preset": preset }))).into_response()
        }
        Err(error) => user_permission_preset_error(&headers, error),
    }
}

async fn admin_update_user_permission_preset(
    headers: HeaderMap,
    Path(preset_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<UpdateUserPermissionPresetRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    if request.version == 0 {
        return invalid_user_permission_preset_request(&headers);
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match crate::application::user_presets::update_user_permission_preset(
        database,
        &preset_id,
        request.version,
        request.into_input(),
    )
    .await
    {
        Ok(preset) => {
            record_audit_event(
                &state,
                &headers,
                "USER_PERMISSION_PRESET_UPDATED",
                Some("user_permission_preset"),
                Some(&preset.id),
                &json!({ "version": preset.version }).to_string(),
            )
            .await;
            Json(json!({ "preset": preset })).into_response()
        }
        Err(error) => user_permission_preset_error(&headers, error),
    }
}

async fn admin_delete_user_permission_preset(
    headers: HeaderMap,
    Path(preset_id): Path<String>,
    Query(query): Query<DeleteUserPermissionPresetQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(version) = query.version.filter(|version| *version > 0) else {
        return invalid_user_permission_preset_request(&headers);
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match crate::application::user_presets::delete_user_permission_preset(
        database, &preset_id, version,
    )
    .await
    {
        Ok(()) => {
            record_audit_event(
                &state,
                &headers,
                "USER_PERMISSION_PRESET_DELETED",
                Some("user_permission_preset"),
                Some(&preset_id),
                &json!({ "version": version }).to_string(),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => user_permission_preset_error(&headers, error),
    }
}

async fn admin_apply_user_permission_preset(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<ApplyUserPermissionPresetRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Ok(user_id) = user_id.parse::<crate::domain::ids::UserId>() else {
        return invalid_user_permission_preset_request(&headers);
    };
    let user_id = user_id.to_string();
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match crate::application::user_presets::apply_user_permission_preset(
        database,
        &user_id,
        &request.preset_id,
    )
    .await
    {
        Ok(preset) => {
            record_audit_event(
                &state,
                &headers,
                "USER_PERMISSION_PRESET_APPLIED",
                Some("user"),
                Some(&user_id),
                &json!({ "presetId": preset.id, "version": preset.version }).to_string(),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => user_permission_preset_application_error(&headers, error),
    }
}

async fn admin_batch_apply_user_permission_preset(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<BatchApplyUserPermissionPresetRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    if request.user_ids.is_empty()
        || request.user_ids.len()
            > crate::application::user_presets::MAX_BATCH_USER_PERMISSION_PRESET_TARGETS
        || has_duplicate_user_ids(&request.user_ids)
    {
        return invalid_user_permission_preset_request(&headers);
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match crate::application::user_presets::apply_user_permission_preset_batch(
        database,
        &request.user_ids,
        &request.preset_id,
    )
    .await
    {
        Ok(result) => {
            for item in result.results.iter().filter(|item| item.success) {
                record_audit_event(
                    &state,
                    &headers,
                    "USER_PERMISSION_PRESET_APPLIED",
                    Some("user"),
                    Some(&item.user_id),
                    &json!({
                        "presetId": result.preset.id,
                        "version": result.preset.version,
                        "batch": true
                    })
                    .to_string(),
                )
                .await;
            }
            Json(result).into_response()
        }
        Err(error) => user_permission_preset_application_error(&headers, error),
    }
}

async fn admin_batch_update_user_status(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<BatchUserStatusRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    if request.user_ids.is_empty()
        || request.user_ids.len()
            > crate::application::admin_user_lifecycle::MAX_BATCH_USER_STATUS_TARGETS
        || has_duplicate_user_ids(&request.user_ids)
    {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "批量账号状态请求无效",
        )
        .into_response();
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let users = match UserStore::new(database.clone()) {
        Ok(users) => users,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let result = crate::application::admin_user_lifecycle::update_users_disabled_status(
        &users,
        &request.user_ids,
        request.is_disabled,
    )
    .await;
    for item in result.results.iter().filter(|item| item.success) {
        if request.is_disabled
            && let Some(web_playback) = state.web_playback.as_ref()
        {
            web_playback
                .stop_server_hls_sessions(&item.hls_session_ids)
                .await;
        }
        let event_type = if request.is_disabled {
            "USER_DISABLED"
        } else {
            "USER_ENABLED"
        };
        record_audit_event(
            &state,
            &headers,
            event_type,
            Some("user"),
            Some(&item.user_id),
            &json!({ "batch": true, "isDisabled": request.is_disabled }).to_string(),
        )
        .await;
    }
    Json(result).into_response()
}

async fn admin_preview_account_preferences_copy(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(mut request): Json<
        crate::application::user_account_preferences::PreviewAccountPreferencesCopyInput,
    >,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(source_user_id) = canonical_user_id(&request.source_user_id) else {
        return invalid_account_preferences_copy_request(&headers);
    };
    request.source_user_id = source_user_id;
    let Some(target_user_ids) = request
        .target_user_ids
        .iter()
        .map(|user_id| canonical_user_id(user_id))
        .collect::<Option<Vec<_>>>()
    else {
        return invalid_account_preferences_copy_request(&headers);
    };
    request.target_user_ids = target_user_ids;
    match crate::application::user_account_preferences::preview_account_preferences_copy(
        database, request,
    )
    .await
    {
        Ok(preview) => Json(preview).into_response(),
        Err(error) => account_preferences_copy_error(&headers, error),
    }
}

async fn admin_copy_account_preferences(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(mut request): Json<
        crate::application::user_account_preferences::CopyAccountPreferencesInput,
    >,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(source_user_id) = canonical_user_id(&request.source_user_id) else {
        return invalid_account_preferences_copy_request(&headers);
    };
    request.source_user_id = source_user_id;
    let Some(target_user_ids) = request
        .targets
        .iter()
        .map(|target| {
            canonical_user_id(&target.user_id).map(|user_id| {
                crate::application::user_account_preferences::CopyAccountPreferencesTarget {
                    user_id,
                    expected_version: target.expected_version,
                }
            })
        })
        .collect::<Option<Vec<_>>>()
    else {
        return invalid_account_preferences_copy_request(&headers);
    };
    request.targets = target_user_ids;
    let groups = request.groups;
    match crate::application::user_account_preferences::copy_account_preferences_batch(
        database, request,
    )
    .await
    {
        Ok(result) => {
            for item in result.results.iter().filter(|item| item.success) {
                record_audit_event(
                    &state,
                    &headers,
                    "USER_ACCOUNT_PREFERENCES_COPIED",
                    Some("user"),
                    Some(&item.user_id),
                    &json!({
                        "sourceUserId": result.source_user_id,
                        "groups": {
                            "appearance": groups.appearance,
                            "home": groups.home,
                            "playback": groups.playback,
                            "libraryOrder": groups.library_order
                        }
                    })
                    .to_string(),
                )
                .await;
            }
            Json(result).into_response()
        }
        Err(error) => account_preferences_copy_error(&headers, error),
    }
}

async fn admin_preview_user_permission_copy(
    headers: HeaderMap,
    State(state): State<AppState>,
    body: Bytes,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let request = match parse_user_permission_copy_request::<
        crate::application::user_permission_copy::PreviewUserPermissionCopyInput,
    >(&headers, &body)
    {
        Ok(request) => request,
        Err(response) => return response,
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match crate::application::user_permission_copy::preview_user_permission_copy(database, request)
        .await
    {
        Ok(preview) => Json(preview).into_response(),
        Err(error) => user_permission_copy_error(&headers, error),
    }
}

async fn admin_apply_user_permission_copy(
    headers: HeaderMap,
    State(state): State<AppState>,
    body: Bytes,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let request = match parse_user_permission_copy_request::<
        crate::application::user_permission_copy::ApplyUserPermissionCopyInput,
    >(&headers, &body)
    {
        Ok(request) => request,
        Err(response) => return response,
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let groups = request.groups;
    match crate::application::user_permission_copy::apply_user_permission_copy(database, request)
        .await
    {
        Ok(result) => {
            for item in result.results.iter().filter(|item| item.success) {
                record_audit_event(
                    &state,
                    &headers,
                    "USER_PERMISSION_GROUPS_COPIED",
                    Some("user"),
                    Some(&item.user_id),
                    &json!({
                        "sourceUserId": result.source_user_id,
                        "groups": {
                            "library": groups.library,
                            "content": groups.content,
                            "playback": groups.playback
                        }
                    })
                    .to_string(),
                )
                .await;
            }
            Json(result).into_response()
        }
        Err(error) => user_permission_copy_error(&headers, error),
    }
}

fn parse_user_permission_copy_request<T>(headers: &HeaderMap, body: &Bytes) -> Result<T, Response>
where
    T: serde::de::DeserializeOwned,
{
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !content_type
        .split(';')
        .next()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
    {
        return Err(invalid_user_permission_copy_request(headers));
    }
    serde_json::from_slice(body).map_err(|_| invalid_user_permission_copy_request(headers))
}

fn invalid_user_permission_copy_request(headers: &HeaderMap) -> Response {
    api_error(
        headers,
        StatusCode::BAD_REQUEST,
        lux::ApiErrorCode::InvalidRequest,
        "权限复制请求无效",
    )
    .into_response()
}

fn user_permission_copy_error(
    headers: &HeaderMap,
    error: crate::application::user_permission_copy::PermissionCopyApplicationError,
) -> Response {
    use crate::application::user_permission_copy::PermissionCopyApplicationError as CopyError;

    match error {
        CopyError::InvalidRequest | CopyError::InvalidSnapshot => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "权限复制请求或来源策略无效",
        )
        .into_response(),
        CopyError::SourceUnavailable | CopyError::UserUnavailable => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "源账号或目标账号不存在",
        )
        .into_response(),
        CopyError::SourceVersionConflict => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::VersionConflict,
            "源账号权限已变化，请重新预览",
        )
        .into_response(),
        CopyError::Storage(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "权限复制暂时不可用",
        )
        .into_response(),
    }
}

fn canonical_user_id(value: &str) -> Option<String> {
    value
        .parse::<crate::domain::ids::UserId>()
        .ok()
        .map(|user_id| user_id.to_string())
}

fn invalid_account_preferences_copy_request(headers: &HeaderMap) -> Response {
    api_error(
        headers,
        StatusCode::BAD_REQUEST,
        lux::ApiErrorCode::InvalidRequest,
        "账户偏好复制请求无效",
    )
    .into_response()
}

fn account_preferences_copy_error(
    headers: &HeaderMap,
    error: crate::application::user_account_preferences::AccountPreferencesCopyError,
) -> Response {
    use crate::application::user_account_preferences::AccountPreferencesCopyError as CopyError;
    match error {
        CopyError::InvalidRequest | CopyError::InvalidPreferences => {
            invalid_account_preferences_copy_request(headers)
        }
        CopyError::SourceUnavailable | CopyError::UserUnavailable => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "源账号或目标账号不存在",
        )
        .into_response(),
        CopyError::SourceVersionConflict => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::VersionConflict,
            "源账号偏好已变更，请重新预览",
        )
        .into_response(),
        CopyError::VersionConflict => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::VersionConflict,
            "账户偏好版本已变更",
        )
        .into_response(),
        CopyError::Storage(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

fn has_duplicate_user_ids(user_ids: &[String]) -> bool {
    let mut unique_ids = std::collections::HashSet::with_capacity(user_ids.len());
    user_ids.iter().any(|user_id| {
        let key = user_id
            .parse::<crate::domain::ids::UserId>()
            .map(|user_id| user_id.to_string())
            .unwrap_or_else(|_| user_id.clone());
        !unique_ids.insert(key)
    })
}

async fn admin_list_user_sessions(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<AdminUserSessionsQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Ok(user_id) = user_id.parse::<crate::domain::ids::UserId>() else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "用户 ID 无效",
        )
        .into_response();
    };
    let user_id = user_id.to_string();
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database.find_user_by_id(&user_id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "用户不存在",
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(auth) = state.auth.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let page = query.page.unwrap_or(1).max(1);
    let page_size = i64::from(query.page_size.unwrap_or(50).clamp(1, 100));
    let offset = i64::from(page.saturating_sub(1)).saturating_mul(page_size);
    let current_session_token = request_cookie(&headers, "lux_session");
    match auth
        .list_user_sessions_for_admin(
            &user_id,
            current_session_token.as_deref(),
            page_size,
            offset,
        )
        .await
    {
        Ok((sessions, total_count)) => Json(json!({
            "sessions": sessions.iter().map(|session| json!({
                "id": session.id,
                "createdAt": session.created_at,
                "updatedAt": session.updated_at,
                "expiresAt": session.expires_at,
                "lastSeenAt": session.last_seen_at,
                "isCurrent": session.is_current,
            })).collect::<Vec<_>>(),
            "page": page,
            "pageSize": page_size,
            "totalCount": total_count,
        }))
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn admin_list_user_devices(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<AdminUserSessionsQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Ok(user_id) = user_id.parse::<crate::domain::ids::UserId>() else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "用户 ID 无效",
        )
        .into_response();
    };
    let user_id = user_id.to_string();
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database.find_user_by_id(&user_id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "用户不存在",
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }

    let page = query.page.unwrap_or(1).max(1);
    let page_size = query.page_size.unwrap_or(20).clamp(1, 100);
    let offset = i64::from(page.saturating_sub(1)).saturating_mul(i64::from(page_size));
    match database
        .list_user_devices_page(crate::storage::UserDevicePageQuery {
            user_id: &user_id,
            offset,
            limit: i64::from(page_size),
        })
        .await
    {
        Ok((devices, total_count)) => Json(json!({
            "devices": devices.iter().map(|device| json!({
                "deviceId": device.device_id,
                "hasActiveLogin": device.has_active_login,
                "deviceName": device.device_name,
                "deviceType": device.device_type,
                "clientName": device.client_name,
                "clientVersion": device.client_version,
                "firstSeenAt": device.first_seen_at,
                "lastSeenAt": device.last_seen_at,
                "lastRemoteIp": device.last_remote_ip,
            })).collect::<Vec<_>>(),
            "page": page,
            "pageSize": page_size,
            "totalCount": total_count,
        }))
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn admin_list_global_user_access_blacklist_rules(
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<AdminUserAccessBlacklistRulesQuery>,
    State(state): State<AppState>,
) -> Response {
    list_user_access_blacklist_rules(&headers, &state, None, query).await
}

async fn admin_list_scoped_user_access_blacklist_rules(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<AdminUserAccessBlacklistRulesQuery>,
    State(state): State<AppState>,
) -> Response {
    list_user_access_blacklist_rules(&headers, &state, Some(user_id), query).await
}

async fn list_user_access_blacklist_rules(
    headers: &HeaderMap,
    state: &AppState,
    user_id: Option<String>,
    query: AdminUserAccessBlacklistRulesQuery,
) -> Response {
    let user_id = match validate_user_access_blacklist_scope(headers, state, user_id, false).await {
        Ok(user_id) => user_id,
        Err(response) => return response,
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let page = query.page.unwrap_or(1).max(1);
    let page_size = query.page_size.unwrap_or(20).clamp(1, 100);
    let offset = i64::from(page.saturating_sub(1)).saturating_mul(i64::from(page_size));
    let scope = user_id.as_deref().map_or(
        crate::storage::UserAccessBlacklistRuleScope::Global,
        |user_id| crate::storage::UserAccessBlacklistRuleScope::User(user_id),
    );
    match database
        .list_user_access_blacklist_rules_page(crate::storage::UserAccessBlacklistRulePageQuery {
            scope,
            offset,
            limit: i64::from(page_size),
        })
        .await
    {
        Ok((rules, total_count)) => Json(json!({
            "rules": rules.iter().map(user_access_blacklist_rule_json).collect::<Vec<_>>(),
            "page": page,
            "pageSize": page_size,
            "totalCount": total_count,
        }))
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn admin_create_global_user_access_blacklist_rule(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<CreateUserAccessBlacklistRuleRequest>,
) -> Response {
    create_user_access_blacklist_rule(&headers, &state, None, request).await
}

async fn admin_create_scoped_user_access_blacklist_rule(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<CreateUserAccessBlacklistRuleRequest>,
) -> Response {
    create_user_access_blacklist_rule(&headers, &state, Some(user_id), request).await
}

async fn create_user_access_blacklist_rule(
    headers: &HeaderMap,
    state: &AppState,
    user_id: Option<String>,
    request: CreateUserAccessBlacklistRuleRequest,
) -> Response {
    let user_id = match validate_user_access_blacklist_scope(headers, state, user_id, true).await {
        Ok(user_id) => user_id,
        Err(response) => return response,
    };
    let Some(rule_type) = crate::application::user_access_rules::UserAccessBlacklistRuleType::parse(
        &request.rule_type,
    ) else {
        return api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "黑名单规则类型无效",
        )
        .into_response();
    };
    let match_value =
        match crate::application::user_access_rules::normalize_user_access_blacklist_value(
            rule_type,
            &request.match_value,
        ) {
            Ok(match_value) => match_value,
            Err(_) => {
                return api_error(
                    headers,
                    StatusCode::BAD_REQUEST,
                    lux::ApiErrorCode::InvalidRequest,
                    "黑名单规则值无效",
                )
                .into_response();
            }
        };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let is_enabled = request.is_enabled.unwrap_or(true);
    if is_enabled
        && let Err(response) = ensure_user_access_rule_does_not_block_admin(
            headers,
            state,
            user_id.as_deref(),
            rule_type,
            &match_value,
        )
        .await
    {
        return response;
    }
    let rule_id = uuid::Uuid::now_v7().to_string();
    let created_at = time::OffsetDateTime::now_utc().unix_timestamp();
    let created_by = admin_user_access_blacklist_creator(headers, state).await;
    match database
        .insert_user_access_blacklist_rule(crate::storage::NewUserAccessBlacklistRule {
            id: &rule_id,
            user_id: user_id.as_deref(),
            rule_type: rule_type.as_database_value(),
            match_value: &match_value,
            is_enabled,
            created_by: created_by.as_deref(),
            created_at,
        })
        .await
    {
        Ok(()) => (
            StatusCode::CREATED,
            Json(json!({
                "id": rule_id,
                "userId": user_id,
                "ruleType": rule_type.as_database_value(),
                "matchValue": match_value,
                "isEnabled": is_enabled,
                "createdBy": created_by,
                "createdAt": created_at,
                "updatedAt": created_at,
            })),
        )
            .into_response(),
        Err(error) if error.is_unique_violation() => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "该作用范围已存在相同黑名单规则",
        )
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn admin_set_global_user_access_blacklist_rule_enabled(
    headers: HeaderMap,
    Path(rule_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<SetUserAccessBlacklistRuleEnabledRequest>,
) -> Response {
    set_user_access_blacklist_rule_enabled(&headers, &state, None, rule_id, request).await
}

async fn admin_set_scoped_user_access_blacklist_rule_enabled(
    headers: HeaderMap,
    Path((user_id, rule_id)): Path<(String, String)>,
    State(state): State<AppState>,
    Json(request): Json<SetUserAccessBlacklistRuleEnabledRequest>,
) -> Response {
    set_user_access_blacklist_rule_enabled(&headers, &state, Some(user_id), rule_id, request).await
}

async fn set_user_access_blacklist_rule_enabled(
    headers: &HeaderMap,
    state: &AppState,
    user_id: Option<String>,
    rule_id: String,
    request: SetUserAccessBlacklistRuleEnabledRequest,
) -> Response {
    let user_id = match validate_user_access_blacklist_scope(headers, state, user_id, true).await {
        Ok(user_id) => user_id,
        Err(response) => return response,
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let scope = user_id.as_deref().map_or(
        crate::storage::UserAccessBlacklistRuleScope::Global,
        |user_id| crate::storage::UserAccessBlacklistRuleScope::User(user_id),
    );
    if request.is_enabled {
        let rule = match database
            .find_user_access_blacklist_rule(scope, &rule_id)
            .await
        {
            Ok(Some(rule)) => rule,
            Ok(None) => {
                return api_error(
                    headers,
                    StatusCode::NOT_FOUND,
                    lux::ApiErrorCode::NotFound,
                    "黑名单规则不存在",
                )
                .into_response();
            }
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        let Some(rule_type) =
            crate::application::user_access_rules::UserAccessBlacklistRuleType::parse(
                &rule.rule_type,
            )
        else {
            return user_access_blacklist_conflict(headers);
        };
        if let Err(response) = ensure_user_access_rule_does_not_block_admin(
            headers,
            state,
            user_id.as_deref(),
            rule_type,
            &rule.match_value,
        )
        .await
        {
            return response;
        }
    }
    match database
        .set_user_access_blacklist_rule_enabled(scope, &rule_id, request.is_enabled)
        .await
    {
        Ok(true) => Json(json!({ "id": rule_id, "isEnabled": request.is_enabled })).into_response(),
        Ok(false) => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "黑名单规则不存在",
        )
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn ensure_user_access_rule_does_not_block_admin(
    headers: &HeaderMap,
    state: &AppState,
    scope_user_id: Option<&str>,
    rule_type: crate::application::user_access_rules::UserAccessBlacklistRuleType,
    rule_value: &str,
) -> Result<(), Response> {
    // Shared API keys have no Web-session device or user identity and retain their
    // independent recovery path, so these rules do not apply to API-key requests.
    if lux_api_key_from_headers(headers).is_some() {
        return Ok(());
    }
    let Some(session_token) = request_cookie(headers, "lux_session") else {
        return Err(api_error(
            headers,
            StatusCode::UNAUTHORIZED,
            lux::ApiErrorCode::AuthenticationRequired,
            "需要登录",
        )
        .into_response());
    };
    let Some(auth) = state.auth.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE.into_response());
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
        Err(_) => return Err(StatusCode::SERVICE_UNAVAILABLE.into_response()),
    };
    let current_user_id = session.user.id.to_string();
    if scope_user_id.is_some_and(|scope_user_id| scope_user_id != current_user_id) {
        return Ok(());
    }
    let client_ip = if rule_type
        == crate::application::user_access_rules::UserAccessBlacklistRuleType::IpCidr
    {
        match admin_request_access_ip(headers, state).await {
            Ok(client_ip) => Some(client_ip),
            Err(()) => return Err(user_access_blacklist_conflict(headers)),
        }
    } else {
        None
    };
    match crate::application::user_access_rules::user_access_blacklist_rule_matches_context(
        rule_type,
        rule_value,
        session.device_id.as_deref(),
        Some("lux web"),
        client_ip,
    ) {
        Ok(true) | Err(_) => Err(user_access_blacklist_conflict(headers)),
        Ok(false) => Ok(()),
    }
}

async fn admin_request_access_ip(
    headers: &HeaderMap,
    state: &AppState,
) -> Result<std::net::IpAddr, ()> {
    let peer = headers
        .get("x-lux-peer-ip")
        .and_then(|value| value.to_str().ok())
        .ok_or(())?
        .parse::<std::net::IpAddr>()
        .map_err(|_| ())?;
    let database = state.database.as_ref().ok_or(())?;
    let trusted_proxy_values = crate::application::settings::read_trusted_proxy_cidrs(database)
        .await
        .map_err(|_| ())?;
    let trusted_proxies = trusted_proxy_values
        .iter()
        .map(|value| crate::application::user_access_rules::IpCidr::parse(value).map_err(|_| ()))
        .collect::<Result<Vec<_>, _>>()?;
    let forwarded_for_values = headers
        .get_all("x-forwarded-for")
        .iter()
        .map(|value| value.to_str().map_err(|_| ()))
        .collect::<Result<Vec<_>, _>>()?;
    crate::application::user_access_rules::resolve_client_ip_from_trusted_proxies(
        peer,
        &forwarded_for_values,
        &trusted_proxies,
    )
    .map_err(|_| ())
}

fn user_access_blacklist_conflict(headers: &HeaderMap) -> Response {
    api_error(
        headers,
        StatusCode::CONFLICT,
        lux::ApiErrorCode::VersionConflict,
        "规则会阻断当前管理员入口，或无法安全确认其影响",
    )
    .into_response()
}

async fn admin_delete_global_user_access_blacklist_rule(
    headers: HeaderMap,
    Path(rule_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    delete_user_access_blacklist_rule(&headers, &state, None, rule_id).await
}

async fn admin_delete_scoped_user_access_blacklist_rule(
    headers: HeaderMap,
    Path((user_id, rule_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Response {
    delete_user_access_blacklist_rule(&headers, &state, Some(user_id), rule_id).await
}

async fn delete_user_access_blacklist_rule(
    headers: &HeaderMap,
    state: &AppState,
    user_id: Option<String>,
    rule_id: String,
) -> Response {
    let user_id = match validate_user_access_blacklist_scope(headers, state, user_id, true).await {
        Ok(user_id) => user_id,
        Err(response) => return response,
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let scope = user_id.as_deref().map_or(
        crate::storage::UserAccessBlacklistRuleScope::Global,
        |user_id| crate::storage::UserAccessBlacklistRuleScope::User(user_id),
    );
    match database
        .delete_user_access_blacklist_rule(scope, &rule_id)
        .await
    {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "黑名单规则不存在",
        )
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn validate_user_access_blacklist_scope(
    headers: &HeaderMap,
    state: &AppState,
    user_id: Option<String>,
    require_csrf: bool,
) -> Result<Option<String>, Response> {
    require_admin(headers, state, require_csrf).await?;
    let Some(user_id) = user_id else {
        return Ok(None);
    };
    let user_id = user_id
        .parse::<crate::domain::ids::UserId>()
        .map_err(|_| {
            api_error(
                headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "用户 ID 无效",
            )
            .into_response()
        })?
        .to_string();
    let Some(database) = state.database.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE.into_response());
    };
    match database.find_user_by_id(&user_id).await {
        Ok(Some(_)) => Ok(Some(user_id)),
        Ok(None) => Err(api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "用户不存在",
        )
        .into_response()),
        Err(_) => Err(StatusCode::SERVICE_UNAVAILABLE.into_response()),
    }
}

async fn admin_user_access_blacklist_creator(
    headers: &HeaderMap,
    state: &AppState,
) -> Option<String> {
    if lux_api_key_from_headers(headers).is_some() {
        return None;
    }
    let session_token = request_cookie(headers, "lux_session")?;
    let auth = state.auth.as_ref()?;
    auth.resolve(&session_token)
        .await
        .ok()
        .flatten()
        .map(|session| session.user.id.to_string())
}

fn user_access_blacklist_rule_json(rule: &crate::storage::StoredUserAccessBlacklistRule) -> Value {
    json!({
        "id": rule.id,
        "userId": rule.user_id,
        "ruleType": rule.rule_type,
        "matchValue": rule.match_value,
        "isEnabled": rule.is_enabled,
        "createdBy": rule.created_by,
        "createdAt": rule.created_at,
        "updatedAt": rule.updated_at,
    })
}

async fn admin_revoke_user_session(
    headers: HeaderMap,
    Path((user_id, session_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Ok(user_id) = user_id.parse::<crate::domain::ids::UserId>() else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "用户 ID 无效",
        )
        .into_response();
    };
    let Ok(session_id) = session_id.parse::<crate::domain::ids::UserId>() else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "会话 ID 无效",
        )
        .into_response();
    };
    let user_id_string = user_id.to_string();
    let session_id_string = session_id.to_string();
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database.find_user_by_id(&user_id_string).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "用户不存在",
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(auth) = state.auth.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if let Some(current_token) = request_cookie(&headers, "lux_session") {
        match auth
            .is_current_session_for_admin(&user_id_string, &session_id_string, &current_token)
            .await
        {
            Ok(true) => {
                return api_error(
                    &headers,
                    StatusCode::BAD_REQUEST,
                    lux::ApiErrorCode::InvalidRequest,
                    "不能撤销当前会话，请使用退出登录",
                )
                .into_response();
            }
            Ok(false) => {}
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        }
    }

    match auth.revoke_session(&user_id, &session_id_string).await {
        Ok(true) => {
            record_audit_event(
                &state,
                &headers,
                "USER_SESSION_REVOKED",
                Some("web_session"),
                Some(&session_id_string),
                &json!({ "userId": user_id_string }).to_string(),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "活动会话不存在",
        )
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn admin_list_user_emby_login_sessions(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<AdminUserSessionsQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Ok(user_id) = user_id.parse::<crate::domain::ids::UserId>() else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "用户 ID 无效",
        )
        .into_response();
    };
    let user_id = user_id.to_string();
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database.find_user_by_id(&user_id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "用户不存在",
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(auth) = state.emby_auth.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let page = query.page.unwrap_or(1).max(1);
    let page_size = i64::from(query.page_size.unwrap_or(50).clamp(1, 100));
    let offset = i64::from(page.saturating_sub(1)).saturating_mul(page_size);
    match auth
        .list_admin_login_sessions(&user_id, page_size, offset)
        .await
    {
        Ok((sessions, total_count)) => Json(json!({
            "sessions": sessions.iter().map(|session| json!({
                "id": session.id,
                "deviceId": session.device_id,
                "deviceName": session.device_name,
                "deviceType": session.device_type,
                "clientName": session.client_name,
                "clientVersion": session.client_version,
                "createdAt": session.created_at,
                "updatedAt": session.updated_at,
                "lastSeenAt": session.last_seen_at,
            })).collect::<Vec<_>>(),
            "page": page,
            "pageSize": page_size,
            "totalCount": total_count,
        }))
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn admin_list_user_activity(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<AdminUserActivityQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Ok(user_id) = user_id.parse::<crate::domain::ids::UserId>() else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "用户 ID 无效",
        )
        .into_response();
    };
    let user_id = user_id.to_string();
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database.find_user_by_id(&user_id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "用户不存在",
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }

    let event_type = match query.event_type.as_deref() {
        Some(value) => {
            let value = value.trim();
            if value.is_empty()
                || value.chars().count() > 128
                || value.chars().any(char::is_control)
            {
                return api_error(
                    &headers,
                    StatusCode::BAD_REQUEST,
                    lux::ApiErrorCode::InvalidRequest,
                    "用户活动查询参数无效",
                )
                .into_response();
            }
            Some(value.to_owned())
        }
        None => None,
    };
    if query.from_at.is_some_and(|value| value < 0)
        || query.to_at.is_some_and(|value| value < 0)
        || query
            .from_at
            .zip(query.to_at)
            .is_some_and(|(from, to)| from > to)
    {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "用户活动查询参数无效",
        )
        .into_response();
    }

    let page = query.page.unwrap_or(1).max(1);
    let page_size = query.page_size.unwrap_or(20).clamp(1, 100);
    let offset = page.saturating_sub(1).saturating_mul(page_size);
    match database
        .list_user_activity_events(crate::storage::UserActivityEventQuery {
            user_id: &user_id,
            event_type: event_type.as_deref(),
            from_at: query.from_at,
            to_at: query.to_at,
            offset,
            limit: page_size,
        })
        .await
    {
        Ok((events, total_count)) => Json(json!({
            "events": events.into_iter().map(|event| {
                let metadata = serde_json::from_str(&event.metadata_json)
                    .map(crate::observability::logs::redact_sensitive_log_values)
                    .unwrap_or_else(|_| json!({ "invalid": true }));
                let metadata = match metadata {
                    Value::Object(object) => Value::Object(object),
                    value => json!({ "value": value }),
                };
                json!({
                    "id": event.id,
                    "userId": event.user_id,
                    "actorUserId": event.actor_user_id,
                    "eventType": event.event_type,
                    "targetType": event.target_type,
                    "targetId": event.target_id,
                    "metadata": metadata,
                    "occurredAt": event.occurred_at,
                })
            }).collect::<Vec<_>>(),
            "page": page,
            "pageSize": page_size,
            "totalCount": total_count,
        }))
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn admin_list_user_playback_sessions(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<AdminUserSessionsQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Ok(user_id) = user_id.parse::<crate::domain::ids::UserId>() else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "用户 ID 无效",
        )
        .into_response();
    };
    let user_id = user_id.to_string();
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let users = match UserStore::new(database.clone()) {
        Ok(users) => users,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let user = match users.find_admin_profile_by_id(&user_id).await {
        Ok(Some(profile)) => profile.user,
        Ok(None) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "用户不存在",
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let page = query.page.unwrap_or(1).max(1);
    let page_size = i64::from(query.page_size.unwrap_or(20).clamp(1, 100));
    let offset = i64::from(page.saturating_sub(1)).saturating_mul(page_size);
    let (sessions, total_count) = match database
        .list_current_user_playback_sessions(&user_id, page_size, offset)
        .await
    {
        Ok(result) => result,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let playback_data = match crate::application::admin_dashboard::dashboard_playback_data(
        database,
        Some(catalog),
        state.ip_location.as_ref(),
        &sessions,
        std::slice::from_ref(&user),
        page_size as usize,
    )
    .await
    {
        Ok(playback_data) => playback_data,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let sessions = playback_data
        .iter()
        .map(|playback| {
            let can_stop = playback
                .web_session
                .as_ref()
                .is_some_and(|session| session.plan == "SERVER_HLS" && session.state == "ACTIVE");
            let mut value =
                crate::api::legacy::admin::admin_dashboard::dashboard_playback_item_json(
                    &playback.session,
                    &playback.item,
                    playback.series.as_ref(),
                    &playback.user_name,
                    playback.remote_ip_location.as_ref(),
                    playback.web_session.as_ref(),
                );
            value["canStop"] = json!(can_stop);
            value
        })
        .collect::<Vec<_>>();
    Json(json!({
        "sessions": sessions,
        "page": page,
        "pageSize": page_size,
        "totalCount": total_count,
    }))
    .into_response()
}

async fn admin_stop_user_playback_session(
    headers: HeaderMap,
    Path((user_id, playback_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Ok(user_id) = user_id.parse::<crate::domain::ids::UserId>() else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "用户 ID 无效",
        )
        .into_response();
    };
    let Ok(playback_id) = playback_id.parse::<uuid::Uuid>() else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "播放会话 ID 无效",
        )
        .into_response();
    };
    let user_id = user_id.to_string();
    let playback_id = playback_id.to_string();
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database.find_user_by_id(&user_id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "用户不存在",
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let playback = match database.find_playback_session_by_id(&playback_id).await {
        Ok(Some(playback)) if playback.user_id == user_id && playback.state != "STOPPED" => {
            playback
        }
        Ok(_) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "当前播放会话不存在",
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let web_sessions = match database
        .find_web_playback_sessions_for_playbacks(std::slice::from_ref(&playback.play_session_id))
        .await
    {
        Ok(sessions) => sessions,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let Some(web_session) = web_sessions.into_iter().find(|session| {
        session.user_id == user_id
            && session.item_id == playback.item_id
            && session.plan == "SERVER_HLS"
            && session.state == "ACTIVE"
    }) else {
        return api_error(
            &headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "仅能停止 Lux 管理的服务端 HLS；直放或外部播放可能已交给播放器",
        )
        .into_response();
    };
    let Some(web_playback) = state.web_playback.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if web_playback.stop(&web_session.id, &user_id).await.is_err() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    record_audit_event(
        &state,
        &headers,
        "USER_PLAYBACK_STOPPED",
        Some("playback_session"),
        Some(&playback_id),
        &json!({ "userId": user_id }).to_string(),
    )
    .await;
    StatusCode::NO_CONTENT.into_response()
}

async fn admin_revoke_user_emby_login_session(
    headers: HeaderMap,
    Path((user_id, session_id)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Ok(user_id) = user_id.parse::<crate::domain::ids::UserId>() else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "用户 ID 无效",
        )
        .into_response();
    };
    let Ok(session_id) = session_id.parse::<crate::domain::ids::UserId>() else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "登录 ID 无效",
        )
        .into_response();
    };
    let user_id = user_id.to_string();
    let session_id = session_id.to_string();
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database.find_user_by_id(&user_id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "用户不存在",
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(auth) = state.emby_auth.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match auth.revoke_admin_login_session(&user_id, &session_id).await {
        Ok(true) => {
            record_audit_event(
                &state,
                &headers,
                "USER_EMBY_LOGIN_SESSION_REVOKED",
                Some("emby_login_session"),
                Some(&session_id),
                &json!({ "userId": user_id }).to_string(),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "Emby 登录凭证不存在或已撤销",
        )
        .into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn admin_revoke_all_user_emby_login_sessions(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Ok(user_id) = user_id.parse::<crate::domain::ids::UserId>() else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "用户 ID 无效",
        )
        .into_response();
    };
    let user_id = user_id.to_string();
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database.find_user_by_id(&user_id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "用户不存在",
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(auth) = state.emby_auth.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match auth.revoke_all_admin_login_sessions(&user_id).await {
        Ok(revoked_count) => {
            if revoked_count > 0 {
                record_audit_event(
                    &state,
                    &headers,
                    "USER_EMBY_LOGIN_SESSIONS_REVOKED",
                    Some("user"),
                    Some(&user_id),
                    &json!({ "userId": user_id, "revokedCount": revoked_count }).to_string(),
                )
                .await;
            }
            Json(json!({ "revokedCount": revoked_count })).into_response()
        }
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

fn invalid_user_permission_preset_request(headers: &HeaderMap) -> Response {
    api_error(
        headers,
        StatusCode::BAD_REQUEST,
        lux::ApiErrorCode::InvalidRequest,
        "权限预设请求无效",
    )
    .into_response()
}

fn user_permission_preset_error(
    headers: &HeaderMap,
    error: crate::application::user_presets::UserPermissionPresetError,
) -> Response {
    use crate::application::user_presets::UserPermissionPresetError as PresetError;

    match error {
        PresetError::InvalidName
        | PresetError::InvalidDescription
        | PresetError::InvalidRole
        | PresetError::InvalidSnapshot => invalid_user_permission_preset_request(headers),
        PresetError::DuplicateName
        | PresetError::BuiltInPreset
        | PresetError::ConcurrentUpdate
        | PresetError::VersionConflict
        | PresetError::PresetLimitReached => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "权限预设已变化或与现有预设冲突",
        )
        .into_response(),
        PresetError::NotFound => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "权限预设不存在",
        )
        .into_response(),
        PresetError::Storage(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "权限预设暂时不可用",
        )
        .into_response(),
    }
}

fn user_permission_preset_application_error(
    headers: &HeaderMap,
    error: crate::application::user_presets::UserPermissionPresetApplicationError,
) -> Response {
    use crate::application::user_presets::UserPermissionPresetApplicationError as ApplyError;

    match error {
        ApplyError::InvalidUserId | ApplyError::InvalidSnapshot => {
            invalid_user_permission_preset_request(headers)
        }
        ApplyError::NotFound | ApplyError::PresetNotFound => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "用户或权限预设不存在",
        )
        .into_response(),
        ApplyError::LastManager => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "不能移除最后一个服务器管理账户",
        )
        .into_response(),
        ApplyError::Storage(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "权限预设暂时不可用",
        )
        .into_response(),
    }
}

pub(crate) async fn admin_list_users(
    headers: HeaderMap,
    Query(query): Query<AdminUsersQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let users = match UserStore::new(database.clone()) {
        Ok(users) => users,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let status = match query.status.as_deref().unwrap_or("all") {
        "all" => crate::storage::AdminUserStatusFilter::All,
        "active" => crate::storage::AdminUserStatusFilter::Active,
        "disabled" => crate::storage::AdminUserStatusFilter::Disabled,
        "deleted" => crate::storage::AdminUserStatusFilter::Deleted,
        _ => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "用户状态筛选项无效",
            )
            .into_response();
        }
    };
    let sort_by = match query.sort_by.as_deref().unwrap_or("lastActivityAt") {
        "createdAt" => crate::storage::AdminUserSortField::Created,
        "lastLoginAt" => crate::storage::AdminUserSortField::LastLogin,
        "lastActivityAt" => crate::storage::AdminUserSortField::LastActivity,
        _ => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "用户排序项无效",
            )
            .into_response();
        }
    };
    let descending = match query.sort_order.as_deref().unwrap_or("desc") {
        "asc" => false,
        "desc" => true,
        _ => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "排序方向无效",
            )
            .into_response();
        }
    };
    let search = query
        .search
        .as_deref()
        .map(str::trim)
        .filter(|search| !search.is_empty());
    if search.is_some_and(|search| search.chars().count() > 200) {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "搜索内容不能超过 200 个字符",
        )
        .into_response();
    }
    let page = i64::from(query.page.unwrap_or(1).max(1));
    let page_size = i64::from(query.page_size.unwrap_or(20).clamp(1, 100));
    match users
        .list_admin_users(status, search, sort_by, descending, page, page_size)
        .await
    {
        Ok(result) => Json(json!({
            "users": result.entries.iter().map(admin_user_list_entry_json).collect::<Vec<_>>(),
            "page": result.page,
            "pageSize": result.page_size,
            "totalCount": result.total_count,
        }))
        .into_response(),
        Err(error) => user_store_error(&headers, error),
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AdminUsersQuery {
    page: Option<u32>,
    page_size: Option<u32>,
    search: Option<String>,
    status: Option<String>,
    sort_by: Option<String>,
    sort_order: Option<String>,
}

fn admin_user_list_entry_json(entry: &crate::auth::users::AdminUserListEntry) -> Value {
    let mut user = user_json(&entry.user);
    user["email"] = json!(entry.email);
    user["emailVerifiedAt"] = json!(entry.email_verified_at);
    user["deletedAt"] = json!(entry.deleted_at);
    user["deletionPurgeAt"] = json!(entry.deletion_purge_at);
    user["createdAt"] = json!(entry.created_at);
    user["lastLoginAt"] = json!(entry.user.last_login_at);
    user["lastActivityAt"] = json!(entry.user.last_activity_at);
    user["currentPlaybackCount"] = json!(entry.current_playback_count);
    admin_user_permission_preset_source_fields(&mut user, entry.preset_source.as_ref());
    user
}

fn admin_user_permission_preset_source_fields(
    user: &mut Value,
    source: Option<&crate::storage::StoredUserPermissionPresetSource>,
) {
    user["presetSource"] = source
        .map(|source| json!(source.preset_name))
        .unwrap_or(Value::Null);
    user["presetSourceDetails"] = source
        .map(|source| {
            json!({
                "id": source.preset_id,
                "name": source.preset_name,
                "version": source.preset_version,
                "appliedAt": source.applied_at,
            })
        })
        .unwrap_or(Value::Null);
}

pub(crate) async fn admin_get_user(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Ok(user_id) = user_id.parse::<crate::domain::ids::UserId>() else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "用户 ID 无效",
        )
        .into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let users = match UserStore::new(database.clone()) {
        Ok(users) => users,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match users.find_admin_profile_by_id(&user_id.to_string()).await {
        Ok(Some(profile)) => {
            Json(json!({ "user": admin_user_profile_json(&profile) })).into_response()
        }
        Ok(None) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "用户不存在",
        )
        .into_response(),
        Err(error) => user_store_error(&headers, error),
    }
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateUserProfileRequest {
    pub(crate) display_name: Option<String>,
    pub(crate) email: Option<String>,
}

pub(crate) async fn admin_update_user_profile(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<UpdateUserProfileRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let users = match UserStore::new(database.clone()) {
        Ok(users) => users,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match users
        .update_profile(
            &user_id,
            request.display_name.as_deref(),
            request.email.as_deref().map(|email| {
                if email.trim().is_empty() {
                    None
                } else {
                    Some(email)
                }
            }),
        )
        .await
    {
        Ok(Some(profile)) => {
            Json(json!({ "user": admin_user_profile_json(&profile) })).into_response()
        }
        Ok(None) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "用户不存在",
        )
        .into_response(),
        Err(error) => user_store_error(&headers, error),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SetUserPlaybackPolicyRequest {
    can_play: bool,
    max_simultaneous_playbacks: i64,
    allow_remux: bool,
    allow_audio_transcode: bool,
    allow_video_transcode: bool,
}

pub(crate) async fn admin_get_user_playback_policy(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let users = match UserStore::new(database.clone()) {
        Ok(users) => users,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match users.get_playback_policy(&user_id).await {
        Ok(Some(policy)) => {
            Json(json!({ "policy": admin_user_playback_policy_json(policy) })).into_response()
        }
        Ok(None) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "用户不存在",
        )
        .into_response(),
        Err(error) => user_store_error(&headers, error),
    }
}

pub(crate) async fn admin_set_user_playback_policy(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<SetUserPlaybackPolicyRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let users = match UserStore::new(database.clone()) {
        Ok(users) => users,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let policy = crate::storage::UserPlaybackPolicy {
        can_play: request.can_play,
        max_simultaneous_playbacks: request.max_simultaneous_playbacks,
        allow_remux: request.allow_remux,
        allow_audio_transcode: request.allow_audio_transcode,
        allow_video_transcode: request.allow_video_transcode,
    };
    match users.update_playback_policy(&user_id, policy).await {
        Ok(true) => {
            let metadata = json!({
                "canPlay": policy.can_play,
                "maxSimultaneousPlaybacks": policy.max_simultaneous_playbacks,
                "allowRemux": policy.allow_remux,
                "allowAudioTranscode": policy.allow_audio_transcode,
                "allowVideoTranscode": policy.allow_video_transcode,
            })
            .to_string();
            record_audit_event(
                &state,
                &headers,
                "USER_PLAYBACK_POLICY_UPDATED",
                Some("user"),
                Some(&user_id),
                &metadata,
            )
            .await;
            Json(json!({ "policy": admin_user_playback_policy_json(policy) })).into_response()
        }
        Ok(false) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "用户不存在",
        )
        .into_response(),
        Err(error) => user_store_error(&headers, error),
    }
}

async fn admin_get_user_content_restrictions(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Ok(user_id) = user_id.parse::<crate::domain::ids::UserId>() else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "用户 ID 无效",
        )
        .into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };

    match crate::application::user_content_restrictions::get_user_content_restrictions(
        database, user_id,
    )
    .await
    {
        Ok(policy) => Json(json!({ "policy": policy })).into_response(),
        Err(error) => user_content_restrictions_error(&headers, error),
    }
}

async fn admin_update_user_content_restrictions(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<
        crate::application::user_content_restrictions::UpdateUserContentRestrictionsInput,
    >,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Ok(user_id) = user_id.parse::<crate::domain::ids::UserId>() else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "用户 ID 无效",
        )
        .into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };

    match crate::application::user_content_restrictions::update_user_content_restrictions(
        database, user_id, request,
    )
    .await
    {
        Ok(policy) => {
            let metadata = json!({
                "ratingSystem": policy.rating_system,
                "hasMaximumRating": policy.maximum_rating.is_some(),
                "allowUnrated": policy.allow_unrated,
                "allowedTagCount": policy.allowed_tags.len(),
                "blockedTagCount": policy.blocked_tags.len(),
                "version": policy.version,
            })
            .to_string();
            record_audit_event(
                &state,
                &headers,
                "USER_CONTENT_RESTRICTIONS_UPDATED",
                Some("user"),
                Some(&user_id.to_string()),
                &metadata,
            )
            .await;
            Json(json!({ "policy": policy })).into_response()
        }
        Err(error) => user_content_restrictions_error(&headers, error),
    }
}

fn user_content_restrictions_error(
    headers: &HeaderMap,
    error: crate::application::user_content_restrictions::UserContentRestrictionsError,
) -> Response {
    use crate::application::user_content_restrictions::UserContentRestrictionsError as PolicyError;

    match error {
        PolicyError::InvalidPolicy => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "内容限制策略无效",
        )
        .into_response(),
        PolicyError::VersionConflict => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "内容限制策略已变化，请重新读取后再保存",
        )
        .into_response(),
        PolicyError::NotFound => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "用户不存在或不可用",
        )
        .into_response(),
        PolicyError::Storage(StorageError::Conflict(_)) => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "内容限制策略无效",
        )
        .into_response(),
        PolicyError::Storage(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "内容限制策略暂时不可用",
        )
        .into_response(),
    }
}

pub(crate) async fn admin_list_user_library_access(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Ok(user_id) = user_id.parse::<crate::domain::ids::UserId>() else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "用户 ID 无效",
        )
        .into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let user_id = user_id.to_string();
    let mode = match database.user_library_access_mode(&user_id).await {
        Ok(Some(mode)) => mode,
        Ok(None) => {
            return api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "用户不存在",
            )
            .into_response();
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let library_ids = if mode == "SELECTED" {
        match database.list_selected_library_ids(&user_id).await {
            Ok(library_ids) => library_ids,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        }
    } else {
        Vec::new()
    };
    Json(json!({
        "libraryAccessMode": mode,
        "libraryIds": library_ids,
    }))
    .into_response()
}

pub(crate) async fn admin_replace_user_library_access(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<ReplaceLibraryAccessRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let user_id = match user_id.parse::<crate::domain::ids::UserId>() {
        Ok(id) => id.to_string(),
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "用户 ID 无效",
            )
            .into_response();
        }
    };
    let mut library_ids = Vec::with_capacity(request.library_ids.len());
    for library_id in request.library_ids {
        match library_id.parse::<crate::domain::ids::LibraryId>() {
            Ok(id) => library_ids.push(id.to_string()),
            Err(_) => {
                return api_error(
                    &headers,
                    StatusCode::BAD_REQUEST,
                    lux::ApiErrorCode::InvalidRequest,
                    "媒体库 ID 无效",
                )
                .into_response();
            }
        }
    }
    let Some(database) = state.database.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    match crate::application::admin_user_access::replace_library_access(
        database,
        state.home.as_ref(),
        &user_id,
        &request.library_access_mode,
        &library_ids,
    )
    .await
    {
        Ok((mode, library_ids)) => {
            let metadata = json!({
                "libraryAccessMode": mode,
                "libraryIds": library_ids,
            })
            .to_string();
            record_audit_event(
                &state,
                &headers,
                "LIBRARY_ACCESS_MODE_UPDATED",
                Some("user_library_access"),
                Some(&user_id),
                &metadata,
            )
            .await;
            Json(json!({
                "userId": user_id,
                "libraryAccessMode": mode,
                "libraryIds": library_ids,
            }))
            .into_response()
        }
        Err(crate::application::admin_user_access::ReplaceLibraryAccessError::UserNotFound) => {
            api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "用户不存在",
            )
            .into_response()
        }
        Err(crate::application::admin_user_access::ReplaceLibraryAccessError::LibraryNotFound) => {
            api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "媒体库不存在或未启用",
            )
            .into_response()
        }
        Err(crate::application::admin_user_access::ReplaceLibraryAccessError::InvalidMode)
        | Err(crate::application::admin_user_access::ReplaceLibraryAccessError::DuplicateLibrary) => {
            api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "媒体库权限模式或选择无效",
            )
            .into_response()
        }
        Err(crate::application::admin_user_access::ReplaceLibraryAccessError::Storage) => {
            api_error(
                &headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "数据库暂时不可用",
            )
            .into_response()
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateUserRequest {
    pub(crate) username: String,
    #[serde(default)]
    pub(crate) display_name: String,
    pub(crate) password: String,
    #[serde(default)]
    pub(crate) email: Option<String>,
    #[serde(default)]
    pub(crate) is_admin: bool,
}

pub(crate) async fn admin_create_user(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<CreateUserRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let users = match UserStore::new(database.clone()) {
        Ok(users) => users,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let permissions = match crate::application::settings::read_new_user_permissions(database).await
    {
        Ok(permissions) => permissions,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match users
        .create_user_with_email_and_permission_snapshot(
            &request.username,
            &request.display_name,
            &request.password,
            request.is_admin,
            request.email.as_deref(),
            permissions.stored_snapshot(),
        )
        .await
    {
        Ok(user) => {
            let target_id = user.id.to_string();
            record_audit_event(
                &state,
                &headers,
                "USER_CREATED",
                Some("user"),
                Some(&target_id),
                "{}",
            )
            .await;
            (
                StatusCode::CREATED,
                Json(json!({ "user": user_json(&user) })),
            )
                .into_response()
        }
        Err(error) => user_store_error(&headers, error),
    }
}

fn admin_user_profile_json(profile: &crate::auth::users::AdminUserProfile) -> Value {
    let mut user = user_json(&profile.user);
    user["email"] = json!(profile.email);
    user["emailVerifiedAt"] = json!(profile.email_verified_at);
    user["deletedAt"] = json!(profile.deleted_at);
    user["deletionPurgeAt"] = json!(profile.deletion_purge_at);
    admin_user_permission_preset_source_fields(&mut user, profile.preset_source.as_ref());
    user
}

fn admin_user_playback_policy_json(policy: crate::storage::UserPlaybackPolicy) -> Value {
    json!({
        "canPlay": policy.can_play,
        "maxSimultaneousPlaybacks": policy.max_simultaneous_playbacks,
        "allowRemux": policy.allow_remux,
        "allowAudioTranscode": policy.allow_audio_transcode,
        "allowVideoTranscode": policy.allow_video_transcode,
    })
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpdateUserRequest {
    pub(crate) display_name: Option<String>,
    pub(crate) password: Option<String>,
    pub(crate) is_disabled: Option<bool>,
    pub(crate) is_admin: Option<bool>,
    pub(crate) can_manage_server: Option<bool>,
    pub(crate) can_remote_access: Option<bool>,
    pub(crate) can_download: Option<bool>,
}

pub(crate) async fn admin_update_user(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<UpdateUserRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let users = match UserStore::new(database.clone()) {
        Ok(users) => users,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match users
        .update_user_with_hls_session_ids(
            &user_id,
            UserUpdate {
                display_name: request.display_name.as_deref(),
                password: request.password.as_deref(),
                is_disabled: request.is_disabled,
                is_admin: request.is_admin,
                can_manage_server: request.can_manage_server,
                can_remote_access: request.can_remote_access,
                can_download: request.can_download,
            },
        )
        .await
    {
        Ok(Some((user, hls_session_ids))) => {
            if let Some(web_playback) = state.web_playback.as_ref() {
                web_playback
                    .stop_server_hls_sessions(&hls_session_ids)
                    .await;
            }
            let event_type = if request.is_disabled == Some(true) {
                "USER_DISABLED"
            } else {
                "USER_UPDATED"
            };
            record_audit_event(
                &state,
                &headers,
                event_type,
                Some("user"),
                Some(&user_id),
                "{}",
            )
            .await;
            Json(json!({ "user": user_json(&user) })).into_response()
        }
        Ok(None) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "用户不存在",
        )
        .into_response(),
        Err(error) => user_store_error(&headers, error),
    }
}

pub(crate) async fn admin_delete_user(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let users = match UserStore::new(database.clone()) {
        Ok(users) => users,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match users.soft_delete_user(&user_id).await {
        Ok(Some(hls_session_ids)) => {
            if let Some(web_playback) = state.web_playback.as_ref() {
                web_playback
                    .stop_server_hls_sessions(&hls_session_ids)
                    .await;
            }
            let profile = match users.find_admin_profile_by_id(&user_id).await {
                Ok(Some(profile)) => profile,
                Ok(None) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            };
            record_audit_event(
                &state,
                &headers,
                "USER_DELETED",
                Some("user"),
                Some(&user_id),
                &json!({ "deletionPurgeAt": profile.deletion_purge_at }).to_string(),
            )
            .await;
            Json(json!({ "user": admin_user_profile_json(&profile) })).into_response()
        }
        Ok(None) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "用户不存在",
        )
        .into_response(),
        Err(error) => user_store_error(&headers, error),
    }
}

pub(crate) async fn admin_restore_user(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let users = match UserStore::new(database.clone()) {
        Ok(users) => users,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    match users.restore_deleted_user(&user_id).await {
        Ok(true) => {
            let profile = match users.find_admin_profile_by_id(&user_id).await {
                Ok(Some(profile)) => profile,
                Ok(None) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            };
            record_audit_event(
                &state,
                &headers,
                "USER_RESTORED",
                Some("user"),
                Some(&user_id),
                "{}",
            )
            .await;
            Json(json!({ "user": admin_user_profile_json(&profile) })).into_response()
        }
        Ok(false) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "已删除用户不存在",
        )
        .into_response(),
        Err(error) => user_store_error(&headers, error),
    }
}

pub(crate) async fn admin_purge_user(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let users = match UserStore::new(database.clone()) {
        Ok(users) => users,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let profile = match users.find_admin_profile_by_id(&user_id).await {
        Ok(Some(profile)) => profile,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => return user_store_error(&headers, error),
    };
    if profile.deleted_at.is_none() {
        return api_error(
            &headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "只能彻底删除回收站中的用户",
        )
        .into_response();
    }
    match users.purge_deleted_user(&user_id).await {
        Ok(true) => {
            if let (Some(avatars), Ok(user_id)) = (
                state.user_avatars.as_ref(),
                user_id.parse::<crate::domain::ids::UserId>(),
            ) && let Err(error) = avatars.remove(user_id).await
            {
                tracing::warn!(%error, "failed to remove purged user avatar");
            }
            record_audit_event(
                &state,
                &headers,
                "USER_PURGED",
                Some("user"),
                Some(&user_id),
                "{}",
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => api_error(
            &headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "已删除用户不存在",
        )
        .into_response(),
        Err(error) => user_store_error(&headers, error),
    }
}

pub(crate) fn user_store_error(headers: &HeaderMap, error: UserStoreError) -> Response {
    match error {
        UserStoreError::InvalidUsername
        | UserStoreError::InvalidEmail
        | UserStoreError::InvalidPlaybackPolicy
        | UserStoreError::Password(_) => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "用户请求无效",
        )
        .into_response(),
        UserStoreError::LastManager => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::PermissionDenied,
            "至少需要一个启用的服务器管理账户",
        )
        .into_response(),
        UserStoreError::InvalidUserId(_) => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "用户 ID 无效",
        )
        .into_response(),
        UserStoreError::Storage(error) if error.is_unique_violation() => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "用户名或邮箱已存在",
        )
        .into_response(),
        UserStoreError::Storage(_) => api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "用户数据暂时不可用",
        )
        .into_response(),
        UserStoreError::SetupAlreadyCompleted => api_error(
            headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "初始化已完成",
        )
        .into_response(),
    }
}

pub(crate) fn user_json(user: &UserRecord) -> Value {
    json!({
        "id": user.id.to_string(),
        "usernameNormalized": user.username_normalized,
        "displayName": user.display_name,
        "isDisabled": user.is_disabled,
        "isAdmin": user.is_admin,
        "canManageServer": user.can_manage_server,
        "canRemoteAccess": user.can_remote_access,
        "canDownload": user.can_download
    })
}

pub(crate) async fn admin_set_library_access(
    headers: HeaderMap,
    Path((user_id, library_id)): Path<(String, String)>,
    State(state): State<AppState>,
    Json(request): Json<SetLibraryAccessRequest>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let user_id = match user_id.parse::<crate::domain::ids::UserId>() {
        Ok(id) => id.to_string(),
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "用户 ID 无效",
            )
            .into_response();
        }
    };
    let library_id = match library_id.parse::<crate::domain::ids::LibraryId>() {
        Ok(id) => id.to_string(),
        Err(_) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "媒体库 ID 无效",
            )
            .into_response();
        }
    };
    let Some(database) = state.database.as_ref() else {
        return api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "服务尚未就绪",
        )
        .into_response();
    };
    match crate::application::admin_user_access::set_library_access(
        database,
        state.home.as_ref(),
        &user_id,
        &library_id,
        request.can_view,
    )
    .await
    {
        Ok(()) => {
            record_audit_event(
                &state,
                &headers,
                "LIBRARY_ACCESS_UPDATED",
                Some("user_library_access"),
                Some(&user_id),
                &format!(
                    r#"{{"libraryId":"{library_id}","canView":{}}}"#,
                    request.can_view
                ),
            )
            .await;
            Json(json!({
                "userId": user_id,
                "libraryId": library_id,
                "canView": request.can_view,
            }))
            .into_response()
        }
        Err(crate::application::admin_user_access::SetLibraryAccessError::UserNotFound) => {
            api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "用户不存在",
            )
            .into_response()
        }
        Err(crate::application::admin_user_access::SetLibraryAccessError::LibraryNotFound) => {
            api_error(
                &headers,
                StatusCode::NOT_FOUND,
                lux::ApiErrorCode::NotFound,
                "媒体库不存在",
            )
            .into_response()
        }
        Err(crate::application::admin_user_access::SetLibraryAccessError::Storage) => api_error(
            &headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "数据库暂时不可用",
        )
        .into_response(),
    }
}

pub(crate) async fn admin_get_user_avatar(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Ok(user_id) = user_id.parse::<crate::domain::ids::UserId>() else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "用户 ID 无效",
        )
        .into_response();
    };
    let Some(avatars) = state.user_avatars.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match avatars.load(user_id).await {
        Ok(Some(avatar)) => Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, avatar.content_type)
            .header(CACHE_CONTROL, "private, no-cache")
            .body(Body::from(avatar.bytes))
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => user_avatar_error(&headers, error),
    }
}

pub(crate) async fn admin_update_user_avatar(
    headers: HeaderMap,
    Path(user_id): Path<String>,
    State(state): State<AppState>,
    body: Bytes,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Ok(user_id) = user_id.parse::<crate::domain::ids::UserId>() else {
        return api_error(
            &headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "用户 ID 无效",
        )
        .into_response();
    };
    let Some(avatars) = state.user_avatars.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let content_type = headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    match avatars.store(user_id, content_type, &body).await {
        Ok(()) => Json(json!({ "avatarUrl": format!("/api/v1/admin/users/{user_id}/avatar") }))
            .into_response(),
        Err(error) => user_avatar_error(&headers, error),
    }
}

pub(super) fn api_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/admin/content-restriction-index",
            get(admin_content_restriction_index_status),
        )
        .route(
            "/api/v1/admin/user-permission-presets",
            get(admin_list_user_permission_presets).post(admin_create_user_permission_preset),
        )
        .route(
            "/api/v1/admin/user-permission-presets/{preset_id}",
            put(admin_update_user_permission_preset).delete(admin_delete_user_permission_preset),
        )
        .route(
            "/api/v1/admin/users",
            get(admin_list_users).post(admin_create_user),
        )
        .route(
            "/api/v1/admin/users/batch/permission-preset",
            post(admin_batch_apply_user_permission_preset),
        )
        .route(
            "/api/v1/admin/users/batch/status",
            post(admin_batch_update_user_status),
        )
        .route(
            "/api/v1/admin/users/account-preferences/copy/preview",
            post(admin_preview_account_preferences_copy),
        )
        .route(
            "/api/v1/admin/users/account-preferences/copy",
            post(admin_copy_account_preferences),
        )
        .route(
            "/api/v1/admin/users/permission-copy/preview",
            post(admin_preview_user_permission_copy),
        )
        .route(
            "/api/v1/admin/users/permission-copy",
            post(admin_apply_user_permission_copy),
        )
        .route(
            "/api/v1/admin/users/{user_id}",
            get(admin_get_user)
                .patch(admin_update_user)
                .delete(admin_delete_user),
        )
        .route(
            "/api/v1/admin/users/{user_id}/profile",
            patch(admin_update_user_profile),
        )
        .route(
            "/api/v1/admin/users/{user_id}/sessions",
            get(admin_list_user_sessions),
        )
        .route(
            "/api/v1/admin/users/{user_id}/devices",
            get(admin_list_user_devices),
        )
        .route(
            "/api/v1/admin/access-blacklist/global",
            get(admin_list_global_user_access_blacklist_rules)
                .post(admin_create_global_user_access_blacklist_rule),
        )
        .route(
            "/api/v1/admin/access-blacklist/global/{rule_id}",
            patch(admin_set_global_user_access_blacklist_rule_enabled)
                .delete(admin_delete_global_user_access_blacklist_rule),
        )
        .route(
            "/api/v1/admin/users/{user_id}/access-blacklist",
            get(admin_list_scoped_user_access_blacklist_rules)
                .post(admin_create_scoped_user_access_blacklist_rule),
        )
        .route(
            "/api/v1/admin/users/{user_id}/access-blacklist/{rule_id}",
            patch(admin_set_scoped_user_access_blacklist_rule_enabled)
                .delete(admin_delete_scoped_user_access_blacklist_rule),
        )
        .route(
            "/api/v1/admin/users/{user_id}/sessions/{session_id}",
            delete(admin_revoke_user_session),
        )
        .route(
            "/api/v1/admin/users/{user_id}/emby-sessions",
            get(admin_list_user_emby_login_sessions)
                .delete(admin_revoke_all_user_emby_login_sessions),
        )
        .route(
            "/api/v1/admin/users/{user_id}/activity",
            get(admin_list_user_activity),
        )
        .route(
            "/api/v1/admin/users/{user_id}/playback-sessions",
            get(admin_list_user_playback_sessions),
        )
        .route(
            "/api/v1/admin/users/{user_id}/playback-sessions/{playback_id}",
            delete(admin_stop_user_playback_session),
        )
        .route(
            "/api/v1/admin/users/{user_id}/emby-sessions/{session_id}",
            delete(admin_revoke_user_emby_login_session),
        )
        .route(
            "/api/v1/admin/users/{user_id}/permission-preset",
            post(admin_apply_user_permission_preset),
        )
        .route(
            "/api/v1/admin/users/{user_id}/playback-policy",
            get(admin_get_user_playback_policy).put(admin_set_user_playback_policy),
        )
        .route(
            "/api/v1/admin/users/{user_id}/content-restrictions",
            get(admin_get_user_content_restrictions).put(admin_update_user_content_restrictions),
        )
        .route(
            "/api/v1/admin/users/{user_id}/avatar",
            get(admin_get_user_avatar).put(admin_update_user_avatar),
        )
        .route(
            "/api/v1/admin/users/{user_id}/restore",
            post(admin_restore_user),
        )
        .route(
            "/api/v1/admin/users/{user_id}/purge",
            delete(admin_purge_user),
        )
        .route(
            "/api/v1/admin/users/{user_id}/libraries",
            get(admin_list_user_library_access).put(admin_replace_user_library_access),
        )
        .route(
            "/api/v1/admin/users/{user_id}/libraries/{library_id}",
            patch(admin_set_library_access),
        )
}
