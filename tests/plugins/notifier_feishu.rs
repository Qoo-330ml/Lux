//! Contract tests for the Feishu / Lark notifier plugin process.
//!
//! Every test drives the real `lux-plugin-notifier-feishu` binary over the stdio JSON-RPC protocol
//! against a local mock HTTP server. No test reaches the public Feishu or Lark API, and every
//! credential used here is a synthetic test value.

use std::{
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use hmac::{Hmac, Mac};
use luxd::application::plugin_protocol::{
    NotificationSendRpcResult, NotificationSendStatus, PluginManifest, PluginRequest,
    PluginResponse,
};
use serde_json::{Value, json};
use sha2::Sha256;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command,
};

const PLUGIN_BIN: &str = env!("CARGO_BIN_EXE_lux-plugin-notifier-feishu");
const TEST_HOOK_ID: &str = "test-hook-1";
const TEST_SIGN_SECRET: &str = "test-sign-secret";

/// The host's private-network approval, forwarded unchanged into `target.allowPrivateNetwork`.
#[derive(Clone, Copy, Debug)]
enum PrivateNetwork {
    Approved,
    Denied,
}

/// The local mock server runs on `127.0.0.1`, so the ordinary delivery tests need the host's
/// explicit private-network approval.
fn backend(secret: Option<&str>, config: Value) -> PluginRequest {
    backend_with(secret, config, PrivateNetwork::Approved)
}

fn backend_with(secret: Option<&str>, config: Value, approval: PrivateNetwork) -> PluginRequest {
    let allow_private_network = match approval {
        PrivateNetwork::Approved => true,
        PrivateNetwork::Denied => false,
    };
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
        "target": {"url": "", "allowPrivateNetwork": allow_private_network},
        "config": config
    });
    if let Some(secret) = secret {
        params["secret"] = json!(secret);
    }
    PluginRequest::new("send", "notification.send", params)
}

#[derive(Clone)]
struct MockReply {
    status: StatusCode,
    body: Value,
    retry_after: Option<&'static str>,
    delay: Option<Duration>,
}

impl MockReply {
    fn ok(body: Value) -> Self {
        Self {
            status: StatusCode::OK,
            body,
            retry_after: None,
            delay: None,
        }
    }

    fn status(status: u16) -> Self {
        Self {
            status: StatusCode::from_u16(status).expect("test status is valid"),
            body: json!({}),
            retry_after: None,
            delay: None,
        }
    }

    fn with_retry_after(mut self, seconds: &'static str) -> Self {
        self.retry_after = Some(seconds);
        self
    }

    fn delayed(mut self, delay: Duration) -> Self {
        self.delay = Some(delay);
        self
    }
}

struct MockState {
    reply: MockReply,
    requests: Mutex<Vec<(String, Value)>>,
}

impl MockState {
    fn requests(&self) -> Vec<(String, Value)> {
        self.requests
            .lock()
            .expect("mock request log is not poisoned")
            .clone()
    }
}

struct MockServer {
    base_url: String,
    state: Arc<MockState>,
    handle: tokio::task::JoinHandle<()>,
}

impl MockServer {
    async fn start(reply: MockReply) -> Result<Self, Box<dyn std::error::Error>> {
        let state = Arc::new(MockState {
            reply,
            requests: Mutex::new(Vec::new()),
        });
        let app = Router::new()
            .route("/open-apis/bot/v2/hook/{hook_id}", post(handle_hook))
            .route("/proxy/open-apis/bot/v2/hook/{hook_id}", post(handle_hook))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self {
            base_url: format!("http://{address}"),
            state,
            handle,
        })
    }

    fn api_base(&self) -> String {
        self.base_url.clone()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn handle_hook(
    State(state): State<Arc<MockState>>,
    Path(hook_id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    state
        .requests
        .lock()
        .expect("mock request log is not poisoned")
        .push((hook_id, body));
    let reply = state.reply.clone();
    if let Some(delay) = reply.delay {
        tokio::time::sleep(delay).await;
    }
    let mut response = (reply.status, Json(reply.body)).into_response();
    if let Some(retry_after) = reply.retry_after {
        response.headers_mut().insert(
            "retry-after",
            HeaderValue::from_str(retry_after).expect("test Retry-After value is valid"),
        );
    }
    response
}

struct PluginRun {
    responses: Vec<PluginResponse>,
    logs: String,
}

async fn run_plugin(requests: Vec<PluginRequest>) -> Result<PluginRun, Box<dyn std::error::Error>> {
    run_plugin_with_proxy(requests, None).await
}

async fn run_plugin_with_proxy(
    requests: Vec<PluginRequest>,
    proxy_url: Option<&str>,
) -> Result<PluginRun, Box<dyn std::error::Error>> {
    let mut command = Command::new(PLUGIN_BIN);
    command
        .env_remove("LUX_CONFIG_DIR")
        .env_remove("LUX_PROXY_URL");
    if let Some(proxy_url) = proxy_url {
        command.env("LUX_PROXY_URL", proxy_url);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or("plugin stdin unavailable")?;
    let stdout = child.stdout.take().ok_or("plugin stdout unavailable")?;
    let mut stderr = child.stderr.take().ok_or("plugin stderr unavailable")?;
    let mut lines = BufReader::new(stdout).lines();

    let mut responses = Vec::with_capacity(requests.len());
    for request in requests {
        write_request(&mut stdin, &request).await?;
        let line = lines
            .next_line()
            .await?
            .ok_or("plugin closed stdout unexpectedly")?;
        responses.push(serde_json::from_str(&line)?);
    }
    let shutdown = PluginRequest::new("shutdown", "plugin.shutdown", json!({}));
    write_request(&mut stdin, &shutdown).await?;
    lines
        .next_line()
        .await?
        .ok_or("plugin did not answer plugin.shutdown")?;
    drop(stdin);

    let mut logs = String::new();
    stderr.read_to_string(&mut logs).await?;
    let status = child.wait().await?;
    assert!(status.success(), "plugin process must exit cleanly");

    Ok(PluginRun { responses, logs })
}

async fn write_request(
    stdin: &mut tokio::process::ChildStdin,
    request: &PluginRequest,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut line = serde_json::to_vec(request)?;
    line.push(b'\n');
    stdin.write_all(&line).await?;
    stdin.flush().await?;
    Ok(())
}

fn result_of(response: &PluginResponse) -> NotificationSendRpcResult {
    serde_json::from_value(
        response
            .result
            .clone()
            .expect("notification.send must return a result"),
    )
    .expect("the result must match the notification RPC contract")
}

fn encoded_response(response: &PluginResponse) -> String {
    serde_json::to_string(response).expect("plugin responses are JSON")
}

fn expected_sign(timestamp: &str, sign_secret: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(format!("{timestamp}\n{sign_secret}").as_bytes())
        .expect("HMAC accepts any key length");
    mac.update(b"");
    BASE64.encode(mac.finalize().into_bytes())
}

#[tokio::test]
async fn sends_a_post_message_and_reports_delivery() -> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start(MockReply::ok(json!({"code": 0, "msg": "success"}))).await?;
    let request = backend(
        Some(TEST_HOOK_ID),
        json!({"region": "feishu", "apiBase": server.api_base(), "msgType": "post"}),
    );

    let run = run_plugin(vec![request]).await?;

    let result = result_of(&run.responses[0]);
    assert_eq!(result.status, NotificationSendStatus::Delivered);
    let requests = server.state.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, TEST_HOOK_ID);
    assert_eq!(requests[0].1["msg_type"], "post");
    assert_eq!(
        requests[0].1["content"]["post"]["zh_cn"]["title"],
        "媒体新增"
    );
    assert_eq!(
        requests[0].1["content"]["post"]["zh_cn"]["content"][0][0]["text"],
        "新增媒体：2 个\n媒体库：电影",
        "a post body must not repeat the title it already carries"
    );
    assert!(
        requests[0].1.get("sign").is_none(),
        "an unsigned credential must not send a signature"
    );
    Ok(())
}

#[tokio::test]
async fn signs_the_request_when_the_credential_carries_a_sign_secret()
-> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start(MockReply::ok(json!({"code": 0}))).await?;
    let credential = format!("{TEST_HOOK_ID}#{TEST_SIGN_SECRET}");
    let request = backend(
        Some(&credential),
        json!({"apiBase": server.api_base(), "msgType": "text"}),
    );

    let run = run_plugin(vec![request]).await?;

    assert_eq!(
        result_of(&run.responses[0]).status,
        NotificationSendStatus::Delivered
    );
    let requests = server.state.requests();
    let body = &requests[0].1;
    assert_eq!(body["msg_type"], "text");
    assert_eq!(
        body["content"]["text"],
        "媒体新增\n新增媒体：2 个\n媒体库：电影"
    );
    let timestamp = body["timestamp"]
        .as_str()
        .expect("a signed request carries the timestamp");
    assert!(
        timestamp.parse::<i64>().is_ok(),
        "the timestamp must be unix seconds"
    );
    assert_eq!(
        body["sign"].as_str(),
        Some(expected_sign(timestamp, TEST_SIGN_SECRET).as_str())
    );
    Ok(())
}

#[tokio::test]
async fn sends_through_a_configured_api_base_path() -> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start(MockReply::ok(json!({"code": 0}))).await?;
    let request = backend(
        Some(TEST_HOOK_ID),
        json!({"apiBase": format!("{}/proxy/", server.api_base())}),
    );

    let run = run_plugin(vec![request]).await?;

    assert_eq!(
        result_of(&run.responses[0]).status,
        NotificationSendStatus::Delivered
    );
    assert_eq!(server.state.requests()[0].0, TEST_HOOK_ID);
    Ok(())
}

#[tokio::test]
async fn rejects_a_loopback_destination_without_private_network_approval()
-> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start(MockReply::ok(json!({"code": 0}))).await?;
    let request = backend_with(
        Some(TEST_HOOK_ID),
        json!({"apiBase": server.api_base()}),
        PrivateNetwork::Denied,
    );

    let run = run_plugin(vec![request]).await?;

    let result = result_of(&run.responses[0]);
    assert_eq!(result.status, NotificationSendStatus::Failed);
    assert_eq!(
        result.error_code.as_deref(),
        Some("destination_not_allowed")
    );
    assert!(
        server.state.requests().is_empty(),
        "a rejected destination must never receive the notification"
    );
    assert!(
        !run.logs.contains("127.0.0.1"),
        "the rejection log must not carry the destination: {}",
        run.logs
    );
    Ok(())
}

#[tokio::test]
async fn approving_the_private_network_still_rejects_cloud_metadata()
-> Result<(), Box<dyn std::error::Error>> {
    let request = backend_with(
        Some(TEST_HOOK_ID),
        json!({"apiBase": "http://169.254.169.254"}),
        PrivateNetwork::Approved,
    );

    let run = run_plugin(vec![request]).await?;

    let result = result_of(&run.responses[0]);
    assert_eq!(result.status, NotificationSendStatus::Failed);
    assert_eq!(
        result.error_code.as_deref(),
        Some("destination_not_allowed")
    );
    assert!(
        !run.logs.contains("169.254.169.254"),
        "the rejection log must not carry the destination: {}",
        run.logs
    );
    Ok(())
}

#[tokio::test]
async fn maps_client_errors_to_permanent_failures() -> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start(MockReply::status(400)).await?;
    let request = backend(Some(TEST_HOOK_ID), json!({"apiBase": server.api_base()}));

    let run = run_plugin(vec![request]).await?;

    let result = result_of(&run.responses[0]);
    assert_eq!(result.status, NotificationSendStatus::Failed);
    assert_eq!(result.error_code.as_deref(), Some("PROVIDER_HTTP_400"));
    assert!(result.retry_after_seconds.is_none());
    Ok(())
}

#[tokio::test]
async fn retries_rate_limited_and_server_errors() -> Result<(), Box<dyn std::error::Error>> {
    let limited = MockServer::start(MockReply::status(429).with_retry_after("120")).await?;
    let unavailable = MockServer::start(MockReply::status(503)).await?;

    let run = run_plugin(vec![
        backend(Some(TEST_HOOK_ID), json!({"apiBase": limited.api_base()})),
        backend(
            Some(TEST_HOOK_ID),
            json!({"apiBase": unavailable.api_base()}),
        ),
    ])
    .await?;

    let rate_limited = result_of(&run.responses[0]);
    assert_eq!(rate_limited.status, NotificationSendStatus::Retryable);
    assert_eq!(rate_limited.retry_after_seconds, Some(120));
    let outage = result_of(&run.responses[1]);
    assert_eq!(outage.status, NotificationSendStatus::Retryable);
    assert_eq!(outage.error_code.as_deref(), Some("PROVIDER_HTTP_503"));
    Ok(())
}

#[tokio::test]
async fn retries_only_the_documented_frequency_control_code()
-> Result<(), Box<dyn std::error::Error>> {
    let documented = MockServer::start(MockReply::ok(
        json!({"code": 11232, "msg": "frequency limited"}),
    ))
    .await?;
    let other = MockServer::start(MockReply::ok(
        json!({"code": 19001, "msg": "invalid request"}),
    ))
    .await?;

    let run = run_plugin(vec![
        backend(
            Some(TEST_HOOK_ID),
            json!({"apiBase": documented.api_base()}),
        ),
        backend(Some(TEST_HOOK_ID), json!({"apiBase": other.api_base()})),
    ])
    .await?;

    let rate_limited = result_of(&run.responses[0]);
    assert_eq!(rate_limited.status, NotificationSendStatus::Retryable);
    assert_eq!(
        rate_limited.error_code.as_deref(),
        Some("provider_rate_limited")
    );
    let rejected = result_of(&run.responses[1]);
    assert_eq!(
        rejected.status,
        NotificationSendStatus::Failed,
        "a 19xxx code other than 11232 is not a frequency-control retry"
    );
    assert_eq!(rejected.error_code.as_deref(), Some("provider_code_19001"));
    Ok(())
}

#[tokio::test]
async fn fails_on_other_business_codes_without_echoing_the_upstream_message()
-> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start(MockReply::ok(
        json!({"code": 11228, "msg": "hook token invalid"}),
    ))
    .await?;
    let request = backend(Some(TEST_HOOK_ID), json!({"apiBase": server.api_base()}));

    let run = run_plugin(vec![request]).await?;

    let result = result_of(&run.responses[0]);
    assert_eq!(result.status, NotificationSendStatus::Failed);
    assert_eq!(result.error_code.as_deref(), Some("provider_code_11228"));
    assert!(
        !encoded_response(&run.responses[0]).contains("hook token invalid"),
        "the upstream message must not be forwarded"
    );
    Ok(())
}

#[tokio::test]
async fn times_out_into_a_retryable_failure() -> Result<(), Box<dyn std::error::Error>> {
    let server =
        MockServer::start(MockReply::ok(json!({"code": 0})).delayed(Duration::from_secs(30)))
            .await?;
    let request = backend(Some(TEST_HOOK_ID), json!({"apiBase": server.api_base()}));

    let started = Instant::now();
    let run = run_plugin(vec![request]).await?;
    let elapsed = started.elapsed();

    let result = result_of(&run.responses[0]);
    assert_eq!(result.status, NotificationSendStatus::Retryable);
    assert_eq!(result.error_code.as_deref(), Some("provider_timeout"));
    assert!(
        elapsed >= Duration::from_secs(9) && elapsed < Duration::from_secs(25),
        "the plugin must enforce the SDK's 10 second timeout, took {elapsed:?}"
    );
    Ok(())
}

#[tokio::test]
async fn reports_a_broken_outbound_client_as_retryable() -> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start(MockReply::ok(json!({"code": 0}))).await?;
    let request = backend(Some(TEST_HOOK_ID), json!({"apiBase": server.api_base()}));

    let run = run_plugin_with_proxy(vec![request], Some("ftp://127.0.0.1:3128")).await?;

    let result = result_of(&run.responses[0]);
    assert_eq!(result.status, NotificationSendStatus::Retryable);
    assert_eq!(result.error_code.as_deref(), Some("client_unavailable"));
    assert!(result.retry_after_seconds.is_none());
    assert!(
        server.state.requests().is_empty(),
        "an unusable outbound client must not send a request"
    );
    Ok(())
}

#[tokio::test]
async fn rejects_an_oversized_upstream_response() -> Result<(), Box<dyn std::error::Error>> {
    let oversized = "x".repeat(70 * 1024);
    let server = MockServer::start(MockReply::ok(json!({"code": 0, "msg": oversized}))).await?;
    let request = backend(Some(TEST_HOOK_ID), json!({"apiBase": server.api_base()}));

    let run = run_plugin(vec![request]).await?;

    let result = result_of(&run.responses[0]);
    assert_eq!(result.status, NotificationSendStatus::Failed);
    assert_eq!(
        result.error_code.as_deref(),
        Some("provider_response_too_large")
    );
    Ok(())
}

#[tokio::test]
async fn rejects_bad_configuration_with_a_stable_code() -> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start(MockReply::ok(json!({"code": 0}))).await?;
    let run = run_plugin(vec![
        backend(
            Some(TEST_HOOK_ID),
            json!({"apiBase": server.api_base(), "region": "telegram"}),
        ),
        backend(
            Some(TEST_HOOK_ID),
            json!({"apiBase": server.api_base(), "msgType": "markdown"}),
        ),
        backend(
            Some(TEST_HOOK_ID),
            json!({"apiBase": "ftp://open.feishu.cn"}),
        ),
        backend(
            Some(TEST_HOOK_ID),
            json!({"apiBase": server.api_base(), "unknownField": "value"}),
        ),
        backend(
            Some(TEST_HOOK_ID),
            json!({"apiBase": "https://user:pass@open.feishu.cn"}),
        ),
    ])
    .await?;

    for response in &run.responses {
        let result = result_of(response);
        assert_eq!(result.status, NotificationSendStatus::Failed);
        assert_eq!(result.error_code.as_deref(), Some("invalid_config"));
    }
    assert!(
        server.state.requests().is_empty(),
        "a rejected configuration must never reach the network"
    );
    Ok(())
}

#[tokio::test]
async fn rejects_missing_or_malformed_credentials_without_echoing_them()
-> Result<(), Box<dyn std::error::Error>> {
    let server = MockServer::start(MockReply::ok(json!({"code": 0}))).await?;
    let api_base = server.api_base();
    let run = run_plugin(vec![
        backend(None, json!({"apiBase": api_base})),
        backend(Some(""), json!({"apiBase": api_base})),
        backend(Some("bad hook id"), json!({"apiBase": api_base})),
        backend(Some("test-hook-1#"), json!({"apiBase": api_base})),
    ])
    .await?;

    let missing: Vec<_> = run.responses[..2].iter().map(result_of).collect();
    for result in missing {
        assert_eq!(result.status, NotificationSendStatus::Failed);
        assert_eq!(result.error_code.as_deref(), Some("missing_secret"));
    }
    for response in &run.responses[2..] {
        let result = result_of(response);
        assert_eq!(result.status, NotificationSendStatus::Failed);
        assert_eq!(result.error_code.as_deref(), Some("invalid_secret"));
        assert!(!encoded_response(response).contains("bad hook id"));
        assert!(!encoded_response(response).contains("test-hook-1"));
    }
    assert!(server.state.requests().is_empty());
    Ok(())
}

#[tokio::test]
async fn keeps_the_credential_out_of_results_and_logs() -> Result<(), Box<dyn std::error::Error>> {
    let delivered = MockServer::start(MockReply::ok(json!({"code": 0}))).await?;
    let rejected = MockServer::start(MockReply::status(401)).await?;
    let credential = format!("{TEST_HOOK_ID}#{TEST_SIGN_SECRET}");
    let run = run_plugin(vec![
        backend(Some(&credential), json!({"apiBase": delivered.api_base()})),
        backend(Some(&credential), json!({"apiBase": rejected.api_base()})),
    ])
    .await?;

    assert_eq!(
        result_of(&run.responses[0]).status,
        NotificationSendStatus::Delivered
    );
    assert_eq!(
        result_of(&run.responses[1]).status,
        NotificationSendStatus::Failed
    );
    let encoded_responses = run
        .responses
        .iter()
        .map(encoded_response)
        .collect::<Vec<_>>()
        .join("\n");
    for leaked in [TEST_HOOK_ID, TEST_SIGN_SECRET] {
        assert!(
            !encoded_responses.contains(leaked),
            "RPC results must not carry the credential"
        );
        assert!(
            !run.logs.contains(leaked),
            "plugin diagnostics must not carry the credential: {}",
            run.logs
        );
    }
    assert!(
        !run.logs.contains("127.0.0.1"),
        "plugin diagnostics must not carry the complete destination URL: {}",
        run.logs
    );
    assert!(
        !run.logs.contains(&delivered.api_base()),
        "plugin diagnostics must not carry the configured base URL: {}",
        run.logs
    );
    Ok(())
}

#[test]
fn manifest_declares_a_single_secret_field_and_the_notification_contract() {
    let manifest = PluginManifest::from_value(
        serde_json::from_str(include_str!(
            "../../plugins/org.lux.notifier.feishu/manifest.json"
        ))
        .expect("the plugin manifest is valid JSON"),
    )
    .expect("the plugin manifest passes host validation");

    assert_eq!(manifest.id, "org.lux.notifier.feishu");
    assert_eq!(manifest.plugin_type, "notification");
    assert_eq!(manifest.category, "NOTIFICATION");
    assert_eq!(manifest.capabilities, vec!["notification.send"]);
    assert_eq!(manifest.version, "0.1.0");
    let secret_fields: Vec<_> = manifest
        .config_fields
        .iter()
        .filter(|field| field.key == "secret")
        .collect();
    assert_eq!(secret_fields.len(), 1);
    assert_eq!(secret_fields[0].input_type, "password");
    assert!(secret_fields[0].sensitive);
    assert!(secret_fields[0].required);
    assert!(secret_fields[0].description.is_some());
    for field in &manifest.config_fields {
        if field.key != "secret" {
            assert!(!field.sensitive);
            assert_ne!(field.input_type, "password");
        }
    }
}
