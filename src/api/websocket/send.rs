//! Bounded writes and close frame construction.

use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, Utf8Bytes, WebSocket};

use super::WebsocketLimits;

/// Close code of a WebSocket policy violation (RFC 6455 §7.4.1).
pub(crate) const CLOSE_POLICY_VIOLATION: u16 = 1008;
/// Close code of an endpoint that is going away.
pub(crate) const CLOSE_GOING_AWAY: u16 = 1001;
/// Close code of a normal closure.
pub(crate) const CLOSE_NORMAL: u16 = 1000;

/// Bytes reserved for the WebSocket framing of one outbound message.
///
/// tungstenite charges `frame.len()` (payload **plus** header) against
/// `max_write_buffer_size`, and a control frame (a pong answered by the runtime, or a ping we just
/// queued) can occupy the same buffer. A server-to-client frame uses 2 header bytes up to 125
/// payload bytes, 4 up to 65535 and 10 beyond that; 16 covers the largest header plus a small
/// control frame, so a payload that passes [`payload_fits`] can never make `start_send` fail with
/// `WriteBufferFull` and close the connection.
pub(crate) const WEBSOCKET_FRAME_HEADER_BYTES: usize = 16;

/// Runs one send under a hard time bound. Returns `false` when the peer is too slow (or the
/// socket is broken), which is the caller's signal to close the connection.
pub(crate) async fn bounded<F>(send: F, timeout: Duration) -> bool
where
    F: std::future::Future<Output = Result<(), axum::Error>>,
{
    matches!(tokio::time::timeout(timeout, send).await, Ok(Ok(())))
}

pub(crate) async fn send_bounded(
    socket: &mut WebSocket,
    message: Message,
    timeout: Duration,
) -> bool {
    bounded(socket.send(message), timeout).await
}

/// Whether one serialized document fits into the write buffer.
///
/// A document at or above the cap would make tungstenite reject the frame (`WriteBufferFull`) and
/// force the connection to close, so the push is skipped instead: the peer keeps its connection
/// and the next schedule retries with a document that may have shrunk.
pub(crate) fn payload_fits(payload: &str, limits: &WebsocketLimits) -> bool {
    // The frame header and any buffered control frame count against the same cap in tungstenite,
    // so the comparison reserves that margin instead of assuming an empty buffer.
    payload.len().saturating_add(WEBSOCKET_FRAME_HEADER_BYTES) < limits.max_write_buffer_bytes
}

pub(crate) fn utf8_close(code: u16, reason: &str) -> CloseFrame {
    CloseFrame {
        code,
        reason: Utf8Bytes::from(reason),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tokio::time::Instant;

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
    fn a_document_at_or_above_the_cap_is_not_sent() {
        let limits = WebsocketLimits {
            write_buffer_bytes: 1024,
            max_write_buffer_bytes: 4096,
            ..WebsocketLimits::default()
        }
        .normalized();
        let max = limits.max_write_buffer_bytes;
        // The frame header (and a possible buffered control frame) is reserved: one byte below the
        // margin still fits, the boundary itself is skipped, and anything larger is skipped too.
        assert!(payload_fits(
            &"x".repeat(max - WEBSOCKET_FRAME_HEADER_BYTES - 1),
            &limits
        ));
        assert!(!payload_fits(
            &"x".repeat(max - WEBSOCKET_FRAME_HEADER_BYTES),
            &limits
        ));
        assert!(!payload_fits(&"x".repeat(max - 1), &limits));
        assert!(!payload_fits(&"x".repeat(max), &limits));
        assert!(!payload_fits(&"x".repeat(64 * 1024), &limits));
    }
}
