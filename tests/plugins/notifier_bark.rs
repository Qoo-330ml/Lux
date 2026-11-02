//! End-to-end tests for the Bark notification plugin (`org.lux.notifier.bark`).
//!
//! Every test drives the real plugin process over the Lux stdio JSON-RPC protocol and points the
//! plugin at a local mock Bark server on `127.0.0.1`. No test reaches a real Bark deployment and no
//! real credential appears anywhere in this file.

use std::process::Stdio;

use luxd::application::plugin_protocol::{
    NotificationSendRpcResult, NotificationSendStatus, PluginManifest, PluginRequest,
    PluginResponse,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
};

const PLUGIN_ID: &str = "org.lux.notifier.bark";

struct Plugin {
    child: Child,
    stdin: ChildStdin,
    lines: tokio::io::Lines<BufReader<ChildStdout>>,
    stderr: ChildStderr,
}

impl Plugin {
    fn spawn() -> Result<Self, Box<dyn std::error::Error>> {
        let mut child = Command::new(env!("CARGO_BIN_EXE_lux-plugin-notifier-bark"))
            // A notification plugin never receives LUX_CONFIG_DIR, so the plugin must not depend on
            // it. Proxy variables are cleared so the loopback mock server is always reached directly.
            .env_remove("LUX_CONFIG_DIR")
            .env_remove("LUX_PROXY_URL")
            .env_remove("HTTP_PROXY")
            .env_remove("HTTPS_PROXY")
            .env_remove("ALL_PROXY")
            .env_remove("http_proxy")
            .env_remove("https_proxy")
            .env_remove("all_proxy")
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost")
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
fn notification_params(config: Value, secret: Option<&str>) -> Value {
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
        "target": {"url": "", "allowPrivateNetwork": false},
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
