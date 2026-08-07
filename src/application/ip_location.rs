use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use md5::{Digest, Md5};
use rand_core::{OsRng, RngCore};
use reqwest::{Client, Response};
use serde_json::{Value, json};

use crate::network::{client_builder_from_env_or, is_public_address};

const HIOFD_API_URL: &str = "https://toola.hiofd.com/router/rest";
const HIOFD_SERVICE_ID: &str = "IpQuery";
const HIOFD_KEY: &str = "key11";
const HIOFD_PWD: &str = "pwd11";
const HIOFD_REFERER: &str = "https://tool.hiofd.com/ip/";
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_FIELD_CHARS: usize = 256;
pub const SUCCESS_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const FAILURE_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_CACHE_ENTRIES: usize = 256;
const MAX_IN_FLIGHT: usize = 8;
const RANDOM_ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
const SECURITY_INSERTION: &[u8] = b"3kp";
const SECURITY_TAIL: &str = "135";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IpLocation {
    pub ip: String,
    pub country: Option<String>,
    pub province: Option<String>,
    pub city: Option<String>,
    pub district: Option<String>,
    pub street: Option<String>,
    pub isp: Option<String>,
}

impl IpLocation {
    pub fn formatted_location(&self) -> Option<String> {
        let parts = [&self.country, &self.province, &self.city]
            .into_iter()
            .filter_map(Option::as_deref)
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        (!parts.is_empty()).then(|| parts.join(" · "))
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum IpLocationError {
    ClientBuild,
    EntropyUnavailable,
    ClockUnavailable,
    InvalidIp,
    Request,
    HttpStatus,
    ResponseTooLarge,
    InvalidJson,
    LookupFailed,
    ResultIpMismatch,
}

impl std::fmt::Display for IpLocationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::ClientBuild => "ip location client unavailable",
            Self::EntropyUnavailable => "ip location request entropy unavailable",
            Self::ClockUnavailable => "ip location request clock unavailable",
            Self::InvalidIp => "ip location input is invalid",
            Self::Request => "ip location request failed",
            Self::HttpStatus => "ip location upstream returned an error",
            Self::ResponseTooLarge => "ip location response is too large",
            Self::InvalidJson => "ip location response is invalid",
            Self::LookupFailed => "ip location lookup failed",
            Self::ResultIpMismatch => "ip location result does not match the query",
        })
    }
}

impl std::error::Error for IpLocationError {}

#[derive(Clone)]
pub struct IpLocationService {
    client: Client,
    key: String,
    pwd: String,
    cache: Arc<Mutex<CacheState>>,
}

#[derive(Default)]
struct CacheState {
    entries: HashMap<IpAddr, CacheEntry>,
    in_flight: usize,
}

struct CacheEntry {
    value: Option<IpLocation>,
    expires_at: Instant,
}

impl IpLocationService {
    pub fn new_with_proxy(proxy_url: Option<&str>) -> Result<Self, IpLocationError> {
        let builder =
            client_builder_from_env_or(proxy_url).map_err(|_| IpLocationError::ClientBuild)?;
        let client = builder
            .timeout(LOOKUP_TIMEOUT)
            .user_agent("Lux IP location")
            .build()
            .map_err(|_| IpLocationError::ClientBuild)?;
        Ok(Self {
            client,
            key: HIOFD_KEY.to_owned(),
            pwd: HIOFD_PWD.to_owned(),
            cache: Arc::new(Mutex::new(CacheState::default())),
        })
    }

    pub fn cached_or_schedule(&self, raw_ip: &str) -> Option<IpLocation> {
        let ip = raw_ip.trim().parse::<IpAddr>().ok()?;
        if !is_public_address(ip) {
            return None;
        }

        let should_schedule = {
            let mut state = self.cache.lock().ok()?;
            let now = Instant::now();
            if let Some(entry) = state.entries.get(&ip) {
                if entry.expires_at > now {
                    return entry.value.clone();
                }
            }
            state.entries.remove(&ip);
            if state.in_flight >= MAX_IN_FLIGHT {
                false
            } else {
                state.in_flight += 1;
                true
            }
        };

        if should_schedule {
            let worker = self.clone();
            tokio::spawn(async move {
                let result = worker.lookup(ip).await;
                worker.store_result(ip, result);
            });
        }
        None
    }

    async fn lookup(&self, ip: IpAddr) -> Result<IpLocation, IpLocationError> {
        let (key, timestamp, signature, request_nonce) = build_security_fields()?;
        let timestamp_millis = current_timestamp_millis()?;
        let payload = json!({
            "body": { "input": { "ip": ip.to_string() } },
            "serviceId": HIOFD_SERVICE_ID,
            "key": self.key,
            "pwd": self.pwd,
            "k": key,
            "t": timestamp,
            "x": signature,
            "r": request_nonce,
        });
        let url = format!("{HIOFD_API_URL}?method={HIOFD_SERVICE_ID}&r={timestamp_millis}");
        let response = self
            .client
            .post(url)
            .header("content-type", "application/json; charset=UTF-8")
            .header("referer", HIOFD_REFERER)
            .json(&payload)
            .send()
            .await
            .map_err(|_| IpLocationError::Request)?;
        if !response.status().is_success() {
            return Err(IpLocationError::HttpStatus);
        }
        let body = read_limited_body(response).await?;
        let query_ip = ip.to_string();
        parse_hiofd_response_bytes(&body, &query_ip)
    }

    fn store_result(&self, ip: IpAddr, result: Result<IpLocation, IpLocationError>) {
        let value = result.ok();
        let expires_at = Instant::now()
            + if value.is_some() {
                SUCCESS_CACHE_TTL
            } else {
                FAILURE_CACHE_TTL
            };
        let Ok(mut state) = self.cache.lock() else {
            return;
        };
        state.in_flight = state.in_flight.saturating_sub(1);
        if state.entries.len() >= MAX_CACHE_ENTRIES && !state.entries.contains_key(&ip) {
            if let Some(oldest) = state
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.expires_at)
                .map(|(oldest_ip, _)| *oldest_ip)
            {
                state.entries.remove(&oldest);
            }
        }
        state.entries.insert(ip, CacheEntry { value, expires_at });
    }

    #[cfg(test)]
    fn new_for_test() -> Self {
        Self {
            client: Client::new(),
            key: HIOFD_KEY.to_owned(),
            pwd: HIOFD_PWD.to_owned(),
            cache: Arc::new(Mutex::new(CacheState::default())),
        }
    }

    #[cfg(test)]
    fn store_result_for_test(&self, raw_ip: &str, result: Result<IpLocation, IpLocationError>) {
        if let Ok(ip) = raw_ip.parse::<IpAddr>() {
            self.store_result(ip, result);
        }
    }
}

pub fn is_public_lookup_address(raw_ip: &str) -> bool {
    raw_ip.trim().parse::<IpAddr>().is_ok_and(is_public_address)
}

fn build_security_fields() -> Result<(String, String, String, String), IpLocationError> {
    let mut d = random_string(7)?.into_bytes();
    for character in SECURITY_INSERTION {
        let index = random_index(d.len() + 1)?;
        d.insert(index, *character);
    }
    let d = String::from_utf8(d).map_err(|_| IpLocationError::EntropyUnavailable)?;
    let positions = SECURITY_INSERTION
        .iter()
        .filter_map(|character| d.find(char::from(*character)))
        .map(|index| index.to_string())
        .collect::<String>();
    let random_tail = random_string(22)?;
    let key = format!("{d}{random_tail}");
    let timestamp = current_timestamp_millis()?;
    let timestamp_field = format!(
        "{}{}{}{}",
        random_index(10)?,
        timestamp,
        positions,
        SECURITY_TAIL
    );
    let request_nonce = random_string(32)?;
    let digest_input =
        format!("{timestamp_field}{HIOFD_SERVICE_ID}{timestamp_field}{request_nonce}{key}");
    let digest = Md5::digest(digest_input.as_bytes());
    let digest_hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let signature = format!("{digest_hex}{}", random_string(8)?);
    Ok((key, timestamp_field, signature, request_nonce))
}

fn random_string(length: usize) -> Result<String, IpLocationError> {
    let mut bytes = vec![0u8; length];
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|_| IpLocationError::EntropyUnavailable)?;
    Ok(bytes
        .into_iter()
        .map(|byte| RANDOM_ALPHABET[usize::from(byte) % RANDOM_ALPHABET.len()] as char)
        .collect())
}

fn random_index(max: usize) -> Result<usize, IpLocationError> {
    if max == 0 {
        return Err(IpLocationError::EntropyUnavailable);
    }
    let mut bytes = [0u8; 8];
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|_| IpLocationError::EntropyUnavailable)?;
    Ok((u64::from_le_bytes(bytes) as usize) % max)
}

fn current_timestamp_millis() -> Result<u128, IpLocationError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .map_err(|_| IpLocationError::ClockUnavailable)
}

async fn read_limited_body(mut response: Response) -> Result<Vec<u8>, IpLocationError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(IpLocationError::ResponseTooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| IpLocationError::Request)?
    {
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(IpLocationError::ResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
fn parse_hiofd_response(body: &str, query_ip: &str) -> Result<IpLocation, IpLocationError> {
    parse_hiofd_response_bytes(body.as_bytes(), query_ip)
}

fn parse_hiofd_response_bytes(body: &[u8], query_ip: &str) -> Result<IpLocation, IpLocationError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| IpLocationError::InvalidJson)?;
    let result_code = value
        .get("resultCode")
        .and_then(value_as_i64)
        .ok_or(IpLocationError::InvalidJson)?;
    if result_code != 0 {
        return Err(IpLocationError::LookupFailed);
    }
    let query_ip = query_ip
        .trim()
        .parse::<IpAddr>()
        .map_err(|_| IpLocationError::InvalidIp)?;
    let result_ip = text_field(&value, "ip").unwrap_or_else(|| query_ip.to_string());
    if result_ip.parse::<IpAddr>().ok() != Some(query_ip) {
        return Err(IpLocationError::ResultIpMismatch);
    }
    Ok(IpLocation {
        ip: result_ip,
        country: text_field(&value, "country"),
        province: text_field(&value, "province"),
        city: text_field(&value, "city"),
        district: text_field(&value, "district"),
        street: text_field(&value, "street"),
        isp: text_field(&value, "isp"),
    })
}

fn text_field(value: &Value, key: &str) -> Option<String> {
    let text = match value.get(key)? {
        Value::String(value) => value.trim().to_owned(),
        Value::Number(value) => value.to_string(),
        _ => return None,
    };
    (text.chars().count() <= MAX_FIELD_CHARS && !text.is_empty()).then_some(text)
}

fn value_as_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number.as_i64(),
        Value::String(value) => value.trim().parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::{
        HIOFD_KEY, HIOFD_PWD, IpLocationService, SUCCESS_CACHE_TTL, build_security_fields,
        is_public_lookup_address, parse_hiofd_response,
    };

    #[test]
    fn parses_hiofd_location_fields_and_formats_location() {
        let response = json!({
            "resultCode": 0,
            "ip": "8.8.8.8",
            "country": "美国",
            "province": "加利福尼亚州",
            "city": "山景城",
            "district": "Santa Clara",
            "street": "Amphitheatre Parkway",
            "isp": "Google"
        });

        let result = parse_hiofd_response(&response.to_string(), "8.8.8.8")
            .expect("valid Hiofd response should parse");

        assert_eq!(result.ip, "8.8.8.8");
        assert_eq!(
            result.formatted_location(),
            Some("美国 · 加利福尼亚州 · 山景城".to_owned())
        );
        assert_eq!(result.district.as_deref(), Some("Santa Clara"));
        assert_eq!(result.street.as_deref(), Some("Amphitheatre Parkway"));
        assert_eq!(result.isp.as_deref(), Some("Google"));
    }

    #[test]
    fn rejects_invalid_hiofd_result_ip() {
        let response = json!({
            "resultCode": 0,
            "ip": "1.1.1.1",
            "country": "澳大利亚"
        });

        assert!(parse_hiofd_response(&response.to_string(), "8.8.8.8").is_err());
    }

    #[test]
    fn builds_security_fields_with_hiofd_protocol_shapes() {
        let (key, timestamp, signature, request_nonce) =
            build_security_fields().expect("request fields should be generated");

        assert_eq!(key.len(), 32);
        assert!(
            timestamp
                .chars()
                .all(|character| character.is_ascii_digit())
        );
        assert_eq!(signature.len(), 40);
        assert!(
            signature[..32]
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        );
        assert_eq!(request_nonce.len(), 32);
    }

    #[test]
    fn only_public_addresses_are_eligible_for_lookup() {
        assert!(is_public_lookup_address("8.8.8.8"));
        assert!(is_public_lookup_address("2001:4860:4860::8888"));
        assert!(!is_public_lookup_address("127.0.0.1"));
        assert!(!is_public_lookup_address("192.168.1.20"));
        assert!(!is_public_lookup_address("fc00::1"));
        assert!(!is_public_lookup_address("not-an-ip"));
    }

    #[test]
    fn cached_success_is_returned_without_a_second_lookup_for_24_hours() {
        let service = IpLocationService::new_for_test();
        let result = super::IpLocation {
            ip: "8.8.8.8".to_owned(),
            country: Some("美国".to_owned()),
            province: Some("加利福尼亚州".to_owned()),
            city: Some("山景城".to_owned()),
            district: None,
            street: None,
            isp: Some("Google".to_owned()),
        };

        service.store_result_for_test("8.8.8.8", Ok(result.clone()));

        assert_eq!(service.cached_or_schedule("8.8.8.8"), Some(result));
        assert_eq!(SUCCESS_CACHE_TTL, Duration::from_secs(24 * 60 * 60));
    }

    #[test]
    fn uses_the_builtin_hiofd_protocol_fields() {
        assert_eq!(HIOFD_KEY, "key11");
        assert_eq!(HIOFD_PWD, "pwd11");
        assert_eq!(IpLocationService::new_for_test().key, HIOFD_KEY);
    }
}
