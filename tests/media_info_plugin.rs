use std::{fs, process::Stdio};

use serde_json::{Value, json};
use tempfile::tempdir;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
};

#[cfg(unix)]
#[tokio::test]
async fn media_info_plugin_probes_a_remote_source_and_rejects_local_urls()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = tempdir()?;
    let ffprobe = temp_dir.path().join("ffprobe");
    fs::write(
        &ffprobe,
        r#"#!/bin/sh
printf '%s' '{"format":{"format_name":"matroska","size":"1234","duration":"12.5","bit_rate":"500000"},"streams":[{"index":0,"codec_type":"video","codec_name":"h264","width":1920,"height":1080},{"index":1,"codec_type":"audio","codec_name":"aac","tags":{"language":"eng"}}]}'
"#,
    )?;
    let mut permissions = fs::metadata(&ffprobe)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&ffprobe, permissions)?;

    let binary = plugin_binary()?;
    let mut child = Command::new(binary)
        .env("LUX_FFPROBE_BINARY", &ffprobe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or("plugin stdin missing")?;
    let stdout = child.stdout.take().ok_or("plugin stdout missing")?;
    let mut stdout = BufReader::new(stdout);

    let hello = call(&mut stdin, &mut stdout, "hello", "plugin.hello", json!({})).await?;
    assert_eq!(hello["id"], "org.lux.strm-media-info");
    assert_eq!(hello["name"], "strm媒体信息提取");
    assert_eq!(hello["capabilities"][0], "media.probe");

    let result = call(
        &mut stdin,
        &mut stdout,
        "probe",
        "media.probe",
        json!({"url":"https://media.example.invalid/video.mkv"}),
    )
    .await?;
    assert_eq!(result["container"], "matroska");
    assert_eq!(result["sourceSize"], 1234);
    assert_eq!(result["durationTicks"], 125000000);
    assert_eq!(result["streams"][0]["streamType"], "VIDEO");
    assert_eq!(result["streams"][1]["language"], "eng");

    let error = call_error(
        &mut stdin,
        &mut stdout,
        "local",
        "media.probe",
        json!({"url":"file:///etc/passwd"}),
    )
    .await?;
    assert_eq!(error["code"], "MEDIA_PROBE_INVALID_URL");

    let _ = call(
        &mut stdin,
        &mut stdout,
        "shutdown",
        "plugin.shutdown",
        json!({}),
    )
    .await?;
    drop(stdin);
    let _ = child.wait().await?;
    Ok(())
}

async fn call(
    stdin: &mut tokio::process::ChildStdin,
    stdout: &mut BufReader<tokio::process::ChildStdout>,
    id: &str,
    method: &str,
    params: Value,
) -> Result<Value, Box<dyn std::error::Error>> {
    let response = send(stdin, stdout, id, method, params).await?;
    if let Some(error) = response.get("error") {
        return Err(format!("plugin returned error: {error}").into());
    }
    response
        .get("result")
        .cloned()
        .ok_or_else(|| "plugin result missing".into())
}

async fn call_error(
    stdin: &mut tokio::process::ChildStdin,
    stdout: &mut BufReader<tokio::process::ChildStdout>,
    id: &str,
    method: &str,
    params: Value,
) -> Result<Value, Box<dyn std::error::Error>> {
    let response = send(stdin, stdout, id, method, params).await?;
    response
        .get("error")
        .cloned()
        .ok_or_else(|| "plugin error missing".into())
}

async fn send(
    stdin: &mut tokio::process::ChildStdin,
    stdout: &mut BufReader<tokio::process::ChildStdout>,
    id: &str,
    method: &str,
    params: Value,
) -> Result<Value, Box<dyn std::error::Error>> {
    let mut request = serde_json::to_vec(&json!({
        "id": id,
        "method": method,
        "params": params,
    }))?;
    request.push(b'\n');
    stdin.write_all(&request).await?;
    stdin.flush().await?;
    let mut line = String::new();
    stdout.read_line(&mut line).await?;
    Ok(serde_json::from_str(&line)?)
}

fn plugin_binary() -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    std::env::var_os("CARGO_BIN_EXE_lux-plugin-strm-media-info")
        .or_else(|| std::env::var_os("CARGO_BIN_EXE_lux_plugin_strm_media_info"))
        .map(std::path::PathBuf::from)
        .ok_or_else(|| "strm-media-info plugin binary path is missing".into())
}
