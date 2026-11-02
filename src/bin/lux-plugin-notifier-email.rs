//! SMTP notifier plugin (`org.lux.notifier.email`).
//!
//! The plugin owns one provider-specific concern: turning a Lux notification into a plain-text
//! email and handing it to an SMTP relay. Everything else comes from
//! `luxd::application::notifier_plugin_sdk`: the stdio JSON-RPC loop ([`run_notifier`]), the shared
//! ten second delivery window ([`NOTIFIER_HTTP_TIMEOUT`]), the `DELIVERED`/`RETRYABLE`/`FAILED`
//! classification helpers ([`delivered`], [`retryable`], [`failed`]) and the core notification text
//! ([`render_event_text`]).
//!
//! The SMTP password arrives exactly once, through the controlled
//! `NotificationSendRpcRequest.secret` field. It is never read from a config file or the process
//! environment, never written to the log, and never copied into a result or an error message. The
//! log line only carries the event ID, the transport, the port, the classification and the stable
//! error code.
//!
//! Before any outbound connection the relay goes through the shared SDK destination guard with the
//! host's `target.allowPrivateNetwork` approval. The guard resolves the relay once; the transport
//! then connects to that validated address while the configured host name still drives the TLS
//! handshake, so a second DNS answer cannot redirect the delivery.

use std::{str::FromStr, time::Duration};

use lettre::{
    Address, AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::{Mailbox, header::ContentType},
    transport::smtp::{
        authentication::Credentials,
        client::{Tls, TlsParameters},
    },
};
use luxd::application::{
    notifier_plugin_sdk::{
        DestinationGuardError, GuardedHost, NOTIFIER_HTTP_TIMEOUT, delivered, failed, guard_host,
        render_event_text, retryable, run_notifier,
    },
    plugin_protocol::{
        NotificationSendRpcRequest, NotificationSendRpcResult, NotificationSendStatus,
    },
};
use serde_json::Value;

/// Stable plugin identity, also used as the log prefix so a captured log is attributable.
const PLUGIN_ID: &str = "org.lux.notifier.email";
/// Port used when the destination configuration omits `smtpPort`.
const DEFAULT_STARTTLS_PORT: u16 = 587;
/// Port used when the destination omits `smtpPort` and selects implicit TLS.
const DEFAULT_SMTPS_PORT: u16 = 465;
const MAX_SMTP_HOST_CHARS: usize = 253;
const MAX_ADDRESS_CHARS: usize = 320;
const MAX_SMTP_USERNAME_CHARS: usize = 320;
const MAX_SECRET_CHARS: usize = 4_096;
/// Upper bound of one destination's recipient list.
const MAX_RECIPIENTS: usize = 20;
/// Test-only tightening knob: it can only shorten the delivery window, never extend it.
const TIMEOUT_SECONDS_ENV: &str = "LUX_PLUGIN_SMTP_TIMEOUT_SECONDS";
/// Test-only plaintext seam.
///
/// A notification destination never offers an unencrypted SMTP mode: the manifest declares only
/// STARTTLS and implicit TLS, and `security: "none"` is rejected unless this environment flag is
/// `1` *and* the relay is a loopback address. The seam exists because the integration suite drives
/// the real binary against a plaintext fake relay bound to `127.0.0.1`; it is deliberately absent
/// from the manifest and from the documented user-facing contract.
const PLAINTEXT_TEST_ENV: &str = "LUX_NOTIFIER_EMAIL_TEST_PLAINTEXT";

/// The destination configuration is unusable; retrying cannot fix it.
const INVALID_CONFIG: &str = "invalid_config";
/// The host passed no usable credential; the host must not retry either.
const MISSING_SECRET: &str = "missing_secret";
/// The SMTP server did not answer inside the delivery window.
const SMTP_TIMEOUT: &str = "smtp_timeout";
/// A 4xx SMTP reply: the relay asked for a later attempt.
const SMTP_TRANSIENT: &str = "smtp_transient";
/// A 5xx SMTP reply: authentication, sender or recipient were rejected permanently.
const SMTP_REJECTED: &str = "smtp_rejected";
/// The configured transport security could not be established.
const SMTP_TLS_FAILED: &str = "smtp_tls_failed";
/// The server cannot satisfy the configured security or the SMTP dialogue broke down.
const SMTP_PROTOCOL_ERROR: &str = "smtp_protocol_error";
/// The relay could not be reached at all.
const SMTP_UNAVAILABLE: &str = "smtp_unavailable";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    run_notifier(|request| async move { deliver(request).await }).await?;
    Ok(())
}

/// Classifies one `notification.send` request and reports the outcome on stderr.
async fn deliver(request: NotificationSendRpcRequest) -> NotificationSendRpcResult {
    let config = EmailConfig::parse(&request.config).ok();
    let result = match attempt(&request, config.as_ref()).await {
        Ok(result) => result,
        Err(code) => failed(code),
    };
    log_outcome(&request, config.as_ref(), &result);
    result
}

/// Runs one delivery attempt.
///
/// `Err` carries the stable error code of a condition the plugin detects before it talks to the
/// relay; everything the relay answers is classified by [`classify_smtp_error`].
async fn attempt(
    request: &NotificationSendRpcRequest,
    config: Option<&EmailConfig>,
) -> Result<NotificationSendRpcResult, &'static str> {
    let secret = transport_secret(request.secret.as_deref())?;
    let config = config.ok_or(INVALID_CONFIG)?;
    // The host decides whether the relay may be private; the plugin never widens that approval.
    // A rejected destination never reaches the network.
    let guarded = match guard_host(
        &config.host,
        config.port,
        allowed_private_network(&request.target),
    )
    .await
    {
        Ok(guarded) => guarded,
        Err(error) => return Ok(guard_failure(error)),
    };
    // The plaintext seam is decided by the address the guard validated, never by the configured
    // name: an approved private relay that is not loopback still must not receive plaintext SMTP.
    if config.security == SmtpSecurity::None && !guarded.primary().ip().is_loopback() {
        return Err(INVALID_CONFIG);
    }
    let (title, body) = render_event_text(&request.event);
    let message = config.message(&title, &body)?;
    let timeout = delivery_timeout();
    let transport = build_transport(config, &guarded, config.credentials(secret), timeout)?;
    match tokio::time::timeout(timeout, transport.send(message)).await {
        Err(_) => Ok(retryable(SMTP_TIMEOUT)),
        Ok(Ok(_)) => Ok(delivered()),
        Ok(Err(error)) => Ok(classify_smtp_error(&error)),
    }
}

/// Reads the host `allowPrivateNetwork` approval; an absent or non-boolean value means "not allowed".
fn allowed_private_network(target: &Value) -> bool {
    target
        .get("allowPrivateNetwork")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Maps a destination guard rejection onto the delivery classification.
fn guard_failure(error: DestinationGuardError) -> NotificationSendRpcResult {
    if error.is_retryable() {
        retryable(error.code())
    } else {
        failed(error.code())
    }
}

/// Validates the RPC credential.
///
/// The host always sends a non-empty secret for a destination that declares the `secret` field, so
/// an empty or missing value is a broken request rather than a retryable condition.
fn transport_secret(secret: Option<&str>) -> Result<&str, &'static str> {
    let secret = secret.ok_or(MISSING_SECRET)?;
    if secret.trim().is_empty() {
        return Err(MISSING_SECRET);
    }
    if secret.chars().count() > MAX_SECRET_CHARS {
        return Err(INVALID_CONFIG);
    }
    Ok(secret)
}

/// The delivery window: the SDK's ten second bound, optionally tightened by `TIMEOUT_SECONDS_ENV`.
fn delivery_timeout() -> Duration {
    let limit = NOTIFIER_HTTP_TIMEOUT.as_secs().max(1);
    let seconds = std::env::var(TIMEOUT_SECONDS_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(|seconds| seconds.clamp(1, limit))
        .unwrap_or(limit);
    Duration::from_secs(seconds)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SmtpSecurity {
    StartTls,
    Ssl,
    None,
}

impl SmtpSecurity {
    /// The configuration value this mode was parsed from, for the diagnostic log line.
    fn label(self) -> &'static str {
        match self {
            Self::StartTls => "starttls",
            Self::Ssl => "ssl",
            Self::None => "plaintext",
        }
    }
}

/// One destination's validated non-secret configuration.
struct EmailConfig {
    host: String,
    port: u16,
    security: SmtpSecurity,
    username: Option<String>,
    from: Address,
    recipients: Vec<Address>,
}

impl EmailConfig {
    /// Parses the per-destination `providerConfig` object.
    ///
    /// Unknown keys are ignored: the host already validates the object against the manifest, and a
    /// future manifest field must not break an older package.
    fn parse(config: &Value) -> Result<Self, &'static str> {
        let object = config.as_object().ok_or(INVALID_CONFIG)?;
        let host = object
            .get("smtpHost")
            .and_then(Value::as_str)
            .ok_or(INVALID_CONFIG)?;
        if !valid_smtp_host(host) {
            return Err(INVALID_CONFIG);
        }
        let security = match object.get("security").and_then(Value::as_str) {
            None => SmtpSecurity::StartTls,
            Some("starttls") => SmtpSecurity::StartTls,
            Some("ssl") => SmtpSecurity::Ssl,
            // Unencrypted SMTP is not a destination option: the value is only reachable while the
            // test seam is enabled, and even then the guarded connection address must be loopback.
            Some("none") if plaintext_test_enabled() => SmtpSecurity::None,
            Some(_) => return Err(INVALID_CONFIG),
        };
        // An unusable TLS server name is a configuration error, not a retryable upstream failure.
        // Validate it before the guard resolves anything: `lettre` only parses the TLS server name
        // during the handshake, so a bad name would otherwise surface as `destination_unresolved`
        // (retryable) or as a noisy handshake failure.
        if security != SmtpSecurity::None && !valid_tls_server_name(host) {
            return Err(INVALID_CONFIG);
        }
        let port = match object.get("smtpPort") {
            None | Some(Value::Null) => default_port(security),
            Some(value) => value
                .as_u64()
                .and_then(|port| u16::try_from(port).ok())
                .filter(|port| *port > 0)
                .ok_or(INVALID_CONFIG)?,
        };
        let username = match object.get("username").and_then(Value::as_str) {
            None => None,
            Some(value) if value.trim().is_empty() => None,
            Some(value) => {
                if value.chars().count() > MAX_SMTP_USERNAME_CHARS
                    || value.chars().any(char::is_control)
                    || value.trim() != value
                {
                    return Err(INVALID_CONFIG);
                }
                Some(value.to_owned())
            }
        };
        let from = parse_address(
            object
                .get("from")
                .and_then(Value::as_str)
                .ok_or(INVALID_CONFIG)?,
        )?;
        let recipients = parse_recipients(
            object
                .get("to")
                .and_then(Value::as_str)
                .ok_or(INVALID_CONFIG)?,
        )?;
        Ok(Self {
            host: host.to_owned(),
            port,
            security,
            username,
            from,
            recipients,
        })
    }

    /// The SMTP credential, or `None` when the destination does not authenticate.
    fn credentials(&self, secret: &str) -> Option<Credentials> {
        self.username
            .as_ref()
            .map(|username| Credentials::new(username.clone(), secret.to_owned()))
    }

    /// Builds the plain-text message: the core title becomes the subject, the core body the body.
    fn message(&self, title: &str, body: &str) -> Result<Message, &'static str> {
        let mut builder = Message::builder()
            .from(Mailbox::new(None, self.from.clone()))
            .subject(header_text(title))
            .header(ContentType::TEXT_PLAIN);
        for recipient in &self.recipients {
            builder = builder.to(Mailbox::new(None, recipient.clone()));
        }
        builder.body(body.to_owned()).map_err(|_| INVALID_CONFIG)
    }
}

/// Flattens the core title into a single safe header value.
fn header_text(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .trim()
        .to_owned()
}

fn parse_address(value: &str) -> Result<Address, &'static str> {
    if value.is_empty()
        || value.chars().count() > MAX_ADDRESS_CHARS
        || value.chars().any(char::is_control)
        || value.trim() != value
    {
        return Err(INVALID_CONFIG);
    }
    Address::from_str(value).map_err(|_| INVALID_CONFIG)
}

fn parse_recipients(value: &str) -> Result<Vec<Address>, &'static str> {
    let mut recipients = Vec::new();
    for candidate in value.split(',') {
        let candidate = candidate.trim();
        if candidate.is_empty() {
            continue;
        }
        recipients.push(parse_address(candidate)?);
        if recipients.len() > MAX_RECIPIENTS {
            return Err(INVALID_CONFIG);
        }
    }
    if recipients.is_empty() {
        return Err(INVALID_CONFIG);
    }
    Ok(recipients)
}

/// Builds the async SMTP transport for the configured security mode.
///
/// The transport connects to the address the destination guard validated instead of resolving the
/// configured host again, so a second DNS answer cannot redirect the delivery. The configured host
/// name still drives the TLS handshake, so the certificate must match the administrator's target
/// rather than the resolved address. A relay that cannot even be described (for example an
/// unusable TLS server name) is a configuration failure rather than a retryable upstream condition.
fn build_transport(
    config: &EmailConfig,
    guarded: &GuardedHost,
    credentials: Option<Credentials>,
    timeout: Duration,
) -> Result<AsyncSmtpTransport<Tokio1Executor>, &'static str> {
    let address = guarded.primary();
    let server = address.ip().to_string();
    let builder = match config.security {
        SmtpSecurity::Ssl => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&server)
            .tls(Tls::Wrapper(tls_parameters(&config.host)?)),
        SmtpSecurity::StartTls => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&server)
            .tls(Tls::Required(tls_parameters(&config.host)?)),
        SmtpSecurity::None => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&server),
    };
    let builder = builder.port(address.port()).timeout(Some(timeout));
    let builder = match credentials {
        Some(credentials) => builder.credentials(credentials),
        None => builder,
    };
    Ok(builder.build())
}

/// Builds the TLS parameters for the configured host name.
fn tls_parameters(host: &str) -> Result<TlsParameters, &'static str> {
    TlsParameters::new(host.to_owned()).map_err(|_| INVALID_CONFIG)
}

/// The submission port a security mode falls back to when the destination omits `smtpPort`.
fn default_port(security: SmtpSecurity) -> u16 {
    match security {
        SmtpSecurity::Ssl => DEFAULT_SMTPS_PORT,
        SmtpSecurity::StartTls | SmtpSecurity::None => DEFAULT_STARTTLS_PORT,
    }
}

/// A host the plugin accepts as an SMTP relay.
///
/// Only an ASCII DNS name (letters, digits, `.`, `-`, `_`) or a bare IPv6 literal is accepted, which
/// keeps `user@host`, `[::1]`, `host:port`, absolute paths and backslashes out of the destination
/// before the guard or the TLS layer ever sees them.
fn valid_smtp_host(host: &str) -> bool {
    if host.is_empty()
        || host.chars().count() > MAX_SMTP_HOST_CHARS
        || host.chars().any(char::is_control)
    {
        return false;
    }
    if host.parse::<std::net::Ipv6Addr>().is_ok() {
        return true;
    }
    host.chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_'))
}

/// A host the TLS layer can validate a certificate against.
///
/// An IP literal is accepted as-is. Otherwise the name must follow the DNS name rules the TLS
/// backend enforces: at most 253 bytes, labels of 1-63 bytes made of letters, digits, `-` and `_`,
/// no label that starts or ends with `-`, and a final label that is not all digits. This mirrors
/// the backend so a bad name fails as `invalid_config` before the guard or a connection.
fn valid_tls_server_name(host: &str) -> bool {
    if host.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    let name = host.strip_suffix('.').unwrap_or(host);
    if name.is_empty() || name.len() > MAX_SMTP_HOST_CHARS {
        return false;
    }
    let mut labels = name.split('.').peekable();
    while let Some(label) = labels.next() {
        if !valid_dns_label(label) {
            return false;
        }
        if labels.peek().is_none() {
            return !label.chars().all(|character| character.is_ascii_digit());
        }
    }
    false
}

fn valid_dns_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 63
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

/// Whether the test-only plaintext seam is enabled by the environment.
///
/// It is not enough on its own: the address the guard actually resolved must also be loopback.
fn plaintext_test_enabled() -> bool {
    std::env::var(PLAINTEXT_TEST_ENV).is_ok_and(|value| value.trim() == "1")
}

/// Maps the SMTP client error onto the host's delivery classification.
///
/// A timeout, a transient 4xx reply and a relay that drops the connection ask for a later attempt;
/// a permanent 5xx reply, a TLS failure or an incompatible server is a configuration or provider
/// failure. `lettre`'s rustls backend reports a TLS handshake or certificate problem as a
/// connection error and reports an early end of the dialogue as a response error, so the error
/// source chain is inspected for both classes before the plain response/client mapping.
fn classify_smtp_error(error: &lettre::transport::smtp::Error) -> NotificationSendRpcResult {
    if error.is_timeout() {
        return retryable(SMTP_TIMEOUT);
    }
    if error.is_transient() {
        return retryable(SMTP_TRANSIENT);
    }
    if error.is_permanent() {
        return failed(SMTP_REJECTED);
    }
    if error.is_tls() || error_matches(error, &TLS_FAILURE_MARKERS) {
        return failed(SMTP_TLS_FAILED);
    }
    if error_matches(error, &INTERRUPTED_CONNECTION_MARKERS) {
        return retryable(SMTP_UNAVAILABLE);
    }
    if error.is_client() || error.is_response() {
        return failed(SMTP_PROTOCOL_ERROR);
    }
    retryable(SMTP_UNAVAILABLE)
}

/// Lower-case fragments the rustls (and TLS in general) error messages use for a handshake or a
/// certificate rejection.
const TLS_FAILURE_MARKERS: [&str; 9] = [
    "handshake",
    "certificate",
    "corrupt message",
    "peer sent no certificates",
    "peer is incompatible",
    "received fatal alert",
    "decrypt error",
    "invalid content type",
    "unknown issuer",
];

/// Lower-case fragments of an interrupted dialogue.
///
/// `lettre` reports an early end of the SMTP conversation as a response error even though the
/// network peer dropped the connection, so it belongs with the retryable network failures.
const INTERRUPTED_CONNECTION_MARKERS: [&str; 4] = [
    "incomplete response",
    "connection closed",
    "connection reset",
    "broken pipe",
];

/// Checks the error and its source chain for one of the given message fragments.
///
/// Without taking a TLS dependency, the plugin recognises wrapped backend failures by their stable
/// message fragments instead of their crate-private types.
fn error_matches(error: &lettre::transport::smtp::Error, markers: &[&str]) -> bool {
    let mut current: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(source) = current {
        let message = source.to_string().to_ascii_lowercase();
        if markers.iter().any(|marker| message.contains(marker)) {
            return true;
        }
        current = source.source();
    }
    false
}

/// Writes one diagnostic line per attempt to stderr.
///
/// The line carries the event ID, the transport and port, the classification and the stable error
/// code. It never carries the credential, a recipient address, the relay host or any complete
/// external URL: the destination guard contract forbids logging the destination itself.
fn log_outcome(
    request: &NotificationSendRpcRequest,
    config: Option<&EmailConfig>,
    result: &NotificationSendRpcResult,
) {
    let event_id = request
        .event
        .get("eventId")
        .and_then(Value::as_str)
        .map(log_label)
        .unwrap_or_else(|| "-".to_owned());
    let transport = config
        .map(|config| {
            format!(
                " transport={} port={}",
                config.security.label(),
                config.port
            )
        })
        .unwrap_or_default();
    eprintln!(
        "{PLUGIN_ID}: event {event_id}{transport} finished with {} ({})",
        status_label(result.status),
        result.error_code.as_deref().unwrap_or("none")
    );
}

fn status_label(status: NotificationSendStatus) -> &'static str {
    match status {
        NotificationSendStatus::Delivered => "DELIVERED",
        NotificationSendStatus::Retryable => "RETRYABLE",
        NotificationSendStatus::Failed => "FAILED",
    }
}

/// Keeps an untrusted event ID bounded and free of control characters in the log.
fn log_label(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(64)
        .collect()
}
