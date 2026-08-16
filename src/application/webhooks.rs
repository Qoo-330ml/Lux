use std::{
    collections::BTreeMap,
    fmt, io,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand_core::{OsRng, RngCore};
use reqwest::{Client, StatusCode, redirect::Policy};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{fs, net::lookup_host, sync::Mutex, time::sleep};
use url::Url;

use crate::storage::{
    Database, NewNotificationDestination, NewNotificationEvent, StorageError,
    StoredNotificationDelivery, StoredNotificationDestination, UpdateNotificationDestination,
};

pub const WEBHOOK_SECRET_FILE: &str = "lux_webhook_secrets.json";
const MAX_NAME_LENGTH: usize = 128;
const MAX_URL_LENGTH: usize = 2048;
const MAX_EVENT_PAYLOAD_BYTES: usize = 256 * 1024;
const MAX_DELIVERY_ATTEMPTS: i64 = 8;
const DELIVERY_LEASE_SECONDS: i64 = 60;
const DELIVERY_POLL_INTERVAL: Duration = Duration::from_secs(1);
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookDestinationView {
    pub id: String,
    pub name: String,
    pub url: String,
    pub enabled: bool,
    pub allow_private_network: bool,
    pub event_types: Vec<String>,
    pub secret_configured: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookDeliveryView {
    pub id: String,
    pub event_id: String,
    pub destination_id: String,
    pub destination_name: String,
    pub event_type: String,
    pub status: String,
    pub attempt_count: i64,
    pub next_attempt_at: i64,
    pub last_http_status: Option<i64>,
    pub last_error: Option<String>,
    pub delivered_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug)]
pub enum WebhookError {
    Storage(StorageError),
    Io(io::Error),
    Serialization(String),
    Invalid(String),
    NotFound,
    SecretUnavailable,
    RequestSetup(String),
}

impl fmt::Display for WebhookError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(error) => write!(formatter, "webhook storage error: {error}"),
            Self::Io(error) => write!(formatter, "webhook secret storage error: {error}"),
            Self::Serialization(error) => write!(formatter, "webhook serialization error: {error}"),
            Self::Invalid(error) => write!(formatter, "invalid webhook configuration: {error}"),
            Self::NotFound => formatter.write_str("webhook destination not found"),
            Self::SecretUnavailable => formatter.write_str("webhook secret is unavailable"),
            Self::RequestSetup(error) => write!(formatter, "webhook request setup failed: {error}"),
        }
    }
}

impl std::error::Error for WebhookError {}

impl From<StorageError> for WebhookError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<io::Error> for WebhookError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Clone)]
pub struct WebhookService {
    database: Database,
    config_dir: PathBuf,
    secret_lock: Arc<Mutex<()>>,
    server_id: String,
}

impl WebhookService {
    pub fn new(database: Database, config_dir: PathBuf) -> Result<Self, WebhookError> {
        let server_id = database.server_id().to_owned();
        Ok(Self {
            database,
            config_dir,
            secret_lock: Arc::new(Mutex::new(())),
            server_id,
        })
    }

    pub async fn create_destination(
        &self,
        name: &str,
        url: &str,
        enabled: bool,
        allow_private_network: bool,
        event_types: &[String],
        secret: Option<&str>,
    ) -> Result<(WebhookDestinationView, String), WebhookError> {
        let name = validate_name(name)?;
        let url = validate_destination(url, allow_private_network)?;
        let event_types = normalize_event_types(event_types)?;
        let secret = normalize_or_generate_secret(secret)?;
        let id = uuid::Uuid::now_v7().to_string();
        self.set_secret(&id, Some(&secret)).await?;
        let event_types_json = serde_json::to_string(&event_types)
            .map_err(|error| WebhookError::Serialization(error.to_string()))?;
        if let Err(error) = self
            .database
            .create_notification_destination(NewNotificationDestination {
                id: &id,
                name,
                url: url.as_str(),
                enabled,
                allow_private_network,
                event_types_json: &event_types_json,
            })
            .await
        {
            let _ = self.set_secret(&id, None).await;
            return Err(error.into());
        }
        let destination = self.view_by_id(&id).await?.ok_or(WebhookError::NotFound)?;
        Ok((destination, secret))
    }

    pub async fn list_destinations(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<WebhookDestinationView>, WebhookError> {
        let destinations = self
            .database
            .list_notification_destinations(offset, limit)
            .await?;
        let secrets = self.read_secrets().await?;
        destinations
            .into_iter()
            .map(|destination| {
                let secret_configured = secrets.contains_key(&destination.id);
                destination_view(destination, secret_configured)
            })
            .collect()
    }

    pub async fn get_destination(
        &self,
        id: &str,
    ) -> Result<Option<WebhookDestinationView>, WebhookError> {
        self.view_by_id(id).await
    }

    pub async fn list_deliveries(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<WebhookDeliveryView>, WebhookError> {
        Ok(self
            .database
            .list_notification_deliveries(offset, limit)
            .await?
            .into_iter()
            .map(delivery_view)
            .collect())
    }

    pub async fn retry_delivery(&self, id: &str) -> Result<(), WebhookError> {
        if !self.database.retry_notification_delivery(id).await? {
            return Err(WebhookError::NotFound);
        }
        Ok(())
    }

    pub async fn update_destination(
        &self,
        id: &str,
        name: Option<&str>,
        url: Option<&str>,
        enabled: Option<bool>,
        allow_private_network: Option<bool>,
        event_types: Option<&[String]>,
    ) -> Result<WebhookDestinationView, WebhookError> {
        let current = self
            .database
            .find_notification_destination(id)
            .await?
            .ok_or(WebhookError::NotFound)?;
        let next_allow_private_network =
            allow_private_network.unwrap_or(current.allow_private_network);
        let validated_name = name.map(validate_name).transpose()?;
        let validated_url = url
            .map(|value| validate_destination(value, next_allow_private_network))
            .transpose()?;
        let normalized_event_types = event_types.map(normalize_event_types).transpose()?;
        let event_types_json = normalized_event_types
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| WebhookError::Serialization(error.to_string()))?;
        self.database
            .update_notification_destination(
                id,
                UpdateNotificationDestination {
                    name: validated_name,
                    url: validated_url.as_ref().map(Url::as_str),
                    enabled,
                    allow_private_network,
                    event_types_json: event_types_json.as_deref(),
                },
            )
            .await?;
        self.view_by_id(id).await?.ok_or(WebhookError::NotFound)
    }

    pub async fn delete_destination(&self, id: &str) -> Result<(), WebhookError> {
        if !self.database.delete_notification_destination(id).await? {
            return Err(WebhookError::NotFound);
        }
        self.set_secret(id, None).await
    }

    pub async fn rotate_secret(&self, id: &str) -> Result<String, WebhookError> {
        if self
            .database
            .find_notification_destination(id)
            .await?
            .is_none()
        {
            return Err(WebhookError::NotFound);
        }
        let secret = generate_secret()?;
        self.set_secret(id, Some(&secret)).await?;
        Ok(secret)
    }

    pub async fn publish(
        &self,
        event_type: WebhookEventType,
        dedupe_key: &str,
        occurred_at: i64,
        data: Value,
    ) -> Result<Option<String>, WebhookError> {
        let destinations = self
            .database
            .list_enabled_notification_destinations()
            .await?;
        let destination_ids = destinations
            .iter()
            .filter_map(|destination| {
                event_types_from_json(&destination.event_types_json)
                    .ok()
                    .filter(|event_types| {
                        event_types.is_empty()
                            || event_types.iter().any(|value| value == event_type.as_str())
                    })
                    .map(|_| destination.id.clone())
            })
            .collect::<Vec<_>>();
        let event_id = uuid::Uuid::now_v7().to_string();
        let payload =
            build_event_payload(&self.server_id, &event_id, event_type, occurred_at, data)?;
        let payload_json = serde_json::to_vec(&payload)
            .map_err(|error| WebhookError::Serialization(error.to_string()))?;
        if payload_json.len() > MAX_EVENT_PAYLOAD_BYTES {
            return Err(WebhookError::Invalid(
                "event payload is too large".to_owned(),
            ));
        }
        let payload_json = String::from_utf8(payload_json)
            .map_err(|error| WebhookError::Serialization(error.to_string()))?;
        let inserted = self
            .database
            .insert_notification_event_with_deliveries(
                NewNotificationEvent {
                    id: &event_id,
                    event_type: event_type.as_str(),
                    schema_version: 1,
                    occurred_at,
                    dedupe_key,
                    payload_json: &payload_json,
                },
                &destination_ids,
            )
            .await?;
        Ok(inserted.then_some(event_id))
    }

    pub async fn test_destination(&self, id: &str) -> Result<u16, WebhookError> {
        let destination = self
            .database
            .find_notification_destination(id)
            .await?
            .ok_or(WebhookError::NotFound)?;
        let secret = self
            .read_secrets()
            .await?
            .remove(id)
            .ok_or(WebhookError::SecretUnavailable)?;
        let event_id = uuid::Uuid::now_v7().to_string();
        let payload = build_event_payload(
            &self.server_id,
            &event_id,
            WebhookEventType::JobFailed,
            unix_now(),
            json!({"test": true}),
        )?;
        let payload = serde_json::to_vec(&payload)
            .map_err(|error| WebhookError::Serialization(error.to_string()))?;
        self.send_http(
            &destination.url,
            destination.allow_private_network,
            &secret,
            &event_id,
            WebhookEventType::JobFailed.as_str(),
            &payload,
        )
        .await
    }

    pub fn spawn_worker(&self) {
        let service = self.clone();
        tokio::spawn(async move {
            loop {
                match service.process_ready_deliveries().await {
                    Ok(0) => sleep(DELIVERY_POLL_INTERVAL).await,
                    Ok(_) => {}
                    Err(error) => {
                        tracing::error!(%error, "webhook delivery worker iteration failed");
                        sleep(DELIVERY_POLL_INTERVAL).await;
                    }
                }
            }
        });
    }

    pub async fn process_ready_deliveries(&self) -> Result<usize, WebhookError> {
        let deliveries = self.database.list_ready_notification_deliveries(10).await?;
        let mut processed = 0;
        for delivery in deliveries {
            if !self
                .database
                .claim_notification_delivery(&delivery.id, DELIVERY_LEASE_SECONDS)
                .await?
            {
                continue;
            }
            processed += 1;
            self.process_delivery(delivery).await?;
        }
        Ok(processed)
    }

    async fn process_delivery(
        &self,
        delivery: StoredNotificationDelivery,
    ) -> Result<(), WebhookError> {
        let secret = self
            .read_secrets()
            .await?
            .remove(&delivery.destination_id)
            .ok_or(WebhookError::SecretUnavailable);
        let result = match secret {
            Ok(secret) => {
                self.send_http(
                    &delivery.destination_url,
                    delivery.allow_private_network,
                    &secret,
                    &delivery.event_id,
                    &delivery.event_type,
                    delivery.payload_json.as_bytes(),
                )
                .await
            }
            Err(error) => Err(error),
        };
        match result {
            Ok(status) => {
                self.database
                    .mark_notification_delivered(&delivery.id, i64::from(status))
                    .await?
            }
            Err(error) => {
                let retryable = matches!(error, WebhookError::RequestSetup(_))
                    || matches!(error, WebhookError::Invalid(ref message) if message == "retryable HTTP response");
                let status = if delivery.attempt_count >= MAX_DELIVERY_ATTEMPTS || !retryable {
                    "FAILED"
                } else {
                    "PENDING"
                };
                let delay = retry_delay(delivery.attempt_count);
                self.database
                    .mark_notification_retry(
                        &delivery.id,
                        status,
                        None,
                        &public_error_message(&error),
                        unix_now().saturating_add(delay),
                    )
                    .await?;
            }
        }
        Ok(())
    }

    async fn send_http(
        &self,
        raw_url: &str,
        allow_private_network: bool,
        secret: &str,
        event_id: &str,
        event_type: &str,
        body: &[u8],
    ) -> Result<u16, WebhookError> {
        let url = validate_destination(raw_url, allow_private_network)?;
        let (host, address) = resolve_webhook_address(&url, allow_private_network).await?;
        let client = Client::builder()
            .timeout(DELIVERY_TIMEOUT)
            .redirect(Policy::none())
            .resolve(&host, address)
            .build()
            .map_err(|error| WebhookError::RequestSetup(error.to_string()))?;
        let timestamp = unix_now().to_string();
        let response = client
            .post(url)
            .header("Content-Type", "application/json")
            .header("X-Lux-Event-Id", event_id)
            .header("X-Lux-Event-Type", event_type)
            .header("X-Lux-Timestamp", &timestamp)
            .header(
                "X-Lux-Signature",
                canonical_signature(secret, &timestamp, body),
            )
            .body(body.to_vec())
            .send()
            .await
            .map_err(|_| WebhookError::RequestSetup("network request failed".to_owned()))?;
        let status = response.status();
        if status.is_success() {
            return Ok(status.as_u16());
        }
        if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
            return Err(WebhookError::Invalid("retryable HTTP response".to_owned()));
        }
        Err(WebhookError::Invalid(format!(
            "HTTP {} response",
            status.as_u16()
        )))
    }

    async fn view_by_id(&self, id: &str) -> Result<Option<WebhookDestinationView>, WebhookError> {
        let Some(destination) = self.database.find_notification_destination(id).await? else {
            return Ok(None);
        };
        let secrets = self.read_secrets().await?;
        Ok(Some(destination_view(
            destination,
            secrets.contains_key(id),
        )?))
    }

    async fn read_secrets(&self) -> Result<BTreeMap<String, String>, WebhookError> {
        read_secret_map(&self.secret_path()).await
    }

    async fn set_secret(&self, id: &str, secret: Option<&str>) -> Result<(), WebhookError> {
        let _guard = self.secret_lock.lock().await;
        let mut secrets = read_secret_map(&self.secret_path()).await?;
        match secret {
            Some(value) => {
                secrets.insert(id.to_owned(), value.to_owned());
            }
            None => {
                secrets.remove(id);
            }
        }
        write_secret_map(&self.secret_path(), &secrets).await
    }

    fn secret_path(&self) -> PathBuf {
        self.config_dir.join(WEBHOOK_SECRET_FILE)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WebhookEventType {
    MediaAdded,
    MediaRemoved,
    ScanCompleted,
    ScanFailed,
    MetadataUpdated,
    JobFailed,
}

impl WebhookEventType {
    pub const ALL: [Self; 6] = [
        Self::MediaAdded,
        Self::MediaRemoved,
        Self::ScanCompleted,
        Self::ScanFailed,
        Self::MetadataUpdated,
        Self::JobFailed,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MediaAdded => "MEDIA_ADDED",
            Self::MediaRemoved => "MEDIA_REMOVED",
            Self::ScanCompleted => "SCAN_COMPLETED",
            Self::ScanFailed => "SCAN_FAILED",
            Self::MetadataUpdated => "METADATA_UPDATED",
            Self::JobFailed => "JOB_FAILED",
        }
    }

    pub fn from_wire_name(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|event_type| event_type.as_str() == value)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum WebhookUrlError {
    Invalid,
    Scheme,
    Credentials,
    QueryOrFragment,
    MissingHost,
    PrivateNetwork,
    DangerousAddress,
}

impl fmt::Display for WebhookUrlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Invalid => "webhook URL is invalid",
            Self::Scheme => "webhook URL must use http or https",
            Self::Credentials => "webhook URL must not contain credentials",
            Self::QueryOrFragment => "webhook URL must not contain a query or fragment",
            Self::MissingHost => "webhook URL host is missing",
            Self::PrivateNetwork => "webhook URL targets a private network",
            Self::DangerousAddress => "webhook URL targets a dangerous reserved address",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for WebhookUrlError {}

pub fn validate_webhook_url(
    value: &str,
    allow_private_network: bool,
) -> Result<Url, WebhookUrlError> {
    let url = Url::parse(value.trim()).map_err(|_| WebhookUrlError::Invalid)?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(WebhookUrlError::Scheme);
    }
    if url.username() != "" || url.password().is_some() {
        return Err(WebhookUrlError::Credentials);
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(WebhookUrlError::QueryOrFragment);
    }
    let Some(host) = url.host_str() else {
        return Err(WebhookUrlError::MissingHost);
    };
    let normalized_host = host.to_ascii_lowercase();
    if (normalized_host == "localhost"
        || normalized_host.ends_with(".localhost")
        || normalized_host.ends_with(".local")
        || normalized_host.ends_with(".internal")
        || normalized_host.ends_with(".home.arpa"))
        && !allow_private_network
    {
        return Err(WebhookUrlError::PrivateNetwork);
    }

    if let Ok(address) = host.parse::<IpAddr>() {
        if is_dangerous_address(address) {
            return Err(WebhookUrlError::DangerousAddress);
        }
        if !allow_private_network && is_private_address(address) {
            return Err(WebhookUrlError::PrivateNetwork);
        }
    }

    Ok(url)
}

pub fn canonical_signature(secret: &str, timestamp: &str, body: &[u8]) -> String {
    let mut message = Vec::with_capacity(timestamp.len() + 1 + body.len());
    message.extend_from_slice(timestamp.as_bytes());
    message.push(b'.');
    message.extend_from_slice(body);
    let digest = hmac_sha256(secret.as_bytes(), &message);
    let mut encoded = String::with_capacity(7 + digest.len() * 2);
    encoded.push_str("sha256=");
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK_SIZE: usize = 64;
    let mut normalized_key = [0_u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        normalized_key[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        normalized_key[..key.len()].copy_from_slice(key);
    }

    let mut inner_pad = [0x36_u8; BLOCK_SIZE];
    let mut outer_pad = [0x5c_u8; BLOCK_SIZE];
    for index in 0..BLOCK_SIZE {
        inner_pad[index] ^= normalized_key[index];
        outer_pad[index] ^= normalized_key[index];
    }

    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message);
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    let digest = outer.finalize();
    let mut output = [0_u8; 32];
    output.copy_from_slice(&digest);
    output
}

fn is_dangerous_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(value) => {
            value.is_unspecified()
                || value.is_link_local()
                || value.is_multicast()
                || value.octets()[0] == 0
        }
        IpAddr::V6(value) => {
            if let Some(mapped) = value.to_ipv4() {
                return is_dangerous_address(IpAddr::V4(mapped));
            }
            value.is_unspecified() || value.is_multicast() || value.is_unicast_link_local()
        }
    }
}

fn is_private_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(value) => {
            value.is_private()
                || value.is_loopback()
                || value.is_link_local()
                || value.octets()[0] == 100 && (value.octets()[1] & 0b1100_0000) == 0b0100_0000
                || value.octets()[0] == 198 && (value.octets()[1] == 18 || value.octets()[1] == 19)
                || value.octets()[0] == 192 && value.octets()[1] == 0 && value.octets()[2] == 0
        }
        IpAddr::V6(value) => {
            if let Some(mapped) = value.to_ipv4() {
                return is_private_address(IpAddr::V4(mapped));
            }
            let octets = value.octets();
            value.is_loopback() || value.is_unicast_link_local() || (octets[0] & 0xfe) == 0xfc
        }
    }
}

fn validate_name(value: &str) -> Result<&str, WebhookError> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > MAX_NAME_LENGTH {
        return Err(WebhookError::Invalid(
            "webhook name must be between 1 and 128 characters".to_owned(),
        ));
    }
    Ok(value)
}

fn validate_destination(value: &str, allow_private_network: bool) -> Result<Url, WebhookError> {
    if value.trim().len() > MAX_URL_LENGTH {
        return Err(WebhookError::Invalid("webhook URL is too long".to_owned()));
    }
    validate_webhook_url(value, allow_private_network)
        .map_err(|error| WebhookError::Invalid(error.to_string()))
}

fn normalize_event_types(values: &[String]) -> Result<Vec<String>, WebhookError> {
    let mut result = values
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(|value| {
            WebhookEventType::from_wire_name(value)
                .map(|event_type| event_type.as_str().to_owned())
                .ok_or_else(|| WebhookError::Invalid("unknown webhook event type".to_owned()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    result.sort();
    result.dedup();
    Ok(result)
}

fn event_types_from_json(value: &str) -> Result<Vec<String>, WebhookError> {
    let values = serde_json::from_str::<Vec<String>>(value)
        .map_err(|error| WebhookError::Serialization(error.to_string()))?;
    normalize_event_types(&values)
}

fn normalize_or_generate_secret(value: Option<&str>) -> Result<String, WebhookError> {
    let value = value.map(str::trim).filter(|value| !value.is_empty());
    if let Some(value) = value {
        if !(16..=256).contains(&value.len()) {
            return Err(WebhookError::Invalid(
                "webhook secret must be between 16 and 256 bytes".to_owned(),
            ));
        }
        return Ok(value.to_owned());
    }
    generate_secret()
}

fn generate_secret() -> Result<String, WebhookError> {
    let mut bytes = [0_u8; 32];
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|error| WebhookError::RequestSetup(error.to_string()))?;
    Ok(format!("lux_wh_{}", URL_SAFE_NO_PAD.encode(bytes)))
}

fn build_event_payload(
    server_id: &str,
    event_id: &str,
    event_type: WebhookEventType,
    occurred_at: i64,
    data: Value,
) -> Result<Value, WebhookError> {
    let Value::Object(mut data) = data else {
        return Err(WebhookError::Invalid(
            "webhook event data must be a JSON object".to_owned(),
        ));
    };
    data.insert("schemaVersion".to_owned(), json!(1));
    data.insert("eventId".to_owned(), json!(event_id));
    data.insert("eventType".to_owned(), json!(event_type.as_str()));
    data.insert("occurredAt".to_owned(), json!(occurred_at));
    data.insert("serverId".to_owned(), json!(server_id));
    Ok(Value::Object(data))
}

async fn resolve_webhook_address(
    url: &Url,
    allow_private_network: bool,
) -> Result<(String, SocketAddr), WebhookError> {
    let Some(host) = url.host_str() else {
        return Err(WebhookError::Invalid(
            "webhook URL host is missing".to_owned(),
        ));
    };
    let port = url
        .port_or_known_default()
        .ok_or_else(|| WebhookError::Invalid("webhook URL port is missing".to_owned()))?;
    let addresses = lookup_host((host, port))
        .await
        .map_err(|_| WebhookError::RequestSetup("webhook host could not be resolved".to_owned()))?
        .collect::<Vec<_>>();
    let Some(first) = addresses.first().copied() else {
        return Err(WebhookError::RequestSetup(
            "webhook host has no resolved address".to_owned(),
        ));
    };
    for address in &addresses {
        if is_dangerous_address(address.ip()) {
            return Err(WebhookError::Invalid(
                "webhook host resolves to a reserved address".to_owned(),
            ));
        }
        if !allow_private_network && is_private_address(address.ip()) {
            return Err(WebhookError::Invalid(
                "webhook host resolves to a private address".to_owned(),
            ));
        }
    }
    Ok((host.to_owned(), first))
}

fn destination_view(
    destination: StoredNotificationDestination,
    secret_configured: bool,
) -> Result<WebhookDestinationView, WebhookError> {
    Ok(WebhookDestinationView {
        id: destination.id,
        name: destination.name,
        url: destination.url,
        enabled: destination.enabled,
        allow_private_network: destination.allow_private_network,
        event_types: event_types_from_json(&destination.event_types_json)?,
        secret_configured,
        created_at: destination.created_at,
        updated_at: destination.updated_at,
    })
}

fn delivery_view(delivery: StoredNotificationDelivery) -> WebhookDeliveryView {
    WebhookDeliveryView {
        id: delivery.id,
        event_id: delivery.event_id,
        destination_id: delivery.destination_id,
        destination_name: delivery.destination_name,
        event_type: delivery.event_type,
        status: delivery.status,
        attempt_count: delivery.attempt_count,
        next_attempt_at: delivery.next_attempt_at,
        last_http_status: delivery.last_http_status,
        last_error: delivery.last_error,
        delivered_at: delivery.delivered_at,
        created_at: delivery.created_at,
        updated_at: delivery.updated_at,
    }
}

async fn read_secret_map(path: &Path) -> Result<BTreeMap<String, String>, WebhookError> {
    let contents = match fs::read_to_string(path).await {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(WebhookError::Io(error)),
    };
    serde_json::from_str(&contents).map_err(|error| WebhookError::Serialization(error.to_string()))
}

async fn write_secret_map(
    path: &Path,
    secrets: &BTreeMap<String, String>,
) -> Result<(), WebhookError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    let contents = serde_json::to_vec_pretty(secrets)
        .map_err(|error| WebhookError::Serialization(error.to_string()))?;
    let path = path.to_owned();
    let temporary = path.with_file_name(format!(".{WEBHOOK_SECRET_FILE}.tmp"));
    tokio::task::spawn_blocking(move || {
        use std::{fs::OpenOptions, io::Write};
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&temporary)?;
        #[cfg(unix)]
        std::fs::set_permissions(
            &temporary,
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )?;
        file.write_all(&contents)?;
        file.sync_all()?;
        std::fs::rename(temporary, path)
    })
    .await
    .map_err(|error| WebhookError::Io(io::Error::other(error.to_string())))?
    .map_err(WebhookError::Io)
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or(i64::MAX)
}

fn retry_delay(attempt_count: i64) -> i64 {
    match attempt_count {
        0 | 1 => 1,
        2 => 10,
        3 => 60,
        4 => 300,
        5 => 1_800,
        _ => 7_200,
    }
}

fn public_error_message(error: &WebhookError) -> String {
    let message = error.to_string();
    if message.len() <= 512 {
        message
    } else {
        message.chars().take(512).collect()
    }
}
