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
    collections::{HashMap, HashSet},
    hash::{DefaultHasher, Hash, Hasher},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use luxd::application::{
    notifier_plugin_sdk::{
        DestinationGuardError, delivered, failed, guard_destination,
        outbound_http_client_builder_from_env, render_event_text, result_from_http_status,
        retryable, run_notifier,
    },
    plugin_protocol::{
        NotificationSendRpcRequest, NotificationSendRpcResult, NotificationSendStatus,
    },
};
use reqwest::{Client, Url, header::HeaderMap};
use serde_json::{Value, json};

/// WeCom app API base used when the destination config does not override it.
const DEFAULT_API_BASE: &str = "https://qyapi.weixin.qq.com";
/// Hard deadline for one whole delivery, kept below the host's 30 second notification RPC timeout.
const TOTAL_DELIVERY_BUDGET: Duration = Duration::from_secs(25);
/// Deadline for one upstream request; the worst case (two attempts of a token plus a message) stays
/// inside [`TOTAL_DELIVERY_BUDGET`] and well below the SDK client's 10 second cap.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(6);
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
/// Target config and credential values the host accepts, mirrored so the plugin never rejects a
/// value the host already validated.
const MAX_CONFIG_TEXT_CHARS: usize = 4_096;
const MAX_SECRET_CHARS: usize = 4_096;
const MAX_ACCESS_TOKEN_CHARS: usize = 1_024;
/// The recipient list used only when the target omits `toUser` entirely.
const DEFAULT_TO_USER: &str = "@all";
/// WeCom `agentid` is a positive 32-bit integer.
const MAX_AGENT_ID: i64 = 2_147_483_647;
const MAX_PROVIDER_REQUEST_ID_CHARS: usize = 128;
/// Upper bound of a `Retry-After` hint accepted from upstream, in seconds.
const MAX_RETRY_AFTER_SECONDS: i64 = 7_200;

const ERROR_MISSING_SECRET: &str = "missing_secret";
const ERROR_INVALID_SECRET: &str = "invalid_secret";
const ERROR_INVALID_CONFIG: &str = "invalid_config";
const ERROR_NETWORK: &str = "network_error";
const ERROR_RESPONSE_TOO_LARGE: &str = "response_too_large";
const ERROR_INVALID_RESPONSE: &str = "invalid_response";
const ERROR_RATE_LIMITED: &str = "rate_limited";
const ERROR_SYSTEM_BUSY: &str = "system_busy";
const ERROR_TOKEN_REJECTED: &str = "token_rejected";
const ERROR_NO_VALID_RECIPIENT: &str = "no_valid_recipient";
/// Nothing had been sent yet when the delivery budget expired, so a retry cannot duplicate.
const ERROR_DELIVERY_TIMEOUT: &str = "delivery_timeout";
/// A message may already have reached WeCom, so retrying could send a duplicate notification.
const ERROR_TIMEOUT_AFTER_SEND: &str = "timeout_after_send";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Refuse to serve deliveries with a client that ignores the host proxy policy. The delivery
    // path rebuilds the client around each guarded destination, so only the policy is checked here.
    let _ = outbound_http_client_builder_from_env()?;
    run_notifier(deliver).await?;
    Ok(())
}

/// Delivers one `notification.send` request.
async fn deliver(request: NotificationSendRpcRequest) -> NotificationSendRpcResult {
    let secret = match destination_secret(request.secret.as_deref()) {
        Ok(secret) => secret,
        Err(code) => {
            log_failure(code);
            return failed(code);
        }
    };
    let Ok(config) = WecomAppConfig::from_config_value(&request.config) else {
        log_failure(ERROR_INVALID_CONFIG);
        return failed(ERROR_INVALID_CONFIG);
    };
    // The host decides whether the destination may be private; the plugin never widens it.
    let allow_private = allowed_private_network(&request.target);
    let (title, body) = render_event_text(&request.event);
    let content = compose_text(&title, &body);
    let cache_key = token_cache_key(&config, &secret);
    // Set once a `message/send` request may have reached WeCom, which decides whether a budget
    // expiry may be retried.
    let dispatched = AtomicBool::new(false);

    let delivery = match tokio::time::timeout(
        TOTAL_DELIVERY_BUDGET,
        run_delivery(
            &config,
            &secret,
            cache_key,
            &content,
            allow_private,
            &dispatched,
        ),
    )
    .await
    {
        Ok(delivery) => {
            if delivery.invalid_recipients > 0 {
                log_partial_recipients(delivery.invalid_recipients);
            }
            delivery.result
        }
        Err(_) => budget_expiry_result(dispatched.load(Ordering::SeqCst)),
    };
    log_outcome(&delivery);
    delivery
}

/// The outcome of one delivery attempt together with the recipient detail worth logging.
struct Delivery {
    result: NotificationSendRpcResult,
    invalid_recipients: usize,
}

impl Delivery {
    fn plain(result: NotificationSendRpcResult) -> Self {
        Self {
            result,
            invalid_recipients: 0,
        }
    }
}

/// Runs the delivery attempts inside the total deadline set by [`deliver`].
///
/// The configured API base passes the shared destination guard before any connection is opened, and
/// the accepted address is pinned into the request client against DNS rebinding.
async fn run_delivery(
    config: &WecomAppConfig,
    secret: &str,
    cache_key: u64,
    content: &str,
    allow_private: bool,
    dispatched: &AtomicBool,
) -> Delivery {
    let guarded = match guard_destination(&config.api_base, allow_private).await {
        Ok(guarded) => guarded,
        Err(error) => return Delivery::plain(guard_failure(error)),
    };
    let client = match outbound_http_client_builder_from_env() {
        Ok(builder) => builder,
        Err(_) => return Delivery::plain(retryable(ERROR_NETWORK)),
    };
    let client = match guarded.pin(client).build() {
        Ok(client) => client,
        Err(_) => return Delivery::plain(retryable(ERROR_NETWORK)),
    };
    match send_message(
        &client, config, secret, cache_key, content, false, dispatched,
    )
    .await
    {
        Attempt::Done(delivery) => delivery,
        Attempt::TokenRejected => {
            // WeCom rejected the cached token: drop it, fetch a fresh one and retry exactly once.
            evict_token(cache_key);
            match send_message(
                &client, config, secret, cache_key, content, true, dispatched,
            )
            .await
            {
                Attempt::Done(delivery) => delivery,
                Attempt::TokenRejected => Delivery::plain(failed(ERROR_TOKEN_REJECTED)),
            }
        }
    }
}

/// Reads the host `allowPrivateNetwork` approval; absent or non-boolean means "not allowed".
fn allowed_private_network(target: &Value) -> bool {
    target
        .get("allowPrivateNetwork")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Maps a destination guard rejection onto the delivery classification.
fn guard_failure(error: DestinationGuardError) -> NotificationSendRpcResult {
    eprintln!(
        "wecom-app notifier: destination rejected ({})",
        error.code()
    );
    if error.is_retryable() {
        retryable(error.code())
    } else {
        failed(error.code())
    }
}

/// One WeCom delivery attempt.
enum Attempt {
    Done(Delivery),
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
    dispatched: &AtomicBool,
) -> Attempt {
    let access_token = match access_token(client, config, secret, cache_key, force_refresh).await {
        Ok(access_token) => access_token,
        Err(result) => return Attempt::Done(Delivery::plain(result)),
    };
    let Some(mut url) = api_url(&config.api_base, &["cgi-bin", "message", "send"]) else {
        return Attempt::Done(Delivery::plain(failed(ERROR_INVALID_CONFIG)));
    };
    url.query_pairs_mut()
        .append_pair("access_token", &access_token);
    let payload = json!({
        "touser": config.to_user,
        "msgtype": "text",
        "agentid": config.agent_id,
        "text": {"content": content},
    });
    // From this point on WeCom may deliver the message even if this attempt never completes.
    dispatched.store(true, Ordering::SeqCst);
    let exchange =
        tokio::time::timeout(REQUEST_TIMEOUT, exchange(client.post(url).json(&payload))).await;
    let (body, retry_after) = match exchange {
        Ok(Ok(value)) => value,
        Ok(Err(result)) => return Attempt::Done(Delivery::plain(result)),
        Err(_) => return Attempt::Done(Delivery::plain(retryable(ERROR_NETWORK))),
    };
    let Some(value) = parse_body(&body) else {
        return Attempt::Done(Delivery::plain(failed(ERROR_INVALID_RESPONSE)));
    };
    let Some(errcode) = value.get("errcode").and_then(Value::as_i64) else {
        return Attempt::Done(Delivery::plain(failed(ERROR_INVALID_RESPONSE)));
    };
    match errcode {
        0 => Attempt::Done(delivered_message(&value, &config.to_user)),
        40001 | 40014 | 42001 => Attempt::TokenRejected,
        other => Attempt::Done(Delivery::plain(errcode_result(other, retry_after))),
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
    let Some(mut url) = api_url(&config.api_base, &["cgi-bin", "gettoken"]) else {
        return Err(failed(ERROR_INVALID_CONFIG));
    };
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("corpid", &config.corp_id);
        query.append_pair("corpsecret", secret);
    }
    let exchange = tokio::time::timeout(REQUEST_TIMEOUT, exchange(client.get(url))).await;
    let (body, retry_after) = match exchange {
        Ok(result) => result?,
        Err(_) => return Err(retryable(ERROR_NETWORK)),
    };
    let Some(value) = parse_body(&body) else {
        return Err(failed(ERROR_INVALID_RESPONSE));
    };
    let Some(errcode) = value.get("errcode").and_then(Value::as_i64) else {
        return Err(failed(ERROR_INVALID_RESPONSE));
    };
    if errcode != 0 {
        return Err(errcode_result(errcode, retry_after));
    }
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

/// Sends one request and returns its bounded body together with its `Retry-After` hint.
async fn exchange(
    request: reqwest::RequestBuilder,
) -> Result<(Vec<u8>, Option<i64>), NotificationSendRpcResult> {
    let response = request.send().await.map_err(|_| retryable(ERROR_NETWORK))?;
    let status = response.status();
    let retry_after = retry_after_seconds(response.headers());
    if !status.is_success() {
        return Err(result_from_http_status(status.as_u16(), retry_after));
    }
    let body = read_bounded(response).await?;
    Ok((body, retry_after))
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

fn parse_body(body: &[u8]) -> Option<Value> {
    serde_json::from_slice(body).ok()
}

/// Classifies an expired total delivery budget.
fn budget_expiry_result(message_dispatched: bool) -> NotificationSendRpcResult {
    if message_dispatched {
        // A message may already be delivered upstream, so a retry could duplicate the notification.
        failed(ERROR_TIMEOUT_AFTER_SEND)
    } else {
        retryable(ERROR_DELIVERY_TIMEOUT)
    }
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

/// Builds the `DELIVERED` result of an `errcode: 0` response.
fn delivered_message(value: &Value, to_user: &str) -> Delivery {
    match recipient_outcome(value, to_user) {
        RecipientOutcome::AllInvalid => Delivery::plain(failed(ERROR_NO_VALID_RECIPIENT)),
        RecipientOutcome::PartialInvalid(count) => Delivery {
            result: delivered_with_provider_id(value),
            invalid_recipients: count,
        },
        RecipientOutcome::AllValid => Delivery::plain(delivered_with_provider_id(value)),
    }
}

/// Describes whether WeCom rejected every requested recipient while still answering `errcode: 0`.
enum RecipientOutcome {
    AllValid,
    PartialInvalid(usize),
    AllInvalid,
}

fn recipient_outcome(value: &Value, to_user: &str) -> RecipientOutcome {
    // `@all` cannot be enumerated, so WeCom's `errcode: 0` already states the whole scope received
    // the message.
    if to_user.eq_ignore_ascii_case("@all") {
        return RecipientOutcome::AllValid;
    }
    let requested: Vec<&str> = to_user
        .split('|')
        .map(str::trim)
        .filter(|user_id| !user_id.is_empty())
        .collect();
    if requested.is_empty() {
        return RecipientOutcome::AllValid;
    }
    let invalid = invalid_recipient_ids(value);
    let invalid_count = requested
        .iter()
        .filter(|user_id| invalid.contains(&user_id.to_ascii_lowercase()))
        .count();
    if invalid_count == 0 {
        RecipientOutcome::AllValid
    } else if invalid_count == requested.len() {
        RecipientOutcome::AllInvalid
    } else {
        RecipientOutcome::PartialInvalid(invalid_count)
    }
}

/// Collects the invalid recipient identifiers WeCom reports, lowercased and split into ids.
fn invalid_recipient_ids(value: &Value) -> HashSet<String> {
    let mut ids = HashSet::new();
    for key in [
        "invaliduser",
        "invalidparty",
        "invalidtag",
        "unlicenseduser",
    ] {
        let Some(text) = value.get(key).and_then(Value::as_str) else {
            continue;
        };
        for user_id in text.split(['|', ',', ';']) {
            let user_id = user_id.trim();
            if !user_id.is_empty() {
                ids.insert(user_id.to_ascii_lowercase());
            }
        }
    }
    ids
}

fn delivered_with_provider_id(value: &Value) -> NotificationSendRpcResult {
    let mut result = delivered();
    result.provider_request_id = value
        .get("msgid")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|msg_id| {
            !msg_id.is_empty()
                && msg_id.chars().count() <= MAX_PROVIDER_REQUEST_ID_CHARS
                && !msg_id.chars().any(char::is_control)
        })
        .map(str::to_owned);
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

/// Appends API path segments to the configured base without re-encoding existing path escapes.
fn api_url(base: &Url, segments: &[&str]) -> Option<Url> {
    let mut url = base.clone();
    url.set_query(None);
    url.set_fragment(None);
    {
        let mut path = url.path_segments_mut().ok()?;
        path.pop_if_empty();
        for segment in segments {
            path.push(segment);
        }
    }
    Some(url)
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
        if corp_id.chars().count() > MAX_CONFIG_TEXT_CHARS
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
    // Only an absent key uses the declared default. An explicitly cleared value is a target
    // configuration error, so a mis-edited target cannot silently notify everyone instead.
    let Some(value) = value else {
        return Ok(DEFAULT_TO_USER.to_owned());
    };
    let to_user = value.as_str().ok_or(())?.trim();
    if to_user.is_empty()
        || to_user.chars().count() > MAX_CONFIG_TEXT_CHARS
        || !to_user
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "@._-|".contains(character))
    {
        return Err(());
    }
    Ok(to_user.to_owned())
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

/// Validates the destination credential.
///
/// A missing or blank credential is `missing_secret`; a present value the plugin cannot forward
/// safely is `invalid_secret`. The value is only kept in memory for the outbound request; it is
/// never logged or returned.
fn destination_secret(secret: Option<&str>) -> Result<String, &'static str> {
    let Some(secret) = secret else {
        return Err(ERROR_MISSING_SECRET);
    };
    let secret = secret.trim();
    if secret.is_empty() {
        return Err(ERROR_MISSING_SECRET);
    }
    if secret.chars().count() > MAX_SECRET_CHARS || secret.chars().any(char::is_control) {
        return Err(ERROR_INVALID_SECRET);
    }
    Ok(secret.to_owned())
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
///
/// The API base (scheme, host, port and path) is part of the identity: a token issued for one
/// endpoint or proxy prefix must never be replayed against another.
fn token_cache_key(config: &WecomAppConfig, secret: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    config.corp_id.hash(&mut hasher);
    config.agent_id.hash(&mut hasher);
    config.api_base.scheme().hash(&mut hasher);
    config.api_base.host_str().hash(&mut hasher);
    config.api_base.port_or_known_default().hash(&mut hasher);
    config.api_base.path().hash(&mut hasher);
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

fn log_partial_recipients(count: usize) {
    eprintln!("wecom-app notifier: notification delivered with {count} invalid recipient(s)");
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

#[cfg(test)]
mod tests {
    use super::{
        ERROR_DELIVERY_TIMEOUT, ERROR_TIMEOUT_AFTER_SEND, NotificationSendStatus, REQUEST_TIMEOUT,
        TOTAL_DELIVERY_BUDGET, budget_expiry_result,
    };

    // The total budget can only be reached after the per-request deadlines are exhausted, which is
    // impractical to reproduce end to end, so its classification is asserted directly here.
    #[test]
    fn budget_expiry_after_a_send_is_permanent_to_avoid_duplicates() {
        let result = budget_expiry_result(true);
        assert_eq!(result.status, NotificationSendStatus::Failed);
        assert_eq!(result.error_code.as_deref(), Some(ERROR_TIMEOUT_AFTER_SEND));
    }

    #[test]
    fn budget_expiry_before_any_send_is_retryable() {
        let result = budget_expiry_result(false);
        assert_eq!(result.status, NotificationSendStatus::Retryable);
        assert_eq!(result.error_code.as_deref(), Some(ERROR_DELIVERY_TIMEOUT));
    }

    #[test]
    fn wire_deadlines_stay_inside_the_delivery_budget() {
        assert!(TOTAL_DELIVERY_BUDGET < std::time::Duration::from_secs(30));
        // Two attempts, each a token request plus a message request, must fit the budget.
        assert!(REQUEST_TIMEOUT * 4 < TOTAL_DELIVERY_BUDGET);
    }
}
