//! `GET /api/v1/ws`: the Lux Web realtime invalidation channel (ADR-054).
//!
//! One connection carries both channels: a user always receives `user` invalidations, and a
//! principal that can manage the server additionally receives `admin` invalidations. The channels
//! are derived from the authenticated principal only; the client sends no subscription message.
//!
//! The handshake is decided before anything is upgraded:
//!
//! 1. a request without a legal `Upgrade`/`Connection` pair answers `426 Upgrade Required` from
//!    this handler (not the extractor's `400`, and never an empty `200`);
//! 2. the strict same-origin gate compares the `Host` of `Origin` with the request `Host`
//!    (host and port only, lower-cased, default ports dropped, no forwarded header is read);
//! 3. [`require_web_user`] authenticates the session cookie or a client token;
//! 4. the connection budget of the principal's channel is taken before the upgrade, so an
//!    exhausted budget answers `429` + `Retry-After` and never a `101`.
//!
//! Query parameters are ignored: this endpoint never reads a credential from the query string.
//!
//! Downstream frames are frozen by ADR-054 D3 (`ready` and `invalidate` only, `scope` verbatim, an
//! optional `itemId`, never a `libraryId`). Upstream only `{"type":"ping"}` is recognized, and it
//! only resets the idle deadline: no `pong` frame exists in the envelope.
//!
//! The channel qualification is revalidated on the shared `revalidate_interval` timer (45 s), which
//! is the one deliberate difference from `/embywebsocket`'s per-push revalidation (ADR-054: Lux's
//! invalidation rate follows business events, so a per-frame session lookup would bind database
//! queries to the event rate). A `401`/`403` closes the connection with `1008`; a temporary `5xx`
//! keeps it and is warned about at most once per `auth_warn_interval`. A demotion
//! (`can_manage_server` becomes false) only stops the `admin` channel.

use super::*;

use crate::api::websocket::{
    CLOSE_GOING_AWAY, CLOSE_NORMAL, CLOSE_POLICY_VIOLATION,
    LUX_WEBSOCKET_DEFAULT_ADMIN_MAX_CONNECTIONS,
    LUX_WEBSOCKET_DEFAULT_ADMIN_MAX_CONNECTIONS_PER_USER, LUX_WEBSOCKET_DEFAULT_MAX_MESSAGE_BYTES,
    LUX_WEBSOCKET_DEFAULT_USER_MAX_CONNECTIONS,
    LUX_WEBSOCKET_DEFAULT_USER_MAX_CONNECTIONS_PER_USER, LUX_WEBSOCKET_FRAME_BUFFER_LIMIT,
    WebsocketConnections, WebsocketLimits, WebsocketPermit, WebsocketShutdown, send_bounded,
    utf8_close,
};

use crate::application::admin_events::UserEvent;
use axum::extract::FromRequestParts;
use axum::extract::ws::{Message, Utf8Bytes, WebSocket, WebSocketUpgrade};
use axum::http::Uri;
use tokio::sync::{broadcast, watch};
use tokio::time::Instant;

/// Close code of an inbound frame type this endpoint does not accept (binary frames).
const CLOSE_UNSUPPORTED_DATA: u16 = 1003;
/// Close code of an inbound message above the contract limit.
const CLOSE_MESSAGE_TOO_BIG: u16 = 1009;
/// The first frame of every connection (ADR-054 D3, sent exactly once).
const READY_FRAME: &str = r#"{"type":"ready","version":1}"#;
/// Largest accepted `itemId`. The Web client enforced the same bound at 256 characters, so a longer
/// value can only come from a corrupted publisher; the whole frame is dropped instead of truncating
/// it (ADR-054 "协议与信封").
const MAX_ITEM_ID_CHARS: usize = 256;

/// Resource limits of `/api/v1/ws`.
///
/// The two channels own separate budget instances (ADR-054 D8) but share one protocol contract:
/// the timings, the write boundary and the inbound message limit. A test injects smaller values so
/// a budget or a timer is observable without waiting for the production numbers.
#[derive(Clone, Debug)]
pub struct LuxWebsocketLimits {
    /// Concurrent connections of the `user` budget for the whole process.
    pub user_max_connections: usize,
    /// Concurrent connections of the `user` budget for one account.
    pub user_max_connections_per_user: usize,
    /// Concurrent connections of the `admin` budget for the whole process.
    pub admin_max_connections: usize,
    /// Concurrent connections of the `admin` budget for one account.
    pub admin_max_connections_per_user: usize,
    /// Timings, write boundary and inbound message limit shared by both channels.
    pub protocol: WebsocketLimits,
}

impl Default for LuxWebsocketLimits {
    fn default() -> Self {
        Self {
            user_max_connections: LUX_WEBSOCKET_DEFAULT_USER_MAX_CONNECTIONS,
            user_max_connections_per_user: LUX_WEBSOCKET_DEFAULT_USER_MAX_CONNECTIONS_PER_USER,
            admin_max_connections: LUX_WEBSOCKET_DEFAULT_ADMIN_MAX_CONNECTIONS,
            admin_max_connections_per_user: LUX_WEBSOCKET_DEFAULT_ADMIN_MAX_CONNECTIONS_PER_USER,
            protocol: WebsocketLimits {
                max_message_bytes: LUX_WEBSOCKET_DEFAULT_MAX_MESSAGE_BYTES,
                ..WebsocketLimits::default()
            },
        }
    }
}

/// The two budget instances of the endpoint. One connection belongs to exactly one of them,
/// chosen from the handshake principal, so it is never counted twice.
#[derive(Clone)]
pub(crate) struct LuxWebsocketConnections {
    user: WebsocketConnections,
    admin: WebsocketConnections,
}

impl Default for LuxWebsocketConnections {
    fn default() -> Self {
        Self::new(LuxWebsocketLimits::default())
    }
}

impl LuxWebsocketConnections {
    pub(crate) fn new(limits: LuxWebsocketLimits) -> Self {
        let protocol = limits.protocol.normalized();
        let channel_limits =
            |max_connections: usize, max_connections_per_user: usize| WebsocketLimits {
                max_connections,
                max_connections_per_user,
                ..protocol.clone()
            };
        Self {
            user: WebsocketConnections::new(channel_limits(
                limits.user_max_connections,
                limits.user_max_connections_per_user,
            )),
            admin: WebsocketConnections::new(channel_limits(
                limits.admin_max_connections,
                limits.admin_max_connections_per_user,
            )),
        }
    }

    /// The budget the handshake principal is counted against. An administrator's connection also
    /// carries the `user` channel, but it still occupies one slot of the `admin` budget only.
    fn budget(&self, can_manage_server: bool) -> &WebsocketConnections {
        if can_manage_server {
            &self.admin
        } else {
            &self.user
        }
    }

    fn channel(can_manage_server: bool) -> &'static str {
        if can_manage_server { "admin" } else { "user" }
    }

    /// Timings and the write boundary, which are identical in both instances.
    fn protocol_limits(&self) -> &WebsocketLimits {
        self.user.limits()
    }

    /// Waits up to `timeout` for both channels to release every slot.
    pub(crate) async fn wait_until_idle(&self, timeout: Duration) -> bool {
        let (user, admin) = tokio::join!(
            self.user.wait_until_idle(timeout),
            self.admin.wait_until_idle(timeout)
        );
        user && admin
    }
}

impl CoreState {
    pub(crate) fn lux_websocket_connections(&self) -> &LuxWebsocketConnections {
        &self.lux_websocket_connections
    }

    /// The shutdown broadcast both realtime endpoints watch. `/embywebsocket` and `/api/v1/ws`
    /// share one signal so a shutdown can never reach one endpoint without the other (ADR-054
    /// D10).
    pub(crate) fn websocket_shutdown(&self) -> &WebsocketShutdown {
        &self.websocket_shutdown
    }
}

pub(super) fn api_routes() -> Router<ReadyAppState> {
    Router::new().route("/api/v1/ws", get(lux_websocket))
}

/// Everything one open connection needs; bundled so the loop signature stays readable.
struct LuxConnectionContext {
    state: ReadyAppState,
    headers: HeaderMap,
    user_id: String,
    can_manage_server: bool,
    limits: WebsocketLimits,
    shutdown: watch::Receiver<bool>,
    permit: WebsocketPermit,
    connection: u64,
}

/// How the connection ended. `Dropped` means the socket is already unusable (the peer went away, or
/// the frame stream broke), so no close frame can be written any more.
enum LuxClose {
    Frame { code: u16, reason: &'static str },
    Dropped,
}

impl LuxClose {
    fn frame(code: u16, reason: &'static str) -> Self {
        Self::Frame { code, reason }
    }

    fn dropped() -> Self {
        Self::Dropped
    }
}

/// The outcome of one subscription wake-up.
enum SubscriptionWake {
    /// A frame to write, or nothing when the event itself was invalid (an over-long `itemId`).
    Frame(Option<String>),
    /// The event hub is gone; the channel is not listened to any more.
    Closed,
}

/// What the strict same-origin gate decided.
#[derive(Debug, Eq, PartialEq)]
enum OriginCheck {
    Allowed,
    /// No usable `Origin` (absent, `null`, unparsable, several values) on a cookie-only handshake.
    Missing,
    /// A usable `Origin` whose host[:port] differs from the request's host[:port].
    Mismatch,
    /// `Sec-Fetch-Site: cross-site`.
    CrossSite,
}

pub(super) async fn lux_websocket(
    State(state): State<ReadyAppState>,
    request: Request<Body>,
) -> Response {
    let (mut parts, _body) = request.into_parts();
    // The upgrade request is recognized here instead of by the `WebSocketUpgrade` extractor so a
    // plain GET answers 426 (ADR-054 "端点"): Axum's extractor would answer 400 and an empty 200
    // would look like a health probe.
    if !is_websocket_upgrade(&parts.headers) {
        let mut response = api_error(
            &parts.headers,
            StatusCode::UPGRADE_REQUIRED,
            lux::ApiErrorCode::InvalidRequest,
            "需要 WebSocket Upgrade",
        )
        .into_response();
        response
            .headers_mut()
            .insert("upgrade", HeaderValue::from_static("websocket"));
        response
            .headers_mut()
            .insert("connection", HeaderValue::from_static("upgrade"));
        return response;
    }
    // A shutting-down process refuses new handshakes instead of upgrading into a connection that
    // is closed on the next tick (ADR-054 D10).
    if state.websocket_shutdown().is_signalled() {
        log_handshake_rejected("shutting-down", StatusCode::SERVICE_UNAVAILABLE);
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match check_origin(&parts.headers, &parts.uri) {
        OriginCheck::Allowed => {}
        OriginCheck::Missing => {
            // A cookie handshake without a usable Origin cannot be told apart from a cross-site
            // one, so it is treated as unauthenticated (ADR-054 "Origin 校验" rule 6). A client
            // token is allowed through because scripts and non-browser clients cannot send it.
            log_handshake_rejected("origin-missing", StatusCode::UNAUTHORIZED);
            return api_error(
                &parts.headers,
                StatusCode::UNAUTHORIZED,
                lux::ApiErrorCode::AuthenticationRequired,
                "跨源实时连接仅接受客户端令牌",
            )
            .into_response();
        }
        OriginCheck::Mismatch => {
            log_handshake_rejected("origin-mismatch", StatusCode::BAD_REQUEST);
            return api_error(
                &parts.headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "跨源实时连接被拒绝",
            )
            .into_response();
        }
        OriginCheck::CrossSite => {
            log_handshake_rejected("sec-fetch-site", StatusCode::BAD_REQUEST);
            return api_error(
                &parts.headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                "跨源实时连接被拒绝",
            )
            .into_response();
        }
    }
    let ws = match WebSocketUpgrade::from_request_parts(&mut parts, &state).await {
        Ok(ws) => ws,
        Err(rejection) => {
            // The Upgrade tokens were accepted above, so this can only be a malformed handshake
            // (a missing key or version, or a non-GET method on the same path).
            let response = rejection.into_response();
            log_handshake_rejected("upgrade", response.status());
            return response;
        }
    };
    let headers = parts.headers.clone();
    // Authentication happens before the upgrade on purpose: a refused handshake must answer its
    // own status instead of completing a 101 and closing afterwards (ADR-054 "握手认证").
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => {
            log_handshake_rejected(unauthenticated_reason(response.status()), response.status());
            return response;
        }
    };
    let user_id = user.id.to_string();
    // ADR-054 D7: the `admin` channel comes from the server-side principal only. For a session
    // cookie this is exactly `require_admin(.., false)`'s permission branch; a client token whose
    // subject can manage the server gets the channel too, because the channel follows the subject
    // (the token cannot call the admin JSON API anyway, so no privilege is added).
    let can_manage_server = user.can_manage_server;
    let connections = state.lux_websocket_connections().clone();
    let Some(permit) = connections.budget(can_manage_server).try_acquire(&user_id) else {
        log_handshake_rejected("budget", StatusCode::TOO_MANY_REQUESTS);
        tracing::warn!(
            user = %user_id,
            channel = LuxWebsocketConnections::channel(can_manage_server),
            "rejected a lux websocket connection: connection budget exhausted"
        );
        return lux_websocket_capacity_response(&headers);
    };
    let limits = connections.protocol_limits().clone();
    let shutdown = state.websocket_shutdown().subscribe();
    let connection = next_connection_sequence();
    let channel = LuxWebsocketConnections::channel(can_manage_server);
    tracing::debug!(
        user = %user_id,
        connection,
        channel,
        "lux websocket connection accepted"
    );
    let context = LuxConnectionContext {
        state,
        headers,
        user_id,
        can_manage_server,
        limits: limits.clone(),
        shutdown,
        permit,
        connection,
    };
    ws.max_message_size(LUX_WEBSOCKET_FRAME_BUFFER_LIMIT)
        .max_frame_size(LUX_WEBSOCKET_FRAME_BUFFER_LIMIT)
        // ADR-054 "资源与预算": the write boundary is shared with `/embywebsocket`, including its
        // initial capacity, its hard cap and the per-send timeout.
        .write_buffer_size(limits.write_buffer_bytes)
        .max_write_buffer_size(limits.max_write_buffer_bytes)
        .on_upgrade(move |socket| run_connection(socket, context))
        .into_response()
}

fn unauthenticated_reason(status: StatusCode) -> &'static str {
    match status {
        StatusCode::UNAUTHORIZED => "unauthenticated",
        StatusCode::FORBIDDEN => "permission-denied",
        _ => "authentication-unavailable",
    }
}

/// Logs the rejection class only. Header values (including `Origin`, which leaks internal host
/// names) are never recorded (ADR-054 "入站输入与日志").
fn log_handshake_rejected(reason: &'static str, status: StatusCode) {
    tracing::info!(
        reason,
        status = status.as_u16(),
        "rejected a lux websocket handshake"
    );
}

/// The request carries a legal WebSocket upgrade: `Upgrade` contains `websocket` and `Connection`
/// contains `upgrade`, both case-insensitive (RFC 6455 §4.2.1).
fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    header_tokens(headers, "upgrade")
        .iter()
        .any(|token| token.eq_ignore_ascii_case("websocket"))
        && header_tokens(headers, "connection")
            .iter()
            .any(|token| token.eq_ignore_ascii_case("upgrade"))
}

/// Every comma-separated token of all values of one header.
fn header_tokens(headers: &HeaderMap, name: &str) -> Vec<String> {
    headers
        .get_all(name)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|token| token.trim().to_owned())
        .filter(|token| !token.is_empty())
        .collect()
}

/// Strict same-origin gate (ADR-054 "Origin 校验", rules 1-7).
///
/// Only the host and the port are compared, never the scheme, and no forwarded header is read: a
/// deployment behind a proxy must keep the browser-visible `Host` (LUX-896 documents the required
/// `proxy_set_header Host $http_host;`).
fn check_origin(headers: &HeaderMap, uri: &Uri) -> OriginCheck {
    // Rule 5: a cheap additional signal. Missing is not a rejection (older clients do not send
    // it), and it is a rejection even when the Origin would match.
    if header_tokens(headers, "sec-fetch-site")
        .iter()
        .any(|token| token.eq_ignore_ascii_case("cross-site"))
    {
        return OriginCheck::CrossSite;
    }
    let origin = single_header_value(headers, "origin");
    // Rule 1: a path, several values, a literal `null` or an unparsable value is "missing".
    let origin_authority = origin.as_deref().and_then(parse_origin_authority);
    match origin_authority {
        Some(origin_authority) => {
            // Rule 3: the request authority comes from `Host`, falling back to the URI authority.
            let host_authority = request_authority(headers, uri);
            match host_authority {
                // Rule 4: only an exact host[:port] match is allowed.
                Some(host_authority) if host_authority == origin_authority => OriginCheck::Allowed,
                _ => OriginCheck::Mismatch,
            }
        }
        // Rule 6: without an Origin only a client token may complete the handshake.
        None if users::has_client_token(headers) => OriginCheck::Allowed,
        None => OriginCheck::Missing,
    }
}

/// The only value of one header, or `None` when it is absent or repeated. A repeated header is not
/// a single serialized origin, so it is handled as missing (ADR-054 "Origin 校验" rule 1).
fn single_header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let mut values = headers
        .get_all(name)
        .iter()
        .filter_map(|value| value.to_str().ok());
    let first = values.next()?;
    if values.next().is_some() {
        return None;
    }
    Some(first.to_owned())
}

/// Parses a serialized origin (`http://host[:port]` / `https://host[:port]`) into its normalized
/// authority. Returns `None` for anything that is not exactly one such origin.
fn parse_origin_authority(value: &str) -> Option<String> {
    // A serialized origin never carries surrounding whitespace (the header parser strips OWS), so
    // one with it is not the value a browser sent.
    if value.is_empty() || value.trim() != value || value.eq_ignore_ascii_case("null") {
        return None;
    }
    let (scheme, rest) = value.split_once("://")?;
    let default_port = match scheme.to_ascii_lowercase().as_str() {
        "http" => 80,
        "https" => 443,
        _ => return None,
    };
    normalize_authority(rest, &[default_port])
}

/// The request authority: `Host` when present, otherwise the URI authority. A malformed or repeated
/// `Host` is not usable, so the handshake cannot match.
fn request_authority(headers: &HeaderMap, uri: &Uri) -> Option<String> {
    match single_header_value(headers, "host") {
        Some(host) => normalize_authority(&host, &[80, 443]),
        None => uri
            .authority()
            .and_then(|authority| normalize_authority(authority.as_str(), &[80, 443])),
    }
}

/// Normalizes `host[:port]`: the host is lower-cased, a port equal to one of `default_ports` is
/// dropped and every other port is kept (ADR-054 "Origin 校验" rule 2). IPv6 literals keep their
/// brackets. Anything with a path, user info, whitespace or a non-numeric port is rejected.
fn normalize_authority(value: &str, default_ports: &[u16]) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value.chars().any(|character| {
            character.is_whitespace() || matches!(character, '/' | '?' | '#' | '@' | ',')
        })
    {
        return None;
    }
    let (host, port) = if let Some(rest) = value.strip_prefix('[') {
        let (host, rest) = rest.split_once(']')?;
        if host.is_empty()
            || !host
                .chars()
                .all(|character| character.is_ascii_hexdigit() || character == ':')
        {
            return None;
        }
        let port = match rest {
            "" => None,
            rest => Some(rest.strip_prefix(':')?.parse::<u16>().ok()?),
        };
        (format!("[{host}]"), port)
    } else {
        let (host, port) = match value.rsplit_once(':') {
            Some((host, port)) => {
                if host.contains(':')
                    || port.is_empty()
                    || !port.chars().all(|c| c.is_ascii_digit())
                {
                    return None;
                }
                (host, Some(port.parse::<u16>().ok()?))
            }
            None => (value, None),
        };
        if host.is_empty() {
            return None;
        }
        (host.to_ascii_lowercase(), port)
    };
    match port {
        Some(port) if !default_ports.contains(&port) => Some(format!("{host}:{port}")),
        _ => Some(host),
    }
}

/// `429` + `Retry-After: 30`, the same shape the SSE budget answered with (ADR-054 "超限行为").
fn lux_websocket_capacity_response(headers: &HeaderMap) -> Response {
    let mut response = api_error(
        headers,
        StatusCode::TOO_MANY_REQUESTS,
        lux::ApiErrorCode::RateLimited,
        "同时打开的实时连接过多，请稍后重试",
    )
    .into_response();
    response
        .headers_mut()
        .insert("retry-after", HeaderValue::from_static("30"));
    response
}

/// A per-process connection sequence for log correlation. It never appears on the wire: the
/// envelope is frozen (ADR-054 D3), so the `ready` frame stays `{"type":"ready","version":1}`.
fn next_connection_sequence() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

async fn run_connection(mut socket: WebSocket, context: LuxConnectionContext) {
    let LuxConnectionContext {
        state,
        headers,
        user_id,
        can_manage_server,
        limits,
        mut shutdown,
        permit,
        connection,
    } = context;
    let channel = LuxWebsocketConnections::channel(can_manage_server);
    let mut can_manage_server = can_manage_server;
    let mut user_events = state.user_events.subscribe_for(&user_id);
    let mut admin_events = can_manage_server.then(|| state.admin_events.subscribe());
    let mut idle_deadline = Instant::now() + limits.idle_timeout;
    let mut next_heartbeat = Instant::now() + limits.heartbeat_interval;
    let mut next_revalidate = Instant::now() + limits.revalidate_interval;
    let mut last_revalidation_warning: Option<Instant> = None;

    let close = 'connection: {
        if !send_text(&mut socket, READY_FRAME, &limits).await {
            break 'connection LuxClose::dropped();
        }
        loop {
            if *shutdown.borrow() {
                break 'connection LuxClose::frame(CLOSE_GOING_AWAY, "server shutting down");
            }
            // Timer work runs at the top of every iteration, before the loop waits for an inbound
            // frame: a client that keeps sending must not be able to starve heartbeats or the
            // periodic revalidation.
            let now = Instant::now();
            if now >= idle_deadline {
                break 'connection LuxClose::frame(CLOSE_GOING_AWAY, "idle timeout");
            }
            if now >= next_heartbeat {
                if !send_bounded(
                    &mut socket,
                    Message::Ping(Bytes::new()),
                    limits.send_timeout,
                )
                .await
                {
                    break 'connection LuxClose::dropped();
                }
                next_heartbeat = now + limits.heartbeat_interval;
            }
            if now >= next_revalidate {
                match require_web_user(&headers, &state).await {
                    Ok(principal) => {
                        // A different account on the same connection cannot happen, but closing
                        // is the safe answer if it ever does.
                        if principal.id.to_string() != user_id {
                            break 'connection LuxClose::frame(
                                CLOSE_POLICY_VIOLATION,
                                "session changed",
                            );
                        }
                        let granted = principal.can_manage_server;
                        if granted && admin_events.is_none() {
                            // No replay and no "recovered" frame (ADR-054 D3/D7): the next
                            // published admin event is the first one this connection sees again.
                            admin_events = Some(state.admin_events.subscribe());
                        }
                        if !granted {
                            // Demotion stops the admin frames only; the connection and the user
                            // channel keep working (ADR-054 D7).
                            admin_events = None;
                        }
                        can_manage_server = granted;
                    }
                    Err(response) => {
                        let status = response.status();
                        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
                            break 'connection LuxClose::frame(
                                CLOSE_POLICY_VIOLATION,
                                "session revoked",
                            );
                        }
                        // A temporary failure must not drop the connection: the next scheduled
                        // attempt retries, and the warning is rate limited.
                        warn_revalidation_unavailable(
                            &mut last_revalidation_warning,
                            now,
                            &limits,
                            status,
                        );
                    }
                }
                next_revalidate = now + limits.revalidate_interval;
            }
            let next_wake = next_revalidate.min(next_heartbeat.min(idle_deadline));
            let wake = tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() {
                        Wake::Shutdown
                    } else {
                        Wake::Timer
                    }
                }
                event = user_events.recv() => Wake::User(event),
                event = recv_optional(&mut admin_events) => Wake::Admin(event),
                message = socket.recv() => Wake::Inbound(message),
                () = tokio::time::sleep_until(next_wake) => Wake::Timer,
            };
            match wake {
                Wake::Shutdown => {
                    break 'connection LuxClose::frame(CLOSE_GOING_AWAY, "server shutting down");
                }
                // The peer went away, or the frame stream is already broken (protocol error, the
                // transport limit, a broken socket); nothing more can be written.
                Wake::Inbound(None) | Wake::Inbound(Some(Err(_))) => {
                    break 'connection LuxClose::dropped();
                }
                Wake::Inbound(Some(Ok(message))) => {
                    // Any inbound frame, including a protocol pong and an unknown application
                    // message, is proof of life for the idle deadline.
                    idle_deadline = Instant::now() + limits.idle_timeout;
                    match message {
                        Message::Text(text) => {
                            // ADR-054 D4: the 4 KiB cap is a contract limit, so the handler owns
                            // it (the transport limit only bounds the buffer).
                            if text.len() > limits.max_message_bytes {
                                break 'connection LuxClose::frame(
                                    CLOSE_MESSAGE_TOO_BIG,
                                    "message too large",
                                );
                            }
                            // Only `{"type":"ping"}` is recognized, and it only resets the idle
                            // deadline: the envelope has no `pong` frame (ADR-054 D3). Unknown
                            // types and unparsable JSON are ignored on purpose.
                            tracing::trace!(
                                user = %user_id,
                                connection,
                                application_ping = is_application_ping(&text),
                                "lux websocket inbound message"
                            );
                        }
                        Message::Binary(_) => {
                            break 'connection LuxClose::frame(
                                CLOSE_UNSUPPORTED_DATA,
                                "binary frames are not supported",
                            );
                        }
                        Message::Close(_) => {
                            break 'connection LuxClose::frame(CLOSE_NORMAL, "client closed");
                        }
                        // A protocol-level ping is answered by the WebSocket implementation, and
                        // the pong is what keeps the idle deadline moving forward.
                        Message::Ping(_) | Message::Pong(_) => {}
                    }
                }
                Wake::User(result) => match user_wake(result) {
                    SubscriptionWake::Frame(Some(payload)) => {
                        if !send_text(&mut socket, &payload, &limits).await {
                            break 'connection LuxClose::dropped();
                        }
                    }
                    SubscriptionWake::Frame(None) => {}
                    SubscriptionWake::Closed => {
                        break 'connection LuxClose::frame(CLOSE_GOING_AWAY, "event hub closed");
                    }
                },
                Wake::Admin(result) => match admin_wake(result) {
                    // A demoted connection may still have one buffered admin event; it is not
                    // delivered any more (ADR-054 D7).
                    SubscriptionWake::Frame(_) if !can_manage_server => {}
                    SubscriptionWake::Frame(Some(payload)) => {
                        if !send_text(&mut socket, &payload, &limits).await {
                            break 'connection LuxClose::dropped();
                        }
                    }
                    SubscriptionWake::Frame(None) => {}
                    SubscriptionWake::Closed => admin_events = None,
                },
                // The timer pass at the top of the loop already did the work.
                Wake::Timer => {}
            }
        }
    };

    let close_code = match close {
        LuxClose::Frame { code, reason } => {
            let _ = send_bounded(
                &mut socket,
                Message::Close(Some(utf8_close(code, reason))),
                limits.send_timeout,
            )
            .await;
            Some(code)
        }
        LuxClose::Dropped => None,
    };
    // Releases the connection budget; must happen after the close frame was written.
    drop(permit);
    tracing::debug!(
        user = %user_id,
        connection,
        channel,
        close_code = ?close_code,
        "lux websocket connection closed"
    );
}

enum Wake {
    User(Result<UserEvent, broadcast::error::RecvError>),
    Admin(Result<AdminEventScope, broadcast::error::RecvError>),
    Inbound(Option<Result<Message, axum::Error>>),
    Shutdown,
    Timer,
}

/// Waits for the next admin event, or forever when the connection does not carry the admin
/// channel. The future never resolves in the latter case, so it cannot spin the select loop.
async fn recv_optional(
    receiver: &mut Option<broadcast::Receiver<AdminEventScope>>,
) -> Result<AdminEventScope, broadcast::error::RecvError> {
    match receiver {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

/// Logs a transient revalidation failure at most once per `auth_warn_interval`, so a flapping
/// database cannot turn one connection into a log flood. Never includes a credential.
fn warn_revalidation_unavailable(
    last_warning: &mut Option<Instant>,
    now: Instant,
    limits: &WebsocketLimits,
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
        "lux websocket session revalidation is unavailable; keeping the connection"
    );
}

async fn send_text(socket: &mut WebSocket, payload: &str, limits: &WebsocketLimits) -> bool {
    send_bounded(
        socket,
        Message::Text(Utf8Bytes::from(payload.to_owned())),
        limits.send_timeout,
    )
    .await
}

/// Whether the inbound text is the one recognized application message. Extra fields are ignored
/// and everything else (including unparsable JSON) is an unknown type.
fn is_application_ping(text: &str) -> bool {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|message| {
            message
                .get("type")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .as_deref()
        == Some("ping")
}

/// Maps one user-channel receive outcome to the frame to write.
///
/// A lagged subscriber may have missed invalidations of any scope, so it gets the full `home`
/// refresh instead (ADR-054 "下行"): the degraded frame carries no `itemId` because the scope is
/// no longer known.
fn user_wake(result: Result<UserEvent, broadcast::error::RecvError>) -> SubscriptionWake {
    match result {
        Ok(event) => SubscriptionWake::Frame(user_invalidation_frame(&event)),
        Err(broadcast::error::RecvError::Lagged(_)) => SubscriptionWake::Frame(Some(
            invalidate_frame("user", UserEventScope::Home.as_str(), None),
        )),
        Err(broadcast::error::RecvError::Closed) => SubscriptionWake::Closed,
    }
}

/// Maps one admin-channel receive outcome to the frame to write. A lagged subscriber degrades to
/// `all`, again without an `itemId` (the admin envelope never carries one).
fn admin_wake(result: Result<AdminEventScope, broadcast::error::RecvError>) -> SubscriptionWake {
    match result {
        Ok(scope) => SubscriptionWake::Frame(Some(invalidate_frame("admin", scope.as_str(), None))),
        Err(broadcast::error::RecvError::Lagged(_)) => SubscriptionWake::Frame(Some(
            invalidate_frame("admin", AdminEventScope::All.as_str(), None),
        )),
        Err(broadcast::error::RecvError::Closed) => SubscriptionWake::Closed,
    }
}

/// Renders one user invalidation. `None` means the whole frame is invalid (an over-long `itemId`),
/// so nothing is written.
fn user_invalidation_frame(event: &UserEvent) -> Option<String> {
    let item_id = match event.item_id() {
        Some(item_id) if item_id.chars().count() > MAX_ITEM_ID_CHARS => {
            // Never log the value itself: it comes from business data, not from the client.
            tracing::warn!(
                item_id_chars = item_id.chars().count(),
                "dropped a lux websocket invalidation with an over-long item id"
            );
            return None;
        }
        item_id => item_id,
    };
    Some(invalidate_frame("user", event.scope().as_str(), item_id))
}

/// The frozen envelope (ADR-054 D3). No `libraryId`, no user id, no payload: the frame only says
/// "re-read the API, which checks permissions again".
fn invalidate_frame(channel: &'static str, scope: &str, item_id: Option<&str>) -> String {
    #[derive(serde::Serialize)]
    struct Frame<'a> {
        #[serde(rename = "type")]
        kind: &'static str,
        channel: &'static str,
        scope: &'a str,
        #[serde(rename = "itemId", skip_serializing_if = "Option::is_none")]
        item_id: Option<&'a str>,
    }

    serde_json::to_string(&Frame {
        kind: "invalidate",
        channel,
        scope,
        item_id,
    })
    // A frame of three string slices cannot fail to serialize; the fallback keeps the function
    // total without a panic in a production path.
    .unwrap_or_else(|_| invalidate_frame_fallback(channel, scope))
}

fn invalidate_frame_fallback(channel: &'static str, scope: &str) -> String {
    format!(r#"{{"type":"invalidate","channel":"{channel}","scope":"{scope}"}}"#)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::application::admin_events::EVENT_CHANNEL_CAPACITY;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(
                *name,
                HeaderValue::from_str(value).expect("test header value"),
            );
        }
        headers
    }

    fn origin(headers: &HeaderMap) -> OriginCheck {
        check_origin(headers, &Uri::from_static("/api/v1/ws"))
    }

    #[test]
    fn origin_hosts_are_lower_cased_and_default_ports_are_dropped() {
        assert_eq!(
            parse_origin_authority("http://Example.COM:80").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            parse_origin_authority("HTTPS://Example.COM").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            parse_origin_authority("https://example.com:443").as_deref(),
            Some("example.com")
        );
        // A non-default port is kept on both sides and has to match.
        assert_eq!(
            parse_origin_authority("http://example.com:8097").as_deref(),
            Some("example.com:8097")
        );
        assert_eq!(
            parse_origin_authority("http://[::1]:8097").as_deref(),
            Some("[::1]:8097")
        );
        assert_eq!(
            normalize_authority("Example.COM:8097", &[80, 443]).as_deref(),
            Some("example.com:8097")
        );
        assert_eq!(
            normalize_authority("Example.COM:443", &[80, 443]).as_deref(),
            Some("example.com")
        );
        assert_eq!(
            normalize_authority("[::1]", &[80, 443]).as_deref(),
            Some("[::1]")
        );
    }

    #[test]
    fn anything_but_one_serialized_origin_is_missing() {
        for value in [
            "null",
            "NULL",
            "http://example.com/",
            "http://example.com/path",
            "http://example.com?q=1",
            "http://user@example.com",
            "http://example.com, http://evil.example",
            "ws://example.com",
            "example.com",
            "http://",
            "http://example.com:notaport",
            "http://example.com:99999",
            " http://example.com ",
        ] {
            assert_eq!(
                parse_origin_authority(value),
                None,
                "`{value}` is not a single serialized http(s) origin"
            );
        }
    }

    #[test]
    fn a_matching_origin_is_allowed_without_comparing_the_scheme() {
        // The request reaches luxd over plain HTTP, the browser page is HTTPS: the scheme is not
        // part of the comparison (ADR-054 "Origin 校验").
        let allowed = headers(&[
            ("origin", "https://example.com:8097"),
            ("host", "EXAMPLE.com:8097"),
            ("cookie", "lux_session=token"),
        ]);
        assert_eq!(origin(&allowed), OriginCheck::Allowed);
        let default_port = headers(&[("origin", "http://example.com:80"), ("host", "example.com")]);
        assert_eq!(origin(&default_port), OriginCheck::Allowed);
    }

    #[test]
    fn a_different_host_or_port_is_a_mismatch() {
        for (origin_value, host) in [
            ("https://evil.example", "example.com"),
            ("https://example.com", "example.com:8097"),
            ("https://example.com:8097", "example.com"),
            ("https://example.com", "evil.example"),
        ] {
            let request = headers(&[("origin", origin_value), ("host", host)]);
            assert_eq!(
                origin(&request),
                OriginCheck::Mismatch,
                "`{origin_value}` must not match host `{host}`"
            );
        }
        // A matching Origin with a *repeated* Origin header is treated as missing, not allowed.
        let mut repeated = headers(&[("origin", "https://example.com"), ("host", "example.com")]);
        repeated.append("origin", HeaderValue::from_static("https://example.com"));
        assert_eq!(origin(&repeated), OriginCheck::Missing);
    }

    #[test]
    fn a_missing_origin_rejects_cookie_sessions_and_allows_client_tokens() {
        let cookie = headers(&[("host", "example.com"), ("cookie", "lux_session=token")]);
        assert_eq!(origin(&cookie), OriginCheck::Missing);
        let token = headers(&[("host", "example.com"), ("x-lux-token", "token")]);
        assert_eq!(origin(&token), OriginCheck::Allowed);
        let bearer = headers(&[("host", "example.com"), ("authorization", "Bearer token")]);
        assert_eq!(origin(&bearer), OriginCheck::Allowed);
        let literal_null = headers(&[
            ("host", "example.com"),
            ("origin", "null"),
            ("cookie", "lux_session=token"),
        ]);
        assert_eq!(origin(&literal_null), OriginCheck::Missing);
    }

    #[test]
    fn cross_site_is_rejected_even_with_a_matching_origin() {
        let request = headers(&[
            ("origin", "https://example.com"),
            ("host", "example.com"),
            ("cookie", "lux_session=token"),
            ("sec-fetch-site", "cross-site"),
        ]);
        assert_eq!(origin(&request), OriginCheck::CrossSite);
        let same_origin = headers(&[
            ("origin", "https://example.com"),
            ("host", "example.com"),
            ("sec-fetch-site", "same-origin"),
        ]);
        assert_eq!(origin(&same_origin), OriginCheck::Allowed);
        // Forwarded headers never take part in the decision (rule 7).
        let forwarded = headers(&[
            ("origin", "https://evil.example"),
            ("host", "example.com"),
            ("x-forwarded-proto", "http"),
            ("x-forwarded-host", "evil.example"),
            ("x-real-ip", "203.0.113.9"),
        ]);
        assert_eq!(origin(&forwarded), OriginCheck::Mismatch);
    }

    #[test]
    fn only_the_documented_envelope_is_rendered() {
        assert_eq!(
            user_invalidation_frame(&UserEvent::broadcast(UserEventScope::Home)).as_deref(),
            Some(r#"{"type":"invalidate","channel":"user","scope":"home"}"#)
        );
        assert_eq!(
            user_invalidation_frame(
                &UserEvent::for_user(UserEventScope::Progress, "user-a").with_item_id("item-1")
            )
            .as_deref(),
            Some(r#"{"type":"invalidate","channel":"user","scope":"progress","itemId":"item-1"}"#)
        );
        let admin = invalidate_frame("admin", AdminEventScope::Jobs.as_str(), None);
        assert_eq!(
            admin,
            r#"{"type":"invalidate","channel":"admin","scope":"jobs"}"#
        );
        // `libraryId` is never part of the envelope, and the frame carries no scope other than the
        // frozen string.
        assert!(!admin.contains("libraryId"));
        assert!(!admin.contains("itemId"));
    }

    #[test]
    fn an_over_long_item_id_drops_the_whole_frame() {
        let short = UserEvent::for_user(UserEventScope::Progress, "user-a")
            .with_item_id("x".repeat(MAX_ITEM_ID_CHARS));
        assert!(user_invalidation_frame(&short).is_some());
        let long = UserEvent::for_user(UserEventScope::Progress, "user-a")
            .with_item_id("x".repeat(MAX_ITEM_ID_CHARS + 1));
        assert!(user_invalidation_frame(&long).is_none());
    }

    #[test]
    fn only_the_exact_ping_message_is_recognized() {
        assert!(is_application_ping(r#"{"type":"ping"}"#));
        assert!(is_application_ping(r#"{"type":"ping","extra":1}"#));
        assert!(!is_application_ping(r#"{"type":"PING"}"#));
        assert!(!is_application_ping(r#"{"type":"pong"}"#));
        assert!(!is_application_ping(
            r#"{"type":"subscribe","channels":["admin"]}"#
        ));
        assert!(!is_application_ping("not json"));
        assert!(!is_application_ping("[]"));
    }

    #[tokio::test]
    async fn a_lagged_user_subscriber_degrades_to_home_without_an_item_id() {
        let hub = crate::application::admin_events::UserEventHub::new();
        let mut subscriber = hub.subscribe_for("viewer");
        for _ in 0..(EVENT_CHANNEL_CAPACITY + 8) {
            hub.publish(UserEventScope::Progress);
        }
        let result = subscriber.recv().await;
        assert!(matches!(
            result,
            Err(broadcast::error::RecvError::Lagged(_))
        ));
        match user_wake(result) {
            SubscriptionWake::Frame(Some(frame)) => assert_eq!(
                frame,
                r#"{"type":"invalidate","channel":"user","scope":"home"}"#
            ),
            _ => panic!("a lagged subscriber must receive the home downgrade"),
        }
    }

    #[tokio::test]
    async fn a_lagged_admin_subscriber_degrades_to_all_without_an_item_id() {
        let hub = AdminEventHub::new();
        let mut receiver = hub.subscribe();
        for _ in 0..(EVENT_CHANNEL_CAPACITY + 8) {
            hub.publish(AdminEventScope::Jobs);
        }
        let result = receiver.recv().await;
        assert!(matches!(
            result,
            Err(broadcast::error::RecvError::Lagged(_))
        ));
        match admin_wake(result) {
            SubscriptionWake::Frame(Some(frame)) => assert_eq!(
                frame,
                r#"{"type":"invalidate","channel":"admin","scope":"all"}"#
            ),
            _ => panic!("a lagged subscriber must receive the all downgrade"),
        }
    }

    #[test]
    fn a_closed_hub_ends_the_subscription() {
        assert!(matches!(
            user_wake(Err(broadcast::error::RecvError::Closed)),
            SubscriptionWake::Closed
        ));
        assert!(matches!(
            admin_wake(Err(broadcast::error::RecvError::Closed)),
            SubscriptionWake::Closed
        ));
    }

    #[test]
    fn upgrade_detection_requires_both_tokens() {
        let websocket = headers(&[
            ("upgrade", "websocket"),
            ("connection", "keep-alive, Upgrade"),
        ]);
        assert!(is_websocket_upgrade(&websocket));
        let missing_connection = headers(&[("upgrade", "websocket")]);
        assert!(!is_websocket_upgrade(&missing_connection));
        let plain = headers(&[("connection", "keep-alive")]);
        assert!(!is_websocket_upgrade(&plain));
    }
}
