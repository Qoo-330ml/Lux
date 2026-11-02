//! Bark push notification plugin (`org.lux.notifier.bark`).
//!
//! The Lux host owns the delivery queue, retries, persistence and logs; this process only turns one
//! `notification.send` request into one Bark `/push` call. The Bark device key is the per-destination
//! credential: it arrives exclusively through the RPC `secret` field and is never logged, never
//! returned in a result and never placed in a URL (Bark pushes carry it in the JSON body).

use std::process::ExitCode;

use luxd::application::{
    notifier_plugin_sdk::{failed, run_notifier},
    plugin_protocol::{NotificationSendRpcRequest, NotificationSendRpcResult},
};

/// Upper bound of a Bark device key, matching the format Bark issues.
const MAX_DEVICE_KEY_CHARS: usize = 128;

#[tokio::main]
async fn main() -> ExitCode {
    match run_notifier(deliver).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => {
            // The protocol loop failed on stdin/stdout. No request data is included on purpose.
            eprintln!("bark notifier: plugin protocol loop failed");
            ExitCode::FAILURE
        }
    }
}

/// Handles one `notification.send` request.
async fn deliver(request: NotificationSendRpcRequest) -> NotificationSendRpcResult {
    match validate_device_key(request.secret.as_deref()) {
        // The push call lands in the next increment: every validated request fails closed until then.
        Ok(_device_key) => failed("push_not_implemented"),
        Err(code) => failed(code),
    }
}

/// Validates the Bark device key without ever echoing it.
fn validate_device_key(secret: Option<&str>) -> Result<&str, &'static str> {
    let Some(key) = secret else {
        return Err("missing_secret");
    };
    let key = key.trim();
    if key.is_empty() {
        return Err("missing_secret");
    }
    let valid = key.chars().count() <= MAX_DEVICE_KEY_CHARS
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
    if !valid {
        return Err("invalid_secret");
    }
    Ok(key)
}
