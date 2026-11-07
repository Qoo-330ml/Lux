//! Contract tests for the shared realtime test client (`tests/common/realtime.rs`).
//!
//! The behavior tests read invalidations through that client, so it must not be
//! weaker than the SSE parser it replaced: these tests pin the frozen envelope
//! keys (`type`/`channel`/`scope`, plus `itemId` only on `progress`) and the rule
//! that a connection which ends is never mistaken for "silence".

use std::time::Duration;

use futures_util::SinkExt;
use serde_json::json;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

use crate::realtime::{RealtimeChannel, validate_admin_invalidation, validate_user_invalidation};

const READY_FRAME: &str = r#"{"type":"ready","version":1}"#;

/// Boots a throwaway websocket server that sends `ready`, then `frames`, and
/// either drops the connection or holds the socket open so a silence window can
/// elapse.
async fn spawn_test_server(
    frames: Vec<&'static str>,
    end_after_frames: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = tokio_tungstenite::accept_async(stream).await?;
        socket
            .send(Message::Text(READY_FRAME.to_owned().into()))
            .await?;
        for frame in frames {
            socket.send(Message::Text(frame.to_owned().into())).await?;
        }
        if end_after_frames {
            // Drop the TCP stream without a close handshake: this is the failure
            // mode that used to look like "no invalidation arrived".
            drop(socket);
        } else {
            // Longer than any silence window these tests use.
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    });
    Ok(format!("http://{address}"))
}

#[test]
fn scope_only_and_progress_user_frames_pass_the_envelope_check() {
    assert!(
        validate_user_invalidation(&json!({
            "type": "invalidate", "channel": "user", "scope": "home"
        }))
        .is_ok(),
        "a scope-only user frame has exactly the frozen keys"
    );
    assert!(
        validate_user_invalidation(&json!({
            "type": "invalidate",
            "channel": "user",
            "scope": "progress",
            "itemId": "item-1"
        }))
        .is_ok(),
        "progress may carry an itemId"
    );
}

#[test]
fn an_extra_key_on_a_non_progress_user_frame_is_rejected() {
    let error = validate_user_invalidation(&json!({
        "type": "invalidate",
        "channel": "user",
        "scope": "home",
        "detail": 1
    }))
    .expect_err("a non-progress frame must not carry business payload keys");
    assert!(
        error.contains("detail"),
        "the error should name the offending key: {error}"
    );
}

#[test]
fn item_id_is_only_allowed_on_progress_frames() {
    let error = validate_user_invalidation(&json!({
        "type": "invalidate",
        "channel": "user",
        "scope": "item",
        "itemId": "item-1"
    }))
    .expect_err("a non-progress frame must not carry itemId");
    assert!(error.contains("itemId"), "{error}");
}

#[test]
fn admin_frames_reject_item_id_library_id_and_extra_keys() {
    assert!(
        validate_admin_invalidation(&json!({
            "type": "invalidate", "channel": "admin", "scope": "jobs"
        }))
        .is_ok()
    );
    for frame in [
        json!({"type": "invalidate", "channel": "admin", "scope": "jobs", "itemId": "item-1"}),
        json!({"type": "invalidate", "channel": "admin", "scope": "jobs", "libraryId": "library-1"}),
        json!({"type": "invalidate", "channel": "admin", "scope": "jobs", "detail": 1}),
    ] {
        assert!(
            validate_admin_invalidation(&frame).is_err(),
            "the admin envelope is exactly type/channel/scope: {frame}"
        );
    }
    assert!(
        validate_user_invalidation(&json!({
            "type": "invalidate", "channel": "user", "scope": "home", "libraryId": "library-1"
        }))
        .is_err(),
        "the user envelope never carries libraryId either"
    );
}

#[tokio::test]
async fn a_user_frame_with_an_extra_key_fails_the_scope_reader() {
    let base = spawn_test_server(
        vec![r#"{"type":"invalidate","channel":"user","scope":"home","detail":1}"#],
        true,
    )
    .await
    .expect("test server");
    let mut channel = RealtimeChannel::connect(&base, "lux_session=test")
        .await
        .expect("connect");

    let error = channel
        .next_user_scope()
        .await
        .expect_err("an extra key must fail the reader, not be ignored");
    assert!(error.contains("detail"), "{error}");
}

#[tokio::test]
async fn a_closed_connection_fails_the_silence_assertion() {
    let base = spawn_test_server(Vec::new(), true)
        .await
        .expect("test server");
    let mut channel = RealtimeChannel::connect(&base, "lux_session=test")
        .await
        .expect("connect");

    let error = channel
        .expect_user_silence(
            Duration::from_secs(5),
            "a closed viewer connection must never pass a privacy assertion",
        )
        .await
        .expect_err("a closed connection is not silence");
    assert!(
        error.contains("closed") || error.contains("ended"),
        "a closed connection must be reported, not treated as silence: {error}"
    );
}

#[tokio::test]
async fn a_quiet_connection_passes_the_silence_assertion() {
    let base = spawn_test_server(Vec::new(), false)
        .await
        .expect("test server");
    let mut channel = RealtimeChannel::connect(&base, "lux_session=test")
        .await
        .expect("connect");

    channel
        .expect_user_silence(Duration::from_millis(100), "no frame is expected")
        .await
        .expect("a deadline without frames is silence");
}
