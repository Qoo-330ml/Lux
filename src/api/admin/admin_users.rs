use super::*;

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
    user["presetSource"] = Value::Null;
    user
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
            "/api/v1/admin/users",
            get(admin_list_users).post(admin_create_user),
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
            "/api/v1/admin/users/{user_id}/playback-policy",
            get(admin_get_user_playback_policy).put(admin_set_user_playback_policy),
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
