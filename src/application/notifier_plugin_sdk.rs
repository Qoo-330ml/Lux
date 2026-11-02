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
//! - [`guard_destination`] and [`guard_host`] resolve a destination once, reject private, loopback,
//!   link-local, metadata and reserved addresses according to the host's `allowPrivateNetwork`
//!   approval, and pin the validated address so a DNS rebinding cannot redirect the request.
//!
//! The event, target and config values arrive through the RPC request; the optional `secret` is the
//! per-destination credential the host stores outside `providerConfig`. A plugin must not try to
//! read the Lux config directory: the host starts notification plugins without `LUX_CONFIG_DIR`.

use std::{
    fmt,
    future::Future,
    io,
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use reqwest::{ClientBuilder, Url};
use serde_json::{Map, Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, BufWriter},
    net::lookup_host,
};

use crate::{
    application::{
        notification_template,
        plugin_protocol::{
            NOTIFICATION_SEND_METHOD, NotificationSendRpcRequest, NotificationSendRpcResult,
            NotificationSendStatus, PluginRequest, PluginResponse, PluginRpcError,
        },
        remote_url_policy, webhooks,
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

/// Builds the outbound client builder used by notification plugins.
///
/// The builder already applies the Lux proxy policy ([`NOTIFIER_PROXY_URL_ENV`], or `proxy_url`
/// when the host supplies one), never follows redirects and gives every request
/// [`NOTIFIER_HTTP_TIMEOUT`]. Use it instead of [`outbound_http_client`] when a destination has to be
/// pinned with [`GuardedDestination::pin`] before the client is built.
///
/// When no administrator proxy is configured the builder disables proxies entirely
/// (`no_proxy`), so `HTTP_PROXY`/`HTTPS_PROXY` and the system proxy cannot silently redirect a
/// delivery: a guarded destination must reach the address the guard validated. When
/// [`NOTIFIER_PROXY_URL_ENV`] (or `proxy_url`) is set, the request goes through the administrator
/// proxy instead, the proxy resolves the destination, and [`GuardedDestination::pin`] no longer
/// decides which address is contacted.
pub fn outbound_http_client_builder(
    proxy_url: Option<&str>,
) -> Result<ClientBuilder, NotifierSdkError> {
    let proxy_url = match proxy_url {
        Some(proxy_url) => Some(proxy_url.to_owned()),
        None => network::proxy_url_from_env().map_err(|_| NotifierSdkError::InvalidProxy)?,
    };
    let builder = network::apply_proxy(reqwest::Client::builder(), proxy_url.as_deref())
        .map_err(|_| NotifierSdkError::InvalidProxy)?;
    let builder = if proxy_url.is_some() {
        builder
    } else {
        builder.no_proxy()
    };
    Ok(builder
        .timeout(NOTIFIER_HTTP_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("lux-notifier-sdk/", env!("CARGO_PKG_VERSION"))))
}

/// Builds the outbound client builder from the plugin process environment.
pub fn outbound_http_client_builder_from_env() -> Result<ClientBuilder, NotifierSdkError> {
    let proxy_url = network::proxy_url_from_env().map_err(|_| NotifierSdkError::InvalidProxy)?;
    outbound_http_client_builder(proxy_url.as_deref())
}

/// Builds the outbound client used by notification plugins.
///
/// The client honors the Lux proxy policy ([`NOTIFIER_PROXY_URL_ENV`], or `proxy_url` when the host
/// supplies one), never follows redirects and gives every request [`NOTIFIER_HTTP_TIMEOUT`].
pub fn outbound_http_client(proxy_url: Option<&str>) -> Result<reqwest::Client, NotifierSdkError> {
    outbound_http_client_builder(proxy_url)?
        .build()
        .map_err(client_error)
}

/// Builds the outbound client from the plugin process environment.
pub fn outbound_http_client_from_env() -> Result<reqwest::Client, NotifierSdkError> {
    outbound_http_client_builder_from_env()?
        .build()
        .map_err(client_error)
}

fn client_error(error: reqwest::Error) -> NotifierSdkError {
    NotifierSdkError::Client(error.to_string())
}

/// Upper bound of resolved addresses the destination guard inspects for one host.
const MAX_GUARDED_ADDRESSES: usize = 16;

/// A destination the host policy rejected, resolved transiently or validated.
///
/// [`Self::code`] is the stable error code a plugin returns to the host; the plugin never echoes the
/// destination itself, because a complete external URL must not reach a log or a delivery record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DestinationGuardError {
    /// The destination is not a URL with a host and a port.
    InvalidUrl,
    /// The destination does not use `http` or `https`.
    UnsupportedScheme,
    /// The destination URL carries credentials in its authority.
    Credentials,
    /// The host did not resolve, or resolved to no address.
    ResolutionFailed,
    /// A loopback, private or link-local destination the administrator did not approve.
    PrivateAddress,
    /// An address that stays forbidden even with `allowPrivateNetwork`: unspecified (`0.0.0.0`,
    /// `::`), link-local (including cloud metadata such as `169.254.169.254`) and multicast ranges.
    DangerousAddress,
    /// An address that is neither private nor dangerous but is still unroutable, so it stays
    /// forbidden even with `allowPrivateNetwork`: documentation ranges (`192.0.2.0/24`,
    /// `198.51.100.0/24`, `203.0.113.0/24`, `2001:db8::/32`), `240/4`, the limited broadcast
    /// address and tunnelled forms such as 6to4, Teredo and NAT64. The core policy classifies
    /// CGNAT (`100.64/10`), benchmarking (`198.18/19`) and `192.0.0.0/24` as private, so those are
    /// reachable after a private-network approval.
    ReservedAddress,
}

impl DestinationGuardError {
    /// The stable plugin error code for this rejection.
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidUrl | Self::UnsupportedScheme | Self::Credentials => {
                "invalid_destination_url"
            }
            Self::ResolutionFailed => "destination_unresolved",
            Self::PrivateAddress | Self::DangerousAddress | Self::ReservedAddress => {
                "destination_not_allowed"
            }
        }
    }

    /// Whether the host may retry the same destination later.
    pub fn is_retryable(self) -> bool {
        matches!(self, Self::ResolutionFailed)
    }
}

impl fmt::Display for DestinationGuardError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidUrl => "the destination URL is invalid",
            Self::UnsupportedScheme => "the destination URL must use http or https",
            Self::Credentials => "the destination URL must not carry credentials",
            Self::ResolutionFailed => "the destination host could not be resolved",
            Self::PrivateAddress => "the destination resolves to a private address",
            Self::DangerousAddress => "the destination resolves to a reserved address",
            Self::ReservedAddress => "the destination resolves to an unroutable address",
        })
    }
}

impl std::error::Error for DestinationGuardError {}

/// A validated HTTP destination with the address the request must connect to.
#[derive(Clone, Debug)]
pub struct GuardedDestination {
    url: Url,
    host: String,
    address: SocketAddr,
}

impl GuardedDestination {
    /// The destination URL exactly as the channel configured it.
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// The destination host, without a port.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The validated address the request must connect to.
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// Pins the validated address into a client builder, so a second DNS answer cannot redirect the
    /// request (DNS rebinding) after the guard approved the destination.
    pub fn pin(&self, builder: ClientBuilder) -> ClientBuilder {
        builder.resolve(&self.host, self.address)
    }
}

/// A validated host and port, for channels that do not speak HTTP (SMTP, custom TCP).
#[derive(Clone, Debug)]
pub struct GuardedHost {
    host: String,
    port: u16,
    primary: SocketAddr,
    addresses: Vec<SocketAddr>,
}

impl GuardedHost {
    /// The validated host name.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The validated port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Every validated address for the host; the channel may try them in order.
    pub fn addresses(&self) -> &[SocketAddr] {
        &self.addresses
    }

    /// The first validated address, which a single-address client should use.
    pub fn primary(&self) -> SocketAddr {
        self.primary
    }
}

/// Validates an `http`/`https` destination against the host's private-network policy.
///
/// The host passes `allowPrivateNetwork` in `notification.send`; a plugin must forward it and must
/// never widen it. Every resolved address is checked: loopback, private and link-local destinations
/// are rejected unless the administrator approved the private network, while unspecified,
/// link-local (cloud metadata), multicast, documentation, `240/4` and broadcast addresses are always
/// rejected. IPv4-mapped and IPv4-compatible (`::/96`) forms are classified by their embedded IPv4
/// target. Use [`GuardedDestination::pin`] before building the request client.
pub async fn guard_destination(
    url: &Url,
    allow_private_network: bool,
) -> Result<GuardedDestination, DestinationGuardError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(DestinationGuardError::UnsupportedScheme);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(DestinationGuardError::Credentials);
    }
    let host = url.host_str().ok_or(DestinationGuardError::InvalidUrl)?;
    // `host_str` keeps the brackets of an IPv6 literal; the resolver needs the bare address so a
    // literal is classified instead of being sent to DNS as a hostname.
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    let port = url
        .port_or_known_default()
        .ok_or(DestinationGuardError::InvalidUrl)?;
    let guarded = resolve_guarded_host(host, port, allow_private_network).await?;
    Ok(GuardedDestination {
        url: url.clone(),
        host: guarded.host.clone(),
        address: guarded.primary(),
    })
}

/// Parses and validates an `http`/`https` destination string.
///
/// Convenience wrapper around [`guard_destination`] for channels that keep their target as text.
pub async fn guard_destination_str(
    value: &str,
    allow_private_network: bool,
) -> Result<GuardedDestination, DestinationGuardError> {
    let url = Url::parse(value).map_err(|_| DestinationGuardError::InvalidUrl)?;
    guard_destination(&url, allow_private_network).await
}

/// Validates a `host:port` destination for channels that do not speak HTTP (SMTP, custom TCP).
pub async fn guard_host(
    host: &str,
    port: u16,
    allow_private_network: bool,
) -> Result<GuardedHost, DestinationGuardError> {
    let host = host.trim();
    if host.is_empty()
        || host.chars().count() > 255
        || host
            .chars()
            .any(|character| character.is_control() || matches!(character, '/' | '@' | ' ' | '\\'))
        || port == 0
    {
        return Err(DestinationGuardError::InvalidUrl);
    }
    resolve_guarded_host(host, port, allow_private_network).await
}

async fn resolve_guarded_host(
    host: &str,
    port: u16,
    allow_private_network: bool,
) -> Result<GuardedHost, DestinationGuardError> {
    let resolved = lookup_host((host, port))
        .await
        .map_err(|_| DestinationGuardError::ResolutionFailed)?;
    let mut addresses = Vec::new();
    for address in resolved.take(MAX_GUARDED_ADDRESSES) {
        check_destination_address(address.ip(), allow_private_network)?;
        addresses.push(address);
    }
    let Some(primary) = addresses.first().copied() else {
        return Err(DestinationGuardError::ResolutionFailed);
    };
    Ok(GuardedHost {
        host: host.to_owned(),
        port,
        primary,
        addresses,
    })
}

/// Applies the shared Lux IP policy to one resolved destination address.
///
/// The classification reuses the core helpers ([`webhooks::is_dangerous_address`],
/// [`webhooks::is_private_address`] and [`remote_url_policy::is_private_or_reserved`]) instead of
/// keeping a second copy of the address ranges.
pub(crate) fn check_destination_address(
    address: IpAddr,
    allow_private_network: bool,
) -> Result<(), DestinationGuardError> {
    if webhooks::is_dangerous_address(address) {
        return Err(DestinationGuardError::DangerousAddress);
    }
    let private = webhooks::is_private_address(address);
    if !private && remote_url_policy::is_private_or_reserved(address) {
        return Err(DestinationGuardError::ReservedAddress);
    }
    if private && !allow_private_network {
        return Err(DestinationGuardError::PrivateAddress);
    }
    Ok(())
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
        DestinationGuardError, MAX_NOTIFIER_BODY_CHARS, NOTIFIER_HTTP_TIMEOUT,
        NotificationSendRpcResult, NotificationSendStatus, check_destination_address,
        classify_http_status, guard_destination_str, guard_host, outbound_http_client,
        outbound_http_client_builder, outbound_http_client_from_env, render_event_text,
        result_from_http_status, run_notifier_session,
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

    fn check_address(
        value: &str,
        allow_private_network: bool,
    ) -> Result<(), DestinationGuardError> {
        check_destination_address(
            value.parse().expect("the fixture must be an IP address"),
            allow_private_network,
        )
    }

    #[test]
    fn classifies_ipv4_destinations_for_the_private_network_policy() {
        use DestinationGuardError::{DangerousAddress, PrivateAddress, ReservedAddress};

        // Private also covers the ranges the core policy treats as private, such as the CGNAT and
        // benchmarking blocks, so an approved private network may reach them.
        for private in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.9",
            "192.168.1.10",
            "100.64.0.1",
            "198.18.0.1",
        ] {
            assert_eq!(
                check_address(private, false),
                Err(PrivateAddress),
                "{private} must be blocked without private-network approval"
            );
            assert_eq!(
                check_address(private, true),
                Ok(()),
                "{private} must be allowed with private-network approval"
            );
        }

        // Always rejected: cloud metadata, unspecified, multicast and broadcast addresses.
        for dangerous in [
            "169.254.169.254",
            "169.254.0.1",
            "0.0.0.0",
            "224.0.0.1",
            "239.1.2.3",
        ] {
            assert_eq!(
                check_address(dangerous, false),
                Err(DangerousAddress),
                "{dangerous} must be rejected"
            );
            assert_eq!(
                check_address(dangerous, true),
                Err(DangerousAddress),
                "{dangerous} must stay rejected even with private-network approval"
            );
        }

        // Always rejected: documentation, benchmarking, reserved and broadcast ranges.
        for reserved in [
            "192.0.2.7",
            "198.51.100.9",
            "203.0.113.5",
            "240.0.0.1",
            "255.255.255.255",
        ] {
            assert_eq!(
                check_address(reserved, false),
                Err(ReservedAddress),
                "{reserved} must be rejected"
            );
            assert_eq!(
                check_address(reserved, true),
                Err(ReservedAddress),
                "{reserved} must stay rejected even with private-network approval"
            );
        }

        for public in ["93.184.216.34", "1.1.1.1", "8.8.8.8"] {
            assert_eq!(check_address(public, false), Ok(()), "{public}");
            assert_eq!(check_address(public, true), Ok(()), "{public}");
        }
    }

    #[test]
    fn classifies_ipv6_and_ipv4_mapped_destinations() {
        use DestinationGuardError::{DangerousAddress, PrivateAddress, ReservedAddress};

        for private in ["::1", "fd00::1", "fc00::abcd"] {
            assert_eq!(
                check_address(private, false),
                Err(PrivateAddress),
                "{private}"
            );
            assert_eq!(check_address(private, true), Ok(()), "{private}");
        }
        for dangerous in ["fe80::1", "::", "ff02::1"] {
            assert_eq!(
                check_address(dangerous, false),
                Err(DangerousAddress),
                "{dangerous}"
            );
            assert_eq!(
                check_address(dangerous, true),
                Err(DangerousAddress),
                "{dangerous}"
            );
        }
        for reserved in ["2002:c000:0201::1", "64:ff9b::c000:0201"] {
            assert_eq!(
                check_address(reserved, false),
                Err(ReservedAddress),
                "{reserved}"
            );
            assert_eq!(
                check_address(reserved, true),
                Err(ReservedAddress),
                "{reserved}"
            );
        }

        // An IPv4-mapped address is classified by its embedded IPv4 target.
        assert_eq!(
            check_address("::ffff:127.0.0.1", false),
            Err(PrivateAddress)
        );
        assert_eq!(check_address("::ffff:127.0.0.1", true), Ok(()));
        assert_eq!(
            check_address("::ffff:169.254.169.254", true),
            Err(DangerousAddress)
        );
        assert_eq!(
            check_address("::ffff:203.0.113.5", true),
            Err(ReservedAddress)
        );
        assert_eq!(check_address("::ffff:93.184.216.34", false), Ok(()));

        // The deprecated IPv4-compatible form (`::/96`) must not bypass the embedded IPv4 policy.
        assert_eq!(check_address("::127.0.0.1", false), Err(PrivateAddress));
        assert_eq!(check_address("::127.0.0.1", true), Ok(()));
        assert_eq!(check_address("::192.168.1.1", false), Err(PrivateAddress));
        assert_eq!(check_address("::192.168.1.1", true), Ok(()));
        assert_eq!(
            check_address("::169.254.169.254", true),
            Err(DangerousAddress)
        );
        assert_eq!(check_address("::203.0.113.5", true), Err(ReservedAddress));
        assert_eq!(check_address("::93.184.216.34", false), Ok(()));
        assert_eq!(check_address("::", true), Err(DangerousAddress));

        // The IPv6 documentation range is reserved and never reachable.
        assert_eq!(check_address("2001:db8::1", false), Err(ReservedAddress));
        assert_eq!(check_address("2001:db8::1", true), Err(ReservedAddress));

        assert_eq!(check_address("2001:4860:4860::8888", false), Ok(()));
    }

    #[test]
    fn exposes_stable_destination_guard_error_codes() {
        assert_eq!(
            DestinationGuardError::InvalidUrl.code(),
            "invalid_destination_url"
        );
        assert_eq!(
            DestinationGuardError::UnsupportedScheme.code(),
            "invalid_destination_url"
        );
        assert_eq!(
            DestinationGuardError::Credentials.code(),
            "invalid_destination_url"
        );
        assert_eq!(
            DestinationGuardError::ResolutionFailed.code(),
            "destination_unresolved"
        );
        assert_eq!(
            DestinationGuardError::PrivateAddress.code(),
            "destination_not_allowed"
        );
        assert_eq!(
            DestinationGuardError::DangerousAddress.code(),
            "destination_not_allowed"
        );
        assert_eq!(
            DestinationGuardError::ReservedAddress.code(),
            "destination_not_allowed"
        );
        assert!(DestinationGuardError::ResolutionFailed.is_retryable());
        assert!(!DestinationGuardError::DangerousAddress.is_retryable());
        assert!(!DestinationGuardError::PrivateAddress.is_retryable());
    }

    #[tokio::test]
    async fn guards_http_destinations_and_blocks_loopback_by_default() {
        let blocked = guard_destination_str("http://127.0.0.1:8080/push", false)
            .await
            .expect_err("loopback must be blocked without private-network approval");
        assert_eq!(blocked, DestinationGuardError::PrivateAddress);

        let guarded = guard_destination_str("http://127.0.0.1:8080/push", true)
            .await
            .expect("an approved private network must be allowed");
        assert_eq!(guarded.url().as_str(), "http://127.0.0.1:8080/push");
        assert_eq!(guarded.address().port(), 8080);
        assert!(guarded.address().ip().is_loopback());

        // Pinning the resolved address must yield a usable client without another DNS lookup.
        let builder = outbound_http_client_builder(None).expect("outbound builder");
        assert!(guarded.pin(builder).build().is_ok());

        let blocked = guard_destination_str("http://localhost:1234/", false)
            .await
            .expect_err("localhost must be blocked without private-network approval");
        assert_eq!(blocked, DestinationGuardError::PrivateAddress);
        let guarded = guard_destination_str("http://localhost:1234/", true)
            .await
            .expect("localhost must be allowed with private-network approval");
        assert_eq!(guarded.host(), "localhost");
        assert!(guarded.address().ip().is_loopback());
    }

    #[tokio::test]
    async fn rejects_destinations_that_stay_dangerous_with_private_network_approval() {
        for value in [
            "http://169.254.169.254/latest/meta-data",
            "http://0.0.0.0/",
            "http://[fe80::1]/",
        ] {
            assert_eq!(
                guard_destination_str(value, true)
                    .await
                    .expect_err("the destination must stay blocked"),
                DestinationGuardError::DangerousAddress,
                "{value}"
            );
        }
        assert_eq!(
            guard_destination_str("http://[::ffff:169.254.169.254]/", true)
                .await
                .expect_err("the mapped metadata address must stay blocked"),
            DestinationGuardError::DangerousAddress
        );
    }

    #[tokio::test]
    async fn rejects_unsupported_destination_urls() {
        assert_eq!(
            guard_destination_str("ftp://127.0.0.1/", true)
                .await
                .expect_err("ftp must be rejected"),
            DestinationGuardError::UnsupportedScheme
        );
        assert_eq!(
            guard_destination_str("http://user:password@127.0.0.1/", true)
                .await
                .expect_err("credentials must be rejected"),
            DestinationGuardError::Credentials
        );
        assert_eq!(
            guard_destination_str("not a url", true)
                .await
                .expect_err("an unparsable URL must be rejected"),
            DestinationGuardError::InvalidUrl
        );
    }

    #[tokio::test]
    async fn guards_a_host_and_port_for_smtp_destinations() {
        let blocked = guard_host("127.0.0.1", 25, false)
            .await
            .expect_err("loopback SMTP must be blocked without private-network approval");
        assert_eq!(blocked, DestinationGuardError::PrivateAddress);

        let guarded = guard_host("localhost", 587, true)
            .await
            .expect("an approved private SMTP host must be allowed");
        assert_eq!(guarded.host(), "localhost");
        assert_eq!(guarded.port(), 587);
        assert!(!guarded.addresses().is_empty());
        assert!(guarded.primary().ip().is_loopback());
        assert_eq!(guarded.primary().port(), 587);

        assert_eq!(
            guard_host("127.0.0.1", 0, true)
                .await
                .expect_err("port zero must be rejected"),
            DestinationGuardError::InvalidUrl
        );
        assert_eq!(
            guard_host("", 25, true)
                .await
                .expect_err("an empty host must be rejected"),
            DestinationGuardError::InvalidUrl
        );
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
