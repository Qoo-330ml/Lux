mod ip_location_plugin;

use std::time::{SystemTime, UNIX_EPOCH};

use ip_location_plugin::{
    invalid_ip, invalid_response, is_public_ip, read_limited_body, text_field, upstream_error,
    value_as_i64,
};
use luxd::application::plugin_protocol::{
    IP_LOCATION_CAPABILITY, IpLocationRpcRequest, IpLocationRpcResult, PluginRpcError,
};
use md5::{Digest, Md5};
use rand_core::{OsRng, RngCore};
use serde_json::{Value, json};

const PLUGIN_ID: &str = "org.lux.ip-hiofd";
const PLUGIN_NAME: &str = "IP归属地查询增强";
const API_URL: &str = "https://toola.hiofd.com/router/rest";
const SERVICE_ID: &str = "IpQuery";
const KEY: &str = "key11";
const PWD: &str = "pwd11";
const REFERER: &str = "https://tool.hiofd.com/ip/";
const MAX_RESPONSE_BYTES: usize = 64 * 1024;
const LOOKUP_TIMEOUT_SECONDS: u64 = 10;
const RANDOM_ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
const SECURITY_INSERTION: &[u8] = b"3kp";
const SECURITY_TAIL: &str = "135";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    ip_location_plugin::run(|method, params| async move {
        match method.as_str() {
            "plugin.hello" => Ok(json!({
                "id": PLUGIN_ID,
                "name": PLUGIN_NAME,
                "apiVersion": 1,
                "capabilities": [IP_LOCATION_CAPABILITY],
                "supportedItemTypes": []
            })),
            "plugin.health" => Ok(json!({
                "available": true,
                "configured": true
            })),
            "ip.location" => lookup(params).await,
            "plugin.shutdown" => Ok(json!({"accepted": true})),
            _ => Err(PluginRpcError {
                code: "PLUGIN_INVALID_REQUEST".to_owned(),
                message: "unsupported plugin method".to_owned(),
            }),
        }
    })
    .await
}

async fn lookup(params: Value) -> Result<Value, PluginRpcError> {
    let request: IpLocationRpcRequest = serde_json::from_value(params).map_err(|_| invalid_ip())?;
    let ip = is_public_ip(&request.ip).ok_or_else(invalid_ip)?;
    let result = lookup_hiofd(ip).await?;
    serde_json::to_value(result).map_err(|_| invalid_response())
}

async fn lookup_hiofd(ip: std::net::IpAddr) -> Result<IpLocationRpcResult, PluginRpcError> {
    let (key, timestamp, signature, request_nonce) = build_security_fields()?;
    let timestamp_millis = current_timestamp_millis().ok_or_else(upstream_error)?;
    let payload = json!({
        "body": { "input": { "ip": ip.to_string() } },
        "serviceId": SERVICE_ID,
        "key": KEY,
        "pwd": PWD,
        "k": key,
        "t": timestamp,
        "x": signature,
        "r": request_nonce,
    });
    let client = luxd::network::client_builder_from_env()
        .map_err(|_| upstream_error())?
        .timeout(std::time::Duration::from_secs(LOOKUP_TIMEOUT_SECONDS))
        .user_agent("Lux IP location plugin")
        .build()
        .map_err(|_| upstream_error())?;
    let url = format!("{API_URL}?method={SERVICE_ID}&r={timestamp_millis}");
    let response = client
        .post(url)
        .header("content-type", "application/json; charset=UTF-8")
        .header("referer", REFERER)
        .json(&payload)
        .send()
        .await
        .map_err(|_| upstream_error())?;
    if !response.status().is_success() {
        return Err(upstream_error());
    }
    let body = read_limited_body(response, MAX_RESPONSE_BYTES).await?;
    parse_hiofd_response(&body, ip)
}

fn parse_hiofd_response(
    body: &[u8],
    query_ip: std::net::IpAddr,
) -> Result<IpLocationRpcResult, PluginRpcError> {
    let value: Value = serde_json::from_slice(body).map_err(|_| invalid_response())?;
    let result_code = value_as_i64(value.get("resultCode").ok_or_else(invalid_response)?)
        .ok_or_else(invalid_response)?;
    if result_code != 0 {
        return Err(upstream_error());
    }
    let result_ip = text_field(&value, "ip").unwrap_or_else(|| query_ip.to_string());
    if result_ip.trim().parse().ok() != Some(query_ip) {
        return Err(invalid_response());
    }
    Ok(IpLocationRpcResult {
        ip: query_ip.to_string(),
        country: text_field(&value, "country"),
        province: text_field(&value, "province"),
        city: text_field(&value, "city"),
        district: text_field(&value, "district"),
        street: text_field(&value, "street"),
        isp: text_field(&value, "isp"),
        latitude: text_field(&value, "latitude"),
        longitude: text_field(&value, "longitude"),
    })
}

fn build_security_fields() -> Result<(String, String, String, String), PluginRpcError> {
    let mut d = random_string(7)?.into_bytes();
    for character in SECURITY_INSERTION {
        let index = random_index(d.len() + 1)?;
        d.insert(index, *character);
    }
    let d = String::from_utf8(d).map_err(|_| upstream_error())?;
    let positions = SECURITY_INSERTION
        .iter()
        .filter_map(|character| d.find(char::from(*character)))
        .map(|index| index.to_string())
        .collect::<String>();
    let random_tail = random_string(22)?;
    let key = format!("{d}{random_tail}");
    let timestamp = current_timestamp_millis().ok_or_else(upstream_error)?;
    let timestamp_field = format!(
        "{}{}{}{}",
        random_index(10)?,
        timestamp,
        positions,
        SECURITY_TAIL
    );
    let request_nonce = random_string(32)?;
    let digest_input =
        format!("{timestamp_field}{SERVICE_ID}{timestamp_field}{request_nonce}{key}");
    let digest = Md5::digest(digest_input.as_bytes());
    let digest_hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let signature = format!("{digest_hex}{}", random_string(8)?);
    Ok((key, timestamp_field, signature, request_nonce))
}

fn random_string(length: usize) -> Result<String, PluginRpcError> {
    let mut bytes = vec![0_u8; length];
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|_| upstream_error())?;
    Ok(bytes
        .into_iter()
        .map(|byte| RANDOM_ALPHABET[usize::from(byte) % RANDOM_ALPHABET.len()] as char)
        .collect())
}

fn random_index(max: usize) -> Result<usize, PluginRpcError> {
    if max == 0 {
        return Err(upstream_error());
    }
    let mut bytes = [0_u8; 8];
    OsRng
        .try_fill_bytes(&mut bytes)
        .map_err(|_| upstream_error())?;
    Ok((u64::from_le_bytes(bytes) as usize) % max)
}

fn current_timestamp_millis() -> Option<u128> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis())
}
