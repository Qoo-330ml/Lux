use std::{fmt, path::PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use luxd::application::{
    danmaku::{validate_danmaku_xml, validate_provider_base_url},
    media_matching::{MediaKind, normalize_title, parse_media_name},
    plugin_protocol::{
        DANMAKU_MATCH_METHOD, DanmakuMatchRpcRequest, DanmakuMatchRpcResult, DanmakuMatchStatus,
        PluginRequest, PluginResponse, PluginRpcError,
    },
};
use reqwest::{Client, StatusCode, Url};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};

const PLUGIN_ID: &str = "org.lux.danmaku";
const PLUGIN_NAME: &str = "弹幕匹配";
const MAX_RPC_XML_BYTES: usize = 3 * 1024 * 1024;
const MAX_PROVIDER_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const PROVIDER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Clone)]
struct DanmakuProviderClient {
    client: Client,
    base: String,
}

impl DanmakuProviderClient {
    fn new(base_url: &str, proxy_url: Option<&str>) -> Result<Self, DanmakuProviderError> {
        let base = validate_provider_base_url(base_url)
            .map_err(|_| DanmakuProviderError::InvalidProviderUrl)?
            .normalized()
            .to_owned();
        let builder = luxd::network::client_builder_from_env_or(proxy_url)
            .map_err(|_| DanmakuProviderError::InvalidProxy)?;
        let client = builder
            .timeout(PROVIDER_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| DanmakuProviderError::Client)?;
        Ok(Self { client, base })
    }

    async fn match_filename(
        &self,
        file_name: &str,
    ) -> Result<Option<DanmakuMatch>, DanmakuProviderError> {
        if file_name.trim().is_empty() || file_name.chars().count() > 1024 {
            return Err(DanmakuProviderError::InvalidRequest);
        }
        let response = self
            .client
            .post(self.api_url("match")?)
            .json(&json!({ "fileName": file_name }))
            .send()
            .await
            .map_err(|_| DanmakuProviderError::Unavailable)?;
        if matches!(
            response.status(),
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
        ) {
            return self.fallback_match(file_name).await;
        }
        let body = self.read_response(response).await?;
        let value: Value =
            serde_json::from_slice(&body).map_err(|_| DanmakuProviderError::InvalidResponse)?;
        let Some(first) = value
            .get("matches")
            .and_then(Value::as_array)
            .and_then(|matches| matches.first())
        else {
            return Ok(None);
        };
        let episode_id = json_string(first.get("episodeId"))
            .filter(|value| !value.is_empty())
            .ok_or(DanmakuProviderError::InvalidResponse)?;
        Ok(Some(DanmakuMatch {
            anime_id: json_string(first.get("animeId")),
            episode_id,
        }))
    }

    async fn fallback_match(
        &self,
        file_name: &str,
    ) -> Result<Option<DanmakuMatch>, DanmakuProviderError> {
        let Some(parsed) = parse_media_name(file_name, MediaKind::Episode) else {
            return Ok(None);
        };
        let Some(episode_number) = parsed.episode else {
            return Ok(None);
        };
        let mut url = self.api_url("search/episodes")?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("anime", &parsed.title);
            query.append_pair("episode", &episode_number.to_string());
            if let Some(season) = parsed.season {
                query.append_pair("season", &season.to_string());
            }
        }
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|_| DanmakuProviderError::Unavailable)?;
        let body = match self.read_response(response).await {
            Ok(body) => body,
            Err(DanmakuProviderError::Unsupported) => return Ok(None),
            Err(error) => return Err(error),
        };
        let value: Value =
            serde_json::from_slice(&body).map_err(|_| DanmakuProviderError::InvalidResponse)?;
        let normalized_title = normalize_title(&parsed.title);
        let Some((episode_id, anime_id)) =
            find_episode_match(&value, episode_number, &normalized_title, None)
        else {
            return Ok(None);
        };
        Ok(Some(DanmakuMatch {
            anime_id,
            episode_id,
        }))
    }

    async fn fetch_episode_xml(&self, episode_id: &str) -> Result<Vec<u8>, DanmakuProviderError> {
        if episode_id.trim().is_empty() || episode_id.chars().count() > 256 {
            return Err(DanmakuProviderError::InvalidRequest);
        }
        let mut url = self.api_url("comment")?;
        url.path_segments_mut()
            .map_err(|_| DanmakuProviderError::InvalidProviderUrl)?
            .push(episode_id);
        url.query_pairs_mut().append_pair("format", "xml");
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|_| DanmakuProviderError::Unavailable)?;
        let body = self.read_response(response).await?;
        validate_danmaku_xml(&body).map_err(|_| DanmakuProviderError::InvalidXml)?;
        Ok(body)
    }

    fn api_url(&self, operation: &str) -> Result<Url, DanmakuProviderError> {
        let mut url =
            Url::parse(&self.base).map_err(|_| DanmakuProviderError::InvalidProviderUrl)?;
        let base_path = url.path().trim_end_matches('/');
        let path = if base_path.ends_with("/api/v2") {
            format!("{base_path}/{operation}")
        } else {
            format!("{base_path}/api/v2/{operation}")
        };
        url.set_path(&path);
        url.set_query(None);
        Ok(url)
    }

    async fn read_response(
        &self,
        mut response: reqwest::Response,
    ) -> Result<Vec<u8>, DanmakuProviderError> {
        let status = response.status();
        if !status.is_success() {
            return Err(
                if status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED {
                    DanmakuProviderError::Unsupported
                } else {
                    DanmakuProviderError::HttpStatus
                },
            );
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_PROVIDER_RESPONSE_BYTES as u64)
        {
            return Err(DanmakuProviderError::ResponseTooLarge);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| DanmakuProviderError::Unavailable)?
        {
            if body.len().saturating_add(chunk.len()) > MAX_PROVIDER_RESPONSE_BYTES {
                return Err(DanmakuProviderError::ResponseTooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
}

#[derive(Clone, Debug)]
struct DanmakuMatch {
    anime_id: Option<String>,
    episode_id: String,
}

#[derive(Clone, Debug)]
enum DanmakuProviderError {
    InvalidProviderUrl,
    InvalidProxy,
    InvalidRequest,
    Client,
    Unavailable,
    Unsupported,
    HttpStatus,
    ResponseTooLarge,
    InvalidResponse,
    InvalidXml,
}

fn json_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .or_else(|| value.as_i64().map(|value| value.to_string()))
        })
        .filter(|value| !value.trim().is_empty())
}

fn find_episode_match(
    value: &Value,
    expected_episode: u32,
    normalized_title: &str,
    inherited_anime_id: Option<String>,
) -> Option<(String, Option<String>)> {
    match value {
        Value::Array(values) => values.iter().find_map(|value| {
            find_episode_match(
                value,
                expected_episode,
                normalized_title,
                inherited_anime_id.clone(),
            )
        }),
        Value::Object(object) => {
            let anime_id = json_string(object.get("animeId")).or(inherited_anime_id);
            let anime_title = json_string(object.get("animeTitle"));
            let title_matches = anime_title
                .as_deref()
                .map(normalize_title)
                .is_none_or(|title| {
                    normalized_title.is_empty()
                        || title.contains(normalized_title)
                        || normalized_title.contains(&title)
                });
            if title_matches
                && let Some(episode_id) = json_string(object.get("episodeId"))
                && object_episode_number(object).is_some_and(|number| number == expected_episode)
            {
                return Some((episode_id, anime_id));
            }
            object.values().find_map(|value| {
                find_episode_match(value, expected_episode, normalized_title, anime_id.clone())
            })
        }
        _ => None,
    }
}

fn object_episode_number(object: &serde_json::Map<String, Value>) -> Option<u32> {
    object
        .get("episodeNumber")
        .or_else(|| object.get("episodeNo"))
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .or_else(|| {
            json_string(object.get("episodeTitle")).and_then(|title| {
                title
                    .split(|character: char| !character.is_ascii_digit())
                    .filter(|value| !value.is_empty())
                    .find_map(|value| value.parse::<u32>().ok())
            })
        })
}

impl fmt::Display for DanmakuProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(error_message(self))
    }
}

impl std::error::Error for DanmakuProviderError {}

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
    let mut candidate_file_names = Vec::with_capacity(1 + request.alternate_file_names.len());
    candidate_file_names.push(request.file_name);
    candidate_file_names.extend(request.alternate_file_names);
    let mut matched = None;
    for file_name in candidate_file_names {
        if let Some(value) = provider.match_filename(&file_name).await? {
            matched = Some(value);
            break;
        }
    }
    let Some(matched) = matched else {
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
        DanmakuProviderError::InvalidProviderUrl => "INVALID_PROVIDER_URL",
        DanmakuProviderError::InvalidProxy => "INVALID_PROXY",
        DanmakuProviderError::InvalidRequest => "INVALID_REQUEST",
        DanmakuProviderError::Client => "CLIENT_UNAVAILABLE",
        DanmakuProviderError::Unavailable => "PROVIDER_UNAVAILABLE",
        DanmakuProviderError::Unsupported => "PROVIDER_UNSUPPORTED",
        DanmakuProviderError::HttpStatus => "PROVIDER_HTTP_ERROR",
        DanmakuProviderError::ResponseTooLarge => "PROVIDER_RESPONSE_TOO_LARGE",
        DanmakuProviderError::InvalidResponse => "PROVIDER_INVALID_RESPONSE",
        DanmakuProviderError::InvalidXml => "PROVIDER_INVALID_XML",
    }
}

fn error_message(error: &DanmakuProviderError) -> &'static str {
    match error {
        DanmakuProviderError::InvalidProviderUrl => "danmaku provider URL is invalid",
        DanmakuProviderError::InvalidProxy => "danmaku provider proxy is invalid",
        DanmakuProviderError::InvalidRequest => "danmaku provider request is invalid",
        DanmakuProviderError::Client => "danmaku provider client is unavailable",
        DanmakuProviderError::Unavailable => "danmaku provider is unavailable",
        DanmakuProviderError::Unsupported => "danmaku provider endpoint is unsupported",
        DanmakuProviderError::HttpStatus => "danmaku provider returned an HTTP error",
        DanmakuProviderError::ResponseTooLarge => "danmaku provider response is too large",
        DanmakuProviderError::InvalidResponse => "danmaku provider response is invalid",
        DanmakuProviderError::InvalidXml => "danmaku provider XML is invalid",
    }
}
