//! Lux notification plugin for a WeCom (企业微信) self-built application.
//!
//! The process shell, outbound HTTP client, status classification helpers and event text rendering
//! all come from `luxd::application::notifier_plugin_sdk`; this file only owns the WeCom app
//! authentication and payload building described in `docs/PLUGIN-SDK.md`.
//!
//! The application `corpsecret` only ever arrives through the RPC request's `secret` field. It is
//! never logged, never returned and never embedded in an error message, and neither the access
//! token nor any request URL (including its query) is written to stderr or to the RPC result.

use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    sync::{Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use luxd::application::{
    notifier_plugin_sdk::{
        delivered, failed, outbound_http_client_from_env, render_event_text,
        result_from_http_status, retryable, run_notifier,
    },
    plugin_protocol::{
        NotificationSendRpcRequest, NotificationSendRpcResult, NotificationSendStatus,
    },
};
use reqwest::{Client, Url, header::HeaderMap};
use serde_json::{Value, json};

/// WeCom app API base used when the destination config does not override it.
const DEFAULT_API_BASE: &str = "https://qyapi.weixin.qq.com";
/// Upper bound of any upstream response body read by this plugin.
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
/// WeCom text messages are truncated by the upstream API at 2048 bytes.
const WECOM_TEXT_MAX_BYTES: usize = 2_048;
/// A cached access token is refreshed this long before WeCom expires it.
const TOKEN_REFRESH_MARGIN_SECONDS: i64 = 300;
/// WeCom issues tokens for 7200 seconds; anything longer is treated as hostile input.
const DEFAULT_TOKEN_LIFETIME_SECONDS: i64 = 7_200;
const MAX_TOKEN_LIFETIME_SECONDS: i64 = 86_400;
/// Upper bound of cached access tokens; entries are addressable per destination credential.
const MAX_CACHE_ENTRIES: usize = 64;
const MAX_CORP_ID_CHARS: usize = 64;
const MAX_SECRET_CHARS: usize = 512;
const MAX_TO_USER_CHARS: usize = 1_000;
const MAX_ACCESS_TOKEN_CHARS: usize = 1_024;
/// WeCom `agentid` is a positive 32-bit integer.
const MAX_AGENT_ID: i64 = 2_147_483_647;
const MAX_PROVIDER_REQUEST_ID_CHARS: usize = 128;
/// Upper bound of a `Retry-After` hint accepted from upstream, in seconds.
const MAX_RETRY_AFTER_SECONDS: i64 = 7_200;

const ERROR_MISSING_SECRET: &str = "missing_secret";
const ERROR_INVALID_CONFIG: &str = "invalid_config";
const ERROR_NETWORK: &str = "network_error";
const ERROR_RESPONSE_TOO_LARGE: &str = "response_too_large";
const ERROR_INVALID_RESPONSE: &str = "invalid_response";
const ERROR_RATE_LIMITED: &str = "rate_limited";
const ERROR_SYSTEM_BUSY: &str = "system_busy";
const ERROR_TOKEN_REJECTED: &str = "token_rejected";

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
    client: &Client,
    request: NotificationSendRpcRequest,
) -> NotificationSendRpcResult {
    let Some(secret) = destination_secret(request.secret.as_deref()) else {
        log_failure(ERROR_MISSING_SECRET);
        return failed(ERROR_MISSING_SECRET);
    };
    let Ok(config) = WecomAppConfig::from_config_value(&request.config) else {
        log_failure(ERROR_INVALID_CONFIG);
        return failed(ERROR_INVALID_CONFIG);
    };
    let (title, body) = render_event_text(&request.event);
    let content = compose_text(&title, &body);
    let cache_key = token_cache_key(&config, &secret);

    let result = match send_message(client, &config, &secret, cache_key, &content, false).await {
        Attempt::Done(result) => result,
        Attempt::TokenRejected => {
            // WeCom rejected the cached token: drop it, fetch a fresh one and retry exactly once.
            evict_token(cache_key);
            match send_message(client, &config, &secret, cache_key, &content, true).await {
                Attempt::Done(result) => result,
                Attempt::TokenRejected => failed(ERROR_TOKEN_REJECTED),
            }
        }
    };
    log_outcome(&result);
    result
}

/// One WeCom delivery attempt.
enum Attempt {
    Done(NotificationSendRpcResult),
    /// WeCom answered with a token error, so the caller must refresh the token and retry once.
    TokenRejected,
}

async fn send_message(
    client: &Client,
    config: &WecomAppConfig,
    secret: &str,
    cache_key: u64,
    content: &str,
    force_refresh: bool,
) -> Attempt {
    let access_token = match access_token(client, config, secret, cache_key, force_refresh).await {
        Ok(access_token) => access_token,
        Err(result) => return Attempt::Done(result),
    };
    let mut url = api_url(&config.api_base, "/cgi-bin/message/send");
    url.query_pairs_mut()
        .append_pair("access_token", &access_token);
    let payload = json!({
        "touser": config.to_user,
        "msgtype": "text",
        "agentid": config.agent_id,
        "text": {"content": content},
    });
    let response = match client.post(url).json(&payload).send().await {
        Ok(response) => response,
        Err(_) => return Attempt::Done(retryable(ERROR_NETWORK)),
    };
    let status = response.status();
    let retry_after = retry_after_seconds(response.headers());
    if !status.is_success() {
        return Attempt::Done(result_from_http_status(status.as_u16(), retry_after));
    }
    let body = match read_bounded(response).await {
        Ok(body) => body,
        Err(result) => return Attempt::Done(result),
    };
    let Some(errcode) = parse_errcode(&body) else {
        return Attempt::Done(failed(ERROR_INVALID_RESPONSE));
    };
    match errcode {
        0 => Attempt::Done(delivered_message(&body)),
        40001 | 40014 | 42001 => Attempt::TokenRejected,
        other => Attempt::Done(errcode_result(other, retry_after)),
    }
}

/// Returns a usable access token, using and refreshing the in-process cache.
///
/// The cache lock is only held for a map lookup or insert, never across an `.await`.
async fn access_token(
    client: &Client,
    config: &WecomAppConfig,
    secret: &str,
    cache_key: u64,
    force_refresh: bool,
) -> Result<String, NotificationSendRpcResult> {
    let now = unix_now();
    if !force_refresh && let Some(access_token) = cached_token(cache_key, now) {
        return Ok(access_token);
    }
    let mut url = api_url(&config.api_base, "/cgi-bin/gettoken");
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("corpid", &config.corp_id);
        query.append_pair("corpsecret", secret);
    }
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|_| retryable(ERROR_NETWORK))?;
    let status = response.status();
    let retry_after = retry_after_seconds(response.headers());
    if !status.is_success() {
        return Err(result_from_http_status(status.as_u16(), retry_after));
    }
    let body = read_bounded(response).await?;
    let Some(errcode) = parse_errcode(&body) else {
        return Err(failed(ERROR_INVALID_RESPONSE));
    };
    if errcode != 0 {
        return Err(errcode_result(errcode, retry_after));
    }
    let value: Value = serde_json::from_slice(&body).map_err(|_| failed(ERROR_INVALID_RESPONSE))?;
    let access_token = value
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|access_token| {
            !access_token.is_empty()
                && access_token.chars().count() <= MAX_ACCESS_TOKEN_CHARS
                && !access_token.chars().any(char::is_control)
        })
        .map(str::to_owned)
        .ok_or_else(|| failed(ERROR_INVALID_RESPONSE))?;
    let expires_in = value
        .get("expires_in")
        .and_then(Value::as_i64)
        .unwrap_or(DEFAULT_TOKEN_LIFETIME_SECONDS);
    store_token(cache_key, access_token.clone(), expires_in, now);
    Ok(access_token)
}

/// Reads a bounded response body.
async fn read_bounded(
    mut response: reqwest::Response,
) -> Result<Vec<u8>, NotificationSendRpcResult> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(failed(ERROR_RESPONSE_TOO_LARGE));
    }
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                    return Err(failed(ERROR_RESPONSE_TOO_LARGE));
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => return Ok(body),
            Err(_) => return Err(retryable(ERROR_NETWORK)),
        }
    }
}

fn parse_errcode(body: &[u8]) -> Option<i64> {
    serde_json::from_slice::<Value>(body)
        .ok()?
        .get("errcode")?
        .as_i64()
}

/// Maps a WeCom `errcode` onto the host delivery classification.
fn errcode_result(errcode: i64, retry_after: Option<i64>) -> NotificationSendRpcResult {
    match errcode {
        -1 => retryable_with(ERROR_SYSTEM_BUSY, retry_after),
        45009 => retryable_with(ERROR_RATE_LIMITED, retry_after),
        40001 | 40014 | 42001 => failed(ERROR_TOKEN_REJECTED),
        other => failed(&format!("upstream_errcode_{}", other.unsigned_abs())),
    }
}

fn retryable_with(code: &str, retry_after: Option<i64>) -> NotificationSendRpcResult {
    let mut result = retryable(code);
    result.retry_after_seconds =
        retry_after.map(|seconds| seconds.clamp(0, MAX_RETRY_AFTER_SECONDS));
    result
}

fn delivered_message(body: &[u8]) -> NotificationSendRpcResult {
    let mut result = delivered();
    result.provider_request_id = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("msgid")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|msg_id| {
                    !msg_id.is_empty()
                        && msg_id.chars().count() <= MAX_PROVIDER_REQUEST_ID_CHARS
                        && !msg_id.chars().any(char::is_control)
                })
                .map(str::to_owned)
        });
    result
}

fn retry_after_seconds(headers: &HeaderMap) -> Option<i64> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<i64>()
        .ok()
        .map(|seconds| seconds.clamp(0, MAX_RETRY_AFTER_SECONDS))
}

fn api_url(base: &Url, path: &str) -> Url {
    let mut url = base.clone();
    let mut joined = url.path().trim_end_matches('/').to_owned();
    joined.push_str(path);
    url.set_path(&joined);
    url.set_query(None);
    url.set_fragment(None);
    url
}

/// Joins the already rendered notification title and body into one WeCom text message.
fn compose_text(title: &str, body: &str) -> String {
    let title = title.trim();
    let body = body.trim();
    let combined = if body.is_empty() {
        title.to_owned()
    } else if title.is_empty() {
        body.to_owned()
    } else {
        format!("{title}\n\n{body}")
    };
    if combined.len() <= WECOM_TEXT_MAX_BYTES {
        return combined;
    }
    let mut end = WECOM_TEXT_MAX_BYTES;
    while end > 0 && !combined.is_char_boundary(end) {
        end -= 1;
    }
    combined[..end].to_owned()
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

/// A cached WeCom access token and the instant after which it must be refreshed.
#[derive(Debug)]
struct CachedToken {
    access_token: String,
    refresh_at: i64,
}

#[derive(Debug, Default)]
struct TokenCache {
    entries: Mutex<HashMap<u64, CachedToken>>,
}

fn token_cache() -> &'static TokenCache {
    static CACHE: OnceLock<TokenCache> = OnceLock::new();
    CACHE.get_or_init(TokenCache::default)
}

/// Hashes the credential-bearing cache identity so the map key never holds the secret itself.
fn token_cache_key(config: &WecomAppConfig, secret: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    config.corp_id.hash(&mut hasher);
    config.agent_id.hash(&mut hasher);
    secret.hash(&mut hasher);
    hasher.finish()
}

fn cached_token(cache_key: u64, now: i64) -> Option<String> {
    let entries = token_cache().entries.lock().ok()?;
    let entry = entries.get(&cache_key)?;
    (entry.refresh_at > now).then(|| entry.access_token.clone())
}

fn store_token(cache_key: u64, access_token: String, expires_in: i64, now: i64) {
    let lifetime = expires_in.clamp(0, MAX_TOKEN_LIFETIME_SECONDS);
    let refresh_at = now.saturating_add(lifetime - TOKEN_REFRESH_MARGIN_SECONDS);
    let Ok(mut entries) = token_cache().entries.lock() else {
        return;
    };
    // Tokens that can no longer be used are dropped so the cache stays small; when it still grows
    // past the bound it is emptied instead of tracking an eviction order for a pure cache.
    entries.retain(|_, entry| entry.refresh_at > now);
    if entries.len() >= MAX_CACHE_ENTRIES {
        entries.clear();
    }
    entries.insert(
        cache_key,
        CachedToken {
            access_token,
            refresh_at,
        },
    );
}

fn evict_token(cache_key: u64) {
    if let Ok(mut entries) = token_cache().entries.lock() {
        entries.remove(&cache_key);
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .unwrap_or_default()
}

fn log_failure(code: &str) {
    eprintln!("wecom-app notifier: notification failed ({code})");
}

fn log_outcome(result: &NotificationSendRpcResult) {
    match result.status {
        NotificationSendStatus::Delivered => {
            eprintln!("wecom-app notifier: notification delivered");
        }
        NotificationSendStatus::Retryable => eprintln!(
            "wecom-app notifier: notification retryable ({})",
            result.error_code.as_deref().unwrap_or("unknown")
        ),
        NotificationSendStatus::Failed => {
            log_failure(result.error_code.as_deref().unwrap_or("unknown"));
        }
    }
}
