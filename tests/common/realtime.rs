//! A minimal `/api/v1/ws` client for tests that observe realtime invalidations.
//!
//! LUX-897 deleted the SSE endpoints, so the behavior tests that used to parse
//! `text/event-stream` frames now read the same invalidations from the WebSocket
//! channel. Keeping one small client here avoids a copy in every module and keeps
//! the assertions about the frozen envelope (`ready` / `invalidate`) in one place.
//!
//! Included by the `admin` and `playback` integration targets.

#![allow(dead_code)]

use std::time::Duration;

use futures_util::StreamExt;
use serde_json::Value;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{
        Error as WsError, Message,
        client::IntoClientRequest,
        http::{HeaderValue, Request},
    },
};

type ClientSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// How long a test waits for an invalidation before failing.
const FRAME_DEADLINE: Duration = Duration::from_secs(10);

/// Largest `itemId` the server may put in a frame (ADR-054 D3).
const MAX_ITEM_ID_CHARS: usize = 256;

/// One open realtime connection carrying the `user` channel (and the `admin`
/// channel when the handshake principal manages the server).
pub struct RealtimeChannel {
    socket: ClientSocket,
}

impl RealtimeChannel {
    /// Handshakes as the account behind `cookies` against `base` (`http://host:port`).
    ///
    /// The `Origin` header mirrors the page origin because a cookie handshake
    /// without it is refused by design (ADR-054 D5); `cookies` is the raw
    /// `lux_session=…; lux_csrf=…` string the browser would send.
    pub async fn connect(base: &str, cookies: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let ws_base = base.replacen("http://", "ws://", 1);
        let mut request: Request<()> = format!("{ws_base}/api/v1/ws").into_client_request()?;
        request
            .headers_mut()
            .insert("origin", HeaderValue::from_str(base)?);
        request
            .headers_mut()
            .insert("cookie", HeaderValue::from_str(cookies)?);
        let (mut socket, response) = tokio_tungstenite::connect_async(request).await?;
        assert_eq!(
            response.status().as_u16(),
            101,
            "the realtime handshake must upgrade, not answer {}",
            response.status()
        );
        let ready = next_frame(&mut socket, FRAME_DEADLINE)
            .await
            .map_err(|error| format!("no ready frame: {error}"))?
            .ok_or("the server sent no ready frame")?;
        assert_eq!(
            ready,
            serde_json::json!({"type": "ready", "version": 1}),
            "the first frame is the frozen ready frame"
        );
        Ok(Self { socket })
    }

    /// The next `user`-channel invalidation, skipping validated `admin` frames.
    ///
    /// A manager connection carries both channels; these tests assert user-facing
    /// behavior, so an admin frame is not the frame they are waiting for. Every
    /// frame that does arrive is checked against the frozen envelope, which keeps
    /// the assertions the old SSE parser made (`scope` only, plus `itemId` on
    /// `progress`) even though the envelope now also names `type` and `channel`.
    pub async fn next_user_invalidation(&mut self) -> Result<Value, String> {
        loop {
            let frame = next_frame(&mut self.socket, FRAME_DEADLINE)
                .await?
                .ok_or_else(|| format!("no user invalidation within {FRAME_DEADLINE:?}"))?;
            match frame["type"].as_str() {
                Some("invalidate") => match frame["channel"].as_str() {
                    Some("user") => {
                        validate_user_invalidation(&frame)?;
                        return Ok(frame);
                    }
                    Some("admin") => validate_admin_invalidation(&frame)?,
                    other => {
                        return Err(format!(
                            "an invalidation with an unknown channel {other:?}: {frame}"
                        ));
                    }
                },
                other => {
                    return Err(format!(
                        "an unexpected frame type {other:?} after ready: {frame}"
                    ));
                }
            }
        }
    }

    /// The `scope` of the next `user`-channel invalidation.
    pub async fn next_user_scope(&mut self) -> Result<String, String> {
        let frame = self.next_user_invalidation().await?;
        frame["scope"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("an invalidation without a scope: {frame}"))
    }

    /// Asserts that no `user`-channel invalidation arrives within `window`.
    ///
    /// Only the deadline passing is silence. Admin frames, protocol pings and
    /// application pings are ignored, but a closed connection or an IO error is
    /// **not** silence: the privacy assertions would otherwise pass because the
    /// viewer's socket died instead of because no event was sent.
    pub async fn expect_user_silence(
        &mut self,
        window: Duration,
        message: &str,
    ) -> Result<(), String> {
        let until = tokio::time::Instant::now() + window;
        loop {
            match next_frame_until(&mut self.socket, until).await {
                Ok(Some(frame)) if is_user_invalidation(&frame) => {
                    return Err(format!("{message}; received: {frame}"));
                }
                Ok(Some(_)) => continue,
                Ok(None) => return Ok(()),
                Err(error) => {
                    return Err(format!(
                        "{message}; the connection ended while waiting: {error}"
                    ));
                }
            }
        }
    }
}

/// Whether a frame is a `user`-channel invalidation.
///
/// Frames arrive from the server, which the envelope validator checks; this
/// predicate only decides which frames the silence assertion reacts to.
pub(crate) fn is_user_invalidation(frame: &Value) -> bool {
    frame["type"] == "invalidate" && frame["channel"] == "user"
}

/// Checks a `user`-channel invalidation against the frozen envelope (ADR-054 D3).
///
/// Every `invalidate` frame has exactly `type`, `channel` and `scope`. Only a
/// `progress` frame may add `itemId` (at most 256 characters); no frame ever
/// carries `libraryId`, a user id or any business payload.
pub(crate) fn validate_user_invalidation(frame: &Value) -> Result<(), String> {
    let object = validate_invalidation_object(frame, "user")?;
    let scope = frame["scope"]
        .as_str()
        .ok_or_else(|| format!("an invalidation without a string scope: {frame}"))?;
    let allows_item_id = scope == "progress";
    for key in object.keys() {
        match key.as_str() {
            "type" | "channel" | "scope" => {}
            "itemId" if allows_item_id => {
                let item_id = frame["itemId"].as_str().ok_or_else(|| {
                    format!("a progress invalidation with a non-string itemId: {frame}")
                })?;
                if item_id.chars().count() > MAX_ITEM_ID_CHARS {
                    return Err(format!(
                        "an itemId longer than {MAX_ITEM_ID_CHARS} characters: {frame}"
                    ));
                }
            }
            "itemId" => {
                return Err(format!(
                    "only a progress invalidation may carry itemId: {frame}"
                ));
            }
            "libraryId" => {
                return Err(format!("the envelope never carries libraryId: {frame}"));
            }
            other => {
                return Err(format!(
                    "an unexpected key `{other}` in a user invalidation: {frame}"
                ));
            }
        }
    }
    Ok(())
}

/// Checks an `admin`-channel invalidation against the frozen envelope (ADR-054 D3).
///
/// The admin envelope is exactly `type`, `channel` and `scope`: a lagged admin
/// subscriber degrades to `all` without an `itemId`, and `libraryId` never exists.
pub(crate) fn validate_admin_invalidation(frame: &Value) -> Result<(), String> {
    let object = validate_invalidation_object(frame, "admin")?;
    for key in object.keys() {
        match key.as_str() {
            "type" | "channel" | "scope" => {}
            "itemId" => {
                return Err(format!(
                    "an admin invalidation must not carry itemId: {frame}"
                ));
            }
            "libraryId" => {
                return Err(format!("the envelope never carries libraryId: {frame}"));
            }
            other => {
                return Err(format!(
                    "an unexpected key `{other}` in an admin invalidation: {frame}"
                ));
            }
        }
    }
    Ok(())
}

/// Shared shape checks for both channels; answers the JSON object on success.
fn validate_invalidation_object<'a>(
    frame: &'a Value,
    channel: &str,
) -> Result<&'a serde_json::Map<String, Value>, String> {
    let object = frame
        .as_object()
        .ok_or_else(|| format!("an invalidation must be a JSON object: {frame}"))?;
    if frame["type"] != "invalidate" {
        return Err(format!("an unexpected frame type: {frame}"));
    }
    if frame["channel"] != channel {
        return Err(format!("an invalidation on the wrong channel: {frame}"));
    }
    let scope = frame["scope"]
        .as_str()
        .ok_or_else(|| format!("an invalidation without a string scope: {frame}"))?;
    if scope.is_empty() {
        return Err(format!("an invalidation with an empty scope: {frame}"));
    }
    Ok(object)
}

/// The next protocol text frame before the deadline; `Ok(None)` only on timeout.
///
/// A closed connection, an IO/protocol error or a server close frame is an
/// `Err`: callers must not mistake the transport ending for "no events".
async fn next_frame(
    socket: &mut ClientSocket,
    deadline: Duration,
) -> Result<Option<Value>, String> {
    next_frame_until(socket, tokio::time::Instant::now() + deadline).await
}

async fn next_frame_until(
    socket: &mut ClientSocket,
    until: tokio::time::Instant,
) -> Result<Option<Value>, String> {
    loop {
        let remaining = until.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Ok(None);
        }
        match tokio::time::timeout(remaining, socket.next()).await {
            Err(_) => return Ok(None),
            Ok(None) => {
                return Err("the server closed the websocket stream".to_owned());
            }
            Ok(Some(Ok(Message::Text(text)))) => {
                let frame = serde_json::from_str::<Value>(text.as_ref())
                    .map_err(|error| format!("server sent a non-JSON text frame: {error}"))?;
                return Ok(Some(frame));
            }
            Ok(Some(Ok(
                Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_),
            ))) => continue,
            Ok(Some(Ok(Message::Close(frame)))) => {
                let code = frame.as_ref().map(|frame| u16::from(frame.code));
                return Err(format!(
                    "the server closed the realtime connection: {code:?}"
                ));
            }
            Ok(Some(Err(WsError::ConnectionClosed | WsError::Protocol(_) | WsError::Io(_)))) => {
                return Err("the websocket connection ended or failed".to_owned());
            }
            Ok(Some(Err(error))) => return Err(format!("websocket error: {error}")),
        }
    }
}
