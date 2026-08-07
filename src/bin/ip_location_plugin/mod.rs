use std::future::Future;

use luxd::application::plugin_protocol::{PluginRequest, PluginResponse, PluginRpcError};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub async fn run<F, Fut>(handler: F) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
where
    F: Fn(String, Value) -> Fut,
    Fut: Future<Output = Result<Value, PluginRpcError>>,
{
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let mut lines = BufReader::new(stdin).lines();
    let mut output = stdout;

    while let Some(line) = lines.next_line().await? {
        let response = match serde_json::from_str::<PluginRequest>(&line) {
            Ok(request) => {
                let id = request.id.clone();
                match handler(request.method, request.params).await {
                    Ok(result) => PluginResponse {
                        id,
                        result: Some(result),
                        error: None,
                    },
                    Err(error) => PluginResponse {
                        id,
                        result: None,
                        error: Some(error),
                    },
                }
            }
            Err(_) => PluginResponse {
                id: "invalid-request".to_owned(),
                result: None,
                error: Some(PluginRpcError {
                    code: "PLUGIN_INVALID_REQUEST".to_owned(),
                    message: "invalid plugin request".to_owned(),
                }),
            },
        };
        let mut serialized = serde_json::to_vec(&response)?;
        serialized.push(b'\n');
        output.write_all(&serialized).await?;
        output.flush().await?;
    }
    Ok(())
}

pub fn invalid_ip() -> PluginRpcError {
    PluginRpcError {
        code: "IP_LOCATION_INVALID_REQUEST".to_owned(),
        message: "ip location request is invalid".to_owned(),
    }
}

pub fn upstream_error() -> PluginRpcError {
    PluginRpcError {
        code: "IP_LOCATION_UPSTREAM_ERROR".to_owned(),
        message: "ip location provider is unavailable".to_owned(),
    }
}

pub fn invalid_response() -> PluginRpcError {
    PluginRpcError {
        code: "IP_LOCATION_INVALID_RESPONSE".to_owned(),
        message: "ip location provider returned invalid data".to_owned(),
    }
}

pub async fn read_limited_body(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, PluginRpcError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(invalid_response());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| upstream_error())? {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(invalid_response());
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

pub fn is_public_ip(raw_ip: &str) -> Option<std::net::IpAddr> {
    let ip = raw_ip.trim().parse().ok()?;
    luxd::network::is_public_address(ip).then_some(ip)
}

#[allow(dead_code)]
pub fn text_field(value: &Value, key: &str) -> Option<String> {
    let text = match value.get(key)? {
        Value::String(value) => value.trim().to_owned(),
        Value::Number(value) => value.to_string(),
        _ => return None,
    };
    (!text.is_empty() && text.chars().count() <= 256 && !text.chars().any(char::is_control))
        .then_some(text)
}

#[allow(dead_code)]
pub fn value_as_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number.as_i64(),
        Value::String(value) => value.trim().parse().ok(),
        _ => None,
    }
}
