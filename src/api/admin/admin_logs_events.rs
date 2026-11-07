use super::*;

pub(super) fn api_routes() -> Router<ReadyAppState> {
    Router::new()
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

/// Human-readable item name for activity logs. Episodes read as
/// "Series S01E03 Title" so the log says which episode was played.
fn playback_item_display_title(
    item_type: &str,
    title: &str,
    series_title: Option<&str>,
    season_number: Option<i64>,
    episode_number: Option<i64>,
) -> String {
    let Some(series_title) = series_title.filter(|_| item_type == "EPISODE") else {
        return title.to_owned();
    };
    match (season_number, episode_number) {
        (Some(season), Some(episode)) => {
            format!("{series_title} S{season:02}E{episode:02} {title}")
        }
        _ => format!("{series_title} · {title}"),
    }
}

pub(crate) async fn record_activity_event(
    database: Option<&Database>,
    admin_events: &AdminEventHub,
    user_id: &str,
    event_type: &str,
    target_id: Option<&str>,
    mut metadata: Value,
) {
    let Some(database) = database else {
        return;
    };
    if let Some(item_id) = target_id
        && let Some(object) = metadata.as_object_mut()
        && let Ok(Some(item)) = database.find_media_item_metadata(item_id).await
    {
        let title = playback_item_display_title(
            &item.item_type,
            &item.title,
            item.series_title.as_deref(),
            item.season_number,
            item.episode_number,
        );
        object.insert("itemTitle".to_owned(), json!(title));
    }
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

/// Emits the §18.3 failed-login event that Fail2Ban-style log tools parse.
///
/// The message and the field set are deliberately fixed: no username, credential, cookie,
/// client-supplied device label, complete URL or query string may reach the log. The address is
/// whatever the access-IP resolver produced at the call site; an unresolved address leaves the
/// `remoteIp` field out entirely instead of logging a placeholder.
///
/// This touches no storage, so callers emit it for every failed attempt - including an unknown
/// username or user id, which is exactly the credential-guessing case the ban list exists for -
/// before they decide whether an account exists. It is separate from
/// [`record_failed_authentication_event`] so one failure never produces two log lines.
pub(crate) fn log_authentication_failure(remote_ip: Option<&str>) {
    match remote_ip {
        Some(remote_ip) => tracing::warn!(
            "errorCode" = "AUTH_LOGIN_FAILED",
            "remoteIp" = remote_ip,
            "authentication failed"
        ),
        None => tracing::warn!("errorCode" = "AUTH_LOGIN_FAILED", "authentication failed"),
    }
}

/// Records the storage side of a failed login for an account that actually exists.
///
/// The parseable log event is emitted by the call sites through [`log_authentication_failure`];
/// this function owns only the activity index row, the audit record and the dashboard
/// notification, so an unavailable database cannot silence the log event.
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

#[cfg(test)]
mod failed_authentication_log_event_tests {
    use std::{
        error::Error,
        io::{self, Write},
        sync::{Arc, Mutex},
    };

    use serde_json::Value;

    use super::*;

    use crate::{
        application::{admin_events::AdminEventHub, setup::SetupService},
        config::Config,
        storage::Database,
    };

    /// The smallest in-memory sink that lets a test read what `tracing` actually emitted.
    #[derive(Clone)]
    struct CapturedTraceWriterFactory(Arc<Mutex<Vec<u8>>>);

    struct CapturedTraceWriter(Arc<Mutex<Vec<u8>>>);

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedTraceWriterFactory {
        type Writer = CapturedTraceWriter;

        fn make_writer(&'a self) -> Self::Writer {
            CapturedTraceWriter(Arc::clone(&self.0))
        }
    }

    impl Write for CapturedTraceWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let mut output = self
                .0
                .lock()
                .map_err(|_| io::Error::other("captured trace buffer is unavailable"))?;
            output.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Captures `WARN`-level JSON events for the current thread into `buffer`.
    ///
    /// The returned guard must stay alive until the code under test ran; dropping it restores the
    /// thread's previous dispatcher.
    fn capture_warn_json(buffer: &Arc<Mutex<Vec<u8>>>) -> tracing::dispatcher::DefaultGuard {
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_max_level(tracing::Level::WARN)
            .with_writer(CapturedTraceWriterFactory(Arc::clone(buffer)))
            .finish();
        tracing::dispatcher::set_default(&tracing::Dispatch::new(subscriber))
    }

    fn captured_json_events(buffer: &Arc<Mutex<Vec<u8>>>) -> Result<Vec<Value>, Box<dyn Error>> {
        let logs = buffer
            .lock()
            .map_err(|_| "captured trace buffer is unavailable")?
            .clone();
        Ok(String::from_utf8(logs)?
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .collect())
    }

    fn login_failure_event(events: &[Value]) -> Result<Value, Box<dyn Error>> {
        events
            .iter()
            .find(|event| event["fields"]["errorCode"] == "AUTH_LOGIN_FAILED")
            .cloned()
            .ok_or_else(|| "a failed login must emit an AUTH_LOGIN_FAILED log event".into())
    }

    #[test]
    fn authentication_failure_event_carries_the_stable_code_and_remote_ip()
    -> Result<(), Box<dyn Error>> {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let _capture = capture_warn_json(&buffer);

        log_authentication_failure(Some("203.0.113.9"));

        let event = login_failure_event(&captured_json_events(&buffer)?)?;
        assert_eq!(event["level"], "WARN");
        let fields = &event["fields"];
        assert_eq!(fields["errorCode"], "AUTH_LOGIN_FAILED");
        assert_eq!(fields["remoteIp"], "203.0.113.9");
        assert_eq!(fields["message"], "authentication failed");
        Ok(())
    }

    #[test]
    fn authentication_failure_event_omits_an_unknown_remote_ip() -> Result<(), Box<dyn Error>> {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let _capture = capture_warn_json(&buffer);

        log_authentication_failure(None);

        let event = login_failure_event(&captured_json_events(&buffer)?)?;
        assert_eq!(event["level"], "WARN");
        assert_eq!(event["fields"]["errorCode"], "AUTH_LOGIN_FAILED");
        assert_eq!(event["fields"]["message"], "authentication failed");
        // Documented choice: an unresolved address drops the field entirely instead of logging
        // `remoteIp: null`, so downstream parsers never see a non-address value.
        assert!(
            event["fields"].get("remoteIp").is_none(),
            "an unknown address must not produce a remoteIp field: {event}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn recording_the_storage_side_emits_no_log_event() -> Result<(), Box<dyn Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let setup = SetupService::new(database.clone())?;
        let user = setup
            .complete("log-failure", "Failure", "private password")
            .await?;
        let user_id = user.id.to_string();
        let hub = AdminEventHub::new();
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let _capture = capture_warn_json(&buffer);

        record_failed_authentication_event(
            Some(&database),
            &hub,
            &user_id,
            "LUX_WEB_CLIENT_MARKER",
            Some("203.0.113.9".to_owned()),
        )
        .await;

        // The caller already emits the one §18.3 event through
        // `log_authentication_failure`; recording the row must not repeat it, and must not leak
        // the client label or the internal user id either.
        let captured = String::from_utf8(
            buffer
                .lock()
                .map_err(|_| "captured trace buffer is unavailable")?
                .clone(),
        )?;
        assert!(
            captured.is_empty(),
            "the storage side must not log the login failure again: {captured}"
        );
        Ok(())
    }
}

#[cfg(test)]
mod playback_item_title_tests {
    use super::*;

    #[test]
    fn episodes_show_series_season_episode_and_title() {
        assert_eq!(
            playback_item_display_title("EPISODE", "开端", Some("Show"), Some(1), Some(3)),
            "Show S01E03 开端"
        );
        assert_eq!(
            playback_item_display_title("EPISODE", "Long", Some("Show"), Some(12), Some(120)),
            "Show S12E120 Long"
        );
    }

    #[test]
    fn episodes_without_numbers_fall_back_to_series_and_title() {
        assert_eq!(
            playback_item_display_title("EPISODE", "Pilot", Some("Show"), None, Some(1)),
            "Show · Pilot"
        );
        assert_eq!(
            playback_item_display_title("EPISODE", "Pilot", None, Some(1), Some(1)),
            "Pilot"
        );
    }

    #[test]
    fn other_items_keep_their_own_title() {
        assert_eq!(
            playback_item_display_title("MOVIE", "Film", None, None, None),
            "Film"
        );
        assert_eq!(
            playback_item_display_title("SERIES", "Show", None, None, None),
            "Show"
        );
    }
}
