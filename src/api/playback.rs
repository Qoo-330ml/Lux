use super::*;

pub(super) async fn emby_playback_info(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    raw_query: RawQuery,
    State(state): State<AppState>,
) -> Response {
    let query = emby_stream_query_from_raw(raw_query);
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let item = match catalog.find_item(principal, &item_id).await {
        Ok(Some(item)) => item,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(CatalogError::Storage(_)) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => {
            return StatusCode::NOT_FOUND.into_response();
        }
    };
    let mut sources = item.media_sources.iter().collect::<Vec<_>>();
    sources.sort_by(|left, right| {
        right
            .is_default
            .cmp(&left.is_default)
            .then_with(|| left.id.cmp(&right.id))
    });
    if let Some(source_id) = query.media_source_id {
        let Some(index) = sources.iter().position(|source| source.id == source_id) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let source = sources.remove(index);
        sources.insert(0, source);
    }
    let strm_resolver_available = if sources
        .iter()
        .any(|source| emby_source_needs_strm_resolver(source))
    {
        match state.plugins.as_ref() {
            Some(plugins) => match plugins.has_available_strm_resolver().await {
                Ok(available) => available,
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            },
            None => false,
        }
    } else {
        false
    };
    Json(json!({
        "PlaySessionId": Uuid::now_v7().to_string(),
        "MediaSources": sources
            .into_iter()
            .map(|source| {
                let mut value = emby_media_source_json_with_resolver(
                    &item.id,
                    source,
                    true,
                    strm_resolver_available,
                );
                let has_direct_stream_url = value
                    .get("DirectStreamUrl")
                    .is_some_and(Value::is_string);
                if has_direct_stream_url
                    && let Some(service) = state.web_playback.as_ref()
                    && let Some(url) = emby_signed_direct_stream_url(service, &item.id, source, &user)
                    && let Value::Object(object) = &mut value
                {
                    object.insert("DirectStreamUrl".to_owned(), json!(url));
                    // The signed URL is already authorized. Do not ask clients
                    // to append a long-lived Emby token to it as well.
                    object.insert("AddApiKeyToDirectStreamUrl".to_owned(), json!(false));
                }
                value
            })
            .collect::<Vec<_>>(),
    }))
    .into_response()
}

#[derive(Deserialize, Default)]
pub(super) struct PlaybackEventRequest {
    #[serde(rename = "ItemId", alias = "itemId", alias = "mediaServerItemId")]
    item_id: String,
    #[serde(
        rename = "MediaSourceId",
        alias = "mediaSourceId",
        alias = "mediaServerMediaSourceId"
    )]
    media_source_id: Option<String>,
    #[serde(
        rename = "PlaySessionId",
        alias = "playSessionId",
        alias = "mediaServerPlaySessionId"
    )]
    play_session_id: Option<String>,
    #[serde(
        rename = "PositionTicks",
        alias = "positionTicks",
        alias = "PlaybackPositionTicks",
        alias = "playbackPositionTicks",
        default
    )]
    position_ticks: i64,
    #[serde(rename = "RunTimeTicks", alias = "runTimeTicks")]
    duration_ticks: Option<i64>,
    #[serde(rename = "IsPaused", alias = "isPaused", default)]
    is_paused: bool,
    #[serde(rename = "DeviceId", alias = "deviceId")]
    device_id: Option<String>,
    #[serde(rename = "Client", alias = "client")]
    client: Option<String>,
    #[serde(rename = "DeviceName", alias = "deviceName", alias = "Device")]
    device_name: Option<String>,
    #[serde(
        rename = "ApplicationVersion",
        alias = "applicationVersion",
        alias = "ClientVersion",
        alias = "clientVersion",
        alias = "Version"
    )]
    client_version: Option<String>,
    #[serde(rename = "DeviceType", alias = "deviceType")]
    device_type: Option<String>,
}

pub(super) async fn emby_playing(
    headers: HeaderMap,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
    Json(request): Json<PlaybackEventRequest>,
) -> Response {
    handle_emby_playback_event(headers, query, state, request, "PLAYING").await
}

pub(super) async fn emby_playing_progress(
    headers: HeaderMap,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
    Json(request): Json<PlaybackEventRequest>,
) -> Response {
    let state_name = if request.is_paused {
        "PAUSED"
    } else {
        "PLAYING"
    };
    handle_emby_playback_event(headers, query, state, request, state_name).await
}

pub(super) async fn emby_playing_stopped(
    headers: HeaderMap,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
    Json(request): Json<PlaybackEventRequest>,
) -> Response {
    handle_emby_playback_event(headers, query, state, request, "STOPPED").await
}

pub(super) async fn handle_emby_playback_event(
    headers: HeaderMap,
    query: EmbyTokenQuery,
    state: AppState,
    request: PlaybackEventRequest,
    state_name: &'static str,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => {
            tracing::warn!(
                event = "emby_playback_callback_rejected",
                stage = "authentication",
                status_code = status.as_u16(),
                playback_state = state_name,
                "rejected emby playback callback"
            );
            return status.into_response();
        }
    };
    if request.position_ticks < 0
        || request.duration_ticks.is_some_and(|duration| duration < 0)
        || request.item_id.is_empty()
    {
        tracing::warn!(
            event = "emby_playback_callback_rejected",
            stage = "validation",
            status_code = StatusCode::BAD_REQUEST.as_u16(),
            playback_state = state_name,
            item_id_present = !request.item_id.is_empty(),
            position_ticks = request.position_ticks,
            duration_ticks_present = request.duration_ticks.is_some(),
            "rejected invalid emby playback callback"
        );
        return StatusCode::BAD_REQUEST.into_response();
    }
    let item_id_prefix = playback_identifier_prefix(&request.item_id);
    let Some(access) = state.access.as_ref() else {
        tracing::error!(
            event = "emby_playback_callback_rejected",
            stage = "access_service",
            status_code = StatusCode::SERVICE_UNAVAILABLE.as_u16(),
            playback_state = state_name,
            item_id_prefix = %item_id_prefix,
            "playback access service is unavailable"
        );
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match access
        .can_view_item(
            AccessPrincipal::new(user.id, user.is_admin),
            &request.item_id,
        )
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            tracing::warn!(
                event = "emby_playback_callback_rejected",
                stage = "item_access",
                status_code = StatusCode::NOT_FOUND.as_u16(),
                playback_state = state_name,
                item_id_prefix = %item_id_prefix,
                "playback item is not accessible"
            );
            return StatusCode::NOT_FOUND.into_response();
        }
        Err(error) => {
            tracing::error!(
                event = "emby_playback_callback_rejected",
                stage = "item_access",
                status_code = StatusCode::SERVICE_UNAVAILABLE.as_u16(),
                playback_state = state_name,
                item_id_prefix = %item_id_prefix,
                error = %error,
                "failed to check playback item access"
            );
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    }
    let Some(database) = state.database.as_ref() else {
        tracing::error!(
            event = "emby_playback_callback_rejected",
            stage = "database",
            status_code = StatusCode::SERVICE_UNAVAILABLE.as_u16(),
            playback_state = state_name,
            item_id_prefix = %item_id_prefix,
            "playback database is unavailable"
        );
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let media_source_id = request
        .media_source_id
        .as_deref()
        .filter(|value| !value.is_empty());
    if let Some(media_source_id) = media_source_id {
        match database
            .media_source_belongs_to_item(media_source_id, &request.item_id)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                tracing::warn!(
                    event = "emby_playback_callback_rejected",
                    stage = "media_source",
                    status_code = StatusCode::NOT_FOUND.as_u16(),
                    playback_state = state_name,
                    item_id_prefix = %item_id_prefix,
                    "playback media source does not belong to item"
                );
                return StatusCode::NOT_FOUND.into_response();
            }
            Err(error) => {
                tracing::error!(
                    event = "emby_playback_callback_rejected",
                    stage = "media_source",
                    status_code = StatusCode::SERVICE_UNAVAILABLE.as_u16(),
                    playback_state = state_name,
                    item_id_prefix = %item_id_prefix,
                    error = %error,
                    "failed to check playback media source"
                );
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
        }
    }
    let mut header_device = emby_device_info_from_headers(&headers);
    if header_device.client.is_empty()
        || header_device.device.is_empty()
        || header_device.device_id.is_empty()
        || header_device.version.is_empty()
    {
        let token = emby_token_from_headers(&headers).or_else(|| query.api_key.clone());
        if let (Some(auth), Some(token)) = (state.emby_auth.as_ref(), token) {
            match auth.device_info(&token).await {
                Ok(Some(device)) => merge_emby_device_info(&mut header_device, device),
                Ok(None) => {}
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            }
        }
    }
    let device_id = request
        .device_id
        .filter(|value| !value.is_empty())
        .or_else(|| (!header_device.device_id.is_empty()).then_some(header_device.device_id))
        .unwrap_or_else(|| "unknown".to_owned());
    let client = request
        .client
        .as_deref()
        .or_else(|| (!header_device.client.is_empty()).then_some(header_device.client.as_str()));
    let device_name = request
        .device_name
        .as_deref()
        .or_else(|| (!header_device.device.is_empty()).then_some(header_device.device.as_str()));
    let client_version = request
        .client_version
        .as_deref()
        .or_else(|| (!header_device.version.is_empty()).then_some(header_device.version.as_str()));
    let device_type = request
        .device_type
        .as_deref()
        .or_else(|| (!header_device.device.is_empty()).then_some(header_device.device.as_str()));
    let play_session_id = request
        .play_session_id
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("{}:{device_id}", request.item_id));
    let user_id = user.id.to_string();
    let played_percent = match database.user_played_percent(&user_id).await {
        Ok(value) => value,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let previous_session = match database
        .find_playback_session(&user_id, &play_session_id)
        .await
    {
        Ok(session) => session,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let activity_event = playback_activity_event_type(previous_session.as_ref(), state_name);
    let occurred_at = current_unix_timestamp();
    let webhook_event = webhook_event_type_for_playback(
        activity_event,
        should_publish_playback_progress(
            previous_session.as_ref(),
            state_name,
            request.position_ticks,
            occurred_at,
        ),
    );
    let remote_ip = request_client_ip(&headers, &state.remote_access);
    let activity_remote_ip = remote_ip.as_deref().or_else(|| {
        previous_session
            .as_ref()
            .and_then(|session| session.remote_ip.as_deref())
    });
    match database
        .record_playback_event(NewPlaybackEvent {
            user_id: &user_id,
            item_id: &request.item_id,
            media_source_id,
            play_session_id: &play_session_id,
            device_id: &device_id,
            client,
            device_name,
            client_version,
            device_type,
            remote_ip: remote_ip.as_deref(),
            state: state_name,
            position_ticks: request.position_ticks,
            duration_ticks: request.duration_ticks,
            played_percent,
            is_paused: request.is_paused || state_name == "PAUSED",
        })
        .await
    {
        Ok(()) => {
            if database
                .sync_played_container_states(&user_id, &request.item_id)
                .await
                .is_err()
            {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            if let Some(event_type) = activity_event {
                record_activity_event(
                    Some(database),
                    &state.admin_events,
                    &user_id,
                    event_type,
                    Some(&request.item_id),
                    json!({
                        "client": client,
                        "clientVersion": client_version,
                        "deviceName": device_name,
                        "deviceType": device_type,
                        "state": state_name,
                        "remoteIp": activity_remote_ip,
                    }),
                )
                .await;
            }
            if let Some(event_type) = webhook_event {
                publish_playback_webhook(
                    &state,
                    event_type,
                    occurred_at,
                    &request.item_id,
                    media_source_id,
                    &play_session_id,
                    state_name,
                    request.position_ticks,
                    request.duration_ticks,
                    request.is_paused || state_name == "PAUSED",
                    client,
                    device_name,
                    device_type,
                    client_version,
                )
                .await;
            }
            tracing::info!(
                event = "emby_playback_callback_recorded",
                playback_state = state_name,
                item_id_prefix = %item_id_prefix,
                position_ticks = request.position_ticks,
                duration_ticks_present = request.duration_ticks.is_some(),
                is_paused = request.is_paused || state_name == "PAUSED",
                client = playback_client_label(client),
                "recorded emby playback callback"
            );
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => {
            tracing::error!(
                event = "emby_playback_callback_rejected",
                stage = "storage",
                status_code = StatusCode::SERVICE_UNAVAILABLE.as_u16(),
                playback_state = state_name,
                item_id_prefix = %item_id_prefix,
                error = %error,
                "failed to record emby playback callback"
            );
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

pub(super) fn playback_identifier_prefix(value: &str) -> String {
    value.chars().take(8).collect()
}

pub(super) fn playback_client_label(value: Option<&str>) -> &'static str {
    match value.map(str::trim) {
        Some(value) if value.eq_ignore_ascii_case("vidhub") => "vidhub",
        Some(value) if value.eq_ignore_ascii_case("senplayer") => "senplayer",
        Some(value) if value.eq_ignore_ascii_case("infuse") => "infuse",
        Some(_) => "other",
        None => "unknown",
    }
}

const PLAYBACK_WEBHOOK_PROGRESS_INTERVAL_SECONDS: i64 = 30;

pub(super) fn current_unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default()
}

pub(super) fn should_publish_playback_progress(
    previous: Option<&StoredPlaybackSession>,
    state_name: &str,
    position_ticks: i64,
    occurred_at: i64,
) -> bool {
    matches!(state_name, "PLAYING" | "PAUSED")
        && previous.is_some_and(|session| {
            occurred_at.saturating_sub(session.last_event_at)
                >= PLAYBACK_WEBHOOK_PROGRESS_INTERVAL_SECONDS
                && position_ticks > session.position_ticks
        })
}

pub(super) fn webhook_event_type_for_playback(
    activity_event: Option<&str>,
    progress_due: bool,
) -> Option<WebhookEventType> {
    match activity_event {
        Some("PLAYBACK_STARTED") => Some(WebhookEventType::PlaybackStarted),
        Some("PLAYBACK_PAUSED") => Some(WebhookEventType::PlaybackPaused),
        Some("PLAYBACK_STOPPED") => Some(WebhookEventType::PlaybackStopped),
        _ if progress_due => Some(WebhookEventType::PlaybackProgress),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn publish_playback_webhook(
    state: &AppState,
    event_type: WebhookEventType,
    occurred_at: i64,
    item_id: &str,
    media_source_id: Option<&str>,
    play_session_id: &str,
    state_name: &str,
    position_ticks: i64,
    duration_ticks: Option<i64>,
    is_paused: bool,
    client: Option<&str>,
    device_name: Option<&str>,
    device_type: Option<&str>,
    client_version: Option<&str>,
) {
    let Some(webhooks) = state.webhooks.as_ref() else {
        return;
    };
    let dedupe_key = format!(
        "playback:{play_session_id}:{}:{occurred_at}",
        event_type.as_str()
    );
    let data = json!({
        "itemId": item_id,
        "mediaSourceId": media_source_id,
        "playSessionId": play_session_id,
        "state": state_name,
        "positionTicks": position_ticks,
        "durationTicks": duration_ticks,
        "isPaused": is_paused,
        "client": bounded_playback_text(client),
        "deviceName": bounded_playback_text(device_name),
        "deviceType": bounded_playback_text(device_type),
        "clientVersion": bounded_playback_text(client_version),
    });
    if let Err(error) = webhooks
        .publish(event_type, &dedupe_key, occurred_at, data)
        .await
    {
        tracing::warn!(
            event = "playback_webhook_enqueue_failed",
            webhook_event_type = event_type.as_str(),
            error = %error,
            "failed to enqueue playback webhook"
        );
    }
}

pub(super) fn bounded_playback_text(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    (value.chars().count() <= 128).then(|| value.to_owned())
}

pub(super) async fn emby_sessions(
    headers: HeaderMap,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let user_id = user.id.to_string();
    let sessions = match database
        .list_playback_sessions((!user.is_admin).then_some(user_id.as_str()))
        .await
    {
        Ok(sessions) => sessions,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    Json(sessions.iter().map(emby_session_json).collect::<Vec<_>>()).into_response()
}

pub(super) fn emby_session_json(session: &crate::storage::StoredPlaybackSession) -> Value {
    json!({
        "Id": session.id,
        "UserId": session.user_id,
        "ItemId": session.item_id,
        "MediaSourceId": session.media_source_id,
        "PlaySessionId": session.play_session_id,
        "Client": session.client,
        "DeviceId": session.device_id,
        "DeviceName": session.device_name,
        "DeviceType": session.device_type,
        "ApplicationVersion": session.client_version,
        "RemoteEndPoint": session.remote_ip,
        "PlayState": {
            "PositionTicks": session.position_ticks,
            "IsPaused": session.is_paused,
            "CanSeek": true,
            "PlayMethod": "DirectPlay",
        },
        "RunTimeTicks": session.duration_ticks,
        "LastActivityDate": session.last_event_at,
    })
}

pub(super) async fn lux_get_playback(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match access
        .can_view_item(AccessPrincipal::new(user.id, user.is_admin), &item_id)
        .await
    {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let user_id = user.id.to_string();
    let user_state = match database.find_user_item_state(&user_id, &item_id).await {
        Ok(state) => state,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let active_session = match database
        .find_active_playback_session(&user_id, &item_id)
        .await
    {
        Ok(session) => session,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    Json(json!({
        "itemId": item_id,
        "positionTicks": user_state.as_ref().map(|value| value.position_ticks).unwrap_or_default(),
        "isPlayed": user_state.as_ref().map(|value| value.is_played).unwrap_or(false),
        "isFavorite": user_state.as_ref().map(|value| value.is_favorite).unwrap_or(false),
        "playCount": user_state.as_ref().map(|value| value.play_count).unwrap_or_default(),
        "state": active_session.as_ref().map(|value| value.state.as_str()),
        "isPaused": active_session.as_ref().map(|value| value.is_paused).unwrap_or(false),
        "lastEventAt": active_session.as_ref().map(|value| value.last_event_at),
    }))
    .into_response()
}

pub(super) async fn lux_list_playback_history(
    headers: HeaderMap,
    Query(query): Query<AdminJobsQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let (offset, limit) = match page_params(query.page, query.page_size) {
        Ok(params) => params,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    let Some(service) = state.emby_migration.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service
        .list_playback_history(&user.id.to_string(), offset, limit)
        .await
    {
        Ok(events) => Json(json!({
            "events": events,
            "page": offset / limit + 1,
            "pageSize": limit,
        }))
        .into_response(),
        Err(error) => emby_migration_error(&headers, error),
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(super) struct WebPlaybackCapabilitiesRequest {
    #[serde(default)]
    direct_play: bool,
    #[serde(default)]
    hls: bool,
    #[serde(default)]
    video_copy_to_fmp4: bool,
    #[serde(default)]
    audio_copy_to_fmp4: bool,
    #[serde(default)]
    hardware_transcode: bool,
    #[serde(default)]
    software_transcode: bool,
}

impl From<WebPlaybackCapabilitiesRequest> for PlaybackCapabilities {
    fn from(value: WebPlaybackCapabilitiesRequest) -> Self {
        Self {
            direct_play: value.direct_play,
            hls: value.hls,
            video_copy_to_fmp4: value.video_copy_to_fmp4,
            audio_copy_to_fmp4: value.audio_copy_to_fmp4,
            hardware_transcode: value.hardware_transcode,
            software_transcode: value.software_transcode,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WebPlaybackSessionRequest {
    item_id: String,
    source_id: String,
    #[serde(default)]
    capabilities: WebPlaybackCapabilitiesRequest,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WebPlaybackResourceQuery {
    expires: i64,
    signature: String,
}

pub(super) fn web_playback_error(headers: &HeaderMap, error: WebPlaybackSessionError) -> Response {
    let (status, code, message) = match error {
        WebPlaybackSessionError::Invalid(message) => (
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            message,
        ),
        WebPlaybackSessionError::NotFound => (
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "播放会话不存在".to_owned(),
        ),
        WebPlaybackSessionError::Expired => (
            StatusCode::GONE,
            lux::ApiErrorCode::NotFound,
            "播放会话已过期".to_owned(),
        ),
        WebPlaybackSessionError::NotActive => (
            StatusCode::GONE,
            lux::ApiErrorCode::NotFound,
            "播放会话已结束".to_owned(),
        ),
        WebPlaybackSessionError::Hls(_) => (
            StatusCode::BAD_GATEWAY,
            lux::ApiErrorCode::Internal,
            "服务端 HLS 暂时不可用".to_owned(),
        ),
        WebPlaybackSessionError::Storage(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "播放会话暂时不可用".to_owned(),
        ),
    };
    api_error(headers, status, code, &message).into_response()
}

pub(super) fn web_playback_resource_url(
    service: &WebPlaybackSessionService,
    session_id: &str,
    resource: &str,
    expires_at: i64,
) -> Option<String> {
    let signature = service.sign_resource(session_id, resource, expires_at)?;
    Some(format!(
        "/api/v1/playback/sessions/{session_id}/{resource}?expires={}&signature={}",
        signature.expires_at, signature.signature
    ))
}

pub(super) fn web_playback_hls_url(
    service: &WebPlaybackSessionService,
    session_id: &str,
    asset: &str,
    expires_at: i64,
) -> Option<String> {
    let resource = format!("hls:{asset}");
    let signature = service.sign_resource(session_id, &resource, expires_at)?;
    Some(format!(
        "/api/v1/playback/sessions/{session_id}/hls/{asset}?expires={}&signature={}",
        signature.expires_at, signature.signature
    ))
}

pub(super) async fn lux_create_web_playback_session(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<WebPlaybackSessionRequest>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_web_csrf(&headers, &state).await {
        return response;
    }
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let source = match access
        .authorized_playback_source(principal, &request.item_id, Some(&request.source_id))
        .await
    {
        Ok(Some(source)) => source,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let source_kind = match source.source_kind.as_str() {
        "LOCAL_FILE" => PlaybackSourceKind::LocalFile,
        "STRM_URL" => PlaybackSourceKind::Strm,
        _ => return StatusCode::NOT_IMPLEMENTED.into_response(),
    };
    let Some(service) = state.web_playback.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let created = match service
        .create(CreateWebPlaybackSession {
            user_id: &user.id.to_string(),
            is_admin: user.is_admin,
            item_id: &request.item_id,
            media_source_id: &request.source_id,
            source_kind,
            capabilities: request.capabilities.into(),
        })
        .await
    {
        Ok(created) => created,
        Err(error) => return web_playback_error(&headers, error),
    };
    if let WebPlaybackPlan::ServerHls { tier } = &created.plan {
        if source.source_kind != "LOCAL_FILE" {
            let _ = service.stop(&created.id, &user.id.to_string()).await;
            return StatusCode::NOT_IMPLEMENTED.into_response();
        }
        let input = match canonical_local_media_path(&source.root_path, &source.relative_path).await
        {
            Ok(path) => path,
            Err(LocalPathError::Missing) => {
                let _ = service.stop(&created.id, &user.id.to_string()).await;
                return StatusCode::NOT_FOUND.into_response();
            }
            Err(LocalPathError::Forbidden) => {
                let _ = service.stop(&created.id, &user.id.to_string()).await;
                return StatusCode::FORBIDDEN.into_response();
            }
        };
        if let Err(error) = service.start_hls(&created.id, *tier, &input).await {
            let _ = service.stop(&created.id, &user.id.to_string()).await;
            return web_playback_error(&headers, error);
        }
    }
    let plan = match &created.plan {
        WebPlaybackPlan::Direct => {
            let proxy_url = if source.source_kind == "STRM_URL"
                && source.external_url.as_deref().is_some_and(|target| {
                    matches!(classify_strm_target(target).kind, StrmTargetKind::Path)
                }) {
                Some(super::emby_catalog::emby_media_source_stream_url_parts(
                    &request.item_id,
                    &source.source_id,
                    &source.source_kind,
                    source.container.as_deref(),
                ))
            } else {
                None
            };
            json!({
                "type": "DIRECT",
                "url": web_playback_resource_url(service, &created.id, "direct", created.expires_at),
                "proxyUrl": proxy_url,
            })
        }
        WebPlaybackPlan::ServerHls { tier } => json!({
            "type": "SERVER_HLS",
            "manifestUrl": web_playback_hls_url(
                service,
                &created.id,
                "index.m3u8",
                created.expires_at,
            ),
            "tier": tier.number(),
        }),
        WebPlaybackPlan::Unsupported { reason } => json!({
            "type": "UNSUPPORTED",
            "reason": reason.to_string(),
        }),
    };
    Json(json!({
        "sessionId": (!created.id.is_empty()).then_some(created.id),
        "playSessionId": (!created.play_session_id.is_empty()).then_some(created.play_session_id),
        "tier": created.plan.tier().number(),
        "expiresAt": created.expires_at,
        "plan": plan,
        "sourceId": created.media_source_id,
    }))
    .into_response()
}

pub(super) async fn lux_web_playback_direct(
    headers: HeaderMap,
    method: Method,
    Path(session_id): Path<String>,
    Query(query): Query<WebPlaybackResourceQuery>,
    State(state): State<AppState>,
) -> Response {
    let Some(service) = state.web_playback.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let session = match service
        .authorize_resource(&session_id, "direct", query.expires, &query.signature)
        .await
    {
        Ok(session) => session,
        Err(error) => return web_playback_error(&headers, error),
    };
    if session.plan != "DIRECT" {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Ok(user_id) = session.user_id.parse::<crate::domain::ids::UserId>() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    serve_media_file(
        &state,
        AccessPrincipal::new(user_id, session.is_admin),
        &headers,
        &method,
        &session.item_id,
        session.media_source_id.as_deref(),
        None,
    )
    .await
}

pub(super) async fn lux_web_playback_hls(
    headers: HeaderMap,
    method: Method,
    Path((session_id, asset)): Path<(String, String)>,
    Query(query): Query<WebPlaybackResourceQuery>,
    State(state): State<AppState>,
) -> Response {
    let Some(service) = state.web_playback.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let resource = format!("hls:{asset}");
    let session = match service
        .authorize_resource(&session_id, &resource, query.expires, &query.signature)
        .await
    {
        Ok(session) => session,
        Err(error) => return web_playback_error(&headers, error),
    };
    if session.plan != "SERVER_HLS" {
        return StatusCode::NOT_FOUND.into_response();
    }
    if matches!(asset.as_str(), "index.m3u8") {
        let path = match service.wait_for_hls_manifest(&session_id).await {
            Ok(path) => path,
            Err(error) => return web_playback_error(&headers, error),
        };
        let Ok(bytes) = fs::read(path).await else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let Ok(manifest) = String::from_utf8(bytes) else {
            return StatusCode::BAD_GATEWAY.into_response();
        };
        let Some(manifest) = rewrite_hls_manifest(&manifest, |asset| {
            web_playback_hls_url(service, &session_id, asset, session.expires_at)
        }) else {
            return StatusCode::BAD_GATEWAY.into_response();
        };
        return Response::builder()
            .status(StatusCode::OK)
            .header("Cache-Control", "private, no-store")
            .header("Content-Type", "application/vnd.apple.mpegurl")
            .header("Content-Length", manifest.len())
            .body(if method == Method::HEAD {
                Body::empty()
            } else {
                Body::from(manifest)
            })
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    let path = match service.hls_asset_path(&session_id, &asset).await {
        Ok(path) => path,
        Err(error) => return web_playback_error(&headers, error),
    };
    match service.hls_within_quota(&session_id).await {
        Ok(true) => {}
        Ok(false) => {
            let _ = service.stop(&session_id, &session.user_id).await;
            return StatusCode::INSUFFICIENT_STORAGE.into_response();
        }
        Err(error) => return web_playback_error(&headers, error),
    }
    let metadata = match fs::metadata(&path).await {
        Ok(metadata) if metadata.is_file() => metadata,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let content_type = if asset.ends_with(".m4s") || asset.ends_with(".mp4") {
        "video/mp4"
    } else {
        "application/octet-stream"
    };
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        let Ok(file) = fs::File::open(path).await else {
            return StatusCode::NOT_FOUND.into_response();
        };
        Body::from_stream(tokio_util::io::ReaderStream::new(file))
    };
    Response::builder()
        .status(StatusCode::OK)
        .header("Cache-Control", "private, no-store")
        .header("Content-Type", content_type)
        .header("Content-Length", metadata.len())
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

pub(super) fn rewrite_hls_manifest(
    manifest: &str,
    mut url_for_asset: impl FnMut(&str) -> Option<String>,
) -> Option<String> {
    let mut output = String::with_capacity(manifest.len() + 256);
    for line in manifest.lines() {
        let mut rewritten = line.to_owned();
        if let Some(uri_start) = line.find("URI=\"") {
            let value_start = uri_start + 5;
            let value_end = line[value_start..].find('\"')? + value_start;
            let asset = &line[value_start..value_end];
            let url = url_for_asset(asset)?;
            rewritten.replace_range(value_start..value_end, &url);
        } else if !line.trim_start().starts_with('#') && !line.trim().is_empty() {
            let asset = line.trim();
            let url = url_for_asset(asset)?;
            rewritten = line.replace(asset, &url);
        }
        output.push_str(&rewritten);
        output.push('\n');
    }
    Some(output)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WebPlaybackEventRequest {
    event_id: String,
    sequence: i64,
    state: LuxPlaybackState,
    position_ticks: i64,
    duration_ticks: Option<i64>,
}

pub(super) async fn lux_web_playback_event(
    headers: HeaderMap,
    Path(session_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<WebPlaybackEventRequest>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_web_csrf(&headers, &state).await {
        return response;
    }
    let Some(service) = state.web_playback.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let (claim, session) = match service
        .claim_event(WebPlaybackEvent {
            session_id: &session_id,
            user_id: &user.id.to_string(),
            event_id: &request.event_id,
            sequence: request.sequence,
            state: request.state.as_str(),
            position_ticks: request.position_ticks,
            duration_ticks: request.duration_ticks,
        })
        .await
    {
        Ok(result) => result,
        Err(error) => return web_playback_error(&headers, error),
    };
    let Some(session) = session else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if claim == WebPlaybackEventClaim::Accepted {
        let Some(database) = state.database.as_ref() else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        let played_percent = match database.user_played_percent(&user.id.to_string()).await {
            Ok(value) => value,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        if database
            .record_playback_event(NewPlaybackEvent {
                user_id: &session.user_id,
                item_id: &session.item_id,
                media_source_id: session.media_source_id.as_deref(),
                play_session_id: &session.play_session_id,
                device_id: "lux-web",
                client: Some("Lux"),
                device_name: Some("Web"),
                client_version: None,
                device_type: Some("Web"),
                remote_ip: request_client_ip(&headers, &state.remote_access).as_deref(),
                state: request.state.as_str(),
                position_ticks: request.position_ticks,
                duration_ticks: request.duration_ticks,
                played_percent,
                is_paused: matches!(request.state, LuxPlaybackState::Paused),
            })
            .await
            .is_err()
        {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        if database
            .sync_played_container_states(&session.user_id, &session.item_id)
            .await
            .is_err()
        {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    }
    Json(json!({
        "accepted": claim == WebPlaybackEventClaim::Accepted,
        "duplicate": claim == WebPlaybackEventClaim::Duplicate,
        "stale": claim == WebPlaybackEventClaim::Stale,
    }))
    .into_response()
}

pub(super) async fn lux_web_playback_heartbeat(
    headers: HeaderMap,
    Path(session_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_web_csrf(&headers, &state).await {
        return response;
    }
    let Some(service) = state.web_playback.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.heartbeat(&session_id, &user.id.to_string()).await {
        Ok(expires_at) => {
            Json(json!({ "sessionId": session_id, "expiresAt": expires_at })).into_response()
        }
        Err(error) => web_playback_error(&headers, error),
    }
}

pub(super) async fn lux_delete_web_playback_session(
    headers: HeaderMap,
    Path(session_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_web_csrf(&headers, &state).await {
        return response;
    }
    let Some(service) = state.web_playback.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.stop(&session_id, &user.id.to_string()).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => web_playback_error(&headers, error),
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(super) enum LuxPlaybackState {
    #[default]
    Playing,
    Paused,
    Stopped,
}

impl LuxPlaybackState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Playing => "PLAYING",
            Self::Paused => "PAUSED",
            Self::Stopped => "STOPPED",
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LuxProgressRequest {
    position_ticks: i64,
    duration_ticks: Option<i64>,
    #[serde(default)]
    state: LuxPlaybackState,
}

pub(super) async fn lux_post_progress(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<LuxProgressRequest>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_web_csrf(&headers, &state).await {
        return response;
    }
    if request.position_ticks < 0 || request.duration_ticks.is_some_and(|duration| duration < 0) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match access
        .can_view_item(AccessPrincipal::new(user.id, user.is_admin), &item_id)
        .await
    {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let user_id = user.id.to_string();
    let play_session_id = format!("lux-web:{user_id}:{item_id}");
    let playback_state = request.state;
    let previous_session = match database
        .find_playback_session(&user_id, &play_session_id)
        .await
    {
        Ok(session) => session,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let activity_event =
        playback_activity_event_type(previous_session.as_ref(), playback_state.as_str());
    let occurred_at = current_unix_timestamp();
    let webhook_event = webhook_event_type_for_playback(
        activity_event,
        should_publish_playback_progress(
            previous_session.as_ref(),
            playback_state.as_str(),
            request.position_ticks,
            occurred_at,
        ),
    );
    let remote_ip = request_client_ip(&headers, &state.remote_access);
    let activity_remote_ip = remote_ip.as_deref().or_else(|| {
        previous_session
            .as_ref()
            .and_then(|session| session.remote_ip.as_deref())
    });
    match database
        .record_playback_event(NewPlaybackEvent {
            user_id: &user_id,
            item_id: &item_id,
            media_source_id: None,
            play_session_id: &play_session_id,
            device_id: "lux-web",
            client: Some("Lux"),
            device_name: Some("Web"),
            client_version: None,
            device_type: Some("Web"),
            remote_ip: remote_ip.as_deref(),
            state: playback_state.as_str(),
            position_ticks: request.position_ticks,
            duration_ticks: request.duration_ticks,
            played_percent: match database.user_played_percent(&user_id).await {
                Ok(value) => value,
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            },
            is_paused: matches!(playback_state, LuxPlaybackState::Paused),
        })
        .await
    {
        Ok(()) => {
            if database
                .sync_played_container_states(&user_id, &item_id)
                .await
                .is_err()
            {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            if let Some(event_type) = activity_event {
                record_activity_event(
                    Some(database),
                    &state.admin_events,
                    &user_id,
                    event_type,
                    Some(&item_id),
                    json!({
                        "client": "Lux",
                        "deviceType": "Web",
                        "deviceName": "Web",
                        "state": playback_state.as_str(),
                        "remoteIp": activity_remote_ip,
                    }),
                )
                .await;
            }
            if let Some(event_type) = webhook_event {
                publish_playback_webhook(
                    &state,
                    event_type,
                    occurred_at,
                    &item_id,
                    None,
                    &play_session_id,
                    playback_state.as_str(),
                    request.position_ticks,
                    request.duration_ticks,
                    matches!(playback_state, LuxPlaybackState::Paused),
                    Some("Lux"),
                    Some("Web"),
                    Some("Web"),
                    None,
                )
                .await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn emby_mark_played(
    headers: HeaderMap,
    Path((user_id, item_id)): Path<(String, String)>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    handle_emby_user_flag(headers, user_id, item_id, query, state, true, true).await
}

pub(super) async fn emby_unmark_played(
    headers: HeaderMap,
    Path((user_id, item_id)): Path<(String, String)>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    handle_emby_user_flag(headers, user_id, item_id, query, state, true, false).await
}

pub(super) async fn emby_mark_favorite(
    headers: HeaderMap,
    Path((user_id, item_id)): Path<(String, String)>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    handle_emby_user_flag(headers, user_id, item_id, query, state, false, true).await
}

pub(super) async fn emby_unmark_favorite(
    headers: HeaderMap,
    Path((user_id, item_id)): Path<(String, String)>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    handle_emby_user_flag(headers, user_id, item_id, query, state, false, false).await
}

pub(super) async fn handle_emby_user_flag(
    headers: HeaderMap,
    user_id: String,
    item_id: String,
    query: EmbyTokenQuery,
    state: AppState,
    played: bool,
    value: bool,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if let Err(status) = ensure_emby_user_scope(&user, &user_id) {
        return status.into_response();
    }
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match access
        .can_view_item(AccessPrincipal::new(user.id, user.is_admin), &item_id)
        .await
    {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let result = if played {
        database
            .set_user_item_played(&user_id, &item_id, value)
            .await
    } else {
        database
            .set_user_item_favorite(&user_id, &item_id, value)
            .await
    };
    match result {
        Ok(()) => {
            if played
                && database
                    .sync_played_container_states(&user_id, &item_id)
                    .await
                    .is_err()
            {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LuxFavoriteRequest {
    pub(super) favorite: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LuxPlayedRequest {
    pub(super) played: bool,
}

pub(super) async fn lux_set_favorite(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<LuxFavoriteRequest>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_web_csrf(&headers, &state).await {
        return response;
    }
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match access
        .can_view_item(AccessPrincipal::new(user.id, user.is_admin), &item_id)
        .await
    {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database
        .set_user_item_favorite(&user.id.to_string(), &item_id, request.favorite)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn lux_set_played(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<LuxPlayedRequest>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_web_csrf(&headers, &state).await {
        return response;
    }
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match access
        .can_view_item(AccessPrincipal::new(user.id, user.is_admin), &item_id)
        .await
    {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database
        .set_user_item_played(&user.id.to_string(), &item_id, request.played)
        .await
    {
        Ok(()) => {
            if database
                .sync_played_container_states(&user.id.to_string(), &item_id)
                .await
                .is_err()
            {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
