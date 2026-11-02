//! Feishu / Lark custom bot notifier plugin.
//!
//! The plugin speaks the Lux stdio JSON-RPC protocol through
//! [`luxd::application::notifier_plugin_sdk::run_notifier`] and only owns the Feishu-specific
//! payload and signature: the host keeps retries, delivery records and event rendering.
//!
//! The destination credential arrives in [`NotificationSendRpcRequest::secret`] as `hookId` or
//! `hookId#signSecret`. It is never logged, echoed in a result or written to a file.

use std::time::{SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use hmac::{Hmac, Mac};
use luxd::application::{
    notifier_plugin_sdk::{
        delivered, failed, guard_destination, outbound_http_client_builder_from_env,
        render_event_text, result_from_http_status, retryable, run_notifier,
    },
    plugin_protocol::{NotificationSendRpcRequest, NotificationSendRpcResult},
};
use reqwest::{Client, Url};
use serde_json::{Value, json};
use sha2::Sha256;

const PLUGIN_ID: &str = "org.lux.notifier.feishu";
const FEISHU_BASE_URL: &str = "https://open.feishu.cn";
const LARK_BASE_URL: &str = "https://open.larksuite.com";
const HOOK_PATH: &str = "/open-apis/bot/v2/hook";
const MAX_HOOK_ID_CHARS: usize = 128;
const MAX_SIGN_SECRET_CHARS: usize = 256;
const MAX_PROVIDER_RESPONSE_BYTES: usize = 64 * 1024;
/// Returned when the host proxy policy cannot be turned into a usable outbound client.
const CLIENT_UNAVAILABLE: &str = "client_unavailable";
/// Feishu's documented "request frequency limited" business code.
///
/// Only this code has frequency-control semantics. Other business codes, including the rest of the
/// 19xxx range, are ordinary upstream rejections and must stay permanent failures.
const FEISHU_RATE_LIMIT_CODE: i64 = 11_232;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Region {
    Feishu,
    Lark,
}

impl Region {
    fn default_base_url(self) -> &'static str {
        match self {
            Self::Feishu => FEISHU_BASE_URL,
            Self::Lark => LARK_BASE_URL,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MsgType {
    Text,
    Post,
}

#[derive(Clone, Debug)]
struct FeishuConfig {
    base: Url,
    msg_type: MsgType,
}

#[derive(Clone, Debug)]
struct FeishuCredential {
    hook_id: String,
    sign_secret: Option<String>,
}

impl FeishuCredential {
    fn signature(&self) -> Result<Option<(String, String)>, FeishuError> {
        let Some(sign_secret) = self.sign_secret.as_deref() else {
            return Ok(None);
        };
        let timestamp = unix_timestamp().to_string();
        let sign = sign_timestamp(sign_secret, &timestamp).ok_or(FeishuError::InvalidSecret)?;
        Ok(Some((timestamp, sign)))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FeishuError {
    MissingSecret,
    InvalidSecret,
    InvalidConfig,
    Timeout,
    Unreachable,
    ResponseTooLarge,
    InvalidResponse,
}

impl FeishuError {
    fn code(self) -> &'static str {
        match self {
            Self::MissingSecret => "missing_secret",
            Self::InvalidSecret => "invalid_secret",
            Self::InvalidConfig => "invalid_config",
            Self::Timeout => "provider_timeout",
            Self::Unreachable => "provider_unreachable",
            Self::ResponseTooLarge => "provider_response_too_large",
            Self::InvalidResponse => "provider_invalid_response",
        }
    }

    fn into_result(self) -> NotificationSendRpcResult {
        match self {
            Self::Timeout | Self::Unreachable => retryable(self.code()),
            _ => failed(self.code()),
        }
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    run_notifier(deliver).await
}

/// Delivers one notification and classifies the outcome for the host.
async fn deliver(request: NotificationSendRpcRequest) -> NotificationSendRpcResult {
    let config = match parse_config(&request.config) {
        Ok(config) => config,
        Err(error) => return log_failure(error),
    };
    let credential = match parse_credential(request.secret.as_deref()) {
        Ok(credential) => credential,
        Err(error) => return log_failure(error),
    };
    let url = hook_url(&config.base, &credential.hook_id);
    // The host decides whether the destination may be private; the plugin never widens it. The
    // guard runs before any client is built or connected, so a rejected destination cannot send.
    let guarded = match guard_destination(&url, allowed_private_network(&request.target)).await {
        Ok(guarded) => guarded,
        Err(error) => {
            log_line(&format!("destination rejected ({})", error.code()));
            return if error.is_retryable() {
                retryable(error.code())
            } else {
                failed(error.code())
            };
        }
    };
    let builder = match outbound_http_client_builder_from_env() {
        Ok(builder) => builder,
        Err(error) => {
            // A broken proxy configuration is an operator-side transient problem: retrying keeps
            // the delivery queued instead of dropping it permanently.
            log_line(&format!("outbound client unavailable: {error}"));
            return retryable(CLIENT_UNAVAILABLE);
        }
    };
    // Pin the address the guard approved so a second DNS answer cannot redirect this delivery.
    let client = match guarded.pin(builder).build() {
        Ok(client) => client,
        Err(error) => {
            log_line(&format!("outbound client unavailable: {error}"));
            return retryable(CLIENT_UNAVAILABLE);
        }
    };
    match send(&client, guarded.url(), &config, &credential, &request.event).await {
        Ok(result) => result,
        Err(error) => log_failure(error),
    }
}

/// Reads the host `allowPrivateNetwork` approval; an absent or non-boolean value means "not allowed".
fn allowed_private_network(target: &Value) -> bool {
    target
        .get("allowPrivateNetwork")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn log_failure(error: FeishuError) -> NotificationSendRpcResult {
    log_line(&format!("delivery failed: {}", error.code()));
    error.into_result()
}

fn log_line(message: &str) {
    eprintln!("{PLUGIN_ID}: {message}");
}

async fn send(
    client: &Client,
    url: &Url,
    config: &FeishuConfig,
    credential: &FeishuCredential,
    event: &Value,
) -> Result<NotificationSendRpcResult, FeishuError> {
    let (title, body) = render_event_text(event);
    let payload = build_payload(config.msg_type, &title, &body, credential.signature()?);
    let response = client
        .post(url.clone())
        .json(&payload)
        .send()
        .await
        .map_err(classify_request_error)?;
    let status = response.status();
    if !status.is_success() {
        let retry_after = parse_retry_after(
            response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
        );
        return Ok(result_from_http_status(status.as_u16(), retry_after));
    }
    let body = read_response_body(response).await?;
    let value: Value = serde_json::from_slice(&body).map_err(|_| FeishuError::InvalidResponse)?;
    let code = value
        .get("code")
        .and_then(|code| {
            code.as_i64()
                .or_else(|| code.as_str().and_then(|code| code.trim().parse().ok()))
        })
        .ok_or(FeishuError::InvalidResponse)?;
    if code == 0 {
        return Ok(delivered());
    }
    if is_rate_limited(code) {
        return Ok(retryable("provider_rate_limited"));
    }
    Ok(failed(&format!("provider_code_{code}")))
}

/// Parses the non-secret per-destination configuration.
///
/// The host validates the same fields against the manifest; the plugin still validates them
/// because the RPC request is the only input it can trust at delivery time.
fn parse_config(config: &Value) -> Result<FeishuConfig, FeishuError> {
    let Some(object) = config.as_object() else {
        return Err(FeishuError::InvalidConfig);
    };
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "region" | "apiBase" | "msgType"))
    {
        return Err(FeishuError::InvalidConfig);
    }
    let region = match object.get("region") {
        None => Region::Feishu,
        Some(value) => match value.as_str() {
            Some("feishu") => Region::Feishu,
            Some("lark") => Region::Lark,
            _ => return Err(FeishuError::InvalidConfig),
        },
    };
    let msg_type = match object.get("msgType") {
        None => MsgType::Post,
        Some(value) => match value.as_str() {
            Some("post") => MsgType::Post,
            Some("text") => MsgType::Text,
            _ => return Err(FeishuError::InvalidConfig),
        },
    };
    let base = match object.get("apiBase") {
        None => parse_base_url(region.default_base_url())?,
        Some(value) => {
            let raw = value.as_str().ok_or(FeishuError::InvalidConfig)?.trim();
            if raw.is_empty() {
                parse_base_url(region.default_base_url())?
            } else {
                parse_base_url(raw)?
            }
        }
    };
    Ok(FeishuConfig { base, msg_type })
}

/// Accepts only an absolute `http`/`https` base without credentials, query or fragment.
fn parse_base_url(raw: &str) -> Result<Url, FeishuError> {
    let url = Url::parse(raw).map_err(|_| FeishuError::InvalidConfig)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(FeishuError::InvalidConfig);
    }
    Ok(url)
}

/// Parses `hookId` or `hookId#signSecret` without ever echoing the value.
fn parse_credential(secret: Option<&str>) -> Result<FeishuCredential, FeishuError> {
    let raw = secret.map(str::trim).ok_or(FeishuError::MissingSecret)?;
    if raw.is_empty() {
        return Err(FeishuError::MissingSecret);
    }
    let (hook_id, sign_secret) = match raw.split_once('#') {
        Some((hook_id, sign_secret)) => (hook_id, Some(sign_secret)),
        None => (raw, None),
    };
    if !is_valid_hook_id(hook_id) {
        return Err(FeishuError::InvalidSecret);
    }
    let sign_secret = match sign_secret {
        None => None,
        Some(sign_secret) => {
            let valid = !sign_secret.is_empty()
                && sign_secret.chars().count() <= MAX_SIGN_SECRET_CHARS
                && sign_secret
                    .chars()
                    .all(|character| character.is_ascii_graphic());
            if !valid {
                return Err(FeishuError::InvalidSecret);
            }
            Some(sign_secret.to_owned())
        }
    };
    Ok(FeishuCredential {
        hook_id: hook_id.to_owned(),
        sign_secret,
    })
}

fn is_valid_hook_id(hook_id: &str) -> bool {
    !hook_id.is_empty()
        && hook_id.chars().count() <= MAX_HOOK_ID_CHARS
        && hook_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn hook_url(base: &Url, hook_id: &str) -> Url {
    let mut url = base.clone();
    let base_path = url.path().trim_end_matches('/').to_owned();
    url.set_path(&format!("{base_path}{HOOK_PATH}/{hook_id}"));
    url.set_query(None);
    url.set_fragment(None);
    url
}

fn build_payload(
    msg_type: MsgType,
    title: &str,
    body: &str,
    signature: Option<(String, String)>,
) -> Value {
    let body = if body.trim().is_empty() { title } else { body };
    let mut payload = match msg_type {
        // Feishu has no separate text title, so the title is the first line.
        MsgType::Text => json!({
            "msg_type": "text",
            "content": {"text": format!("{title}\n{body}")},
        }),
        // A `post` message carries the title in its own field: repeating it in the body would
        // render the same text twice.
        MsgType::Post => json!({
            "msg_type": "post",
            "content": {
                "post": {
                    "zh_cn": {
                        "title": title,
                        "content": [[{"tag": "text", "text": body}]],
                    },
                },
            },
        }),
    };
    if let Some((timestamp, sign)) = signature
        && let Some(object) = payload.as_object_mut()
    {
        object.insert("timestamp".to_owned(), Value::String(timestamp));
        object.insert("sign".to_owned(), Value::String(sign));
    }
    payload
}

/// Feishu signs `base64(HMAC-SHA256(key = timestamp + "\n" + signSecret, data = ""))`.
fn sign_timestamp(sign_secret: &str, timestamp: &str) -> Option<String> {
    let key = format!("{timestamp}\n{sign_secret}");
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).ok()?;
    mac.update(b"");
    Some(BASE64.encode(mac.finalize().into_bytes()))
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default()
}

fn is_rate_limited(code: i64) -> bool {
    code == FEISHU_RATE_LIMIT_CODE
}

fn parse_retry_after(value: Option<&str>) -> Option<i64> {
    value
        .and_then(|value| value.trim().parse::<i64>().ok())
        .filter(|seconds| *seconds >= 0)
}

fn classify_request_error(error: reqwest::Error) -> FeishuError {
    if error.is_timeout() {
        FeishuError::Timeout
    } else {
        FeishuError::Unreachable
    }
}

async fn read_response_body(mut response: reqwest::Response) -> Result<Vec<u8>, FeishuError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_PROVIDER_RESPONSE_BYTES as u64)
    {
        return Err(FeishuError::ResponseTooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(classify_request_error)? {
        if body.len().saturating_add(chunk.len()) > MAX_PROVIDER_RESPONSE_BYTES {
            return Err(FeishuError::ResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
