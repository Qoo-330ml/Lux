//! Shared runtime for Lux notification plugins.
//!
//! A notification plugin is a separate process that speaks the Lux stdio JSON-RPC protocol: the
//! host writes one [`PluginRequest`] per line and reads one [`PluginResponse`] per line. Lux keeps
//! every cross-cutting concern here so a channel plugin (Bark, WeCom, Feishu, SMTP, Webhook, ...)
//! only owns provider-specific authentication and payload building:
//!
//! - [`run_notifier`] runs the stdin/stdout loop and dispatches `notification.send`.
//! - [`outbound_http_client`] builds the outbound client with the host's proxy policy, a 10 second
//!   timeout and no redirects.
//! - [`classify_http_status`] maps an upstream status onto `DELIVERED`, `RETRYABLE` or `FAILED`.
//! - [`render_event_text`] turns the already desensitized Lux event into the Chinese title and body
//!   a channel needs, without ever reading secrets, local paths or complete URLs.
//!
//! The event, target and config values arrive through the RPC request; the optional `secret` is the
//! per-destination credential the host stores outside `providerConfig`. A plugin must not try to
//! read the Lux config directory: the host starts notification plugins without `LUX_CONFIG_DIR`.

use std::{future::Future, io, time::Duration};

use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, BufWriter};

use crate::{
    application::{
        notification_template,
        plugin_protocol::{
            NOTIFICATION_SEND_METHOD, NotificationSendRpcRequest, NotificationSendRpcResult,
            NotificationSendStatus, PluginRequest, PluginResponse, PluginRpcError,
        },
    },
    network,
};

/// Timeout for every outbound provider request. The host already bounds a delivery attempt, so the
/// plugin must fail inside the same window and let the host own retries.
pub const NOTIFIER_HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// Environment variable carrying the normalized Lux outbound proxy.
pub const NOTIFIER_PROXY_URL_ENV: &str = network::PROXY_URL_ENV;
/// Upper bound of the rendered notification title.
pub const MAX_NOTIFIER_TITLE_CHARS: usize = 256;
/// Upper bound of the rendered notification body.
pub const MAX_NOTIFIER_BODY_CHARS: usize = 4_096;
const PLUGIN_INVALID_REQUEST: &str = "PLUGIN_INVALID_REQUEST";
const PLUGIN_UNSUPPORTED_METHOD: &str = "PLUGIN_UNSUPPORTED_METHOD";
const PLUGIN_INTERNAL_ERROR: &str = "PLUGIN_INTERNAL_ERROR";

/// Event fields the fallback renderer may use.
///
/// The list mirrors the host event allowlist minus the fields a channel must not receive: remote
/// addresses, free-form overview text, media source identifiers and anything that could carry a
/// local path or a complete external URL.
const EVENT_TEXT_FIELDS: [&str; 40] = [
    "addedCount",
    "bitrate",
    "candidateCount",
    "client",
    "clientVersion",
    "container",
    "deletedFileCount",
    "deviceName",
    "deviceType",
    "durationSeconds",
    "durationTicks",
    "errorCode",
    "indexNumber",
    "isPaused",
    "itemId",
    "itemName",
    "itemNameGrandparent",
    "itemNameParent",
    "itemTitle",
    "itemType",
    "jobId",
    "jobType",
    "libraryId",
    "libraryName",
    "mode",
    "parentIndexNumber",
    "playMethod",
    "positionTicks",
    "processedCount",
    "removedCount",
    "resumed",
    "seasonNumber",
    "seriesSeasonCount",
    "seriesTitle",
    "size",
    "state",
    "status",
    "test",
    "totalCount",
    "userName",
];

#[derive(Debug)]
pub enum NotifierSdkError {
    Io(io::Error),
    InvalidProxy,
    Client(String),
}

impl std::fmt::Display for NotifierSdkError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "notifier SDK IO error: {error}"),
            Self::InvalidProxy => {
                formatter.write_str("notifier outbound proxy configuration is invalid")
            }
            Self::Client(message) => {
                write!(formatter, "notifier HTTP client is unavailable: {message}")
            }
        }
    }
}

impl std::error::Error for NotifierSdkError {}

impl From<io::Error> for NotifierSdkError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Runs the notification plugin loop over stdin/stdout.
///
/// `handler` receives the validated `notification.send` request and must return the delivery
/// classification. Build it once around a reusable client:
///
/// ```ignore
/// let client = outbound_http_client_from_env()?;
/// run_notifier(|request| {
///     let client = client.clone();
///     async move { send(&client, request).await }
/// })
/// .await?;
/// ```
pub async fn run_notifier<F, Fut>(handler: F) -> io::Result<()>
where
    F: FnMut(NotificationSendRpcRequest) -> Fut,
    Fut: Future<Output = NotificationSendRpcResult>,
{
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    run_notifier_session(stdin, stdout, handler).await
}

/// Runs the notification plugin loop over arbitrary streams.
///
/// The host protocol is line-delimited JSON: an empty line is ignored, a malformed line produces an
/// error response for id `""` instead of stopping the plugin, and `plugin.shutdown` ends the loop.
pub async fn run_notifier_session<R, W, F, Fut>(
    reader: R,
    writer: W,
    mut handler: F,
) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    F: FnMut(NotificationSendRpcRequest) -> Fut,
    Fut: Future<Output = NotificationSendRpcResult>,
{
    let mut lines = BufReader::new(reader).lines();
    let mut output = BufWriter::new(writer);
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let (response, is_shutdown) = handle_line(&line, &mut handler).await;
        write_response(&mut output, &response).await?;
        if is_shutdown {
            break;
        }
    }
    Ok(())
}

async fn handle_line<F, Fut>(line: &str, handler: &mut F) -> (PluginResponse, bool)
where
    F: FnMut(NotificationSendRpcRequest) -> Fut,
    Fut: Future<Output = NotificationSendRpcResult>,
{
    let request: PluginRequest = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(error) => {
            return (
                error_response(
                    "",
                    PLUGIN_INVALID_REQUEST,
                    format!("invalid plugin request: {error}"),
                ),
                false,
            );
        }
    };
    match request.method.as_str() {
        NOTIFICATION_SEND_METHOD => {
            let Ok(params) = serde_json::from_value::<NotificationSendRpcRequest>(request.params)
            else {
                return (
                    error_response(
                        &request.id,
                        PLUGIN_INVALID_REQUEST,
                        "notification send request is invalid".to_owned(),
                    ),
                    false,
                );
            };
            let result = handler(params).await;
            match serde_json::to_value(result) {
                Ok(result) => (
                    PluginResponse {
                        id: request.id,
                        result: Some(result),
                        error: None,
                    },
                    false,
                ),
                Err(error) => (
                    error_response(
                        &request.id,
                        PLUGIN_INTERNAL_ERROR,
                        format!("notification result is invalid: {error}"),
                    ),
                    false,
                ),
            }
        }
        "plugin.shutdown" => (
            PluginResponse {
                id: request.id,
                result: Some(json!({"accepted": true})),
                error: None,
            },
            true,
        ),
        _ => (
            error_response(
                &request.id,
                PLUGIN_UNSUPPORTED_METHOD,
                "unsupported plugin method".to_owned(),
            ),
            false,
        ),
    }
}

fn error_response(id: &str, code: &str, message: String) -> PluginResponse {
    PluginResponse {
        id: id.to_owned(),
        result: None,
        error: Some(PluginRpcError {
            code: code.to_owned(),
            message,
        }),
    }
}

async fn write_response<W: AsyncWrite + Unpin>(
    output: &mut BufWriter<W>,
    response: &PluginResponse,
) -> io::Result<()> {
    let mut line = serde_json::to_vec(response).map_err(io::Error::other)?;
    line.push(b'\n');
    output.write_all(&line).await?;
    output.flush().await
}

/// Builds the outbound client used by notification plugins.
///
/// The client honors the Lux proxy policy ([`NOTIFIER_PROXY_URL_ENV`], or `proxy_url` when the host
/// supplies one), never follows redirects and gives every request [`NOTIFIER_HTTP_TIMEOUT`].
pub fn outbound_http_client(proxy_url: Option<&str>) -> Result<reqwest::Client, NotifierSdkError> {
    let builder = network::client_builder_from_env_or(proxy_url)
        .map_err(|_| NotifierSdkError::InvalidProxy)?;
    builder
        .timeout(NOTIFIER_HTTP_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("lux-notifier-sdk/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|error| NotifierSdkError::Client(error.to_string()))
}

/// Builds the outbound client from the plugin process environment.
pub fn outbound_http_client_from_env() -> Result<reqwest::Client, NotifierSdkError> {
    let proxy_url = network::proxy_url_from_env().map_err(|_| NotifierSdkError::InvalidProxy)?;
    outbound_http_client(proxy_url.as_deref())
}

/// Classifies an upstream HTTP status exactly like the Lux delivery host.
///
/// `2xx` is delivered, `408`, `425`, `429` and `5xx` are retryable, and every other status is a
/// permanent failure caused by the destination configuration or the request content.
pub fn classify_http_status(status: u16) -> NotificationSendStatus {
    match status {
        200..=299 => NotificationSendStatus::Delivered,
        408 | 425 | 429 | 500..=599 => NotificationSendStatus::Retryable,
        _ => NotificationSendStatus::Failed,
    }
}

/// Builds the RPC result for an upstream HTTP status.
pub fn result_from_http_status(
    status: u16,
    retry_after_seconds: Option<i64>,
) -> NotificationSendRpcResult {
    let classified = classify_http_status(status);
    let mut result = match classified {
        NotificationSendStatus::Delivered => delivered(),
        NotificationSendStatus::Retryable => retryable(&format!("PROVIDER_HTTP_{status}")),
        NotificationSendStatus::Failed => failed(&format!("PROVIDER_HTTP_{status}")),
    };
    if classified == NotificationSendStatus::Retryable {
        result.retry_after_seconds = retry_after_seconds.map(|seconds| seconds.clamp(0, 7_200));
    }
    result
}

/// A delivered notification.
pub fn delivered() -> NotificationSendRpcResult {
    NotificationSendRpcResult {
        status: NotificationSendStatus::Delivered,
        provider_request_id: None,
        retry_after_seconds: None,
        error_code: None,
    }
}

/// A transient provider failure the host may retry.
pub fn retryable(error_code: &str) -> NotificationSendRpcResult {
    NotificationSendRpcResult {
        status: NotificationSendStatus::Retryable,
        provider_request_id: None,
        retry_after_seconds: None,
        error_code: Some(error_code.to_owned()),
    }
}

/// A permanent provider failure the host must not retry.
pub fn failed(error_code: &str) -> NotificationSendRpcResult {
    NotificationSendRpcResult {
        status: NotificationSendStatus::Failed,
        provider_request_id: None,
        retry_after_seconds: None,
        error_code: Some(error_code.to_owned()),
    }
}

/// Renders the notification title and body in Chinese for a channel that only sends text.
///
/// The Lux core generates `title`, `content` and `body` before an event enters the delivery queue;
/// those values are forwarded unchanged because a plugin must not rewrite the notification text.
/// When an event arrives without them the SDK renders a bounded Chinese fallback from the event
/// allowlist only, so a credential, a local path, a complete external URL or a remote address can
/// never reach a channel through this helper.
pub fn render_event_text(event: &Value) -> (String, String) {
    let event_type = event
        .get("eventType")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let occurred_at = event
        .get("occurredAt")
        .and_then(Value::as_i64)
        .unwrap_or_default();
    let data = event.get("data").and_then(Value::as_object);
    let mut title = data
        .and_then(|data| text_value(data.get("title")))
        .unwrap_or_default();
    let mut body = data
        .and_then(|data| text_value(data.get("content")))
        .or_else(|| data.and_then(|data| text_value(data.get("body"))))
        .unwrap_or_default();
    if title.is_empty() || body.is_empty() {
        let fallback =
            notification_template::render(event_type, occurred_at, &fallback_event_data(data));
        if title.is_empty() {
            title = text_value(fallback.get("title")).unwrap_or_default();
        }
        if body.is_empty() {
            body = text_value(fallback.get("content")).unwrap_or_default();
        }
    }
    if title.is_empty() {
        title = "Lux 通知".to_owned();
    }
    (
        bound_text(&title, MAX_NOTIFIER_TITLE_CHARS),
        bound_text(&body, MAX_NOTIFIER_BODY_CHARS),
    )
}

fn fallback_event_data(data: Option<&Map<String, Value>>) -> Map<String, Value> {
    let Some(data) = data else {
        return Map::new();
    };
    data.iter()
        .filter(|(key, _)| EVENT_TEXT_FIELDS.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn text_value(value: Option<&Value>) -> Option<String> {
    let value = value?.as_str()?.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn bound_text(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

#[cfg(test)]
mod tests {
    use std::{
        pin::Pin,
        task::{Context, Poll},
    };

    use super::{
        MAX_NOTIFIER_BODY_CHARS, NOTIFIER_HTTP_TIMEOUT, NotificationSendRpcResult,
        NotificationSendStatus, classify_http_status, outbound_http_client,
        outbound_http_client_from_env, render_event_text, result_from_http_status,
        run_notifier_session,
    };
    use serde_json::{Value, json};
    use tokio::io::AsyncWrite;

    #[derive(Default)]
    struct TestWriter {
        bytes: Vec<u8>,
    }

    impl TestWriter {
        fn lines(&self) -> Vec<Value> {
            String::from_utf8_lossy(&self.bytes)
                .lines()
                .map(|line| {
                    serde_json::from_str(line).expect("each response must be one JSON line")
                })
                .collect()
        }
    }

    impl AsyncWrite for TestWriter {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            buffer: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            self.bytes.extend_from_slice(buffer);
            Poll::Ready(Ok(buffer.len()))
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    fn notification_send_line(id: &str) -> String {
        json!({
            "id": id,
            "method": "notification.send",
            "params": {
                "event": {
                    "schemaVersion": 1,
                    "eventId": "event-1",
                    "eventType": "MEDIA_ADDED",
                    "occurredAt": 1_700_000_000_i64,
                    "serverId": "server-1",
                    "data": {"libraryName": "电影", "addedCount": 2}
                },
                "target": {"url": "", "allowPrivateNetwork": false},
                "config": {"serverUrl": "https://bark.example.invalid"},
                "secret": "device-key-1"
            }
        })
        .to_string()
    }

    async fn run(input: &str, result: NotificationSendRpcResult) -> Vec<Value> {
        let mut reader = input.as_bytes();
        let mut writer = TestWriter::default();
        run_notifier_session(&mut reader, &mut writer, |request| {
            assert_eq!(request.secret.as_deref(), Some("device-key-1"));
            let result = result.clone();
            async move { result }
        })
        .await
        .expect("the plugin session must terminate cleanly");
        writer.lines()
    }

    #[tokio::test]
    async fn dispatches_notification_send_and_stops_on_shutdown() {
        let input = format!(
            "{}\n{}\n{}\n",
            notification_send_line("first"),
            json!({"id": "second", "method": "plugin.shutdown", "params": {}}),
            notification_send_line("third"),
        );

        let responses = run(&input, super::delivered()).await;

        assert_eq!(responses.len(), 2, "the loop must stop at plugin.shutdown");
        assert_eq!(responses[0]["id"], "first");
        assert_eq!(responses[0]["result"]["status"], "DELIVERED");
        assert_eq!(responses[1]["id"], "second");
        assert_eq!(responses[1]["result"]["accepted"], true);
    }

    #[tokio::test]
    async fn reports_malformed_lines_and_unknown_methods_without_stopping() {
        let input = format!(
            "{}\n{}\n{}\n{}\n",
            "this is not json",
            json!({"id": "unknown", "method": "provider.reload", "params": {}}),
            json!({"id": "bad-params", "method": "notification.send", "params": {"event": {}}}),
            json!({"id": "shutdown", "method": "plugin.shutdown", "params": {}}),
        );

        let responses = run(&input, super::delivered()).await;

        assert_eq!(responses.len(), 4);
        assert_eq!(responses[0]["id"], "");
        assert_eq!(responses[0]["error"]["code"], "PLUGIN_INVALID_REQUEST");
        assert_eq!(responses[1]["error"]["code"], "PLUGIN_UNSUPPORTED_METHOD");
        assert_eq!(responses[2]["error"]["code"], "PLUGIN_INVALID_REQUEST");
        assert_eq!(responses[3]["result"]["accepted"], true);
    }

    #[tokio::test]
    async fn forwards_the_handler_classification_verbatim() {
        let input = format!("{}\n", notification_send_line("retry"));
        let responses = run(&input, super::retryable("PROVIDER_RATE_LIMITED")).await;

        assert_eq!(responses[0]["result"]["status"], "RETRYABLE");
        assert_eq!(responses[0]["result"]["errorCode"], "PROVIDER_RATE_LIMITED");
    }

    #[test]
    fn classifies_http_statuses_like_the_delivery_host() {
        for status in [200_u16, 201, 204] {
            assert_eq!(
                classify_http_status(status),
                NotificationSendStatus::Delivered
            );
        }
        for status in [408_u16, 425, 429, 500, 502, 599] {
            assert_eq!(
                classify_http_status(status),
                NotificationSendStatus::Retryable
            );
        }
        for status in [400_u16, 401, 403, 404, 422] {
            assert_eq!(classify_http_status(status), NotificationSendStatus::Failed);
        }

        let retry = result_from_http_status(429, Some(120));
        assert_eq!(retry.status, NotificationSendStatus::Retryable);
        assert_eq!(retry.retry_after_seconds, Some(120));
        assert_eq!(retry.error_code.as_deref(), Some("PROVIDER_HTTP_429"));

        let failed = result_from_http_status(401, Some(120));
        assert_eq!(failed.status, NotificationSendStatus::Failed);
        assert_eq!(failed.retry_after_seconds, None);
    }

    #[test]
    fn forwards_core_generated_notification_text() {
        let (title, body) = render_event_text(&json!({
            "eventType": "MEDIA_ADDED",
            "occurredAt": 1_700_000_000_i64,
            "data": {
                "title": "媒体新增",
                "content": "新增媒体：2 个\n媒体库：电影",
                "source": "lux",
                "timestamp": "2023-11-14T22:13:20Z"
            }
        }));

        assert_eq!(title, "媒体新增");
        assert_eq!(body, "新增媒体：2 个\n媒体库：电影");
    }

    #[test]
    fn renders_a_chinese_fallback_without_private_fields() {
        let (title, body) = render_event_text(&json!({
            "eventType": "PLAYBACK_STOPPED",
            "occurredAt": 1_700_000_000_i64,
            "data": {
                "userName": "小明",
                "itemTitle": "第一集",
                "seriesTitle": "示例剧",
                "seriesSeasonCount": 1,
                "positionTicks": 60_000_000_i64,
                "durationTicks": 120_000_000_i64,
                "remoteIp": "203.0.113.7",
                "overview": "这是一段很长的简介",
                "sourceId": "/media/library/secret-file.mkv",
                "client": "Lux Web",
                "deviceName": "MacBook"
            }
        }));

        assert!(title.contains("停止播放"), "unexpected title: {title}");
        assert!(body.contains("直接播放"), "unexpected body: {body}");
        assert!(body.contains("设备：Lux Web · MacBook"));
        assert!(
            !body.contains("203.0.113.7"),
            "a fallback body must not carry the remote address"
        );
        assert!(
            !body.contains("secret-file"),
            "a fallback body must not carry a local path"
        );
        assert!(!body.contains("这是一段很长的简介"));
    }

    #[test]
    fn renders_a_media_added_fallback_with_library_context() {
        let (title, body) = render_event_text(&json!({
            "eventType": "MEDIA_ADDED",
            "occurredAt": 1_700_000_000_i64,
            "data": {"libraryName": "电影", "addedCount": 2}
        }));

        assert!(title.contains("新增媒体"), "unexpected title: {title}");
        assert!(body.contains("新增媒体：2 个"));
        assert!(body.contains("媒体库：电影"));
    }

    #[test]
    fn bounds_the_rendered_body_and_defaults_a_missing_event() {
        let oversized = "长".repeat(MAX_NOTIFIER_BODY_CHARS + 100);
        let (_, body) = render_event_text(&json!({
            "eventType": "JOB_FAILED",
            "occurredAt": 1_700_000_000_i64,
            "data": {"content": oversized}
        }));
        assert_eq!(body.chars().count(), MAX_NOTIFIER_BODY_CHARS);

        assert_eq!(
            render_event_text(&json!({})),
            ("Lux 通知".to_owned(), "收到新的 Lux 事件".to_owned())
        );
    }

    #[test]
    fn builds_an_outbound_client_without_redirects_and_a_ten_second_timeout() {
        assert_eq!(NOTIFIER_HTTP_TIMEOUT, std::time::Duration::from_secs(10));
        assert!(outbound_http_client(Some("http://127.0.0.1:3128")).is_ok());
        assert!(outbound_http_client(None).is_ok());
        assert!(outbound_http_client(Some("ftp://127.0.0.1")).is_err());
        assert!(outbound_http_client_from_env().is_ok());
    }
}
