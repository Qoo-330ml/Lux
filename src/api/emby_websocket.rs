//! `GET /embywebsocket`: the Emby realtime session and scheduled-task channel.
//!
//! Yamby opens `ws(s)://{host}/embywebsocket?api_key=…&deviceId=…` from its server-status page,
//! then sends `SessionsStart` (`Data="0,1500,0,true,true"`) and `ScheduledTasksInfoStart`
//! (`Data="1000,1000"`). Emby's `Data` for those two messages is `initialDelayMillis,pollInterval
//! Millis,…`, so the server pushes the same session and scheduled-task documents the HTTP
//! endpoints return, at the interval the client asked for. Yamby silently drops a session entry
//! without `NowPlayingItem` or `PlayState`, and it reconnects without any backoff when the
//! handshake fails, which is why the endpoint has to exist rather than answer 404.
//!
//! The implementation is deliberately a per-connection poll loop: it reuses the existing
//! application services (no event bus, no broadcast), revalidates the token before every push, and
//! keeps every resource (connections, message size, wakeups) bounded.

use super::*;

use std::collections::HashMap;

use axum::extract::ws::{CloseFrame, Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use tokio::sync::{Semaphore, watch};
use tokio::time::Instant;

/// Upper bound on concurrently open `/embywebsocket` connections for the whole process.
pub(crate) const MAX_EMBY_WEBSOCKET_CONNECTIONS: usize = 32;
/// Upper bound on concurrently open `/embywebsocket` connections for a single account.
pub(crate) const MAX_EMBY_WEBSOCKET_CONNECTIONS_PER_USER: usize = 4;
/// Largest inbound frame accepted. Emby clients only send small JSON control messages.
pub(crate) const MAX_EMBY_WEBSOCKET_MESSAGE_BYTES: usize = 8 * 1024;
/// Lower bound a client-requested poll interval is clamped to.
pub(crate) const MIN_EMBY_WEBSOCKET_INTERVAL: Duration = Duration::from_secs(1);
/// Upper bound a client-requested poll interval is clamped to.
pub(crate) const MAX_EMBY_WEBSOCKET_INTERVAL: Duration = Duration::from_secs(60);
/// A connection that sends nothing (not even a pong) for this long is closed.
pub(crate) const EMBY_WEBSOCKET_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// Interval of the server-side ping that makes the idle timeout observable.
pub(crate) const EMBY_WEBSOCKET_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(25);
/// How long a graceful shutdown waits for the open connections to finish closing.
pub(crate) const EMBY_WEBSOCKET_SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Close code of a WebSocket policy violation (RFC 6455 §7.4.1).
const CLOSE_POLICY_VIOLATION: u16 = 1008;
/// Close code of an endpoint that is going away.
const CLOSE_GOING_AWAY: u16 = 1001;
/// Close code of a normal closure.
const CLOSE_NORMAL: u16 = 1000;

/// The reply to `KeepAlive`.
const KEEP_ALIVE_REPLY: &str = r#"{"MessageType":"KeepAlive"}"#;

/// Resource limits of the endpoint. The tests shrink them so a limit is observable in production
/// time; the defaults are the constants above.
#[derive(Clone, Debug)]
pub struct EmbyWebsocketLimits {
    /// Concurrent connections for the whole process.
    pub max_connections: usize,
    /// Concurrent connections for one account.
    pub max_connections_per_user: usize,
    /// Lower bound of the poll interval a client may request.
    pub min_interval: Duration,
    /// Upper bound of the poll interval a client may request.
    pub max_interval: Duration,
    /// Largest inbound frame accepted.
    pub max_message_bytes: usize,
    /// A connection without any inbound frame for this long is closed.
    pub idle_timeout: Duration,
    /// Server-side ping interval.
    pub heartbeat_interval: Duration,
}

impl Default for EmbyWebsocketLimits {
    fn default() -> Self {
        Self {
            max_connections: MAX_EMBY_WEBSOCKET_CONNECTIONS,
            max_connections_per_user: MAX_EMBY_WEBSOCKET_CONNECTIONS_PER_USER,
            min_interval: MIN_EMBY_WEBSOCKET_INTERVAL,
            max_interval: MAX_EMBY_WEBSOCKET_INTERVAL,
            max_message_bytes: MAX_EMBY_WEBSOCKET_MESSAGE_BYTES,
            idle_timeout: EMBY_WEBSOCKET_IDLE_TIMEOUT,
            heartbeat_interval: EMBY_WEBSOCKET_HEARTBEAT_INTERVAL,
        }
    }
}

impl EmbyWebsocketLimits {
    /// Keeps an override from creating an unusable schedule (no `min > max`, no zero-duration
    /// wake-up loop). A zero `max_connections` stays zero: it is a legal "refuse everything"
    /// configuration.
    fn normalized(mut self) -> Self {
        self.min_interval = self.min_interval.max(Duration::from_millis(10));
        self.max_interval = self.max_interval.max(self.min_interval);
        self.heartbeat_interval = self.heartbeat_interval.max(Duration::from_millis(10));
        self.idle_timeout = self.idle_timeout.max(self.heartbeat_interval);
        self.max_message_bytes = self.max_message_bytes.max(1024);
        self
    }
}

/// The connection budget: one global semaphore plus a per-account counter.
#[derive(Clone)]
pub(crate) struct EmbyWebsocketConnections {
    inner: Arc<EmbyWebsocketConnectionState>,
}

struct EmbyWebsocketConnectionState {
    limits: EmbyWebsocketLimits,
    total: Arc<Semaphore>,
    per_user: std::sync::Mutex<HashMap<String, usize>>,
}

impl Default for EmbyWebsocketConnections {
    fn default() -> Self {
        Self::new(EmbyWebsocketLimits::default())
    }
}

impl EmbyWebsocketConnections {
    pub(crate) fn new(limits: EmbyWebsocketLimits) -> Self {
        let limits = limits.normalized();
        let total = Arc::new(Semaphore::new(limits.max_connections));
        Self {
            inner: Arc::new(EmbyWebsocketConnectionState {
                limits,
                total,
                per_user: std::sync::Mutex::new(HashMap::new()),
            }),
        }
    }

    pub(crate) fn limits(&self) -> &EmbyWebsocketLimits {
        &self.inner.limits
    }

    /// Takes one connection slot or answers `None` when a limit is reached. Both limits are
    /// checked before the handshake completes, so a rejected client never sees a `101`.
    pub(crate) fn try_acquire(&self, user_key: &str) -> Option<EmbyWebsocketPermit> {
        let total = self.inner.total.clone().try_acquire_owned().ok()?;
        let mut per_user = self
            .inner
            .per_user
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let count = per_user.entry(user_key.to_owned()).or_insert(0);
        if *count >= self.inner.limits.max_connections_per_user {
            return None;
        }
        *count += 1;
        drop(per_user);
        Some(EmbyWebsocketPermit {
            _total: total,
            connections: self.clone(),
            user_key: user_key.to_owned(),
        })
    }

    /// Waits until every connection released its slot. The graceful shutdown uses it so the
    /// database is not closed under a connection that is still building a payload.
    pub(crate) async fn wait_until_idle(&self, timeout: Duration) -> bool {
        let Ok(capacity) = u32::try_from(self.inner.limits.max_connections) else {
            return false;
        };
        if capacity == 0 {
            return true;
        }
        tokio::time::timeout(timeout, self.inner.total.acquire_many(capacity))
            .await
            .is_ok()
    }
}

/// Holds one connection slot; the per-account count is decremented when it is dropped, which is
/// what keeps an abandoned connection from leaking capacity.
pub(crate) struct EmbyWebsocketPermit {
    _total: tokio::sync::OwnedSemaphorePermit,
    connections: EmbyWebsocketConnections,
    user_key: String,
}

impl Drop for EmbyWebsocketPermit {
    fn drop(&mut self) {
        let mut per_user = self
            .connections
            .inner
            .per_user
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(count) = per_user.get_mut(&self.user_key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                per_user.remove(&self.user_key);
            }
        }
    }
}

/// The shutdown broadcast every open connection watches.
#[derive(Clone)]
pub(crate) struct EmbyWebsocketShutdown {
    sender: watch::Sender<bool>,
}

impl Default for EmbyWebsocketShutdown {
    fn default() -> Self {
        Self {
            sender: watch::channel(false).0,
        }
    }
}

impl EmbyWebsocketShutdown {
    pub(crate) fn signal(&self) {
        self.sender.send_replace(true);
    }

    pub(crate) fn subscribe(&self) -> watch::Receiver<bool> {
        self.sender.subscribe()
    }
}

impl CoreState {
    pub(crate) fn emby_websocket_connections(&self) -> &EmbyWebsocketConnections {
        &self.emby_websocket_connections
    }

    pub(crate) fn emby_websocket_shutdown(&self) -> &EmbyWebsocketShutdown {
        &self.emby_websocket_shutdown
    }
}

pub(super) async fn emby_websocket(
    State(state): State<ReadyAppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    // Authentication happens before the upgrade on purpose: an invalid or revoked token must
    // answer 401 to the handshake request instead of completing a 101 and closing afterwards, so
    // the client cannot mistake the failure for a live connection.
    let principal = match require_emby_principal(&headers, &state, None).await {
        Ok(principal) => principal,
        Err(status) => return status.into_response(),
    };
    let user_key = principal
        .user_id()
        .map(|user_id| user_id.to_string())
        .unwrap_or_else(|| "shared-admin-api-key".to_owned());
    let connections = state.emby_websocket_connections().clone();
    let Some(permit) = connections.try_acquire(&user_key) else {
        tracing::warn!(
            user = %user_key,
            "rejected an emby websocket connection: connection budget exhausted"
        );
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let limits = connections.limits().clone();
    let shutdown = state.emby_websocket_shutdown().subscribe();
    // The token selection already happened in `normalize_credentials`; the normalized header is
    // what the per-push revalidation reads. `query.api_key` stays empty because the middleware
    // removed the credential from the query string.
    let context = ConnectionContext {
        state,
        headers,
        query: EmbyTokenQuery {
            api_key: None,
            tag: None,
            fields: None,
            active_within_seconds: None,
        },
        can_manage_server: principal.can_manage_server(),
        limits: limits.clone(),
        shutdown,
        permit,
    };
    tracing::debug!(user = %user_key, "emby websocket connection accepted");
    ws.max_message_size(limits.max_message_bytes)
        .max_frame_size(limits.max_message_bytes)
        .on_upgrade(move |socket| run_connection(socket, context))
        .into_response()
}

/// Everything one open connection needs; bundled so the loop signature stays readable.
struct ConnectionContext {
    state: ReadyAppState,
    headers: HeaderMap,
    query: EmbyTokenQuery,
    can_manage_server: bool,
    limits: EmbyWebsocketLimits,
    shutdown: watch::Receiver<bool>,
    permit: EmbyWebsocketPermit,
}

/// One periodic push stream: the first message is due at `next_push`, then every `interval`.
struct StreamSchedule {
    next_push: Instant,
    interval: Duration,
}

enum Wake {
    Inbound(Option<Result<Message, axum::Error>>),
    Timer,
    Shutdown,
}

enum ClientAction {
    None,
    Reply(&'static str),
}

async fn run_connection(mut socket: WebSocket, context: ConnectionContext) {
    let ConnectionContext {
        state,
        headers,
        query,
        can_manage_server,
        limits,
        mut shutdown,
        permit,
    } = context;
    let mut sessions: Option<StreamSchedule> = None;
    let mut tasks: Option<StreamSchedule> = None;
    let mut idle_deadline = Instant::now() + limits.idle_timeout;
    let mut next_heartbeat = Instant::now() + limits.heartbeat_interval;
    let close = loop {
        if *shutdown.borrow() {
            break Some(utf8_close(CLOSE_GOING_AWAY, "server shutting down"));
        }
        let next_wake = next_wake(&sessions, &tasks, idle_deadline, next_heartbeat);
        let wake = tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    Wake::Shutdown
                } else {
                    Wake::Timer
                }
            }
            message = socket.recv() => Wake::Inbound(message),
            () = tokio::time::sleep_until(next_wake) => Wake::Timer,
        };
        match wake {
            Wake::Shutdown => break Some(utf8_close(CLOSE_GOING_AWAY, "server shutting down")),
            // The peer went away, or the frame stream is already broken (protocol error, the
            // message size limit, transport); tungstenite queued its own close, so nothing more
            // can be sent on this socket.
            Wake::Inbound(None) | Wake::Inbound(Some(Err(_))) => break None,
            Wake::Inbound(Some(Ok(message))) => {
                idle_deadline = Instant::now() + limits.idle_timeout;
                match message {
                    Message::Text(text) => {
                        match handle_client_message(
                            &text,
                            &mut sessions,
                            &mut tasks,
                            can_manage_server,
                            &limits,
                        ) {
                            ClientAction::None => {}
                            ClientAction::Reply(payload) => {
                                if socket
                                    .send(Message::Text(Utf8Bytes::from(payload)))
                                    .await
                                    .is_err()
                                {
                                    break None;
                                }
                            }
                        }
                    }
                    Message::Close(_) => break Some(utf8_close(CLOSE_NORMAL, "")),
                    // Pings are answered by the WebSocket implementation, and the pong is what
                    // keeps the idle deadline moving forward.
                    Message::Ping(_) | Message::Pong(_) | Message::Binary(_) => {}
                }
            }
            Wake::Timer => {
                let now = Instant::now();
                if now >= idle_deadline {
                    break Some(utf8_close(CLOSE_GOING_AWAY, "idle timeout"));
                }
                if now >= next_heartbeat {
                    if socket.send(Message::Ping(Bytes::new())).await.is_err() {
                        break None;
                    }
                    next_heartbeat = now + limits.heartbeat_interval;
                }
                if push_due(&sessions) {
                    match revalidated_principal(&state, &headers, &query).await {
                        Ok(principal) => match emby_session_entries(&state, &principal, None).await
                        {
                            Ok(entries) => {
                                let payload =
                                    json!({"MessageType": "Sessions", "Data": entries}).to_string();
                                if socket.send(Message::Text(payload.into())).await.is_err() {
                                    break None;
                                }
                            }
                            Err(status) => tracing::debug!(
                                status = status.as_u16(),
                                "skipped an emby websocket session push"
                            ),
                        },
                        Err(()) => break Some(utf8_close(CLOSE_POLICY_VIOLATION, "token revoked")),
                    }
                    advance(&mut sessions);
                }
                if push_due(&tasks) {
                    match revalidated_principal(&state, &headers, &query).await {
                        Ok(principal) if principal.can_manage_server() => {
                            match emby_scheduled_task_entries(&state).await {
                                Ok(entries) => {
                                    let payload = json!({
                                        "MessageType": "ScheduledTasksInfo",
                                        "Data": entries,
                                    })
                                    .to_string();
                                    if socket.send(Message::Text(payload.into())).await.is_err() {
                                        break None;
                                    }
                                }
                                Err(status) => tracing::debug!(
                                    status = status.as_u16(),
                                    "skipped an emby websocket scheduled-task push"
                                ),
                            }
                        }
                        // A demoted administrator stops observing the scan state; the stream is
                        // dropped instead of failing the whole connection.
                        Ok(_) => tasks = None,
                        Err(()) => break Some(utf8_close(CLOSE_POLICY_VIOLATION, "token revoked")),
                    }
                    advance(&mut tasks);
                }
            }
        }
    };
    if let Some(frame) = close {
        let _ = socket.send(Message::Close(Some(frame))).await;
    }
    // Releases the connection budget; must happen after the close frame was written.
    drop(permit);
    tracing::debug!("emby websocket connection closed");
}

/// The next wake-up of the loop: the earliest push, heartbeat and idle deadline.
fn next_wake(
    sessions: &Option<StreamSchedule>,
    tasks: &Option<StreamSchedule>,
    idle_deadline: Instant,
    next_heartbeat: Instant,
) -> Instant {
    let mut next = idle_deadline.min(next_heartbeat);
    for schedule in [sessions, tasks].into_iter().flatten() {
        next = next.min(schedule.next_push);
    }
    next
}

fn push_due(schedule: &Option<StreamSchedule>) -> bool {
    schedule
        .as_ref()
        .is_some_and(|schedule| Instant::now() >= schedule.next_push)
}

/// Schedules the next push after the current one was attempted, keeping the loop moving even when
/// a push was skipped.
fn advance(schedule: &mut Option<StreamSchedule>) {
    if let Some(schedule) = schedule.as_mut() {
        schedule.next_push = Instant::now() + schedule.interval;
    }
}

/// Revalidates the connection credential. A revoked token (logout, deleted user, revoked admin
/// key) closes the connection instead of continuing to push data.
async fn revalidated_principal(
    state: &ReadyAppState,
    headers: &HeaderMap,
    query: &EmbyTokenQuery,
) -> Result<crate::auth::users::AuthenticationPrincipal, ()> {
    require_emby_principal_with_query(headers, state, query)
        .await
        .map_err(|_| ())
}

fn utf8_close(code: u16, reason: &str) -> CloseFrame {
    CloseFrame {
        code,
        reason: Utf8Bytes::from(reason),
    }
}

/// Routes one inbound text frame. Unknown message types and unparsable JSON are ignored on
/// purpose: Emby clients send messages this endpoint does not implement, and dropping the
/// connection would only make them reconnect.
fn handle_client_message(
    text: &str,
    sessions: &mut Option<StreamSchedule>,
    tasks: &mut Option<StreamSchedule>,
    can_manage_server: bool,
    limits: &EmbyWebsocketLimits,
) -> ClientAction {
    let Ok(message) = serde_json::from_str::<Value>(text) else {
        return ClientAction::None;
    };
    let Some(message_type) = message.get("MessageType").and_then(Value::as_str) else {
        return ClientAction::None;
    };
    let data = message.get("Data");
    match message_type.trim().to_ascii_lowercase().as_str() {
        "sessionsstart" => {
            let timing = stream_timing(data, 1500, limits);
            *sessions = Some(StreamSchedule {
                next_push: Instant::now() + timing.initial,
                interval: timing.interval,
            });
            ClientAction::None
        }
        "sessionsstop" => {
            *sessions = None;
            ClientAction::None
        }
        // A non-administrator can open the channel but must never learn the scan state, so the
        // message is ignored silently: no push, no error, and the connection stays open.
        "scheduledtasksinfostart" => {
            if can_manage_server {
                let timing = stream_timing(data, 1000, limits);
                *tasks = Some(StreamSchedule {
                    next_push: Instant::now() + timing.initial,
                    interval: timing.interval,
                });
            }
            ClientAction::None
        }
        "scheduledtasksinfostop" => {
            *tasks = None;
            ClientAction::None
        }
        "keepalive" => ClientAction::Reply(KEEP_ALIVE_REPLY),
        _ => ClientAction::None,
    }
}

struct StreamTiming {
    initial: Duration,
    interval: Duration,
}

/// Parses Emby's `Data` (`"initialDelayMillis,pollIntervalMillis,…"`) and clamps both values
/// instead of failing: a client asking for a 1 ms poll must not be able to generate load, and a
/// client asking for a day must still see data within a bounded time.
fn stream_timing(
    data: Option<&Value>,
    default_interval_ms: u64,
    limits: &EmbyWebsocketLimits,
) -> StreamTiming {
    let raw = data.and_then(Value::as_str).unwrap_or_default();
    let mut parts = raw.split(',');
    let initial_ms = parts.next().and_then(parse_millis).unwrap_or(0);
    let interval_ms = parts
        .next()
        .and_then(parse_millis)
        .unwrap_or(default_interval_ms);
    StreamTiming {
        initial: clamp_millis(initial_ms, Duration::ZERO, limits.max_interval),
        interval: clamp_millis(interval_ms, limits.min_interval, limits.max_interval),
    }
}

fn parse_millis(value: &str) -> Option<u64> {
    value
        .trim()
        .parse::<i64>()
        .ok()
        .and_then(|value| u64::try_from(value).ok())
}

fn clamp_millis(value_ms: u64, min: Duration, max: Duration) -> Duration {
    Duration::from_millis(value_ms).clamp(min, max)
}
