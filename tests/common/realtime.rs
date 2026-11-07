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
            .ok_or("the server closed before the ready frame")?;
        assert_eq!(
            ready,
            serde_json::json!({"type": "ready", "version": 1}),
            "the first frame is the frozen ready frame"
        );
        Ok(Self { socket })
    }

    /// The next `user`-channel invalidation, skipping frames of the `admin` channel.
    ///
    /// A manager connection carries both channels; these tests assert user-facing
    /// behavior, so an admin frame is not the frame they are waiting for.
    pub async fn next_user_invalidation(&mut self) -> Result<Value, String> {
        loop {
            let frame = next_frame(&mut self.socket, FRAME_DEADLINE)
                .await?
                .ok_or_else(|| {
                    "the realtime connection closed before an invalidation".to_owned()
                })?;
            if frame["type"] == "invalidate" && frame["channel"] == "user" {
                return Ok(frame);
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
    /// Admin frames, protocol pings and application pings are ignored: the
    /// assertion is about the user's media caches, not about the transport.
    pub async fn expect_user_silence(&mut self, window: Duration, message: &str) {
        let until = tokio::time::Instant::now() + window;
        loop {
            match next_frame_until(&mut self.socket, until).await {
                Ok(Some(frame)) if frame["type"] == "invalidate" && frame["channel"] == "user" => {
                    panic!("{message}; received: {frame}");
                }
                Ok(Some(_)) => continue,
                Ok(None) => return,
                Err(error) => panic!("{message}; received error: {error}"),
            }
        }
    }
}

/// The next protocol text frame before the deadline; `Ok(None)` on timeout or close.
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
            Ok(None) => return Ok(None),
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
                return Ok(None);
            }
            Ok(Some(Err(error))) => return Err(format!("websocket error: {error}")),
        }
    }
}
