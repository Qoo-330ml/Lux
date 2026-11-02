//! Behaviour of the `org.lux.notifier.email` SMTP notifier plugin process.
//!
//! Every test drives the real binary over its stdio JSON-RPC protocol against a local fake SMTP
//! server bound to `127.0.0.1` on a random port, so no test reaches a real mail provider and no
//! real credential is ever used. The tests also assert that the per-target credential only travels
//! in the `secret` RPC field: it must never appear in a result or in the captured plugin log.

use std::{
    io,
    net::SocketAddr,
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use luxd::application::plugin_protocol::{
    NotificationSendRpcResult, NotificationSendStatus, PluginRequest, PluginResponse,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines},
    net::{TcpListener, TcpStream},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
};

/// Test-only credential. It must never appear in a result or the plugin log.
const TEST_SECRET: &str = "smtp-test-password-1";
/// A second, clearly fake credential used for the rejected-authentication cases.
const REJECTED_SECRET: &str = "smtp-test-password-rejected";
/// Test-only timeout knob, clamped by the plugin to at most the SDK's delivery window.
const TIMEOUT_ENV: &str = "LUX_PLUGIN_SMTP_TIMEOUT_SECONDS";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SmtpReply {
    /// Full happy path: accept the credential, both recipients and the message.
    Deliver,
    /// Transient 4xx reply for the first recipient.
    TransientRecipient,
    /// Permanent 5xx reply for the first recipient.
    PermanentRecipient,
    /// Permanent 5xx reply to `AUTH`.
    AuthRejected,
    /// Accept the connection but never answer, so the client must hit its own timeout.
    Silent,
}

#[derive(Default)]
struct Received {
    auth_commands: Vec<String>,
    recipients: Vec<String>,
    body: Vec<String>,
}

/// A minimal SMTP server that only speaks what `lettre` needs for one delivery.
struct FakeSmtp {
    address: SocketAddr,
    received: Arc<Mutex<Received>>,
}

impl FakeSmtp {
    async fn start(reply: SmtpReply) -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let received = Arc::new(Mutex::new(Received::default()));
        let shared = Arc::clone(&received);
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let shared = Arc::clone(&shared);
                tokio::spawn(async move {
                    let _ = serve(stream, reply, shared).await;
                });
            }
        });
        Ok(Self { address, received })
    }

    fn auth_commands(&self) -> Vec<String> {
        self.received
            .lock()
            .expect("fake SMTP state lock")
            .auth_commands
            .clone()
    }

    fn recipients(&self) -> Vec<String> {
        self.received
            .lock()
            .expect("fake SMTP state lock")
            .recipients
            .clone()
    }

    fn body(&self) -> String {
        self.received
            .lock()
            .expect("fake SMTP state lock")
            .body
            .join("\n")
    }
}

async fn serve(
    mut stream: TcpStream,
    reply: SmtpReply,
    received: Arc<Mutex<Received>>,
) -> io::Result<()> {
    if reply == SmtpReply::Silent {
        // Read and discard whatever the client sends; never answer.
        let mut buffer = [0_u8; 1024];
        loop {
            if stream.read(&mut buffer).await? == 0 {
                return Ok(());
            }
        }
    }

    let (reader, mut writer) = stream.split();
    let mut lines = BufReader::new(reader).lines();
    writer.write_all(b"220 fake.smtp.test ESMTP\r\n").await?;
    let mut in_data = false;
    while let Some(line) = lines.next_line().await? {
        if in_data {
            if line == "." {
                in_data = false;
                writer.write_all(b"250 2.0.0 queued\r\n").await?;
            } else {
                received
                    .lock()
                    .expect("fake SMTP state lock")
                    .body
                    .push(line);
            }
            continue;
        }
        let upper = line.to_ascii_uppercase();
        if upper.starts_with("EHLO") || upper.starts_with("HELO") {
            // No STARTTLS extension on purpose: the security tests build a STARTTLS transport
            // against this plain server.
            writer
                .write_all(
                    b"250-fake.smtp.test\r\n250-8BITMIME\r\n250-AUTH PLAIN LOGIN\r\n250 SIZE 10485760\r\n",
                )
                .await?;
        } else if upper.starts_with("AUTH") {
            received
                .lock()
                .expect("fake SMTP state lock")
                .auth_commands
                .push(line.clone());
            if reply == SmtpReply::AuthRejected {
                writer
                    .write_all(b"535 5.7.8 authentication credentials invalid\r\n")
                    .await?;
            } else {
                writer
                    .write_all(b"235 2.7.0 authentication successful\r\n")
                    .await?;
            }
        } else if upper.starts_with("MAIL FROM") {
            writer.write_all(b"250 2.1.0 sender ok\r\n").await?;
        } else if upper.starts_with("RCPT TO") {
            received
                .lock()
                .expect("fake SMTP state lock")
                .recipients
                .push(line.clone());
            match reply {
                SmtpReply::TransientRecipient => {
                    writer.write_all(b"451 4.3.0 try again later\r\n").await?
                }
                SmtpReply::PermanentRecipient => {
                    writer.write_all(b"550 5.1.1 no such user here\r\n").await?
                }
                _ => writer.write_all(b"250 2.1.5 recipient ok\r\n").await?,
            }
        } else if upper == "DATA" {
            in_data = true;
            writer
                .write_all(b"354 end data with <CR><LF>.<CR><LF>\r\n")
                .await?;
        } else if upper == "QUIT" {
            writer.write_all(b"221 2.0.0 bye\r\n").await?;
            return Ok(());
        } else {
            writer.write_all(b"250 2.0.0 ok\r\n").await?;
        }
    }
    Ok(())
}

/// One running plugin process driven over its stdio protocol.
struct Session {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    stderr: Option<ChildStderr>,
}

impl Session {
    async fn start(timeout_seconds: Option<u64>) -> io::Result<Self> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_lux-plugin-notifier-email"));
        command
            .env_remove("LUX_PROXY_URL")
            .env_remove(TIMEOUT_ENV)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(seconds) = timeout_seconds {
            command.env(TIMEOUT_ENV, seconds.to_string());
        }
        let mut child = command.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("plugin stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("plugin stdout unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("plugin stderr unavailable"))?;
        Ok(Self {
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            stderr: Some(stderr),
        })
    }

    async fn send(&mut self, id: &str, method: &str, params: Value) -> io::Result<PluginResponse> {
        let line = serde_json::to_vec(&PluginRequest::new(id, method, params))
            .map_err(io::Error::other)?;
        self.stdin.write_all(&line).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;
        let line = self
            .lines
            .next_line()
            .await?
            .ok_or_else(|| io::Error::other("plugin exited before answering"))?;
        serde_json::from_str(&line).map_err(io::Error::other)
    }

    async fn notify(&mut self, params: Value) -> io::Result<NotificationSendRpcResult> {
        let response = self.send("deliver", "notification.send", params).await?;
        let result = response.result.ok_or_else(|| {
            io::Error::other(format!("notification.send failed: {:?}", response.error))
        })?;
        serde_json::from_value(result).map_err(io::Error::other)
    }

    /// Shuts the plugin down and returns everything it wrote to stderr.
    async fn shutdown(mut self) -> io::Result<String> {
        let _ = self.send("shutdown", "plugin.shutdown", json!({})).await;
        let mut stderr = self
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("plugin stderr unavailable"))?;
        let mut log = String::new();
        stderr.read_to_string(&mut log).await?;
        let _ = self.child.wait().await;
        Ok(log)
    }
}

fn email_config(address: SocketAddr, security: &str, username: Option<&str>) -> Value {
    let mut config = json!({
        "smtpHost": address.ip().to_string(),
        "smtpPort": address.port(),
        "security": security,
        "from": "lux@example.invalid",
        "to": "ops@example.invalid, oncall@example.invalid",
    });
    if let Some(username) = username {
        config["username"] = json!(username);
    }
    config
}

fn send_params(config: Value, secret: Option<&str>) -> Value {
    let mut params = json!({
        "event": {
            "schemaVersion": 1,
            "eventId": "event-1",
            "eventType": "MEDIA_ADDED",
            "occurredAt": 1_700_000_000_i64,
            "serverId": "server-1",
            "data": {
                "title": "媒体新增",
                "content": "新增媒体：2 个\n媒体库：电影",
                "source": "lux",
                "timestamp": "2023-11-14T22:13:20Z"
            }
        },
        "target": {"url": "", "allowPrivateNetwork": false},
        "config": config,
    });
    if let Some(secret) = secret {
        params["secret"] = json!(secret);
    }
    params
}

fn error_code(result: &NotificationSendRpcResult) -> &str {
    result.error_code.as_deref().unwrap_or_default()
}

fn decode_base64(value: &str) -> String {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let compact: String = value
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    STANDARD
        .decode(compact)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_default()
}

/// Decodes the RFC 2047 encoded `Subject` header of the captured message.
fn decode_subject(raw: &str) -> String {
    let subject = raw
        .lines()
        .find_map(|line| line.strip_prefix("Subject: "))
        .unwrap_or_default()
        .trim();
    for prefix in ["=?utf-8?b?", "=?UTF-8?b?"] {
        if let Some(encoded) = subject.strip_prefix(prefix)
            && let Some(encoded) = encoded.strip_suffix("?=")
        {
            return decode_base64(encoded);
        }
    }
    subject.to_owned()
}

/// Decodes the captured message body according to its transfer encoding.
fn decode_message_body(raw: &str) -> String {
    let Some((headers, body)) = raw.split_once("\n\n") else {
        return raw.to_owned();
    };
    let encoding = headers
        .lines()
        .find_map(|line| line.strip_prefix("Content-Transfer-Encoding: "))
        .unwrap_or("7bit")
        .trim()
        .to_ascii_lowercase();
    if encoding == "base64" {
        decode_base64(body)
    } else {
        body.to_owned()
    }
}

fn assert_no_secret_leak(result: &NotificationSendRpcResult, log: &str) {
    let encoded = serde_json::to_string(result).expect("result must serialize");
    assert!(
        !encoded.contains(TEST_SECRET) && !encoded.contains(REJECTED_SECRET),
        "the credential must never reach the RPC result: {encoded}"
    );
    assert!(
        !log.contains(TEST_SECRET) && !log.contains(REJECTED_SECRET),
        "the credential must never reach the plugin log: {log}"
    );
    assert!(
        !log.contains("smtp://") && !log.contains("smtps://"),
        "the plugin log must not contain a complete external URL: {log}"
    );
    assert!(
        log.contains("org.lux.notifier.email"),
        "the plugin must log a stable diagnostic line, got: {log}"
    );
}

#[tokio::test]
async fn delivers_a_plain_text_message_over_smtp() -> Result<(), Box<dyn std::error::Error>> {
    let server = FakeSmtp::start(SmtpReply::Deliver).await?;
    let mut session = Session::start(None).await?;

    let result = session
        .notify(send_params(
            email_config(server.address, "none", Some("lux@example.invalid")),
            Some(TEST_SECRET),
        ))
        .await?;

    assert_eq!(result.status, NotificationSendStatus::Delivered);
    assert_eq!(result.error_code, None);

    let recipients = server.recipients();
    assert_eq!(
        recipients.len(),
        2,
        "both destinations must receive one RCPT TO each: {recipients:?}"
    );
    assert!(recipients[0].contains("ops@example.invalid"));
    assert!(recipients[1].contains("oncall@example.invalid"));

    let auth = server.auth_commands();
    assert_eq!(auth.len(), 1, "the configured account must authenticate");
    assert!(
        auth[0].starts_with("AUTH "),
        "unexpected AUTH command: {}",
        auth[0]
    );

    let body = server.body();
    assert_eq!(
        decode_subject(&body),
        "媒体新增",
        "the message must carry the core title as its subject: {body}"
    );
    let decoded_body = decode_message_body(&body);
    assert!(
        decoded_body.contains("新增媒体：2 个"),
        "the message must carry the core content as its body: {body}"
    );
    assert!(
        decoded_body.contains("媒体库：电影"),
        "the message must carry the full core content, not a rewritten summary: {body}"
    );

    let log = session.shutdown().await?;
    assert!(
        log.contains("DELIVERED"),
        "the plugin must log the delivery classification, got: {log}"
    );
    assert_no_secret_leak(&result, &log);
    Ok(())
}

#[tokio::test]
async fn delivers_without_authentication_when_no_username_is_configured()
-> Result<(), Box<dyn std::error::Error>> {
    let server = FakeSmtp::start(SmtpReply::Deliver).await?;
    let mut session = Session::start(None).await?;

    let result = session
        .notify(send_params(
            email_config(server.address, "none", None),
            // The host always requires a non-empty secret; without a username it is a placeholder
            // the plugin must not use for authentication.
            Some(TEST_SECRET),
        ))
        .await?;

    assert_eq!(result.status, NotificationSendStatus::Delivered);
    assert!(
        server.auth_commands().is_empty(),
        "an empty username must skip SMTP authentication"
    );

    let log = session.shutdown().await?;
    assert_no_secret_leak(&result, &log);
    Ok(())
}

#[tokio::test]
async fn a_missing_or_empty_secret_is_a_permanent_failure() -> Result<(), Box<dyn std::error::Error>>
{
    let server = FakeSmtp::start(SmtpReply::Deliver).await?;
    let mut session = Session::start(None).await?;
    let config = email_config(server.address, "none", Some("lux@example.invalid"));

    let missing = session.notify(send_params(config.clone(), None)).await?;
    assert_eq!(missing.status, NotificationSendStatus::Failed);
    assert_eq!(error_code(&missing), "missing_secret");

    let empty = session.notify(send_params(config, Some("   "))).await?;
    assert_eq!(empty.status, NotificationSendStatus::Failed);
    assert_eq!(error_code(&empty), "missing_secret");
    assert!(
        server.recipients().is_empty(),
        "the plugin must not contact the SMTP server without a credential"
    );

    let log = session.shutdown().await?;
    assert_no_secret_leak(&missing, &log);
    assert_no_secret_leak(&empty, &log);
    Ok(())
}

#[tokio::test]
async fn an_invalid_configuration_is_a_permanent_failure_with_a_stable_code()
-> Result<(), Box<dyn std::error::Error>> {
    let server = FakeSmtp::start(SmtpReply::Deliver).await?;
    let mut session = Session::start(None).await?;
    let host = server.address.ip().to_string();
    let port = server.address.port();
    let too_many_recipients = (0..21)
        .map(|index| format!("user{index}@example.invalid"))
        .collect::<Vec<_>>()
        .join(",");
    let configurations = [
        (
            "missing host",
            json!({"smtpPort": port, "from": "lux@example.invalid", "to": "ops@example.invalid"}),
        ),
        (
            "empty host",
            json!({"smtpHost": "", "smtpPort": port, "from": "lux@example.invalid", "to": "ops@example.invalid"}),
        ),
        (
            "host with whitespace",
            json!({"smtpHost": format!("{host} "), "smtpPort": port, "from": "lux@example.invalid", "to": "ops@example.invalid"}),
        ),
        (
            "unknown security",
            json!({"smtpHost": host, "smtpPort": port, "security": "tls1", "from": "lux@example.invalid", "to": "ops@example.invalid"}),
        ),
        (
            "zero port",
            json!({"smtpHost": host, "smtpPort": 0, "from": "lux@example.invalid", "to": "ops@example.invalid"}),
        ),
        (
            "missing from",
            json!({"smtpHost": host, "smtpPort": port, "to": "ops@example.invalid"}),
        ),
        (
            "invalid from",
            json!({"smtpHost": host, "smtpPort": port, "from": "not-an-address", "to": "ops@example.invalid"}),
        ),
        (
            "no recipients",
            json!({"smtpHost": host, "smtpPort": port, "from": "lux@example.invalid", "to": " , "}),
        ),
        (
            "invalid recipient",
            json!({"smtpHost": host, "smtpPort": port, "from": "lux@example.invalid", "to": "ops@example.invalid, nope"}),
        ),
        (
            "too many recipients",
            json!({"smtpHost": host, "smtpPort": port, "from": "lux@example.invalid", "to": too_many_recipients}),
        ),
    ];

    for (label, config) in configurations {
        let result = session
            .notify(send_params(config, Some(TEST_SECRET)))
            .await?;
        assert_eq!(
            result.status,
            NotificationSendStatus::Failed,
            "{label} must be rejected as a permanent failure"
        );
        assert_eq!(error_code(&result), "invalid_config", "{label}");
    }
    assert!(
        server.recipients().is_empty(),
        "an invalid configuration must never reach the SMTP server"
    );

    let log = session.shutdown().await?;
    assert_no_secret_leak(
        &NotificationSendRpcResult {
            status: NotificationSendStatus::Failed,
            provider_request_id: None,
            retry_after_seconds: None,
            error_code: Some("invalid_config".to_owned()),
        },
        &log,
    );
    Ok(())
}

#[tokio::test]
async fn a_transient_smtp_reply_is_retryable() -> Result<(), Box<dyn std::error::Error>> {
    let server = FakeSmtp::start(SmtpReply::TransientRecipient).await?;
    let mut session = Session::start(None).await?;

    let result = session
        .notify(send_params(
            email_config(server.address, "none", Some("lux@example.invalid")),
            Some(TEST_SECRET),
        ))
        .await?;

    assert_eq!(result.status, NotificationSendStatus::Retryable);
    assert_eq!(error_code(&result), "smtp_transient");

    let log = session.shutdown().await?;
    assert_no_secret_leak(&result, &log);
    Ok(())
}

#[tokio::test]
async fn permanent_smtp_replies_are_failures() -> Result<(), Box<dyn std::error::Error>> {
    for reply in [SmtpReply::PermanentRecipient, SmtpReply::AuthRejected] {
        let server = FakeSmtp::start(reply).await?;
        let mut session = Session::start(None).await?;

        let result = session
            .notify(send_params(
                email_config(server.address, "none", Some("lux@example.invalid")),
                Some(REJECTED_SECRET),
            ))
            .await?;

        assert_eq!(
            result.status,
            NotificationSendStatus::Failed,
            "{reply:?} must be a permanent failure"
        );
        assert_eq!(error_code(&result), "smtp_rejected", "{reply:?}");

        let log = session.shutdown().await?;
        assert_no_secret_leak(&result, &log);
    }
    Ok(())
}

#[tokio::test]
async fn an_unreachable_smtp_server_is_retryable() -> Result<(), Box<dyn std::error::Error>> {
    // Reserve a port and release it so the connection is refused immediately.
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    drop(listener);

    let mut session = Session::start(None).await?;
    let result = session
        .notify(send_params(
            email_config(address, "none", None),
            Some(TEST_SECRET),
        ))
        .await?;

    assert_eq!(result.status, NotificationSendStatus::Retryable);
    assert_eq!(error_code(&result), "smtp_unavailable");

    let log = session.shutdown().await?;
    assert_no_secret_leak(&result, &log);
    Ok(())
}

#[tokio::test]
async fn a_silent_smtp_server_ticks_the_delivery_timeout() -> Result<(), Box<dyn std::error::Error>>
{
    let server = FakeSmtp::start(SmtpReply::Silent).await?;
    let mut session = Session::start(Some(1)).await?;

    let started = Instant::now();
    let result = session
        .notify(send_params(
            email_config(server.address, "none", Some("lux@example.invalid")),
            Some(TEST_SECRET),
        ))
        .await?;
    let elapsed = started.elapsed();

    assert_eq!(result.status, NotificationSendStatus::Retryable);
    assert_eq!(error_code(&result), "smtp_timeout");
    assert!(
        elapsed < Duration::from_secs(5),
        "the plugin must bound its own delivery attempt, took {elapsed:?}"
    );

    let log = session.shutdown().await?;
    assert_no_secret_leak(&result, &log);
    Ok(())
}

#[tokio::test]
async fn the_transport_security_modes_are_validated_before_relaying()
-> Result<(), Box<dyn std::error::Error>> {
    // STARTTLS: the fake server never advertises the extension, so the transport must refuse to
    // downgrade instead of silently sending plaintext.
    let server = FakeSmtp::start(SmtpReply::Deliver).await?;
    let mut session = Session::start(None).await?;
    let starttls = session
        .notify(send_params(
            email_config(server.address, "starttls", Some("lux@example.invalid")),
            Some(TEST_SECRET),
        ))
        .await?;
    assert_eq!(starttls.status, NotificationSendStatus::Failed);
    assert_eq!(error_code(&starttls), "smtp_protocol_error");
    assert!(
        server.auth_commands().is_empty() && server.recipients().is_empty(),
        "a STARTTLS requirement must fail before any credential or recipient is sent"
    );

    // SMTPS: a plaintext greeting cannot answer a TLS handshake. `lettre` reports a rustls
    // handshake failure as a connection error, so the plugin keeps it retryable instead of
    // claiming a delivery, and it still never sends the credential to an unencrypted peer.
    let server = FakeSmtp::start(SmtpReply::Deliver).await?;
    let smtps = session
        .notify(send_params(
            email_config(server.address, "ssl", Some("lux@example.invalid")),
            Some(TEST_SECRET),
        ))
        .await?;
    assert_eq!(smtps.status, NotificationSendStatus::Retryable);
    assert_eq!(error_code(&smtps), "smtp_unavailable");
    assert!(
        server.auth_commands().is_empty() && server.recipients().is_empty(),
        "an SSL requirement must fail before any credential or recipient is sent"
    );

    let log = session.shutdown().await?;
    assert_no_secret_leak(&starttls, &log);
    assert_no_secret_leak(&smtps, &log);
    Ok(())
}

#[tokio::test]
async fn unknown_methods_are_reported_without_stopping_the_plugin()
-> Result<(), Box<dyn std::error::Error>> {
    let mut session = Session::start(None).await?;

    let response = session.send("ping", "plugin.health", json!({})).await?;
    assert_eq!(
        response.error.map(|error| error.code).as_deref(),
        Some("PLUGIN_UNSUPPORTED_METHOD")
    );

    let server = FakeSmtp::start(SmtpReply::Deliver).await?;
    let result = session
        .notify(send_params(
            email_config(server.address, "none", None),
            Some(TEST_SECRET),
        ))
        .await?;
    assert_eq!(result.status, NotificationSendStatus::Delivered);

    let log = session.shutdown().await?;
    assert_no_secret_leak(&result, &log);
    Ok(())
}
