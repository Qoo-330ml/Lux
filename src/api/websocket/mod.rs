//! Protocol-neutral WebSocket infrastructure shared by the realtime endpoints.
//!
//! ADR-054 D9 requires `/embywebsocket` and `/api/v1/ws` to share one implementation of the
//! connection budget, the shutdown broadcast, the bounded write boundary, the close frame
//! construction and the limits type, so the two endpoints cannot drift apart. Everything
//! protocol-specific (message formats, push schedules, credential revalidation) stays in the
//! endpoint module, and every endpoint owns its own budget instance and injects its own
//! [`WebsocketLimits`] (ADR-054 D8).

mod budget;
mod send;
mod shutdown;

pub(crate) use budget::{WebsocketConnections, WebsocketPermit};
pub(crate) use send::{
    CLOSE_GOING_AWAY, CLOSE_NORMAL, CLOSE_POLICY_VIOLATION, payload_fits, send_bounded, utf8_close,
};
pub(crate) use shutdown::WebsocketShutdown;

use std::time::Duration;

use tokio::sync::Semaphore;

/// Default upper bound on concurrently open connections of one budget instance for the whole
/// process.
pub(crate) const WEBSOCKET_DEFAULT_MAX_CONNECTIONS: usize = 32;
/// Default upper bound on concurrently open connections of one budget instance for a single
/// account.
pub(crate) const WEBSOCKET_DEFAULT_MAX_CONNECTIONS_PER_USER: usize = 4;
/// Default largest inbound frame accepted. Emby clients only send small JSON control messages.
pub(crate) const WEBSOCKET_DEFAULT_MAX_MESSAGE_BYTES: usize = 8 * 1024;
/// Lower bound a client-requested interval is clamped to by default.
pub(crate) const WEBSOCKET_DEFAULT_MIN_INTERVAL: Duration = Duration::from_secs(1);
/// Upper bound a client-requested interval is clamped to by default.
pub(crate) const WEBSOCKET_DEFAULT_MAX_INTERVAL: Duration = Duration::from_secs(60);
/// Default for a connection that sends nothing (not even a pong) for this long: it is closed.
pub(crate) const WEBSOCKET_DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
/// Default interval of the server-side ping that makes the idle timeout observable.
pub(crate) const WEBSOCKET_DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(25);
/// Default initial capacity of a connection's write buffer. It is only a capacity hint: a single
/// message larger than this is buffered up to [`WEBSOCKET_DEFAULT_MAX_WRITE_BUFFER_BYTES`] before it
/// is flushed, so this value must never be used as a document limit.
pub(crate) const WEBSOCKET_DEFAULT_WRITE_BUFFER_BYTES: usize = 16 * 1024;
/// Default hard cap of a connection's write buffer, and therefore the largest document one push may
/// have.
///
/// An Emby `Sessions` document grows with the listed sessions
/// (`MAX_LISTED_PLAYBACK_SESSIONS = 500` in `src/storage/sessions.rs`). One session entry measures
/// about 1.3-1.5 KiB with `NowPlayingItem`, `PlayState` and `TranscodingInfo`, so the worst case is
/// roughly 500 x 1.5 KiB = 750 KiB. 4 MiB leaves more than five times that headroom, so a busy
/// server never hits the cap, while a peer that stops reading still cannot grow the buffer past a
/// few MiB. Anything larger than the cap is skipped (see [`payload_fits`]) instead of closing the
/// connection, because Yamby reconnects without backoff.
pub(crate) const WEBSOCKET_DEFAULT_MAX_WRITE_BUFFER_BYTES: usize = 4 * 1024 * 1024;
/// Upper bound of the configurable initial write-buffer capacity. It is only a hint, so a
/// misconfigured override must not allocate a huge buffer per connection up front, and it must
/// stay below the hard cap by at least one byte (tungstenite asserts a strict inequality).
pub(crate) const WEBSOCKET_WRITE_BUFFER_BYTES_LIMIT: usize = 1024 * 1024;
/// Hard ceiling of the configurable connection budget. The default is 32; a thousand connections
/// already exceed any realistic deployment's descriptor and buffer budget, and
/// `Semaphore::MAX_PERMITS` (2^61-1) is not a limit an operator should be able to request.
pub(crate) const WEBSOCKET_MAX_CONNECTIONS_LIMIT: usize = 1024;
/// Hard ceiling of every configurable duration. `Instant::now() + Duration` panics when the sum
/// leaves the platform's `Instant` range, and that arithmetic runs inside the connection task, so
/// an override must stay far below the limit; 24 h is already longer than any state these
/// endpoints keep (the idle timeout is 120 s).
pub(crate) const WEBSOCKET_MAX_DURATION: Duration = Duration::from_secs(24 * 60 * 60);
/// Default bound of a single send. A send that cannot complete within it closes the connection. It
/// is far below the idle timeout so a stalled reader is reaped before the idle deadline would fire.
pub(crate) const WEBSOCKET_DEFAULT_SEND_TIMEOUT: Duration = Duration::from_secs(10);
/// Default token revalidation that runs even when no push stream is active, so a `…Stop` message
/// cannot keep a revoked token (or a disabled remote-access policy) alive.
pub(crate) const WEBSOCKET_DEFAULT_REVALIDATE_INTERVAL: Duration = Duration::from_secs(45);
/// Default minimum spacing of the "revalidation unavailable" warnings of one connection.
pub(crate) const WEBSOCKET_DEFAULT_AUTH_WARN_INTERVAL: Duration = Duration::from_secs(60);

/// Resource limits of one realtime endpoint. Every endpoint owns its own budget instance and
/// injects the limits of its deployment (ADR-054 D8); the tests shrink them so a limit is
/// observable in production time.
///
/// The defaults are the `/embywebsocket` values of ADR-054 (32 connections / 4 per user) because
/// that endpoint is the only channel with a documented budget today; an endpoint with another
/// budget (`/api/v1/ws` uses 128 user / 64 admin) overrides the fields it needs.
#[derive(Clone, Debug)]
pub struct WebsocketLimits {
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

impl Default for WebsocketLimits {
    fn default() -> Self {
        Self {
            max_connections: WEBSOCKET_DEFAULT_MAX_CONNECTIONS,
            max_connections_per_user: WEBSOCKET_DEFAULT_MAX_CONNECTIONS_PER_USER,
            min_interval: WEBSOCKET_DEFAULT_MIN_INTERVAL,
            max_interval: WEBSOCKET_DEFAULT_MAX_INTERVAL,
            max_message_bytes: WEBSOCKET_DEFAULT_MAX_MESSAGE_BYTES,
            idle_timeout: WEBSOCKET_DEFAULT_IDLE_TIMEOUT,
            heartbeat_interval: WEBSOCKET_DEFAULT_HEARTBEAT_INTERVAL,
            write_buffer_bytes: WEBSOCKET_DEFAULT_WRITE_BUFFER_BYTES,
            max_write_buffer_bytes: WEBSOCKET_DEFAULT_MAX_WRITE_BUFFER_BYTES,
            send_timeout: WEBSOCKET_DEFAULT_SEND_TIMEOUT,
            revalidate_interval: WEBSOCKET_DEFAULT_REVALIDATE_INTERVAL,
            auth_warn_interval: WEBSOCKET_DEFAULT_AUTH_WARN_INTERVAL,
        }
    }
}

impl WebsocketLimits {
    /// Keeps an override from creating an unusable schedule (no `min > max`, no zero-duration
    /// wake-up loop). A zero `max_connections` stays zero: it is a legal "refuse everything"
    /// configuration.
    pub(crate) fn normalized(mut self) -> Self {
        self.min_interval = self.min_interval.max(Duration::from_millis(10));
        self.max_interval = self.max_interval.max(self.min_interval);
        self.heartbeat_interval = self.heartbeat_interval.max(Duration::from_millis(10));
        self.idle_timeout = self.idle_timeout.max(self.heartbeat_interval);
        self.max_message_bytes = self.max_message_bytes.max(1024);
        // Every override is public API, so an extreme value must be normalised instead of panicking
        // inside the connection task: `Semaphore::new` panics above `MAX_PERMITS`, and
        // `Instant::now() + duration` panics for a duration near the platform maximum.
        self.max_connections = self
            .max_connections
            .min(WEBSOCKET_MAX_CONNECTIONS_LIMIT)
            .min(Semaphore::MAX_PERMITS);
        self.max_connections_per_user = self
            .max_connections_per_user
            .min(WEBSOCKET_MAX_CONNECTIONS_LIMIT);
        self.min_interval = self
            .min_interval
            .clamp(Duration::from_millis(10), WEBSOCKET_MAX_DURATION);
        self.max_interval = self
            .max_interval
            .clamp(self.min_interval, WEBSOCKET_MAX_DURATION);
        self.heartbeat_interval = self
            .heartbeat_interval
            .clamp(Duration::from_millis(10), WEBSOCKET_MAX_DURATION);
        self.idle_timeout = self
            .idle_timeout
            .clamp(self.heartbeat_interval, WEBSOCKET_MAX_DURATION);
        self.send_timeout = self
            .send_timeout
            .clamp(Duration::from_millis(10), WEBSOCKET_MAX_DURATION);
        self.revalidate_interval = self
            .revalidate_interval
            .clamp(Duration::from_millis(10), WEBSOCKET_MAX_DURATION);
        self.auth_warn_interval = self
            .auth_warn_interval
            .clamp(Duration::from_millis(10), WEBSOCKET_MAX_DURATION);
        // tungstenite asserts `max_write_buffer_size > write_buffer_size` right after the upgrade
        // (`WebSocketConfig::assert_valid`), and that assert runs inside the connection task: an
        // equal or smaller cap would panic there instead of failing the handshake. The initial
        // capacity is clamped so the strict inequality is always reachable, including for
        // `usize::MAX` overrides (saturating add keeps the arithmetic defined).
        self.write_buffer_bytes = self
            .write_buffer_bytes
            .clamp(1024, WEBSOCKET_WRITE_BUFFER_BYTES_LIMIT);
        self.max_write_buffer_bytes = self
            .max_write_buffer_bytes
            .max(self.write_buffer_bytes.saturating_add(1));
        self.send_timeout = self.send_timeout.max(Duration::from_millis(10));
        self.revalidate_interval = self.revalidate_interval.max(Duration::from_millis(10));
        self.auth_warn_interval = self.auth_warn_interval.max(Duration::from_millis(10));
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tokio::time::Instant;

    #[test]
    fn write_buffer_cap_is_always_strictly_greater_than_the_initial_capacity() {
        let equal = WebsocketLimits {
            write_buffer_bytes: 4096,
            max_write_buffer_bytes: 4096,
            ..WebsocketLimits::default()
        }
        .normalized();
        assert!(equal.max_write_buffer_bytes > equal.write_buffer_bytes);

        let smaller = WebsocketLimits {
            write_buffer_bytes: 4096,
            max_write_buffer_bytes: 1024,
            ..WebsocketLimits::default()
        }
        .normalized();
        assert!(smaller.max_write_buffer_bytes > smaller.write_buffer_bytes);

        let maxed = WebsocketLimits {
            write_buffer_bytes: usize::MAX,
            max_write_buffer_bytes: usize::MAX,
            ..WebsocketLimits::default()
        }
        .normalized();
        assert!(maxed.write_buffer_bytes <= WEBSOCKET_WRITE_BUFFER_BYTES_LIMIT);
        assert!(maxed.max_write_buffer_bytes > maxed.write_buffer_bytes);
    }

    #[test]
    fn the_default_cap_leaves_room_for_the_largest_session_document() {
        let limits = WebsocketLimits::default();
        // 500 listed sessions at ~1.5 KiB each is the documented worst case (~750 KiB).
        let worst_case_document = 500 * 1536;
        assert!(worst_case_document < limits.max_write_buffer_bytes);
        // The initial capacity is a hint and must stay well below the cap.
        assert!(limits.write_buffer_bytes < limits.max_write_buffer_bytes / 4);
    }

    #[test]
    fn extreme_overrides_are_clamped_before_they_can_panic() {
        let extreme = WebsocketLimits {
            max_connections: usize::MAX,
            max_connections_per_user: usize::MAX,
            min_interval: Duration::MAX,
            max_interval: Duration::MAX,
            max_message_bytes: usize::MAX,
            idle_timeout: Duration::MAX,
            heartbeat_interval: Duration::MAX,
            write_buffer_bytes: usize::MAX,
            max_write_buffer_bytes: usize::MAX,
            send_timeout: Duration::MAX,
            revalidate_interval: Duration::MAX,
            auth_warn_interval: Duration::MAX,
        }
        .normalized();
        assert!(extreme.max_connections <= WEBSOCKET_MAX_CONNECTIONS_LIMIT);
        assert!(extreme.max_connections <= Semaphore::MAX_PERMITS);
        assert!(extreme.max_connections_per_user <= WEBSOCKET_MAX_CONNECTIONS_LIMIT);
        for value in [
            extreme.min_interval,
            extreme.max_interval,
            extreme.idle_timeout,
            extreme.heartbeat_interval,
            extreme.send_timeout,
            extreme.revalidate_interval,
            extreme.auth_warn_interval,
        ] {
            assert!(value <= WEBSOCKET_MAX_DURATION);
        }
        // Neither the budget nor any deadline computation may panic after normalisation.
        let connections = WebsocketConnections::new(extreme.clone());
        let _ = connections.try_acquire("user");
        let now = Instant::now();
        let _ = now + extreme.idle_timeout;
        let _ = now + extreme.heartbeat_interval;
        let _ = now + extreme.revalidate_interval;
        let _ = now + extreme.min_interval;
        let _ = now + extreme.max_interval;
        let _ = now + extreme.send_timeout;
    }
}
