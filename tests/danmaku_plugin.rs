use std::process::Stdio;

use axum::{
    Router,
    extract::Json,
    http::StatusCode,
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use luxd::application::plugin_protocol::{
    DanmakuMatchRpcResult, DanmakuMatchStatus, PluginRequest, PluginResponse,
};
use serde_json::json;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
};

#[tokio::test]
async fn danmaku_plugin_process_matches_and_returns_xml() -> Result<(), Box<dyn std::error::Error>>
{
    let app = Router::new()
        .route("/api/v2/match", post(|| async { StatusCode::NOT_FOUND }))
        .route(
            "/api/v2/search/episodes",
            get(|| async {
                Json(json!({
                    "animes": [{
                        "animeId": 12,
                        "animeTitle": "Demo",
                        "episodes": [{
                            "episodeId": 34,
                            "episodeNumber": 1,
                            "episodeTitle": "S01E01"
                        }]
                    }]
                }))
            }),
        )
        .route(
            "/api/v2/comment/34",
            get(|| async {
                (
                    StatusCode::OK,
                    [("content-type", "application/xml")],
                    "<i><d p=\"1,1,25,16777215,0,0,0,0\">hello</d></i>",
                )
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    let directory = tempfile::tempdir()?;
    let config_dir = directory.path().join("config");
    tokio::fs::create_dir_all(config_dir.join("plugin-config")).await?;
    tokio::fs::write(
        config_dir.join("plugin-config/org.lux.danmaku.json"),
        serde_json::to_vec(&json!({
            "providerBaseUrl": format!("http://127.0.0.1:{}", address.port())
        }))?,
    )
    .await?;

    let mut child = Command::new(env!("CARGO_BIN_EXE_lux-plugin-danmaku"))
        .env("LUX_CONFIG_DIR", &config_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or("plugin stdin unavailable")?;
    let stdout = child.stdout.take().ok_or("plugin stdout unavailable")?;
    let mut lines = BufReader::new(stdout).lines();

    let hello = PluginRequest::new("hello", "plugin.hello", json!({}));
    send_request(&mut stdin, &hello).await?;
    let response = read_response(&mut lines).await?;
    assert_eq!(response.id, "hello");
    assert_eq!(
        response
            .result
            .as_ref()
            .and_then(|value| value["id"].as_str()),
        Some("org.lux.danmaku")
    );

    let request = PluginRequest::new(
        "match",
        "danmaku.match",
        json!({"fileName": "Demo.S01E01.mkv"}),
    );
    send_request(&mut stdin, &request).await?;
    let response = read_response(&mut lines).await?;
    let result: DanmakuMatchRpcResult =
        serde_json::from_value(response.result.ok_or("missing result")?)?;
    assert_eq!(result.status, DanmakuMatchStatus::Matched);
    assert_eq!(result.episode_id.as_deref(), Some("34"));
    assert_eq!(
        BASE64.decode(result.xml_base64.ok_or("missing XML")?)?,
        b"<i><d p=\"1,1,25,16777215,0,0,0,0\">hello</d></i>"
    );

    let shutdown = PluginRequest::new("shutdown", "plugin.shutdown", json!({}));
    send_request(&mut stdin, &shutdown).await?;
    let response = read_response(&mut lines).await?;
    assert_eq!(response.id, "shutdown");
    assert!(response.error.is_none());
    assert!(child.wait().await?.success());
    server.abort();
    Ok(())
}

#[tokio::test]
async fn danmaku_plugin_tries_alternate_file_names_after_no_match()
-> Result<(), Box<dyn std::error::Error>> {
    let app = Router::new()
        .route(
            "/api/v2/match",
            post(|Json(request): Json<serde_json::Value>| async move {
                if request["fileName"] == "Demo.S01E01.mkv" {
                    return (StatusCode::OK, Json(json!({"matches": []})));
                }
                (
                    StatusCode::OK,
                    Json(json!({"matches": [{"animeId": 12, "episodeId": 34}]})),
                )
            }),
        )
        .route(
            "/api/v2/comment/34",
            get(|| async {
                (
                    StatusCode::OK,
                    [
                        ("content-type", "application/xml"),
                    ],
                    "<i><d p=\"1,1,25,16777215,0,0,0,0\">alternate</d></i>",
                )
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    let directory = tempfile::tempdir()?;
    let config_dir = directory.path().join("config");
    tokio::fs::create_dir_all(config_dir.join("plugin-config")).await?;
    tokio::fs::write(
        config_dir.join("plugin-config/org.lux.danmaku.json"),
        serde_json::to_vec(&json!({
            "providerBaseUrl": format!("http://127.0.0.1:{}", address.port())
        }))?,
    )
    .await?;

    let mut child = Command::new(env!("CARGO_BIN_EXE_lux-plugin-danmaku"))
        .env("LUX_CONFIG_DIR", &config_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().ok_or("plugin stdin unavailable")?;
    let stdout = child.stdout.take().ok_or("plugin stdout unavailable")?;
    let mut lines = BufReader::new(stdout).lines();

    let request = PluginRequest::new(
        "alternate",
        "danmaku.match",
        json!({
            "fileName": "Demo.S01E01.mkv",
            "alternateFileNames": ["Demo 简体标题 S01E01.mkv"]
        }),
    );
    send_request(&mut stdin, &request).await?;
    let response = read_response(&mut lines).await?;
    let result: DanmakuMatchRpcResult =
        serde_json::from_value(response.result.ok_or("missing result")?)?;
    assert_eq!(result.status, DanmakuMatchStatus::Matched);
    assert_eq!(result.episode_id.as_deref(), Some("34"));
    assert_eq!(
        BASE64.decode(result.xml_base64.ok_or("missing XML")?)?,
        b"<i><d p=\"1,1,25,16777215,0,0,0,0\">alternate</d></i>"
    );

    let shutdown = PluginRequest::new("shutdown", "plugin.shutdown", json!({}));
    send_request(&mut stdin, &shutdown).await?;
    let _ = read_response(&mut lines).await?;
    assert!(child.wait().await?.success());
    server.abort();
    Ok(())
}

async fn send_request(
    stdin: &mut tokio::process::ChildStdin,
    request: &PluginRequest,
) -> Result<(), std::io::Error> {
    let mut bytes = serde_json::to_vec(request).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    stdin.write_all(&bytes).await?;
    stdin.flush().await
}

async fn read_response(
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
) -> Result<PluginResponse, Box<dyn std::error::Error>> {
    let line = lines
        .next_line()
        .await?
        .ok_or("plugin exited without response")?;
    Ok(serde_json::from_str(&line)?)
}
