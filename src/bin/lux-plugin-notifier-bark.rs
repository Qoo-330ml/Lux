//! Bark push notification plugin (`org.lux.notifier.bark`).
//!
//! The Lux host owns the delivery queue, retries, persistence and logs; this process only turns one
//! `notification.send` request into one Bark `/push` call. The Bark device key is the per-destination
//! credential: it arrives exclusively through the RPC `secret` field and is never logged, never
//! returned in a result and never placed in a URL (Bark pushes carry it in the JSON body).
//!
//! Classification follows the host contract: `2xx` with `code == 200` is `DELIVERED`, a network
//! failure, a timeout, `429` and `5xx` are `RETRYABLE` (with `Retry-After` when Bark sends it), and
//! every other `4xx`, an invalid configuration and an upstream business error are `FAILED`.

use std::process::ExitCode;

use luxd::application::{
    notifier_plugin_sdk::{
        delivered, failed, outbound_http_client_from_env, render_event_text,
        result_from_http_status, retryable, run_notifier,
    },
    plugin_protocol::{NotificationSendRpcRequest, NotificationSendRpcResult},
};
use reqwest::{
    Client, Url,
    header::{HeaderMap, RETRY_AFTER},
};
use serde_json::{Map, Value, json};

/// Bark's public push server; used when a destination does not configure `serverUrl`.
const DEFAULT_SERVER_URL: &str = "https://api.day.app";
/// Upper bound of a Bark device key, matching the format Bark issues.
const MAX_DEVICE_KEY_CHARS: usize = 128;
/// Upper bound of the configured base URL and of an icon URL.
const MAX_URL_CHARS: usize = 2_048;
/// Upper bound of the short text settings (group, level, sound).
const MAX_TEXT_CHARS: usize = 64;
/// Upper bound of a Bark response body; the plugin only needs the small JSON status envelope.
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
/// The provider request identifier is informational and must stay bounded.
const MAX_PROVIDER_REQUEST_ID_CHARS: usize = 128;
/// The `level` values Bark accepts.
const BARK_LEVELS: [&str; 3] = ["active", "timeSensitive", "passive"];
/// The Bark success code inside a `2xx` response body.
const BARK_SUCCESS_CODE: i64 = 200;

#[tokio::main]
async fn main() -> ExitCode {
    let outcome = match outbound_http_client_from_env() {
        Ok(client) => {
            run_notifier(move |request| {
                let client = client.clone();
                async move { deliver(&client, request).await }
            })
            .await
        }
        Err(_) => {
            // A broken proxy configuration is a plugin-level failure: fail closed, never fall back
            // to a client without the host proxy policy.
            eprintln!("bark notifier: outbound HTTP client is unavailable");
            run_notifier(|_request| async { failed("client_unavailable") }).await
        }
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => {
            // The protocol loop failed on stdin/stdout. No request data is included on purpose.
            eprintln!("bark notifier: plugin protocol loop failed");
            ExitCode::FAILURE
        }
    }
}

/// Handles one `notification.send` request.
async fn deliver(
    client: &Client,
    request: NotificationSendRpcRequest,
) -> NotificationSendRpcResult {
    let device_key = match validate_device_key(request.secret.as_deref()) {
        Ok(device_key) => device_key,
        Err(code) => return rejected(code),
    };
    let target = match parse_target(&request.config) {
        Ok(target) => target,
        Err(error) => return rejected(error.code()),
    };
    let (title, body) = render_event_text(&request.event);
    let payload = push_payload(device_key, &target, title, body);

    let response = match client.post(&target.push_url).json(&payload).send().await {
        Ok(response) => response,
        // A connection error or the client timeout is always worth another host attempt.
        Err(_) => return retryable("provider_unavailable"),
    };
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let retry_after = retry_after_seconds(response.headers());
        eprintln!("bark notifier: upstream status {status}");
        return result_from_http_status(status, retry_after);
    }
    match read_bounded_body(response).await {
        Ok(body) => classify_push_body(&body),
        Err(error) => rejected(error.code()),
    }
}

/// A validated Bark destination.
#[derive(Clone, Debug, Eq, PartialEq)]
struct BarkTarget {
    push_url: String,
    group: Option<String>,
    level: &'static str,
    icon: Option<String>,
    sound: Option<String>,
}

/// A destination configuration the host must reject instead of retrying.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BarkConfigError {
    InvalidServerUrl,
    InvalidConfig,
}

impl BarkConfigError {
    fn code(self) -> &'static str {
        match self {
            Self::InvalidServerUrl => "invalid_server_url",
            Self::InvalidConfig => "invalid_config",
        }
    }
}

/// A response this plugin could not turn into a delivery classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BarkResponseError {
    Unavailable,
    ResponseTooLarge,
}

impl BarkResponseError {
    fn code(self) -> &'static str {
        match self {
            Self::Unavailable => "provider_unavailable",
            Self::ResponseTooLarge => "provider_response_too_large",
        }
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

/// Reads the non-secret destination settings from the host `providerConfig`.
fn parse_target(config: &Value) -> Result<BarkTarget, BarkConfigError> {
    let config = config.as_object().ok_or(BarkConfigError::InvalidConfig)?;
    let server_url = match config.get("serverUrl") {
        None | Some(Value::Null) => DEFAULT_SERVER_URL.to_owned(),
        Some(Value::String(value)) => value.trim().to_owned(),
        Some(_) => return Err(BarkConfigError::InvalidServerUrl),
    };
    let base_url = if server_url.is_empty() {
        DEFAULT_SERVER_URL.to_owned()
    } else {
        server_url
    };
    let base_url = validate_base_url(&base_url)?;
    let group = bounded_text(config, "group", MAX_TEXT_CHARS)?;
    let level = match bounded_text(config, "level", MAX_TEXT_CHARS)? {
        None => "active",
        Some(level) => BARK_LEVELS
            .into_iter()
            .find(|candidate| *candidate == level)
            .ok_or(BarkConfigError::InvalidConfig)?,
    };
    let icon = bounded_text(config, "icon", MAX_URL_CHARS)?
        .map(|icon| validate_icon_url(&icon))
        .transpose()?;
    let sound = bounded_text(config, "sound", MAX_TEXT_CHARS)?;
    Ok(BarkTarget {
        push_url: format!("{base_url}/push"),
        group,
        level,
        icon,
        sound,
    })
}

/// Reads one optional plain-text setting and bounds it.
fn bounded_text(
    config: &Map<String, Value>,
    key: &str,
    max_chars: usize,
) -> Result<Option<String>, BarkConfigError> {
    let value = match config.get(key) {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::String(value)) => value.trim(),
        Some(_) => return Err(BarkConfigError::InvalidConfig),
    };
    if value.is_empty() {
        return Ok(None);
    }
    if value.chars().count() > max_chars || value.chars().any(char::is_control) {
        return Err(BarkConfigError::InvalidConfig);
    }
    Ok(Some(value.to_owned()))
}

/// Validates the configured base URL and drops a trailing slash.
fn validate_base_url(raw: &str) -> Result<String, BarkConfigError> {
    if raw.chars().count() > MAX_URL_CHARS {
        return Err(BarkConfigError::InvalidServerUrl);
    }
    let url = Url::parse(raw).map_err(|_| BarkConfigError::InvalidServerUrl)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(BarkConfigError::InvalidServerUrl);
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

/// Validates an optional Bark icon URL.
fn validate_icon_url(raw: &str) -> Result<String, BarkConfigError> {
    if raw.chars().count() > MAX_URL_CHARS {
        return Err(BarkConfigError::InvalidConfig);
    }
    let url = Url::parse(raw).map_err(|_| BarkConfigError::InvalidConfig)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(BarkConfigError::InvalidConfig);
    }
    Ok(url.as_str().to_owned())
}

/// Builds the Bark `/push` JSON body. The device key travels here, never in the URL.
fn push_payload(device_key: &str, target: &BarkTarget, title: String, body: String) -> Value {
    let mut payload = Map::new();
    payload.insert("device_key".to_owned(), json!(device_key));
    payload.insert("title".to_owned(), json!(title));
    payload.insert("body".to_owned(), json!(body));
    payload.insert("level".to_owned(), json!(target.level));
    if let Some(group) = target.group.as_deref() {
        payload.insert("group".to_owned(), json!(group));
    }
    if let Some(icon) = target.icon.as_deref() {
        payload.insert("icon".to_owned(), json!(icon));
    }
    if let Some(sound) = target.sound.as_deref() {
        payload.insert("sound".to_owned(), json!(sound));
    }
    Value::Object(payload)
}

/// Reads at most [`MAX_RESPONSE_BYTES`] of a `2xx` response body.
async fn read_bounded_body(mut response: reqwest::Response) -> Result<Vec<u8>, BarkResponseError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(BarkResponseError::ResponseTooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| BarkResponseError::Unavailable)?
    {
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(BarkResponseError::ResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Maps the Bark JSON envelope onto a delivery classification.
fn classify_push_body(body: &[u8]) -> NotificationSendRpcResult {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return rejected("invalid_response");
    };
    let Some(code) = value.get("code").and_then(Value::as_i64) else {
        return rejected("invalid_response");
    };
    if code != BARK_SUCCESS_CODE {
        // Only the numeric code is propagated: the upstream message is untrusted free text.
        let code = format!("bark_code_{code}");
        return rejected(&code);
    }
    let mut result = delivered();
    result.provider_request_id = value
        .get("timestamp")
        .and_then(json_text)
        .filter(|timestamp| timestamp.chars().count() <= MAX_PROVIDER_REQUEST_ID_CHARS);
    result
}

/// Reads a short JSON scalar as text without trusting it as a number.
fn json_text(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

/// Parses an integer `Retry-After` header; the HTTP-date form is ignored.
fn retry_after_seconds(headers: &HeaderMap) -> Option<i64> {
    headers
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<i64>()
        .ok()
}

/// Logs a rejection with its stable code only.
fn rejected(code: &str) -> NotificationSendRpcResult {
    eprintln!("bark notifier: delivery rejected ({code})");
    failed(code)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        BarkConfigError, DEFAULT_SERVER_URL, parse_target, retry_after_seconds, validate_device_key,
    };

    #[test]
    fn normalizes_the_bark_base_url_and_defaults() {
        let target = parse_target(&json!({})).expect("the default target must be valid");
        assert_eq!(target.push_url, format!("{DEFAULT_SERVER_URL}/push"));
        assert_eq!(target.level, "active");
        assert_eq!(target.group, None);

        let target = parse_target(&json!({"serverUrl": " http://127.0.0.1:8080/bark/ "}))
            .expect("a trailing slash must be dropped");
        assert_eq!(target.push_url, "http://127.0.0.1:8080/bark/push");

        let target = parse_target(&json!({"serverUrl": "", "level": "passive"}))
            .expect("an empty serverUrl must fall back to the public server");
        assert_eq!(target.push_url, format!("{DEFAULT_SERVER_URL}/push"));
        assert_eq!(target.level, "passive");
    }

    #[test]
    fn rejects_unsafe_or_unknown_target_settings() {
        for config in [
            json!({"serverUrl": "ftp://example.invalid"}),
            json!({"serverUrl": "https://user:secret@example.invalid"}),
            json!({"serverUrl": "https://example.invalid/?token=abc"}),
            json!({"serverUrl": "https://example.invalid/#fragment"}),
            json!({"serverUrl": 42}),
        ] {
            assert_eq!(
                parse_target(&config),
                Err(BarkConfigError::InvalidServerUrl),
                "unexpected classification for {config}"
            );
        }
        for config in [
            json!({"level": "loud"}),
            json!({"group": "g".repeat(65)}),
            json!({"icon": "javascript:alert(1)"}),
            json!({"icon": "ftp://example.invalid/icon.png"}),
            json!({"sound": {"name": "bell"}}),
        ] {
            assert_eq!(
                parse_target(&config),
                Err(BarkConfigError::InvalidConfig),
                "unexpected classification for {config}"
            );
        }
    }

    #[test]
    fn validates_the_device_key_format() {
        assert_eq!(validate_device_key(None), Err("missing_secret"));
        assert_eq!(validate_device_key(Some("  ")), Err("missing_secret"));
        assert_eq!(
            validate_device_key(Some("device key")),
            Err("invalid_secret")
        );
        assert_eq!(
            validate_device_key(Some("device/key")),
            Err("invalid_secret")
        );
        assert_eq!(
            validate_device_key(Some(&"a".repeat(129))),
            Err("invalid_secret")
        );
        assert_eq!(
            validate_device_key(Some("device_key-1")),
            Ok("device_key-1")
        );
    }

    #[test]
    fn parses_only_integer_retry_after_headers() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::RETRY_AFTER,
            reqwest::header::HeaderValue::from_static(" 30 "),
        );
        assert_eq!(retry_after_seconds(&headers), Some(30));

        headers.insert(
            reqwest::header::RETRY_AFTER,
            reqwest::header::HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT"),
        );
        assert_eq!(retry_after_seconds(&headers), None);
    }
}
