use super::*;

pub(super) fn api_routes() -> Router<ReadyAppState> {
    Router::new()
        .route("/api/v1/admin/events", get(admin_events))
        .route("/api/v1/events", get(user_events))
        .route("/api/v1/admin/audit", get(admin_list_audit))
        .route("/api/v1/admin/logs/export", get(admin_export_logs))
        .route("/api/v1/admin/logs", get(admin_list_logs))
}

#[derive(Deserialize, Default)]
pub(crate) struct AdminLogExportQuery {
    pub(crate) from: Option<String>,
    pub(crate) to: Option<String>,
}
pub(crate) async fn record_audit_event(
    state: &ReadyAppState,
    headers: &HeaderMap,
    event_type: &str,
    target_type: Option<&str>,
    target_id: Option<&str>,
    metadata_json: &str,
) {
    let (actor_user_id, actor_username, metadata_json) =
        if let Some(candidate) = lux_api_key_from_headers(headers) {
            let service = state.admin_api_key();
            let Ok(Some(_)) = service.resolve_principal(&candidate).await else {
                return;
            };
            (None, None, audit_metadata_for_shared_api_key(metadata_json))
        } else {
            let auth = state.auth();
            let Some(session_token) = request_cookie(headers, "lux_session") else {
                return;
            };
            let Ok(Some(session)) = auth.resolve(&session_token).await else {
                return;
            };
            (
                Some(session.user.id.to_string()),
                Some(session.user.username_normalized.clone()),
                metadata_json.to_owned(),
            )
        };
    let Some(config_dir) = state.config_dir.as_deref() else {
        return;
    };
    let event_id = Uuid::now_v7().to_string();
    match crate::observability::logs::LogStore::new(config_dir)
        .append_audit_event(crate::observability::logs::NewAuditLogEvent {
            id: &event_id,
            actor_user_id: actor_user_id.as_deref(),
            actor_username: actor_username.as_deref(),
            event_type,
            target_type,
            target_id,
            metadata_json: &metadata_json,
        })
        .await
    {
        Ok(()) => state
            .admin_events
            .publish(admin_event_scope_for_audit(event_type)),
        Err(error) => tracing::error!(
            event_type,
            error_kind = ?error.kind(),
            "failed to persist admin audit event"
        ),
    }
}

pub(crate) fn audit_metadata_for_shared_api_key(metadata_json: &str) -> String {
    let Ok(mut metadata) = serde_json::from_str::<Value>(metadata_json) else {
        return "{\"auth\":\"admin_api_key\"}".to_owned();
    };
    if let Value::Object(object) = &mut metadata {
        object.insert("auth".to_owned(), Value::String("admin_api_key".to_owned()));
    } else {
        metadata = json!({ "auth": "admin_api_key", "details": metadata });
    }
    serde_json::to_string(&metadata).unwrap_or_else(|_| "{\"auth\":\"admin_api_key\"}".to_owned())
}

pub(crate) fn admin_event_scope_for_audit(event_type: &str) -> AdminEventScope {
    if event_type == "SETTINGS_UPDATED" {
        return AdminEventScope::Settings;
    }
    if event_type.starts_with("PLUGIN_") {
        return AdminEventScope::Plugins;
    }
    if event_type.starts_with("USER_") || event_type == "LIBRARY_ACCESS_UPDATED" {
        return AdminEventScope::Users;
    }
    if event_type.starts_with("LIBRARY_") || event_type.starts_with("SCHEDULE_") {
        return AdminEventScope::Libraries;
    }
    if event_type.starts_with("METADATA_") {
        return AdminEventScope::Metadata;
    }
    if event_type.starts_with("SCAN_")
        || event_type.starts_with("STRM_")
        || event_type.starts_with("DANMAKU_")
    {
        return AdminEventScope::Jobs;
    }
    AdminEventScope::All
}

pub(crate) async fn record_activity_event(
    database: Option<&Database>,
    admin_events: &AdminEventHub,
    user_id: &str,
    event_type: &str,
    target_id: Option<&str>,
    metadata: Value,
) {
    let Some(database) = database else {
        return;
    };
    let metadata_json = match serde_json::to_string(&metadata) {
        Ok(metadata_json) => metadata_json,
        Err(_) => return,
    };
    if database
        .insert_user_activity_event(crate::storage::NewUserActivityEvent {
            user_id,
            actor_user_id: Some(user_id),
            event_type,
            target_type: target_id.map(|_| "media_item"),
            target_id,
            metadata_json: &metadata_json,
        })
        .await
        .is_err()
    {
        tracing::warn!(event_type, "failed to write user activity index event");
    }
    if database
        .insert_audit_event(crate::storage::NewAuditEvent {
            actor_user_id: Some(user_id),
            event_type,
            target_type: target_id.map(|_| "media_item"),
            target_id,
            metadata_json: &metadata_json,
        })
        .await
        .is_ok()
    {
        admin_events.publish(AdminEventScope::Dashboard);
    }
}

pub(crate) async fn record_failed_authentication_event(
    database: Option<&Database>,
    admin_events: &AdminEventHub,
    user_id: &str,
    client: &str,
    remote_ip: Option<String>,
) {
    let Some(database) = database else {
        return;
    };
    let metadata_json = serde_json::to_string(&json!({
        "client": client,
        "remoteIp": remote_ip,
    }))
    .unwrap_or_else(|_| "{}".to_owned());
    if database
        .insert_user_activity_event(crate::storage::NewUserActivityEvent {
            user_id,
            actor_user_id: None,
            event_type: "AUTH_LOGIN_FAILED",
            target_type: Some("user"),
            target_id: Some(user_id),
            metadata_json: &metadata_json,
        })
        .await
        .is_err()
    {
        tracing::warn!(
            event_type = "AUTH_LOGIN_FAILED",
            "failed to write user activity index event"
        );
    }
    if database
        .insert_audit_event(crate::storage::NewAuditEvent {
            actor_user_id: None,
            event_type: "AUTH_LOGIN_FAILED",
            target_type: Some("user"),
            target_id: Some(user_id),
            metadata_json: &metadata_json,
        })
        .await
        .is_ok()
    {
        admin_events.publish(AdminEventScope::Dashboard);
    }
}

pub(crate) fn playback_activity_event_type(
    previous: Option<&StoredPlaybackSession>,
    state_name: &str,
) -> Option<&'static str> {
    if previous.is_some_and(|session| session.state == state_name)
        || (previous.is_none() && state_name == "STOPPED")
    {
        return None;
    }
    match state_name {
        "PLAYING" => Some("PLAYBACK_STARTED"),
        "PAUSED" => Some("PLAYBACK_PAUSED"),
        "STOPPED" => Some("PLAYBACK_STOPPED"),
        _ => None,
    }
}

pub(crate) async fn admin_list_audit(
    headers: HeaderMap,
    Query(query): Query<MetadataCandidateQuery>,
    State(state): State<ReadyAppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let (offset, limit) = match metadata_page_params(&query) {
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
    let Some(config_dir) = state.config_dir.as_deref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let file_events = match crate::observability::logs::LogStore::new(config_dir)
        .list_audit_events(offset, limit)
        .await
    {
        Ok((_, events)) => events,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let events = file_events
        .into_iter()
        .map(|event| {
            json!({
                "id": event.id,
                "actorUserId": event.actor_user_id,
                "actorUsername": event.actor_username,
                "eventType": event.event_type,
                "targetType": event.target_type,
                "targetId": event.target_id,
                "metadata": serde_json::from_str::<Value>(&event.metadata_json)
                    .unwrap_or_else(|_| json!({})),
                "createdAt": event.created_at,
            })
        })
        .collect::<Vec<_>>();
    Json(json!({
        "events": events,
        "page": offset / limit + 1,
        "pageSize": limit,
    }))
    .into_response()
}

pub(crate) async fn admin_list_logs(
    headers: HeaderMap,
    Query(query): Query<MetadataCandidateQuery>,
    State(state): State<ReadyAppState>,
) -> Response {
    admin_list_audit(headers, Query(query), State(state)).await
}

pub(crate) async fn admin_export_logs(
    headers: HeaderMap,
    Query(query): Query<AdminLogExportQuery>,
    State(state): State<ReadyAppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let range = match LogDateRange::from_query(query.from.as_deref(), query.to.as_deref()) {
        Ok(range) => range,
        Err(error) => return log_export_error_response(&headers, error),
    };
    let Some(config_dir) = state.config_dir.as_deref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match export_logs(config_dir, range).await {
        Ok(export) => {
            let (content_type, filename, contents) = match export {
                LogExport::Daily { contents, filename } => {
                    ("application/x-ndjson", filename, contents)
                }
                LogExport::Archive { contents, filename } => {
                    ("application/zip", filename, contents)
                }
            };
            Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", content_type)
                .header(
                    "Content-Disposition",
                    format!("attachment; filename=\"{filename}\""),
                )
                .header("Cache-Control", "no-store")
                .body(Body::from(contents))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        Err(error) => log_export_error_response(&headers, error),
    }
}

pub(crate) fn log_export_error_response(headers: &HeaderMap, error: LogExportError) -> Response {
    match error {
        LogExportError::InvalidDate
        | LogExportError::DateRangeReversed
        | LogExportError::DateRangeTooLarge => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            &error.to_string(),
        )
        .into_response(),
        LogExportError::ExportTooLarge => api_error(
            headers,
            StatusCode::PAYLOAD_TOO_LARGE,
            lux::ApiErrorCode::InvalidRequest,
            &error.to_string(),
        )
        .into_response(),
        LogExportError::NoLogs => api_error(
            headers,
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            &error.to_string(),
        )
        .into_response(),
        LogExportError::Io(_) | LogExportError::Archive(_) | LogExportError::Worker(_) => {
            tracing::warn!(%error, "log export failed");
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

pub(crate) async fn admin_events(
    headers: HeaderMap,
    State(state): State<ReadyAppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(connections) = state.sse_permits.try_acquire() else {
        return super::sse_capacity_response(&headers);
    };

    let mut receiver = state.admin_events.subscribe();
    let (mut writer, reader) = tokio::io::duplex(16 * 1024);
    tokio::spawn(async move {
        // Held until the stream ends so the budget reflects live connections.
        let _connections = connections;
        if writer
            .write_all(b"event: ready\ndata: {\"version\":1}\n\n")
            .await
            .is_err()
        {
            return;
        }

        let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
        heartbeat.tick().await;
        loop {
            tokio::select! {
                event = receiver.recv() => {
                    let scope = match event {
                        Ok(scope) => scope,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => AdminEventScope::All,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    };
                    let frame = format!(
                        "event: invalidate\ndata: {{\"scope\":\"{}\"}}\n\n",
                        scope.as_str(),
                    );
                    if writer.write_all(frame.as_bytes()).await.is_err() {
                        break;
                    }
                }
                _ = heartbeat.tick() => {
                    if writer.write_all(b": keep-alive\n\n").await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    event_stream_response(reader)
}

pub(crate) fn event_stream_response(reader: tokio::io::DuplexStream) -> Response {
    let mut response = Response::new(Body::from_stream(tokio_util::io::ReaderStream::new(reader)));
    response.headers_mut().insert(
        "Content-Type",
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    response.headers_mut().insert(
        "Cache-Control",
        HeaderValue::from_static("no-cache, no-store, must-revalidate"),
    );
    response
        .headers_mut()
        .insert("X-Accel-Buffering", HeaderValue::from_static("no"));
    response
}

pub(crate) async fn user_events(
    headers: HeaderMap,
    State(state): State<ReadyAppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let Some(connections) = state.sse_permits.try_acquire() else {
        return super::sse_capacity_response(&headers);
    };

    // The subscription only yields directory-level events and events about this
    // user, so another user's playback ticks never reach this stream.
    let mut receiver = state.user_events.subscribe_for(&user.id.to_string());
    let (mut writer, reader) = tokio::io::duplex(16 * 1024);
    tokio::spawn(async move {
        // Held until the stream ends so the budget reflects live connections.
        let _connections = connections;
        if writer
            .write_all(b"event: ready\ndata: {\"version\":1}\n\n")
            .await
            .is_err()
        {
            return;
        }

        let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
        heartbeat.tick().await;
        loop {
            tokio::select! {
                event = receiver.recv() => {
                    let event = match event {
                        Ok(event) => event,
                        // A lagged subscriber may have missed invalidations of any
                        // scope, so fall back to the full home refresh.
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            crate::application::admin_events::UserEvent::broadcast(
                                crate::application::admin_events::UserEventScope::Home,
                            )
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    };
                    if writer.write_all(user_event_frame(&event).as_bytes()).await.is_err() {
                        break;
                    }
                }
                _ = heartbeat.tick() => {
                    if writer.write_all(b": keep-alive\n\n").await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    event_stream_response(reader)
}

/// Renders one invalidation frame.
///
/// Scope-only payloads keep the legacy `{"scope":"..."}` shape. `progress`
/// events additionally carry the item the user is playing so the client can
/// refresh that item's playback state instead of every media query.
fn user_event_frame(event: &crate::application::admin_events::UserEvent) -> String {
    #[derive(serde::Serialize)]
    struct Frame<'a> {
        scope: &'a str,
        #[serde(rename = "itemId", skip_serializing_if = "Option::is_none")]
        item_id: Option<&'a str>,
    }

    let frame = Frame {
        scope: event.scope().as_str(),
        item_id: event.item_id(),
    };
    let data = serde_json::to_string(&frame).unwrap_or_else(|_| "{\"scope\":\"home\"}".to_owned());
    format!("event: invalidate\ndata: {data}\n\n")
}

#[cfg(test)]
mod user_event_frame_tests {
    use super::*;

    use crate::application::admin_events::{UserEvent, UserEventScope};

    #[test]
    fn keeps_the_legacy_scope_only_frame_for_directory_events() {
        assert_eq!(
            user_event_frame(&UserEvent::broadcast(UserEventScope::Home)),
            "event: invalidate\ndata: {\"scope\":\"home\"}\n\n"
        );
        assert_eq!(
            user_event_frame(&UserEvent::for_user(UserEventScope::Item, "user-a")),
            "event: invalidate\ndata: {\"scope\":\"item\"}\n\n"
        );
    }

    #[test]
    fn narrows_progress_frames_to_the_playing_item() {
        assert_eq!(
            user_event_frame(
                &UserEvent::for_user(UserEventScope::Progress, "user-a").with_item_id("item-1")
            ),
            "event: invalidate\ndata: {\"scope\":\"progress\",\"itemId\":\"item-1\"}\n\n"
        );
    }
}

#[cfg(test)]
mod failed_authentication_activity_tests {
    use super::*;

    use crate::{
        application::{admin_events::AdminEventHub, setup::SetupService},
        config::Config,
        storage::Database,
    };

    #[tokio::test]
    async fn failed_authentication_activity_keeps_actor_empty_and_metadata_safe()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let setup = SetupService::new(database.clone())?;
        let user = setup
            .complete("activity-failure", "Failure", "private password")
            .await?;
        let user_id = user.id.to_string();
        let events = AdminEventHub::new();
        let mut receiver = events.subscribe();

        record_failed_authentication_event(
            Some(&database),
            &events,
            &user_id,
            "LUX_WEB",
            Some("192.0.2.1".to_owned()),
        )
        .await;

        let (event_type, actor_user_id, target_type, target_id, metadata_json): (
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            String,
        ) = sqlx::query_as(
            "SELECT event_type, actor_user_id, target_type, target_id, metadata_json
             FROM user_activity_events WHERE user_id = ?",
        )
        .bind(&user_id)
        .fetch_one(database.pool())
        .await?;
        assert_eq!(event_type, "AUTH_LOGIN_FAILED");
        assert_eq!(actor_user_id, None);
        assert_eq!(target_type.as_deref(), Some("user"));
        assert_eq!(target_id.as_deref(), Some(user_id.as_str()));
        let metadata: Value = serde_json::from_str(&metadata_json)?;
        assert_eq!(metadata["client"], "LUX_WEB");
        assert_eq!(metadata["remoteIp"], "192.0.2.1");
        assert!(!metadata_json.contains("private password"));
        assert!(matches!(
            receiver.try_recv(),
            Ok(AdminEventScope::Dashboard)
        ));
        Ok(())
    }
}
