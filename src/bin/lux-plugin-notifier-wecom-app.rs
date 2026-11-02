//! Lux notification plugin for a WeCom (企业微信) self-built application.
//!
//! The process shell, outbound HTTP client, status classification helpers and event text rendering
//! all come from `luxd::application::notifier_plugin_sdk`; this file only owns the WeCom app
//! authentication and payload building described in `docs/PLUGIN-SDK.md`.
//!
//! The application `corpsecret` only ever arrives through the RPC request's `secret` field. It is
//! never logged, never returned and never embedded in an error message, and neither the access
//! token nor any request URL (including its query) is written to stderr or to the RPC result.

use luxd::application::{
    notifier_plugin_sdk::{failed, outbound_http_client_from_env, render_event_text, run_notifier},
    plugin_protocol::{NotificationSendRpcRequest, NotificationSendRpcResult},
};
use reqwest::{Client, Url};
use serde_json::Value;

/// WeCom app API base used when the destination config does not override it.
const DEFAULT_API_BASE: &str = "https://qyapi.weixin.qq.com";
const MAX_CORP_ID_CHARS: usize = 64;
const MAX_SECRET_CHARS: usize = 512;
const MAX_TO_USER_CHARS: usize = 1_000;
/// WeCom `agentid` is a positive 32-bit integer.
const MAX_AGENT_ID: i64 = 2_147_483_647;

const ERROR_MISSING_SECRET: &str = "missing_secret";
const ERROR_INVALID_CONFIG: &str = "invalid_config";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = outbound_http_client_from_env()?;
    run_notifier(move |request| {
        let client = client.clone();
        async move { deliver(&client, request).await }
    })
    .await?;
    Ok(())
}

/// Delivers one `notification.send` request.
async fn deliver(
    _client: &Client,
    request: NotificationSendRpcRequest,
) -> NotificationSendRpcResult {
    let Some(_secret) = destination_secret(request.secret.as_deref()) else {
        log_failure(ERROR_MISSING_SECRET);
        return failed(ERROR_MISSING_SECRET);
    };
    let Ok(_config) = WecomAppConfig::from_config_value(&request.config) else {
        log_failure(ERROR_INVALID_CONFIG);
        return failed(ERROR_INVALID_CONFIG);
    };
    let (_title, _body) = render_event_text(&request.event);
    failed("not_implemented")
}

/// The non-secret destination settings of one WeCom app target.
#[derive(Debug)]
struct WecomAppConfig {
    corp_id: String,
    agent_id: i64,
    to_user: String,
    api_base: Url,
}

impl WecomAppConfig {
    /// Validates the per-destination `config` object.
    ///
    /// Every rejected value is collapsed into one stable `invalid_config` outcome so a bad target
    /// never produces an error message carrying the value it rejected.
    fn from_config_value(config: &Value) -> Result<Self, ()> {
        let object = config.as_object().ok_or(())?;
        let corp_id = object
            .get("corpId")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or(())?;
        if corp_id.chars().count() > MAX_CORP_ID_CHARS
            || !corp_id.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            })
        {
            return Err(());
        }
        let agent_id = parse_agent_id(object.get("agentId")).ok_or(())?;
        let to_user = parse_to_user(object.get("toUser"))?;
        let api_base = parse_api_base(object.get("apiBase"))?;
        Ok(Self {
            corp_id: corp_id.to_owned(),
            agent_id,
            to_user,
            api_base,
        })
    }
}

fn parse_agent_id(value: Option<&Value>) -> Option<i64> {
    let agent_id = match value? {
        Value::Number(number) => number.as_i64()?,
        Value::String(text) => text.trim().parse::<i64>().ok()?,
        _ => return None,
    };
    (1..=MAX_AGENT_ID).contains(&agent_id).then_some(agent_id)
}

fn parse_to_user(value: Option<&Value>) -> Result<String, ()> {
    let to_user = match value {
        None | Some(Value::Null) => String::new(),
        Some(value) => value.as_str().ok_or(())?.trim().to_owned(),
    };
    if to_user.is_empty() {
        return Ok("@all".to_owned());
    }
    if to_user.chars().count() > MAX_TO_USER_CHARS
        || !to_user
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "@._-|".contains(character))
    {
        return Err(());
    }
    Ok(to_user)
}

fn parse_api_base(value: Option<&Value>) -> Result<Url, ()> {
    let raw = match value {
        None | Some(Value::Null) => DEFAULT_API_BASE,
        Some(Value::String(text)) => text.trim(),
        Some(_) => return Err(()),
    };
    let raw = if raw.is_empty() {
        DEFAULT_API_BASE
    } else {
        raw
    };
    if raw.chars().any(char::is_control) {
        return Err(());
    }
    let url = Url::parse(raw).map_err(|_| ())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(());
    }
    Ok(url)
}

/// Returns the usable destination credential, or `None` when it is missing or malformed.
///
/// The value is only kept in memory for the outbound request; it is never logged or returned.
fn destination_secret(secret: Option<&str>) -> Option<String> {
    let secret = secret?.trim();
    if secret.is_empty()
        || secret.chars().count() > MAX_SECRET_CHARS
        || secret.chars().any(char::is_control)
    {
        return None;
    }
    Some(secret.to_owned())
}

fn log_failure(code: &str) {
    eprintln!("wecom-app notifier: notification failed ({code})");
}
