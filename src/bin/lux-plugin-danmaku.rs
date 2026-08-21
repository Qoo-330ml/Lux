use std::path::PathBuf;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use luxd::application::{
    danmaku::{DanmakuProviderClient, DanmakuProviderError},
    plugin_protocol::{
        DANMAKU_MATCH_METHOD, DanmakuMatchRpcRequest, DanmakuMatchRpcResult, DanmakuMatchStatus,
        PluginRequest, PluginResponse, PluginRpcError,
    },
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};

const PLUGIN_ID: &str = "org.lux.danmaku";
const PLUGIN_NAME: &str = "弹幕匹配";
const MAX_RPC_XML_BYTES: usize = 3 * 1024 * 1024;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let mut lines = BufReader::new(stdin).lines();
    let mut output = BufWriter::new(stdout);

    while let Some(line) = lines.next_line().await? {
        let request: PluginRequest = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(error) => {
                let response = PluginResponse {
                    id: String::new(),
                    result: None,
                    error: Some(PluginRpcError {
                        code: "PLUGIN_INVALID_REQUEST".to_owned(),
                        message: format!("invalid request: {error}"),
                    }),
                };
                write_response(&mut output, response).await?;
                continue;
            }
        };
        let should_shutdown = request.method == "plugin.shutdown";
        let response = handle_request(&request).await;
        write_response(&mut output, response).await?;
        if should_shutdown {
            break;
        }
    }
    Ok(())
}

async fn write_response(
    output: &mut BufWriter<tokio::io::Stdout>,
    response: PluginResponse,
) -> Result<(), std::io::Error> {
    let line = serde_json::to_vec(&response)
        .map_err(std::io::Error::other)
        .map(|mut bytes| {
            bytes.push(b'\n');
            bytes
        })?;
    output.write_all(&line).await?;
    output.flush().await
}

async fn handle_request(request: &PluginRequest) -> PluginResponse {
    match request.method.as_str() {
        "plugin.hello" => result(
            request,
            json!({
                "id": PLUGIN_ID,
                "name": PLUGIN_NAME,
                "apiVersion": 1,
                "capabilities": ["danmaku.match"],
                "supportedItemTypes": []
            }),
        ),
        "plugin.health" => {
            let configured = provider_url().is_some();
            result(
                request,
                json!({"available": configured, "configured": configured}),
            )
        }
        DANMAKU_MATCH_METHOD => {
            match serde_json::from_value::<DanmakuMatchRpcRequest>(request.params.clone()) {
                Ok(params) => match match_danmaku(params).await {
                    Ok(value) => result(request, value),
                    Err(error) => {
                        error_response(request, provider_error_code(&error), error_message(&error))
                    }
                },
                Err(_) => error_response(
                    request,
                    "PLUGIN_INVALID_REQUEST",
                    "danmaku match request is invalid",
                ),
            }
        }
        "plugin.shutdown" => result(request, json!({"accepted": true})),
        _ => error_response(
            request,
            "PLUGIN_UNSUPPORTED_METHOD",
            "unsupported plugin method",
        ),
    }
}

async fn match_danmaku(request: DanmakuMatchRpcRequest) -> Result<Value, DanmakuProviderError> {
    let provider_url = provider_url().ok_or(DanmakuProviderError::InvalidRequest)?;
    let proxy_url = std::env::var("LUX_PROXY_URL").ok();
    let provider = DanmakuProviderClient::new(&provider_url, proxy_url.as_deref())?;
    let Some(matched) = provider.match_filename(&request.file_name).await? else {
        let result = DanmakuMatchRpcResult {
            status: DanmakuMatchStatus::NoMatch,
            provider: None,
            anime_id: None,
            episode_id: None,
            xml_base64: None,
        };
        return serde_json::to_value(result).map_err(|_| DanmakuProviderError::InvalidResponse);
    };
    let xml = provider.fetch_episode_xml(&matched.episode_id).await?;
    if xml.len() > MAX_RPC_XML_BYTES {
        return Err(DanmakuProviderError::ResponseTooLarge);
    }
    serde_json::to_value(DanmakuMatchRpcResult {
        status: DanmakuMatchStatus::Matched,
        provider: Some("dandanplay".to_owned()),
        anime_id: matched.anime_id,
        episode_id: Some(matched.episode_id),
        xml_base64: Some(BASE64.encode(xml)),
    })
    .map_err(|_| DanmakuProviderError::InvalidResponse)
}

fn provider_url() -> Option<String> {
    let config_dir = std::env::var_os("LUX_CONFIG_DIR").map(PathBuf::from)?;
    let path = config_dir
        .join("plugin-config")
        .join(format!("{PLUGIN_ID}.json"));
    let contents = std::fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&contents).ok()?;
    value
        .get("providerBaseUrl")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn result(request: &PluginRequest, value: Value) -> PluginResponse {
    PluginResponse {
        id: request.id.clone(),
        result: Some(value),
        error: None,
    }
}

fn error_response(request: &PluginRequest, code: &str, message: &str) -> PluginResponse {
    PluginResponse {
        id: request.id.clone(),
        result: None,
        error: Some(PluginRpcError {
            code: code.to_owned(),
            message: message.to_owned(),
        }),
    }
}

fn provider_error_code(error: &DanmakuProviderError) -> &'static str {
    match error {
        DanmakuProviderError::InvalidProviderUrl(_) => "INVALID_PROVIDER_URL",
        DanmakuProviderError::InvalidProxy => "INVALID_PROXY",
        DanmakuProviderError::InvalidRequest => "INVALID_REQUEST",
        DanmakuProviderError::Client => "CLIENT_UNAVAILABLE",
        DanmakuProviderError::Unavailable => "PROVIDER_UNAVAILABLE",
        DanmakuProviderError::Unsupported => "PROVIDER_UNSUPPORTED",
        DanmakuProviderError::HttpStatus(_) => "PROVIDER_HTTP_ERROR",
        DanmakuProviderError::ResponseTooLarge => "PROVIDER_RESPONSE_TOO_LARGE",
        DanmakuProviderError::InvalidResponse => "PROVIDER_INVALID_RESPONSE",
        DanmakuProviderError::InvalidXml(_) => "PROVIDER_INVALID_XML",
    }
}

fn error_message(error: &DanmakuProviderError) -> &'static str {
    match error {
        DanmakuProviderError::InvalidProviderUrl(_) => "danmaku provider URL is invalid",
        DanmakuProviderError::InvalidProxy => "danmaku provider proxy is invalid",
        DanmakuProviderError::InvalidRequest => "danmaku provider request is invalid",
        DanmakuProviderError::Client => "danmaku provider client is unavailable",
        DanmakuProviderError::Unavailable => "danmaku provider is unavailable",
        DanmakuProviderError::Unsupported => "danmaku provider endpoint is unsupported",
        DanmakuProviderError::HttpStatus(_) => "danmaku provider returned an HTTP error",
        DanmakuProviderError::ResponseTooLarge => "danmaku provider response is too large",
        DanmakuProviderError::InvalidResponse => "danmaku provider response is invalid",
        DanmakuProviderError::InvalidXml(_) => "danmaku provider XML is invalid",
    }
}
