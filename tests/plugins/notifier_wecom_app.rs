//! Integration tests for the WeCom app notification plugin.
//!
//! Every test drives the real plugin binary over the stdio JSON-RPC protocol against a local mock
//! WeCom endpoint on `127.0.0.1`; nothing here contacts a real WeCom tenant and no value in this
//! file is a real credential.

use std::{
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::Router;
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
    for needle in [TEST_SECRET, TOKEN_ONE, TOKEN_TWO, "corpsecret", "access_token"] {
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
        "target": {"url": "", "allowPrivateNetwork": false},
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
