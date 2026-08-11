use std::{fs, process::Stdio};

use serde_json::{Value, json};
use tempfile::tempdir;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
};

#[cfg(unix)]
#[tokio::test]
async fn media_info_plugin_probes_a_remote_source_and_accepts_opaque_targets()
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

    let ffmpeg = temp_dir.path().join("ffmpeg");
    let ffmpeg_args = temp_dir.path().join("ffmpeg.args");
    fs::write(
        &ffmpeg,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$LUX_FFMPEG_ARGS\"\nprintf '\\377\\330\\377fake-thumb\\377\\331'\n",
    )?;
    let mut permissions = fs::metadata(&ffmpeg)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&ffmpeg, permissions)?;

    let binary = plugin_binary()?;
    let mut child = Command::new(binary)
        .env("LUX_FFPROBE_BINARY", &ffprobe)
        .env("LUX_FFMPEG_BINARY", &ffmpeg)
        .env("LUX_FFMPEG_ARGS", &ffmpeg_args)
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

    let thumbnail = call(
        &mut stdin,
        &mut stdout,
        "thumbnail",
        "media.probe",
        json!({
            "url": "http://192.168.1.10/video.mkv",
            "includeMediaInfo": false,
            "includeThumbnail": true
        }),
    )
    .await?;
    assert!(thumbnail["container"].is_null());
    assert!(thumbnail["streams"].as_array().is_some_and(Vec::is_empty));
    assert!(
        thumbnail["thumbnailJpegBase64"]
            .as_str()
            .is_some_and(|value| value.starts_with("/9j"))
    );
    let ffmpeg_args = fs::read_to_string(&ffmpeg_args)?;
    assert!(
        ffmpeg_args
            .lines()
            .collect::<Vec<_>>()
            .windows(2)
            .any(|pair| { pair == ["-ss", "3.750"] })
    );

    for url in [
        "http://192.168.1.10/video.mkv",
        "http://127.0.0.1/video.mkv",
        "http://localhost/video.mkv",
        "/media/library/Video.mkv",
    ] {
        let target_result = call(
            &mut stdin,
            &mut stdout,
            "private",
            "media.probe",
            json!({"url":url}),
        )
        .await?;
        assert_eq!(target_result["container"], "matroska");
    }

    let error = call_error(
        &mut stdin,
        &mut stdout,
        "empty",
        "media.probe",
        json!({"url":"   "}),
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
