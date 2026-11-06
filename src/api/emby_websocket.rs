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
/// Initial capacity of a connection's write buffer. It is only a capacity hint: a single message
/// larger than this is buffered up to [`EMBY_WEBSOCKET_MAX_WRITE_BUFFER_BYTES`] before it is
/// flushed, so this value must never be used as a document limit.
pub(crate) const EMBY_WEBSOCKET_WRITE_BUFFER_BYTES: usize = 16 * 1024;
/// Hard cap of a connection's write buffer, and therefore the largest document one push may have.
///
/// A `Sessions` document grows with the listed sessions
/// (`MAX_LISTED_PLAYBACK_SESSIONS = 500` in `src/storage/sessions.rs`). One session entry measures
/// about 1.3-1.5 KiB with `NowPlayingItem`, `PlayState` and `TranscodingInfo`, so the worst case is
/// roughly 500 x 1.5 KiB = 750 KiB. 4 MiB leaves more than five times that headroom, so a busy
/// server never hits the cap, while a peer that stops reading still cannot grow the buffer past a
/// few MiB. Anything larger than the cap is skipped (see `payload_fits`) instead of closing the
/// connection, because Yamby reconnects without backoff.
pub(crate) const EMBY_WEBSOCKET_MAX_WRITE_BUFFER_BYTES: usize = 4 * 1024 * 1024;
/// Upper bound of the configurable initial write-buffer capacity. It is only a hint, so a
/// misconfigured override must not allocate a huge buffer per connection up front, and it must
/// stay below the hard cap by at least one byte (tungstenite asserts a strict inequality).
pub(crate) const EMBY_WEBSOCKET_WRITE_BUFFER_BYTES_LIMIT: usize = 1024 * 1024;
/// A single send that cannot complete within this bound closes the connection. It is far below
/// the idle timeout so a stalled reader is reaped before the idle deadline would fire.
pub(crate) const EMBY_WEBSOCKET_SEND_TIMEOUT: Duration = Duration::from_secs(10);
/// Token revalidation that runs even when no push stream is active, so `SessionsStop` cannot keep
/// a revoked token (or a disabled remote-access policy) alive.
pub(crate) const EMBY_WEBSOCKET_REVALIDATE_INTERVAL: Duration = Duration::from_secs(45);
/// Minimum spacing of the "revalidation unavailable" warnings of one connection.
pub(crate) const EMBY_WEBSOCKET_AUTH_WARN_INTERVAL: Duration = Duration::from_secs(60);

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
    /// Initial capacity of the WebSocket write buffer.
    pub write_buffer_bytes: usize,
    /// Hard cap of the WebSocket write buffer; a send that would exceed it fails.
    pub max_write_buffer_bytes: usize,
    /// A single send that cannot complete within this bound closes the connection.
    pub send_timeout: Duration,
    /// Token revalidation that does not depend on an active push stream.
    pub revalidate_interval: Duration,
    /// Minimum spacing of the revalidation warnings of one connection.
    pub auth_warn_interval: Duration,
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
            write_buffer_bytes: EMBY_WEBSOCKET_WRITE_BUFFER_BYTES,
            max_write_buffer_bytes: EMBY_WEBSOCKET_MAX_WRITE_BUFFER_BYTES,
            send_timeout: EMBY_WEBSOCKET_SEND_TIMEOUT,
            revalidate_interval: EMBY_WEBSOCKET_REVALIDATE_INTERVAL,
            auth_warn_interval: EMBY_WEBSOCKET_AUTH_WARN_INTERVAL,
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
        // tungstenite asserts `max_write_buffer_size > write_buffer_size` right after the upgrade
        // (`WebSocketConfig::assert_valid`), and that assert runs inside the connection task: an
        // equal or smaller cap would panic there instead of failing the handshake. The initial
        // capacity is clamped so the strict inequality is always reachable, including for
        // `usize::MAX` overrides (saturating add keeps the arithmetic defined).
        self.write_buffer_bytes = self
            .write_buffer_bytes
            .clamp(1024, EMBY_WEBSOCKET_WRITE_BUFFER_BYTES_LIMIT);
        self.max_write_buffer_bytes = self
            .max_write_buffer_bytes
            .max(self.write_buffer_bytes.saturating_add(1));
        self.send_timeout = self.send_timeout.max(Duration::from_millis(10));
        self.revalidate_interval = self.revalidate_interval.max(Duration::from_millis(10));
        self.auth_warn_interval = self.auth_warn_interval.max(Duration::from_millis(10));
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
        // A peer that stops reading must not be able to grow the write buffer without bound: the
        // initial capacity is small and the hard cap turns a stalled send into an error instead of
        // unbounded memory use (every send also has a time bound, see `send_bounded`).
        .write_buffer_size(limits.write_buffer_bytes)
        .max_write_buffer_size(limits.max_write_buffer_bytes)
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

/// One active push plan: the next push and the interval between pushes.
struct StreamPlan {
    next_push: Instant,
    interval: Duration,
}

/// The schedule of one push stream (`Sessions` or `ScheduledTasksInfo`).
///
/// `last_attempt` survives `Stop`/`Start`, so the minimum interval belongs to the connection and
/// not to the current plan: a burst of `…Start` frames cannot move the next push before one
/// minimum interval after the previous attempt, which is what keeps a tiny client frame from
/// forcing repeated document rebuilds.
struct StreamState {
    min_interval: Duration,
    plan: Option<StreamPlan>,
    last_attempt: Option<Instant>,
}

impl StreamState {
    fn new(min_interval: Duration) -> Self {
        Self {
            min_interval,
            plan: None,
            last_attempt: None,
        }
    }

    /// Applies a `…Start` frame. It only decides when the first push of the new plan may run; the
    /// plan never starts earlier than one minimum interval after the previous attempt.
    fn start(&mut self, now: Instant, timing: StreamTiming) {
        let floor = self
            .last_attempt
            .map(|last| last + self.min_interval)
            .unwrap_or(now)
            .max(now);
        self.plan = Some(StreamPlan {
            next_push: (now + timing.initial).max(floor),
            interval: timing.interval.max(self.min_interval),
        });
    }

    fn stop(&mut self) {
        self.plan = None;
    }

    fn due(&self, now: Instant) -> bool {
        self.plan.as_ref().is_some_and(|plan| now >= plan.next_push)
    }

    fn next_push(&self) -> Option<Instant> {
        self.plan.as_ref().map(|plan| plan.next_push)
    }

    /// Records one push *attempt* (whether it produced a frame or was skipped) and schedules the
    /// following one.
    fn record_attempt(&mut self, now: Instant) {
        self.last_attempt = Some(now);
        if let Some(plan) = self.plan.as_mut() {
            plan.next_push = now + plan.interval.max(self.min_interval);
        }
    }
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

/// What a token revalidation means for the connection.
#[derive(Debug, Eq, PartialEq)]
enum RevalidationOutcome {
    /// The credential is still valid.
    Authorized,
    /// The token was revoked or the account/policy forbids the request: close the connection.
    Closed,
    /// The answer is temporary (database or access-rule trouble): keep the connection and retry on
    /// the next scheduled attempt.
    Retry,
}

fn revalidation_outcome(status: StatusCode) -> RevalidationOutcome {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => RevalidationOutcome::Closed,
        _ => RevalidationOutcome::Retry,
    }
}

/// Logs a transient revalidation failure at most once per `auth_warn_interval`, so a flapping
/// database cannot turn one connection into a log flood. Never includes the credential.
fn warn_revalidation_unavailable(
    last_warning: &mut Option<Instant>,
    now: Instant,
    limits: &EmbyWebsocketLimits,
    status: StatusCode,
) {
    let due = last_warning
        .is_none_or(|last| now.saturating_duration_since(last) >= limits.auth_warn_interval);
    if !due {
        return;
    }
    *last_warning = Some(now);
    tracing::warn!(
        status = status.as_u16(),
        "emby websocket token revalidation is unavailable; keeping the connection"
    );
}

/// Runs one send under a hard time bound. Returns `false` when the peer is too slow (or the
/// socket is broken), which is the caller's signal to close the connection.
async fn bounded<F>(send: F, timeout: Duration) -> bool
where
    F: std::future::Future<Output = Result<(), axum::Error>>,
{
    matches!(tokio::time::timeout(timeout, send).await, Ok(Ok(())))
}

async fn send_bounded(socket: &mut WebSocket, message: Message, timeout: Duration) -> bool {
    bounded(socket.send(message), timeout).await
}

/// Whether one serialized document fits into the write buffer.
///
/// A document at or above the cap would make tungstenite reject the frame (`WriteBufferFull`) and
/// force the connection to close, so the push is skipped instead: the peer keeps its connection
/// and the next schedule retries with a document that may have shrunk.
fn payload_fits(payload: &str, limits: &EmbyWebsocketLimits) -> bool {
    payload.len() < limits.max_write_buffer_bytes
}

/// Logs an oversized document at most once per `auth_warn_interval`; never includes the payload
/// or any credential.
fn warn_document_too_large(
    last_warning: &mut Option<Instant>,
    now: Instant,
    limits: &EmbyWebsocketLimits,
    payload_len: usize,
) {
    let due = last_warning
        .is_none_or(|last| now.saturating_duration_since(last) >= limits.auth_warn_interval);
    if !due {
        return;
    }
    *last_warning = Some(now);
    tracing::warn!(
        payload_bytes = payload_len,
        limit_bytes = limits.max_write_buffer_bytes,
        "skipped an emby websocket push larger than the write buffer"
    );
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
    let mut sessions = StreamState::new(limits.min_interval);
    let mut tasks = StreamState::new(limits.min_interval);
    let mut idle_deadline = Instant::now() + limits.idle_timeout;
    let mut next_heartbeat = Instant::now() + limits.heartbeat_interval;
    let mut next_revalidate = Instant::now() + limits.revalidate_interval;
    let mut last_revalidation_warning: Option<Instant> = None;
    let mut last_document_warning: Option<Instant> = None;
    let close = loop {
        if *shutdown.borrow() {
            break Some(utf8_close(CLOSE_GOING_AWAY, "server shutting down"));
        }
        // Timer work runs at the top of every iteration, *before* the loop waits for an inbound
        // frame: a client that never stops sending must not be able to starve pushes, heartbeats
        // or the periodic revalidation (and with them, a revoked token).
        let now = Instant::now();
        if now >= idle_deadline {
            break Some(utf8_close(CLOSE_GOING_AWAY, "idle timeout"));
        }
        if now >= next_heartbeat {
            if !send_bounded(
                &mut socket,
                Message::Ping(Bytes::new()),
                limits.send_timeout,
            )
            .await
            {
                break None;
            }
            next_heartbeat = now + limits.heartbeat_interval;
        }
        // A connection without an active push plan would otherwise never revalidate, so a revoked
        // token (or a disabled policy) would keep `SessionsStop` connections open.
        if now >= next_revalidate {
            let outcome = match revalidated_principal(&state, &headers, &query).await {
                Ok(_) => RevalidationOutcome::Authorized,
                Err(status) => {
                    let outcome = revalidation_outcome(status);
                    if outcome == RevalidationOutcome::Retry {
                        warn_revalidation_unavailable(
                            &mut last_revalidation_warning,
                            now,
                            &limits,
                            status,
                        );
                    }
                    outcome
                }
            };
            if outcome == RevalidationOutcome::Closed {
                break Some(utf8_close(CLOSE_POLICY_VIOLATION, "token revoked"));
            }
            next_revalidate = now + limits.revalidate_interval;
        }
        if sessions.due(now) {
            match revalidated_principal(&state, &headers, &query).await {
                Ok(principal) => match emby_session_entries(&state, &principal, None).await {
                    Ok(entries) => {
                        let payload =
                            json!({"MessageType": "Sessions", "Data": entries}).to_string();
                        if payload_fits(&payload, &limits) {
                            if !send_bounded(
                                &mut socket,
                                Message::Text(payload.into()),
                                limits.send_timeout,
                            )
                            .await
                            {
                                break None;
                            }
                        } else {
                            warn_document_too_large(
                                &mut last_document_warning,
                                now,
                                &limits,
                                payload.len(),
                            );
                        }
                    }
                    Err(status) => tracing::debug!(
                        status = status.as_u16(),
                        "skipped an emby websocket session push"
                    ),
                },
                Err(status) if revalidation_outcome(status) == RevalidationOutcome::Closed => {
                    break Some(utf8_close(CLOSE_POLICY_VIOLATION, "token revoked"));
                }
                // A temporary failure must not drop the connection: the next scheduled attempt
                // retries, and the warning is rate limited.
                Err(status) => warn_revalidation_unavailable(
                    &mut last_revalidation_warning,
                    now,
                    &limits,
                    status,
                ),
            }
            sessions.record_attempt(Instant::now());
        }
        if tasks.due(now) {
            match revalidated_principal(&state, &headers, &query).await {
                Ok(principal) if principal.can_manage_server() => {
                    match emby_scheduled_task_entries(&state).await {
                        Ok(entries) => {
                            let payload = json!({
                                "MessageType": "ScheduledTasksInfo",
                                "Data": entries,
                            })
                            .to_string();
                            if payload_fits(&payload, &limits) {
                                if !send_bounded(
                                    &mut socket,
                                    Message::Text(payload.into()),
                                    limits.send_timeout,
                                )
                                .await
                                {
                                    break None;
                                }
                            } else {
                                warn_document_too_large(
                                    &mut last_document_warning,
                                    now,
                                    &limits,
                                    payload.len(),
                                );
                            }
                        }
                        Err(status) => tracing::debug!(
                            status = status.as_u16(),
                            "skipped an emby websocket scheduled-task push"
                        ),
                    }
                }
                // A demoted administrator stops observing the scan state; the stream is dropped
                // instead of failing the whole connection.
                Ok(_) => tasks.stop(),
                Err(status) if revalidation_outcome(status) == RevalidationOutcome::Closed => {
                    break Some(utf8_close(CLOSE_POLICY_VIOLATION, "token revoked"));
                }
                Err(status) => warn_revalidation_unavailable(
                    &mut last_revalidation_warning,
                    now,
                    &limits,
                    status,
                ),
            }
            tasks.record_attempt(Instant::now());
        }
        let next_wake = next_wake(
            &sessions,
            &tasks,
            idle_deadline,
            next_heartbeat,
            next_revalidate,
        );
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
                                if !send_bounded(
                                    &mut socket,
                                    Message::Text(Utf8Bytes::from(payload)),
                                    limits.send_timeout,
                                )
                                .await
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
            // The timer pass at the top of the loop already did the work; just iterate again.
            Wake::Timer => {}
        }
    };
    if let Some(frame) = close {
        let _ = send_bounded(
            &mut socket,
            Message::Close(Some(frame)),
            limits.send_timeout,
        )
        .await;
    }
    // Releases the connection budget; must happen after the close frame was written.
    drop(permit);
    tracing::debug!("emby websocket connection closed");
}

/// The next wake-up of the loop: the earliest push, heartbeat, revalidation and idle deadline.
fn next_wake(
    sessions: &StreamState,
    tasks: &StreamState,
    idle_deadline: Instant,
    next_heartbeat: Instant,
    next_revalidate: Instant,
) -> Instant {
    let mut next = idle_deadline.min(next_heartbeat).min(next_revalidate);
    for schedule in [sessions.next_push(), tasks.next_push()]
        .into_iter()
        .flatten()
    {
        next = next.min(schedule);
    }
    next
}

/// Revalidates the connection credential. The status code is preserved so the caller can tell a
/// revoked token (401/403) from a temporary failure (5xx), which keeps the connection.
async fn revalidated_principal(
    state: &ReadyAppState,
    headers: &HeaderMap,
    query: &EmbyTokenQuery,
) -> Result<crate::auth::users::AuthenticationPrincipal, StatusCode> {
    require_emby_principal_with_query(headers, state, query).await
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
    sessions: &mut StreamState,
    tasks: &mut StreamState,
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
            sessions.start(Instant::now(), stream_timing(data, 1500, limits));
            ClientAction::None
        }
        "sessionsstop" => {
            sessions.stop();
            ClientAction::None
        }
        // A non-administrator can open the channel but must never learn the scan state, so the
        // message is ignored silently: no push, no error, and the connection stays open.
        "scheduledtasksinfostart" => {
            if can_manage_server {
                tasks.start(Instant::now(), stream_timing(data, 1000, limits));
            }
            ClientAction::None
        }
        "scheduledtasksinfostop" => {
            tasks.stop();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_limits() -> EmbyWebsocketLimits {
        EmbyWebsocketLimits {
            min_interval: Duration::from_millis(200),
            ..EmbyWebsocketLimits::default()
        }
    }

    #[test]
    fn revalidation_classifies_revocation_and_transient_failures() {
        assert_eq!(
            revalidation_outcome(StatusCode::UNAUTHORIZED),
            RevalidationOutcome::Closed
        );
        assert_eq!(
            revalidation_outcome(StatusCode::FORBIDDEN),
            RevalidationOutcome::Closed
        );
        assert_eq!(
            revalidation_outcome(StatusCode::INTERNAL_SERVER_ERROR),
            RevalidationOutcome::Retry
        );
        assert_eq!(
            revalidation_outcome(StatusCode::SERVICE_UNAVAILABLE),
            RevalidationOutcome::Retry
        );
    }

    #[tokio::test]
    async fn bounded_send_gives_up_on_a_stalled_write() {
        let started = Instant::now();
        let delivered = bounded(
            std::future::pending::<Result<(), axum::Error>>(),
            Duration::from_millis(20),
        )
        .await;
        assert!(!delivered);
        assert!(started.elapsed() >= Duration::from_millis(15));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn bounded_send_reports_a_delivered_frame() {
        assert!(bounded(std::future::ready(Ok(())), Duration::from_secs(1)).await);
    }

    #[test]
    fn repeated_start_frames_cannot_push_before_the_minimum_interval() {
        let limits = test_limits();
        let mut stream = StreamState::new(limits.min_interval);
        let start = Instant::now();
        stream.start(
            start,
            stream_timing(Some(&json!("0,100000")), 1500, &limits),
        );
        assert!(stream.due(start), "the first push has no earlier attempt");

        stream.record_attempt(start);
        // A Start frame right after the push cannot schedule an earlier one.
        stream.start(
            start + Duration::from_millis(5),
            stream_timing(Some(&json!("0,1")), 1500, &limits),
        );
        assert!(!stream.due(start + Duration::from_millis(150)));
        assert!(stream.due(start + Duration::from_millis(205)));
    }

    #[test]
    fn a_stopped_stream_still_respects_the_minimum_interval_after_a_restart() {
        let limits = test_limits();
        let mut stream = StreamState::new(limits.min_interval);
        let start = Instant::now();
        stream.start(start, stream_timing(Some(&json!("0,1000")), 1500, &limits));
        stream.record_attempt(start);
        stream.stop();
        stream.start(
            start + Duration::from_millis(10),
            stream_timing(Some(&json!("0,1000")), 1500, &limits),
        );
        assert!(!stream.due(start + Duration::from_millis(150)));
        assert!(stream.due(start + Duration::from_millis(210)));
    }

    #[test]
    fn write_buffer_cap_is_always_strictly_greater_than_the_initial_capacity() {
        let equal = EmbyWebsocketLimits {
            write_buffer_bytes: 4096,
            max_write_buffer_bytes: 4096,
            ..EmbyWebsocketLimits::default()
        }
        .normalized();
        assert!(equal.max_write_buffer_bytes > equal.write_buffer_bytes);

        let smaller = EmbyWebsocketLimits {
            write_buffer_bytes: 4096,
            max_write_buffer_bytes: 1024,
            ..EmbyWebsocketLimits::default()
        }
        .normalized();
        assert!(smaller.max_write_buffer_bytes > smaller.write_buffer_bytes);

        let maxed = EmbyWebsocketLimits {
            write_buffer_bytes: usize::MAX,
            max_write_buffer_bytes: usize::MAX,
            ..EmbyWebsocketLimits::default()
        }
        .normalized();
        assert!(maxed.write_buffer_bytes <= EMBY_WEBSOCKET_WRITE_BUFFER_BYTES_LIMIT);
        assert!(maxed.max_write_buffer_bytes > maxed.write_buffer_bytes);
    }

    #[test]
    fn the_default_cap_leaves_room_for_the_largest_session_document() {
        let limits = EmbyWebsocketLimits::default();
        // 500 listed sessions at ~1.5 KiB each is the documented worst case (~750 KiB).
        let worst_case_document = 500 * 1536;
        assert!(worst_case_document < limits.max_write_buffer_bytes);
        // The initial capacity is a hint and must stay well below the cap.
        assert!(limits.write_buffer_bytes < limits.max_write_buffer_bytes / 4);
    }

    #[test]
    fn a_document_at_or_above_the_cap_is_not_sent() {
        let limits = EmbyWebsocketLimits {
            write_buffer_bytes: 1024,
            max_write_buffer_bytes: 4096,
            ..EmbyWebsocketLimits::default()
        }
        .normalized();
        assert!(payload_fits(&"x".repeat(4095), &limits));
        assert!(!payload_fits(&"x".repeat(4096), &limits));
        assert!(!payload_fits(&"x".repeat(64 * 1024), &limits));
    }

    #[test]
    fn stream_timing_clamps_both_values() {
        let limits = test_limits();
        let fast = stream_timing(Some(&json!("0,1")), 1500, &limits);
        assert_eq!(fast.initial, Duration::ZERO);
        assert_eq!(fast.interval, Duration::from_millis(200));

        let slow = stream_timing(Some(&json!("0,100000")), 1500, &limits);
        assert_eq!(slow.interval, limits.max_interval);

        let missing = stream_timing(None, 1500, &limits);
        assert_eq!(missing.initial, Duration::ZERO);
        assert_eq!(missing.interval, Duration::from_millis(1500));
    }
}
