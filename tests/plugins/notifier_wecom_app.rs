//! Integration tests for the WeCom app notification plugin.
//!
//! Every test drives the real plugin binary over the stdio JSON-RPC protocol against a local mock
//! WeCom endpoint on `127.0.0.1`; nothing here contacts a real WeCom tenant and no value in this
//! file is a real credential.

use std::{
    collections::HashMap,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    extract::Query,
    http::{StatusCode, header::RETRY_AFTER},
    response::IntoResponse,
    routing::{get, post},
};
use luxd::application::plugin_protocol::{
    NotificationSendRpcResult, NotificationSendStatus, PluginRequest, PluginResponse,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    net::TcpListener,
    process::{Child, ChildStdin, ChildStdout, Command},
    task::JoinHandle,
};

const TEST_SECRET: &str = "demo-corpsecret-not-a-real-credential";
const TOKEN_ONE: &str = "demo-access-token-one";
const TOKEN_TWO: &str = "demo-access-token-two";

/// A running plugin process with piped stdio.
struct Plugin {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    stderr: Arc<Mutex<String>>,
    stderr_task: JoinHandle<()>,
}

impl Plugin {
    async fn start() -> Result<Self, Box<dyn std::error::Error>> {
        let mut child = Command::new(env!("CARGO_BIN_EXE_lux-plugin-notifier-wecom-app"))
            .env_remove("LUX_PROXY_URL")
            .env_remove("HTTP_PROXY")
            .env_remove("HTTPS_PROXY")
            .env_remove("ALL_PROXY")
            .env_remove("http_proxy")
            .env_remove("https_proxy")
            .env_remove("all_proxy")
            .env("NO_PROXY", "127.0.0.1")
            .env("no_proxy", "127.0.0.1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdin = child.stdin.take().ok_or("plugin stdin unavailable")?;
        let stdout = child.stdout.take().ok_or("plugin stdout unavailable")?;
        let stderr = child.stderr.take().ok_or("plugin stderr unavailable")?;
        let buffer = Arc::new(Mutex::new(String::new()));
        let stderr_buffer = buffer.clone();
        let stderr_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(mut text) = stderr_buffer.lock() {
                    text.push_str(&line);
                    text.push('\n');
                }
            }
        });
        Ok(Self {
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            stderr: buffer,
            stderr_task,
        })
    }

    async fn send(
        &mut self,
        id: &str,
        params: Value,
    ) -> Result<NotificationSendRpcResult, Box<dyn std::error::Error>> {
        let request = PluginRequest::new(id, "notification.send", params);
        let mut bytes = serde_json::to_vec(&request)?;
        bytes.push(b'\n');
        self.stdin.write_all(&bytes).await?;
        self.stdin.flush().await?;
        let line = self
            .lines
            .next_line()
            .await?
            .ok_or("plugin exited without a response")?;
        let response: PluginResponse = serde_json::from_str(&line)?;
        assert!(
            response.error.is_none(),
            "unexpected RPC error: {:?}",
            response.error
        );
        assert_eq!(response.id, id);
        Ok(serde_json::from_value(
            response.result.ok_or("missing notification result")?,
        )?)
    }

    /// Sends `plugin.shutdown`, waits for the process and returns everything it logged.
    async fn finish(mut self) -> Result<String, Box<dyn std::error::Error>> {
        let shutdown = PluginRequest::new("shutdown", "plugin.shutdown", json!({}));
        let mut bytes = serde_json::to_vec(&shutdown)?;
        bytes.push(b'\n');
        self.stdin.write_all(&bytes).await?;
        self.stdin.flush().await?;
        let _ = self.lines.next_line().await?;
        let status = self.child.wait().await?;
        assert!(status.success(), "the plugin process must exit cleanly");
        let _ = self.stderr_task.await;
        Ok(self
            .stderr
            .lock()
            .map(|text| text.clone())
            .unwrap_or_default())
    }
}

/// Asserts that the RPC result carries no credential, token or correlation with the upstream URL.
fn assert_result_has_no_credentials(result: &NotificationSendRpcResult) {
    let encoded = serde_json::to_string(result).expect("result should serialize");
    for needle in [
        TEST_SECRET,
        TOKEN_ONE,
        TOKEN_TWO,
        "corpsecret",
        "access_token",
    ] {
        assert!(
            !encoded.contains(needle),
            "the notification result leaked {needle}: {encoded}"
        );
    }
}

/// Asserts that the captured plugin log carries no credential, token or request URL.
fn assert_log_has_no_credentials(stderr: &str) {
    for needle in [
        TEST_SECRET,
        TOKEN_ONE,
        TOKEN_TWO,
        "corpsecret",
        "access_token",
        "127.0.0.1",
        "cgi-bin",
        "http://",
        "https://",
    ] {
        assert!(
            !stderr.contains(needle),
            "the plugin log leaked {needle}: {stderr}"
        );
    }
}

/// Serves `app` on a random loopback port.
async fn spawn_server(app: Router) -> Result<(String, JoinHandle<()>), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok((format!("http://{address}"), handle))
}

fn api_base_config(api_base: &str) -> Value {
    json!({
        "corpId": "corp-demo",
        "agentId": 1_000_002,
        "toUser": "@all",
        "apiBase": api_base,
    })
}

fn send_params(config: Value, secret: Value) -> Value {
    json!({
        "event": {
            "schemaVersion": 1,
            "eventId": "event-1",
            "eventType": "MEDIA_ADDED",
            "occurredAt": 1_700_000_000_i64,
            "serverId": "server-1",
            "data": {
                "libraryName": "电影",
                "addedCount": 2,
                "title": "媒体新增",
                "content": "新增媒体：2 个\n媒体库：电影",
            }
        },
        // The shared destination guard will reject the loopback mock unless the host target opens
        // private networks explicitly; the plugin reads this flag once the guard is available.
        "target": {"url": "", "allowPrivateNetwork": true},
        "config": config,
        "secret": secret,
    })
}

/// Counts the requests that reached a test server so a validation error can prove it stayed local.
#[derive(Clone, Default)]
struct RequestCount {
    count: Arc<AtomicUsize>,
}

impl RequestCount {
    fn get(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }

    fn server(&self) -> Router {
        let count = self.count.clone();
        Router::new()
            .route(
                "/cgi-bin/gettoken",
                axum::routing::get(move || {
                    let count = count.clone();
                    async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        axum::Json(json!({
                            "errcode": 0,
                            "access_token": TOKEN_ONE,
                            "expires_in": 7200
                        }))
                    }
                }),
            )
            .route(
                "/cgi-bin/message/send",
                axum::routing::post(move || async move {
                    axum::Json(json!({"errcode": 0, "msgid": "unexpected"}))
                }),
            )
    }
}

#[tokio::test]
async fn requires_the_destination_secret() -> Result<(), Box<dyn std::error::Error>> {
    let requests = RequestCount::default();
    let (base, server) = spawn_server(requests.server()).await?;
    let mut plugin = Plugin::start().await?;

    for (id, secret) in [
        ("missing", Value::Null),
        ("empty", json!("")),
        ("blank", json!("   \n\t ")),
    ] {
        let result = plugin
            .send(id, send_params(api_base_config(&base), secret))
            .await?;
        assert_eq!(result.status, NotificationSendStatus::Failed);
        assert_eq!(result.error_code.as_deref(), Some("missing_secret"));
        assert!(result.provider_request_id.is_none());
        assert_result_has_no_credentials(&result);
    }

    let stderr = plugin.finish().await?;
    assert_eq!(requests.get(), 0, "a missing secret must not reach WeCom");
    assert_log_has_no_credentials(&stderr);
    server.abort();
    Ok(())
}

#[tokio::test]
async fn rejects_invalid_destination_config_without_touching_the_network()
-> Result<(), Box<dyn std::error::Error>> {
    let requests = RequestCount::default();
    let (base, server) = spawn_server(requests.server()).await?;
    let mut plugin = Plugin::start().await?;

    let mut invalid_configs = Vec::new();
    let mut missing_agent_id = api_base_config(&base);
    missing_agent_id
        .as_object_mut()
        .ok_or("config must be an object")?
        .remove("agentId");
    invalid_configs.push(("no-agent", missing_agent_id));

    let mut text_agent_id = api_base_config(&base);
    text_agent_id["agentId"] = json!("second");
    invalid_configs.push(("text-agent", text_agent_id));

    let mut zero_agent_id = api_base_config(&base);
    zero_agent_id["agentId"] = json!(0);
    invalid_configs.push(("zero-agent", zero_agent_id));

    let mut empty_corp_id = api_base_config(&base);
    empty_corp_id["corpId"] = json!(" ");
    invalid_configs.push(("empty-corp", empty_corp_id));

    let mut bad_to_user = api_base_config(&base);
    bad_to_user["toUser"] = json!("bad user");
    invalid_configs.push(("bad-touser", bad_to_user));

    let mut ftp_base = api_base_config(&base);
    ftp_base["apiBase"] = json!("ftp://127.0.0.1:1");
    invalid_configs.push(("ftp-base", ftp_base));

    let mut credential_base = api_base_config(&base);
    credential_base["apiBase"] = json!("http://user:pass@127.0.0.1:1");
    invalid_configs.push(("credential-base", credential_base));

    let mut query_base = api_base_config(&base);
    query_base["apiBase"] = json!("http://127.0.0.1:1/?token=1");
    invalid_configs.push(("query-base", query_base));

    for (id, config) in invalid_configs {
        let result = plugin
            .send(id, send_params(config, json!(TEST_SECRET)))
            .await?;
        assert_eq!(
            result.status,
            NotificationSendStatus::Failed,
            "{id} must fail permanently"
        );
        assert_eq!(
            result.error_code.as_deref(),
            Some("invalid_config"),
            "{id} must use the stable config error code"
        );
    }

    let stderr = plugin.finish().await?;
    assert_eq!(
        requests.get(),
        0,
        "an invalid destination config must not reach WeCom"
    );
    for needle in [TEST_SECRET, "user:pass", "token=1", "bad user"] {
        assert!(
            !stderr.contains(needle),
            "the plugin log leaked a rejected config value ({needle}): {stderr}"
        );
    }
    server.abort();
    Ok(())
}

fn token_body(token: &str) -> Value {
    json!({
        "errcode": 0,
        "errmsg": "ok",
        "access_token": token,
        "expires_in": 7200,
    })
}

/// Counts upstream calls and records the query and body of every `message/send` request.
#[derive(Clone, Default)]
struct Upstream {
    token_calls: Arc<AtomicUsize>,
    send_calls: Arc<AtomicUsize>,
    token_queries: Arc<Mutex<Vec<Value>>>,
    send_requests: Arc<Mutex<Vec<Value>>>,
}

impl Upstream {
    fn token_calls(&self) -> usize {
        self.token_calls.load(Ordering::SeqCst)
    }

    fn send_calls(&self) -> usize {
        self.send_calls.load(Ordering::SeqCst)
    }

    fn token_queries(&self) -> Vec<Value> {
        self.token_queries
            .lock()
            .map(|queries| queries.clone())
            .unwrap_or_default()
    }

    fn send_requests(&self) -> Vec<Value> {
        self.send_requests
            .lock()
            .map(|requests| requests.clone())
            .unwrap_or_default()
    }

    fn token_handler(&self, token: &str) -> axum::routing::MethodRouter {
        let calls = self.token_calls.clone();
        let queries = self.token_queries.clone();
        let token = token.to_owned();
        get(move |Query(query): Query<HashMap<String, String>>| {
            let calls = calls.clone();
            let queries = queries.clone();
            let token = token.clone();
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                if let Ok(mut captured) = queries.lock() {
                    captured.push(json!(query));
                }
                Json(token_body(&token))
            }
        })
    }

    fn send_handler(&self, response: Value) -> axum::routing::MethodRouter {
        let calls = self.send_calls.clone();
        let requests = self.send_requests.clone();
        post(
            move |Query(query): Query<HashMap<String, String>>, Json(body): Json<Value>| {
                let calls = calls.clone();
                let requests = requests.clone();
                let response = response.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    if let Ok(mut captured) = requests.lock() {
                        captured.push(json!({"query": query, "body": body}));
                    }
                    Json(response)
                }
            },
        )
    }
}

#[tokio::test]
async fn delivers_a_text_message_without_leaking_the_credential()
-> Result<(), Box<dyn std::error::Error>> {
    let upstream = Upstream::default();
    let app = Router::new()
        .route("/cgi-bin/gettoken", upstream.token_handler(TOKEN_ONE))
        .route(
            "/cgi-bin/message/send",
            upstream.send_handler(json!({"errcode": 0, "errmsg": "ok", "msgid": "msg-1"})),
        );
    let (base, server) = spawn_server(app).await?;
    let mut plugin = Plugin::start().await?;
    let result = plugin
        .send(
            "deliver",
            send_params(api_base_config(&base), json!(TEST_SECRET)),
        )
        .await?;
    let stderr = plugin.finish().await?;
    server.abort();

    assert_eq!(result.status, NotificationSendStatus::Delivered);
    assert_eq!(result.provider_request_id.as_deref(), Some("msg-1"));
    assert_eq!(result.error_code, None);
    assert_eq!(result.retry_after_seconds, None);
    assert_result_has_no_credentials(&result);
    assert_log_has_no_credentials(&stderr);

    assert_eq!(upstream.token_calls(), 1);
    assert_eq!(upstream.send_calls(), 1);
    let token_queries = upstream.token_queries();
    assert_eq!(token_queries[0]["corpid"], "corp-demo");
    assert_eq!(token_queries[0]["corpsecret"], TEST_SECRET);
    let requests = upstream.send_requests();
    assert_eq!(requests[0]["query"]["access_token"], TOKEN_ONE);
    assert_eq!(
        requests[0]["body"],
        json!({
            "touser": "@all",
            "msgtype": "text",
            "agentid": 1_000_002,
            "text": {"content": "媒体新增\n\n新增媒体：2 个\n媒体库：电影"},
        })
    );
    Ok(())
}

#[tokio::test]
async fn reuses_the_cached_access_token_for_later_messages()
-> Result<(), Box<dyn std::error::Error>> {
    let upstream = Upstream::default();
    let app = Router::new()
        .route("/cgi-bin/gettoken", upstream.token_handler(TOKEN_ONE))
        .route(
            "/cgi-bin/message/send",
            upstream.send_handler(json!({"errcode": 0, "msgid": "msg-1"})),
        );
    let (base, server) = spawn_server(app).await?;
    let mut plugin = Plugin::start().await?;
    for id in ["first", "second"] {
        let result = plugin
            .send(id, send_params(api_base_config(&base), json!(TEST_SECRET)))
            .await?;
        assert_eq!(result.status, NotificationSendStatus::Delivered);
    }
    let stderr = plugin.finish().await?;
    server.abort();

    assert_eq!(upstream.token_calls(), 1, "the token must be cached");
    assert_eq!(upstream.send_calls(), 2);
    assert_log_has_no_credentials(&stderr);
    Ok(())
}

#[tokio::test]
async fn fails_permanently_on_an_upstream_4xx() -> Result<(), Box<dyn std::error::Error>> {
    let upstream = Upstream::default();
    let app = Router::new()
        .route("/cgi-bin/gettoken", upstream.token_handler(TOKEN_ONE))
        .route(
            "/cgi-bin/message/send",
            post(|| async move { (StatusCode::FORBIDDEN, Json(json!({"errcode": 60020}))) }),
        );
    let (base, server) = spawn_server(app).await?;
    let mut plugin = Plugin::start().await?;
    let result = plugin
        .send(
            "denied",
            send_params(api_base_config(&base), json!(TEST_SECRET)),
        )
        .await?;
    let stderr = plugin.finish().await?;
    server.abort();

    assert_eq!(result.status, NotificationSendStatus::Failed);
    assert_eq!(result.error_code.as_deref(), Some("PROVIDER_HTTP_403"));
    assert_eq!(result.retry_after_seconds, None);
    assert_result_has_no_credentials(&result);
    assert_log_has_no_credentials(&stderr);
    Ok(())
}

#[tokio::test]
async fn retries_on_429_and_keeps_the_retry_after_hint() -> Result<(), Box<dyn std::error::Error>> {
    let upstream = Upstream::default();
    let app = Router::new()
        .route("/cgi-bin/gettoken", upstream.token_handler(TOKEN_ONE))
        .route(
            "/cgi-bin/message/send",
            post(|| async move {
                (
                    StatusCode::TOO_MANY_REQUESTS,
                    [(RETRY_AFTER, "30")],
                    Json(json!({"errcode": 45009})),
                )
            }),
        );
    let (base, server) = spawn_server(app).await?;
    let mut plugin = Plugin::start().await?;
    let result = plugin
        .send(
            "limited",
            send_params(api_base_config(&base), json!(TEST_SECRET)),
        )
        .await?;
    let stderr = plugin.finish().await?;
    server.abort();

    assert_eq!(result.status, NotificationSendStatus::Retryable);
    assert_eq!(result.error_code.as_deref(), Some("PROVIDER_HTTP_429"));
    assert_eq!(result.retry_after_seconds, Some(30));
    assert_log_has_no_credentials(&stderr);
    Ok(())
}

#[tokio::test]
async fn retries_on_an_upstream_5xx() -> Result<(), Box<dyn std::error::Error>> {
    let upstream = Upstream::default();
    let app = Router::new()
        .route(
            "/cgi-bin/gettoken",
            get(|| async { StatusCode::SERVICE_UNAVAILABLE }),
        )
        .route(
            "/cgi-bin/message/send",
            upstream.send_handler(json!({"errcode": 0})),
        );
    let (base, server) = spawn_server(app).await?;
    let mut plugin = Plugin::start().await?;
    let result = plugin
        .send(
            "unavailable",
            send_params(api_base_config(&base), json!(TEST_SECRET)),
        )
        .await?;
    let stderr = plugin.finish().await?;
    server.abort();

    assert_eq!(result.status, NotificationSendStatus::Retryable);
    assert_eq!(result.error_code.as_deref(), Some("PROVIDER_HTTP_503"));
    assert_eq!(upstream.send_calls(), 0, "no message may be sent");
    assert_log_has_no_credentials(&stderr);
    Ok(())
}

#[tokio::test]
async fn classifies_wecom_business_error_codes() -> Result<(), Box<dyn std::error::Error>> {
    let upstream = Upstream::default();
    let attempts = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/cgi-bin/gettoken", upstream.token_handler(TOKEN_ONE))
        .route(
            "/cgi-bin/message/send",
            post({
                let attempts = attempts.clone();
                move || {
                    let attempts = attempts.clone();
                    async move {
                        let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                        match attempt {
                            0 => Json(json!({"errcode": 45009, "errmsg": "api freq out of limit"})),
                            1 => Json(json!({"errcode": -1, "errmsg": "system busy"})),
                            _ => Json(json!({"errcode": 0, "msgid": "msg-busy"})),
                        }
                    }
                }
            }),
        );
    let (base, server) = spawn_server(app).await?;
    let mut plugin = Plugin::start().await?;

    let limited = plugin
        .send(
            "rate-limited",
            send_params(api_base_config(&base), json!(TEST_SECRET)),
        )
        .await?;
    assert_eq!(limited.status, NotificationSendStatus::Retryable);
    assert_eq!(limited.error_code.as_deref(), Some("rate_limited"));

    let busy = plugin
        .send(
            "system-busy",
            send_params(api_base_config(&base), json!(TEST_SECRET)),
        )
        .await?;
    assert_eq!(busy.status, NotificationSendStatus::Retryable);
    assert_eq!(busy.error_code.as_deref(), Some("system_busy"));

    let delivered = plugin
        .send(
            "recovered",
            send_params(api_base_config(&base), json!(TEST_SECRET)),
        )
        .await?;
    assert_eq!(delivered.status, NotificationSendStatus::Delivered);
    assert_eq!(delivered.provider_request_id.as_deref(), Some("msg-busy"));

    let stderr = plugin.finish().await?;
    server.abort();

    assert_eq!(upstream.token_calls(), 1, "the token stays cached");
    assert_result_has_no_credentials(&limited);
    assert_result_has_no_credentials(&busy);
    assert_result_has_no_credentials(&delivered);
    assert_log_has_no_credentials(&stderr);
    Ok(())
}

#[tokio::test]
async fn refreshes_the_token_once_when_wecom_rejects_it() -> Result<(), Box<dyn std::error::Error>>
{
    let upstream = Upstream::default();
    let app = Router::new()
        .route(
            "/cgi-bin/gettoken",
            get({
                let calls = upstream.token_calls.clone();
                move || {
                    let calls = calls.clone();
                    async move {
                        let index = calls.fetch_add(1, Ordering::SeqCst);
                        Json(token_body(if index == 0 { TOKEN_ONE } else { TOKEN_TWO }))
                    }
                }
            }),
        )
        .route(
            "/cgi-bin/message/send",
            post({
                let calls = upstream.send_calls.clone();
                move |Query(query): Query<HashMap<String, String>>| {
                    let calls = calls.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        if query.get("access_token").map(String::as_str) == Some(TOKEN_ONE) {
                            Json(json!({"errcode": 40014, "errmsg": "invalid access_token"}))
                        } else {
                            Json(json!({"errcode": 0, "msgid": "msg-refreshed"}))
                        }
                    }
                }
            }),
        );
    let (base, server) = spawn_server(app).await?;
    let mut plugin = Plugin::start().await?;
    let result = plugin
        .send(
            "refresh",
            send_params(api_base_config(&base), json!(TEST_SECRET)),
        )
        .await?;
    let stderr = plugin.finish().await?;
    server.abort();

    assert_eq!(result.status, NotificationSendStatus::Delivered);
    assert_eq!(result.provider_request_id.as_deref(), Some("msg-refreshed"));
    assert_eq!(upstream.token_calls(), 2, "the rejected token is refreshed");
    assert_eq!(upstream.send_calls(), 2, "the message is retried once");
    assert_result_has_no_credentials(&result);
    assert_log_has_no_credentials(&stderr);
    Ok(())
}

#[tokio::test]
async fn rejects_an_oversized_upstream_response() -> Result<(), Box<dyn std::error::Error>> {
    let app = Router::new().route(
        "/cgi-bin/gettoken",
        get(|| async {
            let mut body = token_body(TOKEN_ONE);
            body["padding"] = json!("x".repeat(100 * 1024));
            Json(body)
        }),
    );
    let (base, server) = spawn_server(app).await?;
    let mut plugin = Plugin::start().await?;
    let result = plugin
        .send(
            "oversized",
            send_params(api_base_config(&base), json!(TEST_SECRET)),
        )
        .await?;
    let stderr = plugin.finish().await?;
    server.abort();

    assert_eq!(result.status, NotificationSendStatus::Failed);
    assert_eq!(result.error_code.as_deref(), Some("response_too_large"));
    assert_log_has_no_credentials(&stderr);
    Ok(())
}

/// Each upstream request carries its own short timeout inside the delivery budget, so this test
/// intentionally waits for the plugin's request timeout (6 seconds), not the SDK's 10 second cap.
#[tokio::test]
async fn reports_a_timeout_as_retryable() -> Result<(), Box<dyn std::error::Error>> {
    let app = Router::new().route(
        "/cgi-bin/gettoken",
        get(|| async {
            tokio::time::sleep(Duration::from_secs(60)).await;
            Json(token_body(TOKEN_ONE))
        }),
    );
    let (base, server) = spawn_server(app).await?;
    let mut plugin = Plugin::start().await?;
    let result = plugin
        .send(
            "timeout",
            send_params(api_base_config(&base), json!(TEST_SECRET)),
        )
        .await?;
    let stderr = plugin.finish().await?;
    server.abort();

    assert_eq!(result.status, NotificationSendStatus::Retryable);
    assert_eq!(result.error_code.as_deref(), Some("network_error"));
    assert_result_has_no_credentials(&result);
    assert_log_has_no_credentials(&stderr);
    Ok(())
}

#[test]
fn manifest_declares_one_sensitive_password_secret_field() -> Result<(), Box<dyn std::error::Error>>
{
    let manifest: luxd::application::plugin_protocol::PluginManifest = serde_json::from_slice(
        &std::fs::read("plugins/org.lux.notifier.wecom-app/manifest.json")?,
    )?;
    manifest
        .validate()
        .map_err(|error| format!("manifest is not installable: {error}"))?;

    assert_eq!(manifest.id, "org.lux.notifier.wecom-app");
    assert_eq!(manifest.version, "0.1.0");
    assert_eq!(manifest.plugin_type, "notification");
    assert_eq!(manifest.category, "NOTIFICATION");
    assert_eq!(manifest.capabilities, vec!["notification.send".to_owned()]);

    let secrets: Vec<_> = manifest
        .config_fields
        .iter()
        .filter(|field| field.key == "secret")
        .collect();
    assert_eq!(secrets.len(), 1, "exactly one secret field is allowed");
    let secret = secrets[0];
    assert!(secret.sensitive);
    assert!(secret.required);
    assert_eq!(secret.input_type, "password");
    assert!(secret.default_value.is_none());

    for field in &manifest.config_fields {
        if field.key == "secret" {
            continue;
        }
        assert!(!field.sensitive, "{} must not be sensitive", field.key);
        assert_ne!(field.input_type, "password", "{}", field.key);
    }
    let agent_id = manifest
        .config_fields
        .iter()
        .find(|field| field.key == "agentId")
        .ok_or("agentId must be declared")?;
    assert_eq!(agent_id.input_type, "number");
    assert!(agent_id.required);
    Ok(())
}

#[tokio::test]
async fn fails_when_every_recipient_is_invalid() -> Result<(), Box<dyn std::error::Error>> {
    let upstream = Upstream::default();
    let responses = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/cgi-bin/gettoken", upstream.token_handler(TOKEN_ONE))
        .route(
            "/cgi-bin/message/send",
            post({
                let responses = responses.clone();
                move || {
                    let responses = responses.clone();
                    async move {
                        match responses.fetch_add(1, Ordering::SeqCst) {
                            0 => Json(json!({
                                "errcode": 0,
                                "msgid": "msg-unused-1",
                                "invaliduser": "user-a|user-b",
                            })),
                            1 => Json(json!({
                                "errcode": 0,
                                "msgid": "msg-unused-2",
                                "unlicenseduser": "user-c",
                            })),
                            _ => Json(json!({
                                "errcode": 0,
                                "msgid": "msg-partial",
                                "invaliduser": "user-b",
                            })),
                        }
                    }
                }
            }),
        );
    let (base, server) = spawn_server(app).await?;
    let mut plugin = Plugin::start().await?;

    let mut no_user = api_base_config(&base);
    no_user["toUser"] = json!("user-a|user-b");
    let all_invalid_users = plugin
        .send("invalid-users", send_params(no_user, json!(TEST_SECRET)))
        .await?;
    assert_eq!(all_invalid_users.status, NotificationSendStatus::Failed);
    assert_eq!(
        all_invalid_users.error_code.as_deref(),
        Some("no_valid_recipient")
    );
    assert!(all_invalid_users.provider_request_id.is_none());

    let mut unlicensed = api_base_config(&base);
    unlicensed["toUser"] = json!("user-c");
    let all_unlicensed = plugin
        .send("unlicensed", send_params(unlicensed, json!(TEST_SECRET)))
        .await?;
    assert_eq!(all_unlicensed.status, NotificationSendStatus::Failed);
    assert_eq!(
        all_unlicensed.error_code.as_deref(),
        Some("no_valid_recipient")
    );

    let mut partial = api_base_config(&base);
    partial["toUser"] = json!("user-a|user-b");
    let partially_invalid = plugin
        .send("partial", send_params(partial, json!(TEST_SECRET)))
        .await?;
    assert_eq!(
        partially_invalid.status,
        NotificationSendStatus::Delivered,
        "a partially invalid recipient list still delivers"
    );
    assert_eq!(
        partially_invalid.provider_request_id.as_deref(),
        Some("msg-partial")
    );

    let stderr = plugin.finish().await?;
    server.abort();

    assert_eq!(upstream.token_calls(), 1, "the token stays cached");
    assert!(
        stderr.contains("1 invalid recipient"),
        "the log must count invalid recipients: {stderr}"
    );
    for user_id in ["user-a", "user-b", "user-c"] {
        assert!(
            !stderr.contains(user_id),
            "the log must not carry a recipient id ({user_id}): {stderr}"
        );
    }
    for result in [&all_invalid_users, &all_unlicensed, &partially_invalid] {
        assert_result_has_no_credentials(result);
    }
    assert_log_has_no_credentials(&stderr);
    Ok(())
}

#[tokio::test]
async fn keeps_percent_encoded_base_paths_intact() -> Result<(), Box<dyn std::error::Error>> {
    let paths = Arc::new(Mutex::new(Vec::<String>::new()));
    let app = Router::new().fallback({
        let paths = paths.clone();
        move |uri: axum::http::Uri| {
            let paths = paths.clone();
            async move {
                let path = uri.path().to_owned();
                if let Ok(mut captured) = paths.lock() {
                    captured.push(path.clone());
                }
                if path.ends_with("/cgi-bin/gettoken") {
                    return Json(token_body(TOKEN_ONE)).into_response();
                }
                if path.ends_with("/cgi-bin/message/send") {
                    return Json(json!({"errcode": 0, "msgid": "msg-encoded"})).into_response();
                }
                StatusCode::NOT_FOUND.into_response()
            }
        }
    });
    let (base, server) = spawn_server(app).await?;
    let mut proxy_config = api_base_config(&format!("{base}/proxy%2Fprefix"));
    proxy_config["toUser"] = json!("@all");
    let mut plugin = Plugin::start().await?;
    let result = plugin
        .send("encoded", send_params(proxy_config, json!(TEST_SECRET)))
        .await?;
    let stderr = plugin.finish().await?;
    server.abort();

    assert_eq!(result.status, NotificationSendStatus::Delivered);
    let paths = paths.lock().map(|paths| paths.clone()).unwrap_or_default();
    assert_eq!(
        paths,
        vec![
            "/proxy%2Fprefix/cgi-bin/gettoken".to_owned(),
            "/proxy%2Fprefix/cgi-bin/message/send".to_owned(),
        ],
        "an existing percent escape must reach the upstream unchanged"
    );
    assert_log_has_no_credentials(&stderr);
    Ok(())
}

#[tokio::test]
async fn bounds_a_stalled_message_send() -> Result<(), Box<dyn std::error::Error>> {
    let upstream = Upstream::default();
    let app = Router::new()
        .route("/cgi-bin/gettoken", upstream.token_handler(TOKEN_ONE))
        .route(
            "/cgi-bin/message/send",
            post(|| async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Json(json!({"errcode": 0, "msgid": "never"}))
            }),
        );
    let (base, server) = spawn_server(app).await?;
    let mut plugin = Plugin::start().await?;
    let started = std::time::Instant::now();
    let result = plugin
        .send(
            "stalled-send",
            send_params(api_base_config(&base), json!(TEST_SECRET)),
        )
        .await?;
    let elapsed = started.elapsed();
    let stderr = plugin.finish().await?;
    server.abort();

    assert_eq!(result.status, NotificationSendStatus::Retryable);
    assert_eq!(result.error_code.as_deref(), Some("network_error"));
    assert!(
        elapsed < Duration::from_secs(10),
        "one request must stay inside its own timeout, took {elapsed:?}"
    );
    assert_log_has_no_credentials(&stderr);
    Ok(())
}

#[tokio::test]
async fn rejects_a_loopback_destination_without_private_network_approval()
-> Result<(), Box<dyn std::error::Error>> {
    let requests = RequestCount::default();
    let (base, server) = spawn_server(requests.server()).await?;
    let mut plugin = Plugin::start().await?;

    let mut without_flag = send_params(api_base_config(&base), json!(TEST_SECRET));
    without_flag["target"] = json!({"url": ""});
    let absent = plugin.send("absent-flag", without_flag).await?;
    assert_eq!(absent.status, NotificationSendStatus::Failed);
    assert_eq!(
        absent.error_code.as_deref(),
        Some("destination_not_allowed")
    );
    assert!(absent.provider_request_id.is_none());

    let mut denied = send_params(api_base_config(&base), json!(TEST_SECRET));
    denied["target"] = json!({"url": "", "allowPrivateNetwork": false});
    let explicit = plugin.send("denied", denied).await?;
    assert_eq!(explicit.status, NotificationSendStatus::Failed);
    assert_eq!(
        explicit.error_code.as_deref(),
        Some("destination_not_allowed")
    );

    let stderr = plugin.finish().await?;
    server.abort();
    assert_eq!(
        requests.get(),
        0,
        "a rejected destination must not be contacted"
    );
    assert!(
        stderr.contains("destination_not_allowed"),
        "the log must carry the stable code: {stderr}"
    );
    assert_log_has_no_credentials(&stderr);
    assert_result_has_no_credentials(&absent);
    assert_result_has_no_credentials(&explicit);
    Ok(())
}

#[tokio::test]
async fn rejects_the_cloud_metadata_address_even_with_private_network_approval()
-> Result<(), Box<dyn std::error::Error>> {
    let mut config = api_base_config("http://169.254.169.254:80");
    config["toUser"] = json!("@all");
    let mut plugin = Plugin::start().await?;
    let result = plugin
        .send("metadata", send_params(config, json!(TEST_SECRET)))
        .await?;
    let stderr = plugin.finish().await?;

    assert_eq!(result.status, NotificationSendStatus::Failed);
    assert_eq!(
        result.error_code.as_deref(),
        Some("destination_not_allowed"),
        "a link-local metadata address stays forbidden after private-network approval"
    );
    assert_result_has_no_credentials(&result);
    assert!(
        !stderr.contains("169.254"),
        "the log must not carry the rejected destination: {stderr}"
    );
    assert_log_has_no_credentials(&stderr);
    Ok(())
}

#[tokio::test]
async fn rejects_an_explicitly_emptied_to_user() -> Result<(), Box<dyn std::error::Error>> {
    let upstream = Upstream::default();
    let app = Router::new()
        .route("/cgi-bin/gettoken", upstream.token_handler(TOKEN_ONE))
        .route(
            "/cgi-bin/message/send",
            upstream.send_handler(json!({"errcode": 0, "msgid": "msg-to-user"})),
        );
    let (base, server) = spawn_server(app).await?;
    let mut plugin = Plugin::start().await?;

    // Only an absent key falls back to the declared default.
    let mut absent = api_base_config(&base);
    absent
        .as_object_mut()
        .ok_or("config must be an object")?
        .remove("toUser");
    let defaulted = plugin
        .send("defaulted", send_params(absent, json!(TEST_SECRET)))
        .await?;
    assert_eq!(defaulted.status, NotificationSendStatus::Delivered);

    // An explicitly cleared value is a target configuration error, not a silent fallback.
    for (id, to_user) in [
        ("empty", json!("")),
        ("blank", json!("   \t  ")),
        ("null", Value::Null),
    ] {
        let mut config = api_base_config(&base);
        config["toUser"] = to_user;
        let result = plugin
            .send(id, send_params(config, json!(TEST_SECRET)))
            .await?;
        assert_eq!(
            result.status,
            NotificationSendStatus::Failed,
            "{id} must fail permanently"
        );
        assert_eq!(
            result.error_code.as_deref(),
            Some("invalid_config"),
            "{id} must use the stable config error code"
        );
    }

    // The plugin-side limit follows the host's 4096 character target value bound.
    let mut long = api_base_config(&base);
    long["toUser"] = json!("u".repeat(4_096));
    let long_result = plugin
        .send("long", send_params(long, json!(TEST_SECRET)))
        .await?;
    assert_eq!(
        long_result.status,
        NotificationSendStatus::Delivered,
        "a 4096 character recipient list is still accepted"
    );

    let stderr = plugin.finish().await?;
    server.abort();

    assert_eq!(upstream.token_calls(), 1, "the token stays cached");
    let requests = upstream.send_requests();
    assert_eq!(requests[0]["body"]["touser"], "@all");
    assert_eq!(
        requests[1]["body"]["touser"],
        json!("u".repeat(4_096)),
        "the long recipient list reaches WeCom unchanged"
    );
    assert_log_has_no_credentials(&stderr);
    Ok(())
}

#[tokio::test]
async fn caches_tokens_per_api_base() -> Result<(), Box<dyn std::error::Error>> {
    let first = Upstream::default();
    let first_app = Router::new()
        .route("/cgi-bin/gettoken", first.token_handler(TOKEN_ONE))
        .route(
            "/cgi-bin/message/send",
            first.send_handler(json!({"errcode": 0, "msgid": "msg-first"})),
        );
    let (first_base, first_server) = spawn_server(first_app).await?;

    let second = Upstream::default();
    let second_app = Router::new()
        .route("/cgi-bin/gettoken", second.token_handler(TOKEN_TWO))
        .route(
            "/cgi-bin/message/send",
            second.send_handler(json!({"errcode": 0, "msgid": "msg-second"})),
        );
    let (second_base, second_server) = spawn_server(second_app).await?;

    let mut plugin = Plugin::start().await?;
    for (id, base, expected_id) in [
        ("first", first_base.as_str(), "msg-first"),
        ("second", second_base.as_str(), "msg-second"),
    ] {
        let result = plugin
            .send(id, send_params(api_base_config(base), json!(TEST_SECRET)))
            .await?;
        assert_eq!(result.status, NotificationSendStatus::Delivered);
        assert_eq!(result.provider_request_id.as_deref(), Some(expected_id));
    }
    let stderr = plugin.finish().await?;
    first_server.abort();
    second_server.abort();

    assert_eq!(
        first.token_calls(),
        1,
        "each API base caches its own access token"
    );
    assert_eq!(
        second.token_calls(),
        1,
        "a token cached for another API base must not be reused"
    );
    assert_eq!(first.send_requests()[0]["query"]["access_token"], TOKEN_ONE);
    assert_eq!(
        second.send_requests()[0]["query"]["access_token"],
        TOKEN_TWO
    );
    assert_log_has_no_credentials(&stderr);
    Ok(())
}

#[tokio::test]
async fn rejects_a_malformed_secret() -> Result<(), Box<dyn std::error::Error>> {
    let upstream = Upstream::default();
    let app = Router::new()
        .route("/cgi-bin/gettoken", upstream.token_handler(TOKEN_ONE))
        .route(
            "/cgi-bin/message/send",
            upstream.send_handler(json!({"errcode": 0, "msgid": "msg-long-secret"})),
        );
    let (base, server) = spawn_server(app).await?;
    let mut plugin = Plugin::start().await?;

    for (id, secret) in [
        ("oversized", json!("s".repeat(4_097))),
        ("control", json!("corp\nsecret")),
    ] {
        let result = plugin
            .send(id, send_params(api_base_config(&base), secret))
            .await?;
        assert_eq!(result.status, NotificationSendStatus::Failed);
        assert_eq!(
            result.error_code.as_deref(),
            Some("invalid_secret"),
            "{id} is present but malformed"
        );
    }
    assert_eq!(upstream.token_calls(), 0, "a malformed secret is not sent");

    let accepted = plugin
        .send(
            "max-length",
            send_params(api_base_config(&base), json!("s".repeat(4_096))),
        )
        .await?;
    assert_eq!(
        accepted.status,
        NotificationSendStatus::Delivered,
        "the host's 4096 character credential bound is accepted"
    );

    let stderr = plugin.finish().await?;
    server.abort();
    assert!(
        !stderr.contains("corp\nsecret"),
        "a rejected secret must never be logged: {stderr}"
    );
    assert_log_has_no_credentials(&stderr);
    Ok(())
}
