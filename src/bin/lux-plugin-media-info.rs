use std::{path::PathBuf, time::Duration};

use luxd::application::{
    plugin_protocol::{
        MediaProbeRpcResult, MediaProbeRpcStream, MediaProbeRpcStreamType, PluginRequest,
        PluginResponse, PluginRpcError,
    },
    probe::{MediaProbeResult, ProbeError, StreamType, parse_probe_json},
    strm_probe_policy::validate_remote_media_url,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command,
    time::timeout,
};

const PLUGIN_ID: &str = "org.lux.media-info";
const PLUGIN_NAME: &str = "strm媒体信息提取";
const FFPROBE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_ERROR_BYTES: usize = 8 * 1024;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MediaProbeRequest {
    url: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let mut lines = BufReader::new(stdin).lines();
    let mut output = stdout;

    while let Some(line) = lines.next_line().await? {
        let response = match serde_json::from_str::<PluginRequest>(&line) {
            Ok(request) => handle_request(request).await,
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

async fn handle_request(request: PluginRequest) -> PluginResponse {
    let id = request.id.clone();
    match handle_method(&request.method, request.params).await {
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

async fn handle_method(method: &str, params: Value) -> Result<Value, PluginRpcError> {
    match method {
        "plugin.hello" => Ok(json!({
            "id": PLUGIN_ID,
            "name": PLUGIN_NAME,
            "apiVersion": 1,
            "capabilities": ["media.probe"],
            "supportedItemTypes": []
        })),
        "plugin.health" => Ok(json!({
            "available": ffprobe_binary().is_ok(),
            "configured": true
        })),
        "media.probe" => probe(params).await,
        "plugin.shutdown" => Ok(json!({"accepted": true})),
        _ => Err(PluginRpcError {
            code: "PLUGIN_INVALID_REQUEST".to_owned(),
            message: "unsupported plugin method".to_owned(),
        }),
    }
}

async fn probe(params: Value) -> Result<Value, PluginRpcError> {
    let request: MediaProbeRequest =
        serde_json::from_value(params).map_err(|_| PluginRpcError {
            code: "MEDIA_PROBE_INVALID_REQUEST".to_owned(),
            message: "media probe request is invalid".to_owned(),
        })?;
    if !validate_remote_media_url(&request.url) {
        return Err(invalid_url());
    }
    let result = run_ffprobe(&request.url).await?;
    let result = rpc_result(result);
    serde_json::to_value(result).map_err(|_| PluginRpcError {
        code: "MEDIA_PROBE_INVALID_OUTPUT".to_owned(),
        message: "media probe result could not be serialized".to_owned(),
    })
}

async fn run_ffprobe(url: &str) -> Result<MediaProbeResult, PluginRpcError> {
    let binary = ffprobe_binary()?;
    let mut child = Command::new(binary)
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(url)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| process_error())?;
    let mut stdout = child.stdout.take().ok_or_else(process_error)?;
    let mut stderr = child.stderr.take().ok_or_else(process_error)?;
    let output = timeout(FFPROBE_TIMEOUT, async {
        let (stdout_read, stderr_read, status) = tokio::try_join!(
            read_limited(&mut stdout, MAX_OUTPUT_BYTES),
            read_limited(&mut stderr, MAX_ERROR_BYTES),
            async { child.wait().await.map_err(|_| process_error()) },
        )?;
        Ok::<_, PluginRpcError>((status, stdout_read, stderr_read))
    })
    .await
    .map_err(|_| timeout_error())??;
    if !output.0.success() {
        return Err(process_error());
    }
    parse_probe_json(&output.1).map_err(map_probe_error)
}

async fn read_limited<R>(reader: &mut R, limit: usize) -> Result<Vec<u8>, PluginRpcError>
where
    R: AsyncRead + Unpin,
{
    let mut output = Vec::with_capacity(limit.min(64 * 1024));
    let mut limited = reader.take((limit as u64).saturating_add(1));
    limited
        .read_to_end(&mut output)
        .await
        .map_err(|_| process_error())?;
    if output.len() > limit {
        return Err(output_error());
    }
    Ok(output)
}

fn ffprobe_binary() -> Result<PathBuf, PluginRpcError> {
    Ok(std::env::var_os("LUX_FFPROBE_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("ffprobe")))
}

fn invalid_url() -> PluginRpcError {
    PluginRpcError {
        code: "MEDIA_PROBE_INVALID_URL".to_owned(),
        message: "media source URL is not allowed".to_owned(),
    }
}

fn process_error() -> PluginRpcError {
    PluginRpcError {
        code: "MEDIA_PROBE_PROCESS_FAILED".to_owned(),
        message: "ffprobe could not inspect the media source".to_owned(),
    }
}

fn timeout_error() -> PluginRpcError {
    PluginRpcError {
        code: "MEDIA_PROBE_TIMEOUT".to_owned(),
        message: "media probe timed out".to_owned(),
    }
}

fn output_error() -> PluginRpcError {
    PluginRpcError {
        code: "MEDIA_PROBE_OUTPUT_TOO_LARGE".to_owned(),
        message: "media probe output is too large".to_owned(),
    }
}

fn map_probe_error(error: ProbeError) -> PluginRpcError {
    let code = match error {
        ProbeError::OutputTooLarge => "MEDIA_PROBE_OUTPUT_TOO_LARGE",
        ProbeError::InvalidOutput(_) => "MEDIA_PROBE_INVALID_OUTPUT",
        ProbeError::Timeout => "MEDIA_PROBE_TIMEOUT",
        ProbeError::Io(_) | ProbeError::Exit { .. } => "MEDIA_PROBE_PROCESS_FAILED",
    };
    PluginRpcError {
        code: code.to_owned(),
        message: match code {
            "MEDIA_PROBE_OUTPUT_TOO_LARGE" => "media probe output is too large",
            "MEDIA_PROBE_INVALID_OUTPUT" => "ffprobe returned invalid media information",
            "MEDIA_PROBE_TIMEOUT" => "media probe timed out",
            _ => "ffprobe could not inspect the media source",
        }
        .to_owned(),
    }
}

fn rpc_result(result: MediaProbeResult) -> MediaProbeRpcResult {
    MediaProbeRpcResult {
        container: result.container,
        source_size: result.source_size,
        duration_ticks: result.duration_ticks,
        bitrate: result.bitrate,
        streams: result.streams.into_iter().map(rpc_stream).collect(),
    }
}

fn rpc_stream(stream: luxd::application::probe::MediaStreamResult) -> MediaProbeRpcStream {
    MediaProbeRpcStream {
        stream_index: stream.stream_index,
        stream_type: match stream.stream_type {
            StreamType::Video => MediaProbeRpcStreamType::Video,
            StreamType::Audio => MediaProbeRpcStreamType::Audio,
            StreamType::Subtitle => MediaProbeRpcStreamType::Subtitle,
        },
        codec: stream.codec,
        language: stream.language,
        title: stream.title,
        is_default: stream.is_default,
        is_forced: stream.is_forced,
        details: stream.details,
    }
}
