//! End-to-end tests for the Bark notification plugin (`org.lux.notifier.bark`).
//!
//! Every test drives the real plugin process over the Lux stdio JSON-RPC protocol and points the
//! plugin at a local mock Bark server on `127.0.0.1`. No test reaches a real Bark deployment and no
//! real credential appears anywhere in this file.

use std::{
    net::SocketAddr,
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderValue, Method, StatusCode, Uri, header},
    response::Response,
    routing::any,
};
use luxd::application::plugin_protocol::{
    NotificationSendRpcResult, NotificationSendStatus, PluginManifest, PluginRequest,
    PluginResponse,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
    task::JoinHandle,
    time::sleep,
};

const PLUGIN_ID: &str = "org.lux.notifier.bark";
/// A fake device key in the Bark format. Never a real credential.
const DEVICE_KEY: &str = "device_key-1";

struct Plugin {
    child: Child,
    stdin: ChildStdin,
    lines: tokio::io::Lines<BufReader<ChildStdout>>,
    stderr: ChildStderr,
}

impl Plugin {
    fn spawn() -> Result<Self, Box<dyn std::error::Error>> {
        Self::spawn_with_environment(&[])
    }

    /// Spawns the plugin with a clean proxy environment plus explicit overrides.
    ///
    /// A notification plugin never receives `LUX_CONFIG_DIR`, so the plugin must not depend on it.
    /// Proxy variables are cleared (and `NO_PROXY` is left unset, so a test that sets a proxy really
    /// exercises it) because the loopback mock server must always be reached directly.
    fn spawn_with_environment(
        overrides: &[(&str, &str)],
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_lux-plugin-notifier-bark"));
        command
            .env_remove("LUX_CONFIG_DIR")
            .env_remove("LUX_PROXY_URL")
            .env_remove("NO_PROXY")
            .env_remove("no_proxy");
        for variable in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            command.env_remove(variable);
        }
        for (key, value) in overrides {
            command.env(key, value);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let stdin = child.stdin.take().ok_or("plugin stdin unavailable")?;
        let stdout = child.stdout.take().ok_or("plugin stdout unavailable")?;
        let stderr = child.stderr.take().ok_or("plugin stderr unavailable")?;
        Ok(Self {
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            stderr,
        })
    }

    async fn call(
        &mut self,
        id: &str,
        method: &str,
        params: Value,
    ) -> Result<PluginResponse, Box<dyn std::error::Error>> {
        let request = PluginRequest::new(id, method, params);
        let mut bytes = serde_json::to_vec(&request).map_err(std::io::Error::other)?;
        bytes.push(b'\n');
        self.stdin.write_all(&bytes).await?;
        self.stdin.flush().await?;
        let line = self
            .lines
            .next_line()
            .await?
            .ok_or("plugin exited without a response")?;
        Ok(serde_json::from_str(&line)?)
    }

    async fn send(
        &mut self,
        params: Value,
    ) -> Result<NotificationSendRpcResult, Box<dyn std::error::Error>> {
        self.send_as("send-1", params).await
    }

    async fn send_as(
        &mut self,
        id: &str,
        params: Value,
    ) -> Result<NotificationSendRpcResult, Box<dyn std::error::Error>> {
        let response = self.call(id, "notification.send", params).await?;
        if let Some(error) = response.error {
            return Err(format!("plugin RPC error {}: {}", error.code, error.message).into());
        }
        let result = response.result.ok_or("plugin returned no result")?;
        Ok(serde_json::from_value(result)?)
    }

    /// Sends `plugin.shutdown`, waits for the exit and returns everything the plugin logged.
    async fn finish(mut self) -> Result<String, Box<dyn std::error::Error>> {
        let _ = self.call("shutdown", "plugin.shutdown", json!({})).await?;
        let _ = self.child.wait().await?;
        let mut stderr = String::new();
        self.stderr.read_to_string(&mut stderr).await?;
        Ok(stderr)
    }
}

/// The `notification.send` params the host builds, including the credential only when it exists.
///
/// The loopback mock server is a private destination, so the default fixtures carry the host's
/// private-network approval; a test that exercises the default policy passes `false` explicitly.
fn notification_params(config: Value, secret: Option<&str>) -> Value {
    notification_params_with_approval(config, secret, true)
}

fn notification_params_with_approval(
    config: Value,
    secret: Option<&str>,
    allow_private_network: bool,
) -> Value {
    let mut params = json!({
        "event": {
            "schemaVersion": 1,
            "eventId": "event-1",
            "eventType": "MEDIA_ADDED",
            "occurredAt": 1_700_000_000_i64,
            "serverId": "server-1",
            "data": {
                "libraryId": "library-1",
                "addedCount": 2,
                "source": "lux",
                "title": "媒体新增",
                "content": "新增媒体：2 个\n媒体库：电影",
                "body": "新增媒体：2 个\n媒体库：电影",
                "timestamp": "2023-11-14T22:13:20Z"
            }
        },
        "target": {"url": "", "allowPrivateNetwork": allow_private_network},
        "config": config,
    });
    if let Some(secret) = secret {
        params["secret"] = json!(secret);
    }
    params
}

fn manifest() -> Result<PluginManifest, Box<dyn std::error::Error>> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("plugins")
        .join(PLUGIN_ID)
        .join("manifest.json");
    let bytes = std::fs::read(path)?;
    Ok(PluginManifest::from_value(serde_json::from_slice(&bytes)?)?)
}

#[test]
fn manifest_declares_one_bark_secret_field_and_non_secret_target_settings()
-> Result<(), Box<dyn std::error::Error>> {
    let manifest = manifest()?;

    assert_eq!(manifest.id, PLUGIN_ID);
    assert_eq!(manifest.plugin_type, "notification");
    assert_eq!(manifest.category, "NOTIFICATION");
    assert_eq!(manifest.capabilities, vec!["notification.send".to_owned()]);
    assert_eq!(manifest.version, "0.1.0");
    assert_eq!(
        manifest.runtime.entrypoint,
        "binaries/${platform}-${arch}/lux-plugin-notifier-bark"
    );

    let keys = manifest
        .config_fields
        .iter()
        .map(|field| field.key.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        keys,
        vec!["secret", "serverUrl", "group", "level", "icon", "sound"]
    );

    let secret = manifest
        .secret_config_field()
        .ok_or("the manifest must declare the secret field")?;
    assert_eq!(secret.input_type, "password");
    assert!(secret.sensitive);
    assert!(secret.required);
    assert!(secret.default_value.is_none());

    for field in manifest
        .config_fields
        .iter()
        .filter(|field| field.key != "secret")
    {
        assert!(!field.sensitive, "{} must stay non-sensitive", field.key);
        assert_ne!(
            field.input_type, "password",
            "{} must stay non-secret",
            field.key
        );
    }

    let server_url = manifest
        .config_fields
        .iter()
        .find(|field| field.key == "serverUrl")
        .ok_or("serverUrl must be declared")?;
    assert_eq!(server_url.default_value, Some(json!("https://api.day.app")));

    let level = manifest
        .config_fields
        .iter()
        .find(|field| field.key == "level")
        .ok_or("level must be declared")?;
    assert_eq!(level.input_type, "select");
    assert_eq!(level.default_value, Some(json!("active")));
    assert_eq!(
        level
            .options
            .iter()
            .map(|option| option.value.as_str())
            .collect::<Vec<_>>(),
        vec!["active", "timeSensitive", "passive"]
    );

    Ok(())
}

#[tokio::test]
async fn fails_with_missing_secret_without_contacting_the_provider()
-> Result<(), Box<dyn std::error::Error>> {
    let mut plugin = Plugin::spawn()?;
    let result = plugin
        .send(notification_params(
            json!({"serverUrl": "http://127.0.0.1:1/base"}),
            None,
        ))
        .await?;

    assert_eq!(result.status, NotificationSendStatus::Failed);
    assert_eq!(result.error_code.as_deref(), Some("missing_secret"));
    assert_eq!(result.retry_after_seconds, None);
    assert_eq!(result.provider_request_id, None);

    let stderr = plugin.finish().await?;
    assert!(
        !stderr.contains("127.0.0.1"),
        "the plugin must not log the configured provider address: {stderr}"
    );
    Ok(())
}

#[tokio::test]
async fn fails_with_a_blank_secret() -> Result<(), Box<dyn std::error::Error>> {
    let mut plugin = Plugin::spawn()?;
    let result = plugin
        .send(notification_params(
            json!({"serverUrl": "http://127.0.0.1:1/base"}),
            Some("   "),
        ))
        .await?;

    assert_eq!(result.status, NotificationSendStatus::Failed);
    assert_eq!(result.error_code.as_deref(), Some("missing_secret"));
    let _ = plugin.finish().await?;
    Ok(())
}

#[tokio::test]
async fn rejects_a_device_key_outside_the_bark_format_without_echoing_it()
-> Result<(), Box<dyn std::error::Error>> {
    let mut plugin = Plugin::spawn()?;
    let oversized = "a".repeat(129);
    for (id, secret) in [
        ("spaces", "device key"),
        ("slash", "device/key"),
        ("long", &oversized),
    ] {
        let result = plugin
            .send_as(
                id,
                notification_params(
                    json!({"serverUrl": "http://127.0.0.1:1/base"}),
                    Some(secret),
                ),
            )
            .await?;
        assert_eq!(result.status, NotificationSendStatus::Failed, "{id}");
        assert_eq!(result.error_code.as_deref(), Some("invalid_secret"), "{id}");
        let encoded = serde_json::to_string(&result)?;
        assert!(
            !encoded.contains(secret),
            "{id} must not echo the credential"
        );
    }

    let stderr = plugin.finish().await?;
    assert!(!stderr.contains("device key"));
    assert!(!stderr.contains("device/key"));
    assert!(!stderr.contains(oversized.as_str()));
    Ok(())
}

/// What the local mock Bark server answers with.
#[derive(Clone, Debug)]
struct PushBehaviour {
    status: StatusCode,
    body: Value,
    retry_after: Option<&'static str>,
    location: Option<String>,
}

/// Everything the local mock Bark server received.
#[derive(Clone, Debug)]
struct RecordedPush {
    method: String,
    path: String,
    body: Value,
}

#[derive(Clone, Default)]
struct PushRecorder {
    requests: Arc<Mutex<Vec<RecordedPush>>>,
}

impl PushRecorder {
    fn requests(&self) -> Vec<RecordedPush> {
        self.requests
            .lock()
            .expect("push recorder must not be poisoned")
            .clone()
    }

    fn len(&self) -> usize {
        self.requests().len()
    }
}

type PushState = (PushBehaviour, PushRecorder);

async fn record_and_respond(
    State((behaviour, recorder)): State<PushState>,
    method: Method,
    uri: Uri,
    body: Bytes,
) -> Response {
    let parsed = serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null);
    recorder
        .requests
        .lock()
        .expect("push recorder must not be poisoned")
        .push(RecordedPush {
            method: method.as_str().to_owned(),
            path: uri.path().to_owned(),
            body: parsed,
        });

    let payload = serde_json::to_vec(&behaviour.body).unwrap_or_default();
    let mut response = Response::new(axum::body::Body::from(payload));
    *response.status_mut() = behaviour.status;
    if let Some(retry_after) = behaviour.retry_after {
        response.headers_mut().insert(
            header::RETRY_AFTER,
            HeaderValue::from_str(retry_after).expect("retry-after header value"),
        );
    }
    if let Some(location) = behaviour.location.as_deref() {
        response.headers_mut().insert(
            header::LOCATION,
            HeaderValue::from_str(location).expect("location header value"),
        );
    }
    response
}

/// Starts a mock Bark server on a random loopback port.
async fn spawn_push_server(
    behaviour: PushBehaviour,
) -> Result<(u16, PushRecorder, JoinHandle<()>), Box<dyn std::error::Error>> {
    let recorder = PushRecorder::default();
    let app = Router::new()
        .route("/push", any(record_and_respond))
        .fallback(record_and_respond)
        .with_state((behaviour, recorder.clone()));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok((port, recorder, server))
}

fn bark_success() -> PushBehaviour {
    PushBehaviour {
        status: StatusCode::OK,
        body: json!({"code": 200, "message": "success", "timestamp": 1_700_000_123_i64}),
        retry_after: None,
        location: None,
    }
}

fn bark_status(status: u16) -> PushBehaviour {
    PushBehaviour {
        status: StatusCode::from_u16(status).expect("valid HTTP status"),
        body: json!({"code": status}),
        retry_after: None,
        location: None,
    }
}

/// Runs exactly one `notification.send` against a freshly started plugin process.
async fn run_send(
    config: Value,
    secret: Option<&str>,
) -> Result<(NotificationSendRpcResult, String), Box<dyn std::error::Error>> {
    run_send_with_approval(config, secret, true).await
}

async fn run_send_with_approval(
    config: Value,
    secret: Option<&str>,
    allow_private_network: bool,
) -> Result<(NotificationSendRpcResult, String), Box<dyn std::error::Error>> {
    let mut plugin = Plugin::spawn()?;
    let result = plugin
        .send(notification_params_with_approval(
            config,
            secret,
            allow_private_network,
        ))
        .await?;
    let stderr = plugin.finish().await?;
    Ok((result, stderr))
}

/// Serves one hand-written HTTP response head and body prefix, then optionally stalls the body.
async fn spawn_raw_response(
    prefix: &'static str,
    hold_body_open: bool,
) -> Result<(u16, JoinHandle<()>), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let server = tokio::spawn(async move {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let mut buffer = [0_u8; 8 * 1024];
        let _ = socket.read(&mut buffer).await;
        let _ = socket.write_all(prefix.as_bytes()).await;
        let _ = socket.flush().await;
        if hold_body_open {
            sleep(Duration::from_secs(30)).await;
        }
    });
    Ok((port, server))
}

struct Delivery {
    result: NotificationSendRpcResult,
    requests: Vec<RecordedPush>,
    stderr: String,
}

/// Delivers one notification to a fresh mock server, building the config from its port.
async fn deliver_once(
    behaviour: PushBehaviour,
    config: impl FnOnce(u16) -> Value,
    secret: Option<&str>,
) -> Result<Delivery, Box<dyn std::error::Error>> {
    let (port, recorder, server) = spawn_push_server(behaviour).await?;
    let outcome = run_send(config(port), secret).await;
    server.abort();
    let (result, stderr) = outcome?;
    Ok(Delivery {
        result,
        requests: recorder.requests(),
        stderr,
    })
}

/// The credential must never leave the plugin through a result, an error or a log line.
fn assert_no_credential(delivery: &Delivery, secret: &str) {
    let encoded = serde_json::to_string(&delivery.result).expect("the result must serialize");
    assert!(
        !encoded.contains(secret),
        "the result must not echo the credential: {encoded}"
    );
    assert!(
        !delivery.stderr.contains(secret),
        "the log must not contain the credential: {}",
        delivery.stderr
    );
    assert!(
        !delivery.stderr.contains("http://") && !delivery.stderr.contains("https://"),
        "the log must not contain an outbound URL: {}",
        delivery.stderr
    );
    assert!(
        !delivery.stderr.contains("/push"),
        "the log must not contain the outbound path or query: {}",
        delivery.stderr
    );
}

#[tokio::test]
async fn delivers_a_push_with_the_configured_target_and_core_text()
-> Result<(), Box<dyn std::error::Error>> {
    let delivery = deliver_once(
        bark_success(),
        |port| {
            json!({
                "serverUrl": format!("http://127.0.0.1:{port}/"),
                "group": "Lux",
                "level": "timeSensitive",
                "icon": "https://example.invalid/icon.png",
                "sound": "bell",
            })
        },
        Some(DEVICE_KEY),
    )
    .await?;

    assert_eq!(delivery.result.status, NotificationSendStatus::Delivered);
    assert_eq!(delivery.result.error_code, None);
    assert_eq!(delivery.result.retry_after_seconds, None);
    assert_eq!(
        delivery.result.provider_request_id.as_deref(),
        Some("1700000123")
    );

    assert_eq!(delivery.requests.len(), 1);
    let push = &delivery.requests[0];
    assert_eq!(push.method, "POST");
    assert_eq!(
        push.path, "/push",
        "a trailing slash in serverUrl must not double the path separator"
    );
    assert_eq!(push.body["device_key"], DEVICE_KEY);
    assert_eq!(push.body["title"], "媒体新增");
    assert_eq!(push.body["body"], "新增媒体：2 个\n媒体库：电影");
    assert_eq!(push.body["group"], "Lux");
    assert_eq!(push.body["level"], "timeSensitive");
    assert_eq!(push.body["icon"], "https://example.invalid/icon.png");
    assert_eq!(push.body["sound"], "bell");

    assert_no_credential(&delivery, DEVICE_KEY);
    Ok(())
}

#[tokio::test]
async fn sends_the_default_level_and_omits_unset_optional_fields()
-> Result<(), Box<dyn std::error::Error>> {
    let delivery = deliver_once(
        bark_success(),
        |port| json!({"serverUrl": format!("http://127.0.0.1:{port}")}),
        Some(DEVICE_KEY),
    )
    .await?;

    assert_eq!(delivery.result.status, NotificationSendStatus::Delivered);
    let push = &delivery.requests[0];
    assert_eq!(push.body["level"], "active");
    assert!(push.body.get("group").is_none());
    assert!(push.body.get("icon").is_none());
    assert!(push.body.get("sound").is_none());
    assert_no_credential(&delivery, DEVICE_KEY);
    Ok(())
}

#[tokio::test]
async fn fails_permanently_on_a_provider_client_error() -> Result<(), Box<dyn std::error::Error>> {
    for status in [400_u16, 401, 403, 422] {
        let delivery = deliver_once(
            bark_status(status),
            |port| json!({"serverUrl": format!("http://127.0.0.1:{port}")}),
            Some(DEVICE_KEY),
        )
        .await?;

        assert_eq!(
            delivery.result.status,
            NotificationSendStatus::Failed,
            "{status}"
        );
        assert_eq!(
            delivery.result.error_code.as_deref(),
            Some(format!("PROVIDER_HTTP_{status}").as_str()),
            "{status}"
        );
        assert_eq!(delivery.result.retry_after_seconds, None, "{status}");
        assert_eq!(delivery.requests.len(), 1, "{status}");
        assert_no_credential(&delivery, DEVICE_KEY);
    }
    Ok(())
}

#[tokio::test]
async fn retries_with_the_provider_retry_after() -> Result<(), Box<dyn std::error::Error>> {
    let mut behaviour = bark_status(429);
    behaviour.retry_after = Some("30");
    let delivery = deliver_once(
        behaviour,
        |port| json!({"serverUrl": format!("http://127.0.0.1:{port}")}),
        Some(DEVICE_KEY),
    )
    .await?;

    assert_eq!(delivery.result.status, NotificationSendStatus::Retryable);
    assert_eq!(
        delivery.result.error_code.as_deref(),
        Some("PROVIDER_HTTP_429")
    );
    assert_eq!(delivery.result.retry_after_seconds, Some(30));
    assert_no_credential(&delivery, DEVICE_KEY);
    Ok(())
}

#[tokio::test]
async fn retries_on_a_provider_server_error() -> Result<(), Box<dyn std::error::Error>> {
    let delivery = deliver_once(
        bark_status(503),
        |port| json!({"serverUrl": format!("http://127.0.0.1:{port}")}),
        Some(DEVICE_KEY),
    )
    .await?;

    assert_eq!(delivery.result.status, NotificationSendStatus::Retryable);
    assert_eq!(
        delivery.result.error_code.as_deref(),
        Some("PROVIDER_HTTP_503")
    );
    assert_eq!(delivery.result.retry_after_seconds, None);
    assert_no_credential(&delivery, DEVICE_KEY);
    Ok(())
}

#[tokio::test]
async fn does_not_follow_a_provider_redirect() -> Result<(), Box<dyn std::error::Error>> {
    let (redirect_port, redirect_recorder, redirect_server) =
        spawn_push_server(bark_success()).await?;

    let mut behaviour = bark_status(302);
    behaviour.location = Some(format!("http://127.0.0.1:{redirect_port}/push"));
    let (port, recorder, server) = spawn_push_server(behaviour).await?;

    let (result, stderr) = run_send(
        json!({"serverUrl": format!("http://127.0.0.1:{port}")}),
        Some(DEVICE_KEY),
    )
    .await?;
    server.abort();
    redirect_server.abort();

    assert_eq!(recorder.len(), 1, "the redirect origin must be called once");
    assert_eq!(
        redirect_recorder.len(),
        0,
        "the plugin must never follow a redirect"
    );
    assert_eq!(result.status, NotificationSendStatus::Failed);
    assert_eq!(result.error_code.as_deref(), Some("PROVIDER_HTTP_302"));
    assert!(!stderr.contains(DEVICE_KEY));
    Ok(())
}

#[tokio::test]
async fn fails_on_an_upstream_business_error() -> Result<(), Box<dyn std::error::Error>> {
    let mut behaviour = bark_success();
    behaviour.body = json!({"code": 400, "message": "invalid device key"});
    let delivery = deliver_once(
        behaviour,
        |port| json!({"serverUrl": format!("http://127.0.0.1:{port}")}),
        Some(DEVICE_KEY),
    )
    .await?;

    assert_eq!(delivery.result.status, NotificationSendStatus::Failed);
    assert_eq!(delivery.result.error_code.as_deref(), Some("bark_code_400"));
    let encoded = serde_json::to_string(&delivery.result)?;
    assert!(
        !encoded.contains("invalid device key"),
        "the untrusted upstream message must not be propagated: {encoded}"
    );
    assert_no_credential(&delivery, DEVICE_KEY);
    Ok(())
}

#[tokio::test]
async fn rejects_an_oversized_provider_response() -> Result<(), Box<dyn std::error::Error>> {
    let mut behaviour = bark_success();
    behaviour.body = json!({
        "code": 200,
        "message": "success",
        "padding": "a".repeat(80 * 1024),
    });
    let delivery = deliver_once(
        behaviour,
        |port| json!({"serverUrl": format!("http://127.0.0.1:{port}")}),
        Some(DEVICE_KEY),
    )
    .await?;

    assert_eq!(delivery.result.status, NotificationSendStatus::Failed);
    assert_eq!(
        delivery.result.error_code.as_deref(),
        Some("provider_response_too_large")
    );
    assert_no_credential(&delivery, DEVICE_KEY);
    Ok(())
}

#[tokio::test]
async fn treats_an_unreachable_provider_as_retryable() -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address: SocketAddr = listener.local_addr()?;
    drop(listener);

    let (result, stderr) = run_send(
        json!({"serverUrl": format!("http://{address}")}),
        Some(DEVICE_KEY),
    )
    .await?;

    assert_eq!(result.status, NotificationSendStatus::Retryable);
    assert_eq!(result.error_code.as_deref(), Some("provider_unavailable"));
    assert!(!stderr.contains(DEVICE_KEY));
    Ok(())
}

#[tokio::test]
async fn times_out_a_silent_provider_as_retryable() -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let server = tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                break;
            };
            // Hold the connection open without answering so the plugin hits its own timeout.
            tokio::spawn(async move {
                sleep(Duration::from_secs(30)).await;
                drop(socket);
            });
        }
    });

    let started = Instant::now();
    let (result, stderr) = run_send(
        json!({"serverUrl": format!("http://127.0.0.1:{port}")}),
        Some(DEVICE_KEY),
    )
    .await?;
    server.abort();

    assert_eq!(result.status, NotificationSendStatus::Retryable);
    assert_eq!(result.error_code.as_deref(), Some("provider_unavailable"));
    assert!(
        started.elapsed() >= Duration::from_secs(9),
        "the plugin must wait for its own request timeout, waited {:?}",
        started.elapsed()
    );
    assert!(!stderr.contains(DEVICE_KEY));
    Ok(())
}

#[tokio::test]
async fn rejects_an_invalid_destination_configuration_without_a_request()
-> Result<(), Box<dyn std::error::Error>> {
    let (port, recorder, server) = spawn_push_server(bark_success()).await?;
    let base = format!("http://127.0.0.1:{port}");
    let cases = [
        (
            "scheme",
            json!({"serverUrl": "ftp://example.invalid"}),
            "invalid_server_url",
        ),
        (
            "credentials",
            json!({"serverUrl": "https://user:password@example.invalid"}),
            "invalid_server_url",
        ),
        (
            "query",
            json!({"serverUrl": format!("{base}/?token=abc")}),
            "invalid_server_url",
        ),
        (
            "fragment",
            json!({"serverUrl": "https://example.invalid/#frag"}),
            "invalid_server_url",
        ),
        (
            "level",
            json!({"serverUrl": base.clone(), "level": "loud"}),
            "invalid_config",
        ),
        (
            "icon",
            json!({"serverUrl": base.clone(), "icon": "javascript:alert(1)"}),
            "invalid_config",
        ),
        (
            "group",
            json!({"serverUrl": base.clone(), "group": "g".repeat(65)}),
            "invalid_config",
        ),
        (
            "sound",
            json!({"serverUrl": base.clone(), "sound": {"name": "bell"}}),
            "invalid_config",
        ),
    ];

    let mut plugin = Plugin::spawn()?;
    for (id, config, code) in cases {
        let result = plugin
            .send_as(id, notification_params(config, Some(DEVICE_KEY)))
            .await?;
        assert_eq!(result.status, NotificationSendStatus::Failed, "{id}");
        assert_eq!(result.error_code.as_deref(), Some(code), "{id}");
    }
    let stderr = plugin.finish().await?;
    server.abort();

    assert_eq!(
        recorder.len(),
        0,
        "an invalid configuration must never reach the provider"
    );
    assert!(!stderr.contains(DEVICE_KEY));
    assert!(!stderr.contains("token=abc"));
    assert!(!stderr.contains(&format!("127.0.0.1:{port}")));
    Ok(())
}

#[tokio::test]
async fn rejects_a_loopback_destination_without_private_network_approval()
-> Result<(), Box<dyn std::error::Error>> {
    let (port, recorder, server) = spawn_push_server(bark_success()).await?;

    let (result, stderr) = run_send_with_approval(
        json!({"serverUrl": format!("http://127.0.0.1:{port}")}),
        Some(DEVICE_KEY),
        false,
    )
    .await?;
    server.abort();

    assert_eq!(result.status, NotificationSendStatus::Failed);
    assert_eq!(
        result.error_code.as_deref(),
        Some("destination_not_allowed")
    );
    assert_eq!(result.retry_after_seconds, None);
    assert_eq!(
        recorder.len(),
        0,
        "a destination rejected by the guard must not be contacted"
    );
    assert!(!stderr.contains(DEVICE_KEY));
    assert!(!stderr.contains(&format!("127.0.0.1:{port}")));

    let result = missing_approval_is_default(&port).await?;
    assert_eq!(result.status, NotificationSendStatus::Failed);
    assert_eq!(
        result.error_code.as_deref(),
        Some("destination_not_allowed")
    );
    Ok(())
}

/// Sends with a `target` object that has no `allowPrivateNetwork` key at all.
async fn missing_approval_is_default(
    port: &u16,
) -> Result<NotificationSendRpcResult, Box<dyn std::error::Error>> {
    let mut plugin = Plugin::spawn()?;
    let mut params = notification_params(
        json!({"serverUrl": format!("http://127.0.0.1:{port}")}),
        Some(DEVICE_KEY),
    );
    params["target"] = json!({"url": ""});
    let result = plugin.send(params).await?;
    let _ = plugin.finish().await?;
    Ok(result)
}

#[tokio::test]
async fn rejects_the_cloud_metadata_address_even_with_private_network_approval()
-> Result<(), Box<dyn std::error::Error>> {
    let (result, stderr) = run_send_with_approval(
        json!({"serverUrl": "http://169.254.169.254"}),
        Some(DEVICE_KEY),
        true,
    )
    .await?;

    assert_eq!(result.status, NotificationSendStatus::Failed);
    assert_eq!(
        result.error_code.as_deref(),
        Some("destination_not_allowed")
    );
    assert!(!stderr.contains(DEVICE_KEY));
    assert!(!stderr.contains("169.254.169.254"));
    Ok(())
}

#[tokio::test]
async fn retries_when_reading_the_response_body_times_out() -> Result<(), Box<dyn std::error::Error>>
{
    let (port, server) = spawn_raw_response(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{\"code\":",
        true,
    )
    .await?;

    let started = Instant::now();
    let (result, stderr) = run_send(
        json!({"serverUrl": format!("http://127.0.0.1:{port}")}),
        Some(DEVICE_KEY),
    )
    .await?;
    server.abort();

    assert_eq!(result.status, NotificationSendStatus::Retryable);
    assert_eq!(result.error_code.as_deref(), Some("provider_unavailable"));
    assert!(
        started.elapsed() >= Duration::from_secs(9),
        "the body read must use the client timeout, waited {:?}",
        started.elapsed()
    );
    assert!(!stderr.contains(DEVICE_KEY));
    Ok(())
}

#[tokio::test]
async fn retries_when_the_response_body_connection_breaks() -> Result<(), Box<dyn std::error::Error>>
{
    let (port, server) = spawn_raw_response(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{\"code\":",
        false,
    )
    .await?;

    let (result, stderr) = run_send(
        json!({"serverUrl": format!("http://127.0.0.1:{port}")}),
        Some(DEVICE_KEY),
    )
    .await?;
    server.abort();

    assert_eq!(result.status, NotificationSendStatus::Retryable);
    assert_eq!(result.error_code.as_deref(), Some("provider_unavailable"));
    assert!(!stderr.contains(DEVICE_KEY));
    Ok(())
}

#[tokio::test]
async fn ignores_an_environment_proxy_for_a_guarded_destination()
-> Result<(), Box<dyn std::error::Error>> {
    let (port, recorder, server) = spawn_push_server(bark_success()).await?;
    // Every ambient proxy variable points at a dead endpoint: an honoured proxy would fail the
    // delivery, while the guarded path must connect straight to the validated address.
    let dead_proxy = "http://127.0.0.1:1";
    let mut plugin = Plugin::spawn_with_environment(&[
        ("HTTP_PROXY", dead_proxy),
        ("HTTPS_PROXY", dead_proxy),
        ("ALL_PROXY", dead_proxy),
        ("http_proxy", dead_proxy),
        ("https_proxy", dead_proxy),
        ("all_proxy", dead_proxy),
    ])?;

    let result = plugin
        .send(notification_params(
            json!({"serverUrl": format!("http://127.0.0.1:{port}")}),
            Some(DEVICE_KEY),
        ))
        .await?;
    let stderr = plugin.finish().await?;
    server.abort();

    assert_eq!(result.status, NotificationSendStatus::Delivered);
    assert_eq!(
        recorder.len(),
        1,
        "the request must reach the mock directly"
    );
    assert!(!stderr.contains(DEVICE_KEY));
    Ok(())
}
