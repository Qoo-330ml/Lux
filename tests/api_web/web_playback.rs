use axum::{Router, body::Bytes, extract::State, response::IntoResponse, routing::post};
use luxd::{
    api::{AppState, app_with_state},
    application::{
        libraries::LibraryService,
        plugins::{MEDIA_INFO_PLUGIN_ID, PluginService},
        scanner::LibraryScanner,
        setup::SetupService,
        strm_playback::StrmPlaybackResolver,
        webhooks::WebhookService,
    },
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    observability::logs::LogStore,
    storage::Database,
};
use reqwest::header::{COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::mpsc,
};

fn emby_public_id(id: &str) -> String {
    uuid::Uuid::parse_str(id)
        .map(|uuid| uuid.as_u128().to_string())
        .unwrap_or_else(|_| id.to_owned())
}

fn cookie_value(headers: &reqwest::header::HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| {
            let (pair, _) = value.split_once(';')?;
            let (cookie_name, cookie_value) = pair.split_once('=')?;
            (cookie_name == name).then(|| cookie_value.to_owned())
        })
}

async fn active_admission_count(
    database: &Database,
    user_id: &str,
    play_session_id: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM playback_admissions
         WHERE user_id = ? AND play_session_id = ? AND state = 'ACTIVE'",
    )
    .bind(user_id)
    .bind(play_session_id)
    .fetch_one(database.pool())
    .await
}

async fn create_direct_ticket(
    client: &reqwest::Client,
    base_url: &str,
    cookies: &str,
    csrf_cookie: &str,
    item_id: &str,
    source_id: &str,
    device_id: Option<&str>,
) -> Result<(String, String), Box<dyn std::error::Error>> {
    let mut request = client
        .post(format!("{base_url}/api/v1/playback/sessions"))
        .header(COOKIE, cookies)
        .header("x-csrf-token", csrf_cookie)
        .json(&json!({
            "itemId": item_id,
            "sourceId": source_id,
            "capabilities": {
                "directPlay": true,
                "hls": true,
                "videoCopyToFmp4": true,
                "audioCopyToFmp4": true,
                "softwareTranscode": true
            }
        }));
    if let Some(device_id) = device_id {
        request = request.header("x-lux-device-id", device_id);
    }
    let response = request.send().await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body = response.json::<Value>().await?;
    Ok((
        body["sessionId"]
            .as_str()
            .ok_or("missing test playback session id")?
            .to_owned(),
        body["plan"]["url"]
            .as_str()
            .ok_or("missing test playback resource URL")?
            .to_owned(),
    ))
}

async fn insert_global_access_rule(
    database: &Database,
    rule_id: &str,
    rule_type: &str,
    match_value: &str,
    created_by: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO user_access_blacklist_rules (
             id, user_id, rule_type, match_value, is_enabled, created_by
         ) VALUES (?, NULL, ?, ?, 1, ?)",
    )
    .bind(rule_id)
    .bind(rule_type)
    .bind(match_value)
    .bind(created_by)
    .execute(database.pool())
    .await?;
    Ok(())
}

#[tokio::test]
async fn web_playback_uses_signed_direct_urls_and_monotonic_events()
-> Result<(), Box<dyn std::error::Error>> {
    let (sender, mut receiver) = mpsc::channel::<Bytes>(2);
    let receiver_app = Router::new()
        .route(
            "/hook",
            post(
                |State(sender): State<mpsc::Sender<Bytes>>, body: Bytes| async move {
                    let _ = sender.send(body).await;
                    ().into_response()
                },
            ),
        )
        .with_state(sender);
    let receiver_listener = TcpListener::bind("127.0.0.1:0").await?;
    let receiver_address = receiver_listener.local_addr()?;
    let receiver_server =
        tokio::spawn(async move { axum::serve(receiver_listener, receiver_app).await });
    let proxy_listener = TcpListener::bind("127.0.0.1:0").await?;
    let proxy_address = proxy_listener.local_addr()?;
    let strm_proxy = tokio::spawn(async move {
        let mut request_number = 0;
        while request_number < 3 {
            let (mut stream, _) = proxy_listener
                .accept()
                .await
                .expect("STRM proxy should accept a range request");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let read = stream
                    .read(&mut buffer)
                    .await
                    .expect("STRM proxy request should be readable");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let request = String::from_utf8(request).expect("STRM proxy request should be HTTP");
            // `/direct` resolves the redirect target with a one-byte probe; answer it without
            // consuming one of the scripted Range requests below.
            if request.lines().any(|line| {
                line.split_once(':').is_some_and(|(name, value)| {
                    name.eq_ignore_ascii_case("range") && value.trim() == "bytes=0-0"
                })
            }) {
                stream
                    .write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Length: 1\r\nContent-Range: bytes 0-0/16\r\nConnection: close\r\n\r\n0")
                    .await
                    .expect("STRM proxy probe response should be writable");
                continue;
            }
            request_number += 1;
            let request_number = request_number - 1;
            let expected_range = if request_number < 2 {
                "bytes=0-3"
            } else {
                "bytes=4-7"
            };
            assert!(request.lines().any(|line| {
                line.split_once(':').is_some_and(|(name, value)| {
                    name.eq_ignore_ascii_case("range") && value.trim() == expected_range
                })
            }));
            let response = if request_number < 2 {
                &b"HTTP/1.1 206 Partial Content\r\nContent-Length: 4\r\nContent-Range: bytes 0-3/16\r\nContent-Type: video/mp4\r\nConnection: close\r\n\r\n0123"[..]
            } else {
                &b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"[..]
            };
            stream
                .write_all(response)
                .await
                .expect("STRM proxy response should be writable");
        }
    });
    let strm_resolver =
        StrmPlaybackResolver::new_with_proxy_for_tests(format!("http://{proxy_address}"))?;
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    };
    let audit_logs = LogStore::new(&config.config_dir);
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    let media_path = root.join("Signed Playback Movie 2026.mp4");
    tokio::fs::write(&media_path, b"web-playback-bytes").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let plugin_root = config_dir.join("plugins").join(MEDIA_INFO_PLUGIN_ID);
    tokio::fs::create_dir_all(plugin_root.join("binaries")).await?;
    tokio::fs::write(plugin_root.join("binaries/plugin"), b"placeholder").await?;
    tokio::fs::write(
        plugin_root.join("manifest.json"),
        serde_json::to_vec_pretty(&json!({
            "formatVersion": 1,
            "id": MEDIA_INFO_PLUGIN_ID,
            "name": "strm媒体信息提取",
            "version": "1.0.0",
            "apiVersion": 1,
            "runtime": {"kind": "process", "entrypoint": "binaries/plugin"},
            "type": "media_probe",
            "category": "MEDIA",
            "capabilities": ["media.probe"],
            "configFields": [
                {"key": "libraryIds", "label": "媒体库", "type": "select", "multiple": true, "required": true, "optionsSource": "media-libraries"},
                {"key": "concurrency", "label": "并发数", "type": "number", "required": true, "defaultValue": 2, "minimum": 1, "maximum": 64},
                {"key": "mediaInfoEnabled", "label": "提取媒体信息", "type": "toggle", "defaultValue": true},
                {"key": "thumbnailEnabled", "label": "补全 STRM 缩略图", "type": "toggle", "defaultValue": false},
                {"key": "thumbnailPositionPercent", "label": "缩略图位置", "type": "number", "required": true, "defaultValue": 30, "minimum": 1, "maximum": 99},
                {"key": "existingInfoPolicy", "label": "已有媒体信息处理方式", "type": "select", "defaultValue": "SKIP", "options": [{"value": "SKIP", "label": "跳过已有媒体信息"}, {"value": "OVERWRITE", "label": "覆盖已有媒体信息"}]},
                {"key": "writeSidecars", "label": "写入旁车", "type": "toggle", "defaultValue": false},
                {"key": "schedule", "label": "执行计划", "type": "text", "required": true, "defaultValue": "0 3 * * *"}
            ],
            "permissions": {"network": ["media-source"], "filesystem": []},
            "files": []
        }))?,
    )
    .await?;
    let plugins = PluginService::new(database.clone(), config_dir.clone());
    plugins.install(MEDIA_INFO_PLUGIN_ID).await?;
    plugins
        .update_dynamic_config(
            MEDIA_INFO_PLUGIN_ID,
            serde_json::Map::from_iter([
                ("libraryIds".to_owned(), json!([library.id.to_string()])),
                ("concurrency".to_owned(), json!(2)),
                ("mediaInfoEnabled".to_owned(), json!(true)),
                ("thumbnailEnabled".to_owned(), json!(false)),
                ("thumbnailPositionPercent".to_owned(), json!(30)),
                ("existingInfoPolicy".to_owned(), json!("OVERWRITE")),
                ("writeSidecars".to_owned(), json!(false)),
                ("schedule".to_owned(), json!("0 3 * * *")),
            ]),
        )
        .await?;
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE'")
            .fetch_one(database.pool())
            .await?;
    let source_id: String = sqlx::query_scalar("SELECT id FROM media_sources WHERE item_id = ?")
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?;
    let webhook_service = WebhookService::new(database.clone(), temp_dir.path().join("config"))?;
    webhook_service
        .create_destination(
            "Web playback receiver",
            &format!("http://{receiver_address}/hook"),
            true,
            true,
            &[
                "PLAYBACK_STARTED".to_owned(),
                "PLAYBACK_PAUSED".to_owned(),
                "PLAYBACK_PROGRESS".to_owned(),
                "PLAYBACK_STOPPED".to_owned(),
            ],
            Some("web-playback-secret"),
        )
        .await?;
    let fixed_caption_tracks = [
        (2_i64, "subrip", "zho", "中文", 1_i64),
        (3, "ass", "eng", "English", 0),
        (4, "ssa", "jpn", "SSA", 0),
        (5, "hdmv_pgs_subtitle", "zho", "PGS 图形字幕", 0),
        (6, "sup", "zho", "SUP 图形字幕", 0),
    ];
    for (stream_index, codec, language, title, is_default) in fixed_caption_tracks {
        sqlx::query(
            "INSERT INTO media_streams
             (id, media_source_id, stream_index, stream_type, codec, language, title,
              is_external, is_default, is_forced)
             VALUES (?, ?, ?, 'SUBTITLE', ?, ?, ?, 0, ?, 0)",
        )
        .bind(uuid::Uuid::now_v7().to_string())
        .bind(&source_id)
        .bind(stream_index)
        .bind(codec)
        .bind(language)
        .bind(title)
        .bind(is_default)
        .execute(database.pool())
        .await?;
    }

    sqlx::query("DROP TABLE audit_events")
        .execute(database.pool())
        .await?;
    let fake_ffmpeg = temp_dir.path().join("fake-ffmpeg");
    let fake_ffmpeg_source = [
        "#!/bin/sh",
        "set -eu",
        "manifest=\"\"",
        "segment=\"\"",
        "init=\"init.mp4\"",
        "while [ \"$#\" -gt 0 ]; do",
        "  case \"$1\" in",
        "    -hls_segment_filename) segment=\"$2\"; shift 2 ;;",
        "    -hls_fmp4_init_filename) init=\"$2\"; shift 2 ;;",
        "    *.m3u8) manifest=\"$1\"; shift ;;",
        "    *) shift ;;",
        "  esac",
        "done",
        "directory=$(dirname \"$manifest\")",
        "mkdir -p \"$directory\"",
        "printf '#EXTM3U\\n#EXT-X-MAP:URI=\\\"%s\\\"\\n#EXTINF:1,\\nsegment_000000.m4s\\n' \"$init\" > \"$manifest\"",
        "printf init > \"$directory/$init\"",
        "printf segment > \"$(printf '%s' \"$segment\" | sed 's/%06d/000000/')\"",
    ]
    .join("\n");
    tokio::fs::write(&fake_ffmpeg, fake_ffmpeg_source).await?;
    let mut fake_permissions = tokio::fs::metadata(&fake_ffmpeg).await?.permissions();
    fake_permissions.set_mode(0o700);
    tokio::fs::set_permissions(&fake_ffmpeg, fake_permissions).await?;

    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let state = AppState::ready(config, database.clone(), setup, auth, emby_auth)
        .with_hls_executable(fake_ffmpeg)
        .with_strm_playback_resolver(strm_resolver);
    let app = app_with_state(state.clone());
    let peer_app = app_with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");
    let peer_listener = TcpListener::bind("127.0.0.1:0").await?;
    let peer_address = peer_listener.local_addr()?;
    let peer_server = tokio::spawn(async move {
        axum::serve(
            peer_listener,
            peer_app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
    });
    let peer_base_url = format!("http://{peer_address}");
    let peer_client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");

    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let session_cookie = cookie_value(login.headers(), "lux_session").ok_or("missing session")?;
    let csrf_cookie = cookie_value(login.headers(), "lux_csrf").ok_or("missing csrf")?;
    let cookies = format!("lux_session={session_cookie}; lux_csrf={csrf_cookie}");

    let item_details = client
        .get(format!("{base_url}/api/v1/items/{item_id}"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(item_details.status(), reqwest::StatusCode::OK);
    let item_details = item_details.json::<Value>().await?;
    let source_details = item_details["mediaSources"]
        .as_array()
        .and_then(|sources| sources.iter().find(|source| source["id"] == source_id))
        .ok_or("missing fixed local source")?;
    let caption_streams = source_details["streams"]
        .as_array()
        .ok_or("missing fixed caption streams")?
        .iter()
        .filter(|stream| stream["type"] == "SUBTITLE")
        .collect::<Vec<_>>();
    assert_eq!(caption_streams.len(), 5);
    assert_eq!(caption_streams[0]["index"], 2);
    assert_eq!(caption_streams[0]["codec"], "subrip");
    assert_eq!(caption_streams[0]["isExternal"], false);
    assert_eq!(caption_streams[0]["isDefault"], true);
    assert_eq!(caption_streams[1]["codec"], "ass");
    assert_eq!(caption_streams[2]["codec"], "ssa");
    assert_eq!(caption_streams[3]["codec"], "hdmv_pgs_subtitle");
    assert_eq!(caption_streams[4]["codec"], "sup");

    let create = client
        .post(format!("{base_url}/api/v1/playback/sessions"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf_cookie)
        .json(&json!({
            "itemId": item_id,
            "sourceId": source_id,
            "capabilities": {
                "directPlay": true,
                "hls": true,
                "videoCopyToFmp4": true,
                "audioCopyToFmp4": true,
                "softwareTranscode": true
            }
        }))
        .send()
        .await?;
    assert_eq!(create.status(), reqwest::StatusCode::OK);
    let body = create.json::<Value>().await?;
    assert_eq!(body["plan"]["type"], "DIRECT");
    let session_id = body["sessionId"].as_str().ok_or("missing session id")?;
    let direct_url = body["plan"]["url"].as_str().ok_or("missing direct url")?;
    assert!(!direct_url.contains("externalUrl"));

    let second_create = client
        .post(format!("{base_url}/api/v1/playback/sessions"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf_cookie)
        .json(&json!({
            "itemId": item_id,
            "sourceId": source_id,
            "capabilities": {
                "directPlay": true,
                "hls": true,
                "videoCopyToFmp4": true,
                "audioCopyToFmp4": true,
                "softwareTranscode": true
            }
        }))
        .send()
        .await?;
    assert_eq!(second_create.status(), reqwest::StatusCode::OK);
    let second_body = second_create.json::<Value>().await?;
    let second_direct_url = second_body["plan"]["url"]
        .as_str()
        .ok_or("missing second direct url")?;

    let admission_before_request: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM playback_admissions
         WHERE play_session_id = ? AND state = 'ACTIVE'",
    )
    .bind(format!("lux-web:{session_id}"))
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        admission_before_request, 0,
        "link issue must not reserve a slot"
    );

    let head = client
        .head(format!("{base_url}{direct_url}"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(head.status(), reqwest::StatusCode::OK);
    let admission_after_head: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM playback_admissions
         WHERE play_session_id = ? AND state = 'ACTIVE'",
    )
    .bind(format!("lux-web:{session_id}"))
    .fetch_one(database.pool())
    .await?;
    assert_eq!(admission_after_head, 0, "HEAD must not reserve a slot");

    let admin_id: String =
        sqlx::query_scalar("SELECT id FROM users WHERE username_normalized = 'admin'")
            .fetch_one(database.pool())
            .await?;
    sqlx::query("UPDATE users SET max_simultaneous_playbacks = 1 WHERE id = ?")
        .bind(&admin_id)
        .execute(database.pool())
        .await?;

    let direct = client
        .get(format!("{base_url}{direct_url}"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    let direct_status = direct.status();
    let direct_bytes = direct.bytes().await?;
    assert_eq!(
        direct_status,
        reqwest::StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&direct_bytes)
    );
    assert_eq!(direct_bytes.as_ref(), b"web-playback-bytes");
    let admission_after_request: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM playback_admissions
         WHERE play_session_id = ? AND state = 'ACTIVE'",
    )
    .bind(format!("lux-web:{session_id}"))
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        admission_after_request, 1,
        "actual media GET must reserve a slot"
    );

    let second_direct = client
        .get(format!("{base_url}{second_direct_url}"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(
        second_direct.status(),
        reqwest::StatusCode::TOO_MANY_REQUESTS
    );
    let second_error = second_direct.json::<Value>().await?;
    assert!(
        second_error["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("当前 1 路") && message.contains("上限 1 路"))
    );

    let repeated_direct = client
        .get(format!("{base_url}{direct_url}"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(repeated_direct.status(), reqwest::StatusCode::OK);
    let admission_after_repeat: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM playback_admissions
         WHERE user_id = ? AND state = 'ACTIVE'",
    )
    .bind(&admin_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(admission_after_repeat, 1, "same session reuses its slot");

    let tampered = direct_url.replacen("signature=", "signature=x", 1);
    let rejected = client.get(format!("{base_url}{tampered}")).send().await?;
    assert_eq!(rejected.status(), reqwest::StatusCode::NOT_FOUND);

    let event = |event_id: &str, sequence: i64, state: &str, position_ticks: i64| {
        client
            .post(format!(
                "{base_url}/api/v1/playback/sessions/{session_id}/events"
            ))
            .header(COOKIE, &cookies)
            .header("x-csrf-token", &csrf_cookie)
            .json(&json!({
                "eventId": event_id,
                "sequence": sequence,
                "state": state,
                "positionTicks": position_ticks,
                "durationTicks": 1_000
            }))
    };
    let accepted = event("event-1", 1, "PLAYING", 500).send().await?;
    assert_eq!(accepted.status(), reqwest::StatusCode::OK);
    assert_eq!(accepted.json::<Value>().await?["accepted"], true);
    let item_title: String = sqlx::query_scalar("SELECT title FROM media_items WHERE id = ?")
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?;
    let started_metadata: String = sqlx::query_scalar(
        "SELECT metadata_json FROM user_activity_events
         WHERE event_type = 'PLAYBACK_STARTED' AND target_id = ?
         ORDER BY occurred_at DESC LIMIT 1",
    )
    .bind(&item_id)
    .fetch_one(database.pool())
    .await?;
    let started_metadata: Value = serde_json::from_str(&started_metadata)?;
    assert_eq!(started_metadata["itemTitle"], json!(item_title));
    assert_eq!(webhook_service.process_ready_deliveries().await?, 1);
    let started: Value = serde_json::from_slice(
        &receiver
            .recv()
            .await
            .ok_or("missing web playback notification")?,
    )?;
    assert_eq!(started["eventType"], "PLAYBACK_STARTED");
    let (audit_total, audit_events) = audit_logs.list_audit_events(0, 10).await?;
    assert_eq!(audit_total, 2);
    assert!(
        audit_events
            .iter()
            .any(|event| event.event_type == "AUTH_LOGIN")
    );
    let playback_event = audit_events
        .iter()
        .find(|event| event.event_type == "PLAYBACK_STARTED")
        .ok_or("missing file-backed playback audit event")?;
    assert_eq!(playback_event.target_id.as_deref(), Some(item_id.as_str()));
    let indexed_playback_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM user_activity_events
         WHERE user_id = ? AND event_type = 'PLAYBACK_STARTED' AND target_id = ?",
    )
    .bind(&admin_id)
    .bind(&item_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(indexed_playback_count, 1);
    let duplicate = event("event-1", 1, "PLAYING", 100).send().await?;
    assert_eq!(duplicate.json::<Value>().await?["duplicate"], true);
    let stale = event("event-2", 0, "PAUSED", 0).send().await?;
    assert_eq!(stale.json::<Value>().await?["stale"], true);
    let paused = event("event-3", 2, "PAUSED", 600).send().await?;
    assert_eq!(paused.status(), reqwest::StatusCode::OK);
    assert_eq!(paused.json::<Value>().await?["accepted"], true);
    assert_eq!(webhook_service.process_ready_deliveries().await?, 1);
    let paused_notification: Value = serde_json::from_slice(
        &receiver
            .recv()
            .await
            .ok_or("missing Web playback pause notification")?,
    )?;
    assert_eq!(paused_notification["eventType"], "PLAYBACK_PAUSED");
    let session = sqlx::query_as::<_, (String, i64)>(
        "SELECT state, position_ticks
         FROM playback_sessions
         WHERE play_session_id = ?",
    )
    .bind(format!("lux-web:{session_id}"))
    .fetch_one(database.pool())
    .await?;
    assert_eq!(session.0, "PAUSED");
    assert_eq!(session.1, 600);
    let sequence: i64 =
        sqlx::query_scalar("SELECT last_sequence FROM web_playback_sessions WHERE id = ?")
            .bind(session_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(sequence, 2);
    let paused_admission: (String, i64) = sqlx::query_as(
        "SELECT state, COUNT(*) FROM playback_admissions
         WHERE user_id = ? AND state = 'ACTIVE' GROUP BY state",
    )
    .bind(&admin_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(paused_admission.0, "ACTIVE");
    assert_eq!(paused_admission.1, 1, "pause keeps its admission slot");

    sqlx::query(
        "UPDATE playback_admissions
         SET admitted_at = unixepoch() - 30, last_seen_at = unixepoch() - 30
         WHERE user_id = ? AND play_session_id = ?",
    )
    .bind(&admin_id)
    .bind(format!("lux-web:{session_id}"))
    .execute(database.pool())
    .await?;
    let heartbeat = client
        .post(format!(
            "{base_url}/api/v1/playback/sessions/{session_id}/heartbeat"
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf_cookie)
        .send()
        .await?;
    assert_eq!(heartbeat.status(), reqwest::StatusCode::OK);
    let renewed_at: i64 = sqlx::query_scalar(
        "SELECT last_seen_at FROM playback_admissions
         WHERE user_id = ? AND play_session_id = ?",
    )
    .bind(&admin_id)
    .bind(format!("lux-web:{session_id}"))
    .fetch_one(database.pool())
    .await?;
    assert!(renewed_at > 1, "active heartbeat renews the admission");

    sqlx::query(
        "UPDATE playback_admissions
         SET admitted_at = unixepoch() - 91, last_seen_at = unixepoch() - 91
         WHERE user_id = ? AND play_session_id = ?",
    )
    .bind(&admin_id)
    .bind(format!("lux-web:{session_id}"))
    .execute(database.pool())
    .await?;
    let stale_heartbeat = client
        .post(format!(
            "{base_url}/api/v1/playback/sessions/{session_id}/heartbeat"
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf_cookie)
        .send()
        .await?;
    assert_eq!(stale_heartbeat.status(), reqwest::StatusCode::OK);
    let stale_seen_at: i64 = sqlx::query_scalar(
        "SELECT last_seen_at FROM playback_admissions
         WHERE user_id = ? AND play_session_id = ?",
    )
    .bind(&admin_id)
    .bind(format!("lux-web:{session_id}"))
    .fetch_one(database.pool())
    .await?;
    let stale_cutoff: i64 = sqlx::query_scalar("SELECT unixepoch() - 90")
        .fetch_one(database.pool())
        .await?;
    assert!(
        stale_seen_at <= stale_cutoff,
        "an expired heartbeat must not recreate an admission"
    );

    let recovered_direct = client
        .get(format!("{base_url}{direct_url}"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(recovered_direct.status(), reqwest::StatusCode::OK);
    let stopped_event = event("event-4", 3, "STOPPED", 700).send().await?;
    assert_eq!(stopped_event.status(), reqwest::StatusCode::OK);
    assert_eq!(stopped_event.json::<Value>().await?["accepted"], true);
    assert_eq!(
        active_admission_count(&database, &admin_id, &format!("lux-web:{session_id}")).await?,
        0,
        "STOPPED callback releases the admission"
    );
    sqlx::query("UPDATE users SET max_simultaneous_playbacks = 0 WHERE id = ?")
        .bind(&admin_id)
        .execute(database.pool())
        .await?;

    tokio::fs::write(
        root.join("Remote Only Movie 2026.strm"),
        "http://media.example.test/media.mp4\n",
    )
    .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let (strm_item_id, strm_source_id): (String, String) = sqlx::query_as(
        "SELECT mi.id, ms.id
         FROM media_items mi
         JOIN media_sources ms ON ms.item_id = mi.id
         WHERE ms.source_kind = 'STRM_URL'
         LIMIT 1",
    )
    .fetch_one(database.pool())
    .await?;
    let emby_strm_item_id = emby_public_id(&strm_item_id);
    let strm_create = client
        .post(format!("{base_url}/api/v1/playback/sessions"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf_cookie)
        .json(&json!({
            "itemId": strm_item_id,
            "sourceId": strm_source_id,
            "capabilities": {
                "directPlay": false,
                "hls": true,
                "videoCopyToFmp4": true,
                "audioCopyToFmp4": true,
                "hardwareTranscode": true,
                "softwareTranscode": true
            }
        }))
        .send()
        .await?;
    assert_eq!(strm_create.status(), reqwest::StatusCode::OK);
    let strm_body = strm_create.json::<Value>().await?;
    assert_eq!(strm_body["plan"]["type"], "UNSUPPORTED");
    assert!(strm_body["sessionId"].is_null());
    let strm_session_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM web_playback_sessions WHERE plan = 'SERVER_HLS'")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(strm_session_count, 0);

    let strm_direct_create = client
        .post(format!("{base_url}/api/v1/playback/sessions"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf_cookie)
        .json(&json!({
            "itemId": strm_item_id,
            "sourceId": strm_source_id,
            "capabilities": {
                "directPlay": true,
                "hls": true,
                "videoCopyToFmp4": true,
                "audioCopyToFmp4": true,
                "softwareTranscode": true
            }
        }))
        .send()
        .await?;
    assert_eq!(strm_direct_create.status(), reqwest::StatusCode::OK);
    let strm_direct_body = strm_direct_create.json::<Value>().await?;
    assert_eq!(strm_direct_body["plan"]["type"], "DIRECT");
    assert_eq!(
        strm_direct_body["plan"]["proxyUrl"],
        format!(
            "/Videos/{emby_strm_item_id}/stream?MediaSourceId={}",
            public_source_id(&strm_source_id)
        )
    );
    assert!(strm_direct_body["sessionId"].is_string());
    let strm_kinds: Vec<&str> = strm_direct_body["plan"]["candidates"]
        .as_array()
        .ok_or("missing STRM candidates")?
        .iter()
        .filter_map(|candidate| candidate["kind"].as_str())
        .collect();
    assert_eq!(strm_kinds, ["proxy", "lux", "origin"]);
    let strm_direct_session_id = strm_direct_body["sessionId"]
        .as_str()
        .ok_or("missing direct STRM session id")?;
    let strm_range_url = strm_direct_body["plan"]["rangeUrl"]
        .as_str()
        .ok_or("missing STRM range url")?;
    let strm_play_session_id = strm_direct_body["playSessionId"]
        .as_str()
        .ok_or("missing STRM play session id")?;
    let strm_admission_before_range =
        active_admission_count(&database, &admin_id, strm_play_session_id).await?;
    assert_eq!(
        strm_admission_before_range, 0,
        "STRM link issue must not reserve a slot"
    );
    let strm_head = client
        .head(format!("{base_url}{strm_range_url}"))
        .header(COOKIE, &cookies)
        .header("Range", "bytes=0-3")
        .send()
        .await?;
    assert_eq!(strm_head.status(), reqwest::StatusCode::METHOD_NOT_ALLOWED);
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM strm_probe_jobs WHERE target_source_id = ?",
        )
        .bind(&strm_source_id)
        .fetch_one(database.pool())
        .await?,
        0,
        "HEAD must not enqueue a playback-triggered probe"
    );
    assert_eq!(
        active_admission_count(&database, &admin_id, strm_play_session_id).await?,
        0,
        "HEAD must not reserve a playback slot"
    );
    let strm_range = client
        .get(format!("{base_url}{strm_range_url}"))
        .header(COOKIE, &cookies)
        .header("Range", "bytes=0-3")
        .send()
        .await?;
    assert_eq!(strm_range.status(), reqwest::StatusCode::PARTIAL_CONTENT);
    assert_eq!(strm_range.bytes().await?.as_ref(), b"0123");
    let mut playback_probe_source = None;
    for _ in 0..100 {
        playback_probe_source = sqlx::query_scalar::<_, Option<String>>(
            "SELECT target_source_id FROM strm_probe_jobs
             WHERE target_source_id = ? ORDER BY created_at DESC LIMIT 1",
        )
        .bind(&strm_source_id)
        .fetch_optional(database.pool())
        .await?
        .flatten();
        if playback_probe_source.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        playback_probe_source.as_deref(),
        Some(strm_source_id.as_str())
    );
    assert_eq!(
        active_admission_count(&database, &admin_id, strm_play_session_id).await?,
        1,
        "first STRM Range request admits the session"
    );
    let repeated_strm_range = client
        .get(format!("{base_url}{strm_range_url}"))
        .header(COOKIE, &cookies)
        .header("Range", "bytes=0-3")
        .send()
        .await?;
    assert_eq!(
        repeated_strm_range.status(),
        reqwest::StatusCode::PARTIAL_CONTENT
    );
    assert_eq!(
        active_admission_count(&database, &admin_id, strm_play_session_id).await?,
        1,
        "subsequent STRM Range requests reuse the slot"
    );
    // Players re-request the same /direct URL (Range probes, moov seeks): a 307 is not a failure
    // and must not end the session.
    let strm_direct_url = strm_direct_body["plan"]["url"]
        .as_str()
        .ok_or("missing STRM direct url")?;
    let no_redirect_client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    for attempt in 0..2 {
        let redirect = no_redirect_client
            .get(format!("{base_url}{strm_direct_url}"))
            .send()
            .await?;
        assert_eq!(
            redirect.status(),
            reqwest::StatusCode::TEMPORARY_REDIRECT,
            "STRM /direct GET #{attempt} must keep redirecting"
        );
    }
    let strm_event = client
        .post(format!(
            "{base_url}/api/v1/playback/sessions/{strm_direct_session_id}/events"
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf_cookie)
        .json(&json!({
            "eventId": "strm-event-1",
            "sequence": 1,
            "state": "PLAYING",
            "positionTicks": 0,
            "durationTicks": 1_000
        }))
        .send()
        .await?;
    assert_eq!(strm_event.status(), reqwest::StatusCode::OK);
    assert_eq!(strm_event.json::<Value>().await?["accepted"], true);
    assert_eq!(webhook_service.process_ready_deliveries().await?, 2);
    // Deliveries are dispatched by a concurrent worker pool (DELIVERY_CONCURRENCY), so the two
    // notifications may arrive in either order; assert on the set instead of the sequence.
    let mut delivered = Vec::new();
    for _ in 0..2 {
        delivered.push(serde_json::from_slice::<Value>(
            &receiver
                .recv()
                .await
                .ok_or("missing playback notification")?,
        )?);
    }
    let web_stopped = delivered
        .iter()
        .find(|event| event["eventType"] == "PLAYBACK_STOPPED")
        .ok_or("missing Web playback stop notification")?;
    assert_eq!(web_stopped["eventType"], "PLAYBACK_STOPPED");
    let strm_started = delivered
        .iter()
        .find(|event| event["eventType"] == "PLAYBACK_STARTED")
        .ok_or("missing STRM playback notification")?;
    assert_eq!(strm_started["playMethod"], "DirectStream");
    let failed_strm_range = client
        .get(format!("{base_url}{strm_range_url}"))
        .header(COOKIE, &cookies)
        .header("Range", "bytes=4-7")
        .send()
        .await?;
    assert_eq!(failed_strm_range.status(), reqwest::StatusCode::BAD_GATEWAY);
    let failed_strm_admission: String = sqlx::query_scalar(
        "SELECT state FROM playback_admissions
         WHERE user_id = ? AND play_session_id = ?",
    )
    .bind(&admin_id)
    .bind(strm_play_session_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(failed_strm_admission, "RELEASED");
    let strm_direct_stopped = client
        .delete(format!(
            "{base_url}/api/v1/playback/sessions/{strm_direct_session_id}"
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf_cookie)
        .send()
        .await?;
    assert_eq!(
        strm_direct_stopped.status(),
        reqwest::StatusCode::NO_CONTENT
    );

    tokio::fs::write(
        root.join("Proxy Path Movie 2027.strm"),
        "/CloudNAS/115-122/media-AV/日本/episode.mp4\n",
    )
    .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let (proxy_item_id, proxy_source_id): (String, String) = sqlx::query_as(
        "SELECT mi.id, ms.id
         FROM media_items mi
         JOIN media_sources ms ON ms.item_id = mi.id
         WHERE mi.title = 'Proxy Path Movie' AND ms.source_kind = 'STRM_URL'",
    )
    .fetch_one(database.pool())
    .await?;
    let emby_proxy_item_id = emby_public_id(&proxy_item_id);
    let proxy_create = client
        .post(format!("{base_url}/api/v1/playback/sessions"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf_cookie)
        .json(&json!({
            "itemId": proxy_item_id,
            "sourceId": proxy_source_id,
            "capabilities": {
                "directPlay": true,
                "hls": true,
                "videoCopyToFmp4": true,
                "audioCopyToFmp4": true,
                "softwareTranscode": true
            }
        }))
        .send()
        .await?;
    assert_eq!(proxy_create.status(), reqwest::StatusCode::OK);
    let proxy_body = proxy_create.json::<Value>().await?;
    assert_eq!(
        proxy_body["plan"]["proxyUrl"],
        format!(
            "/Videos/{emby_proxy_item_id}/stream?MediaSourceId={}",
            public_source_id(&proxy_source_id)
        )
    );

    // Direct plans also carry the ordered candidate ladder. A path-type STRM has no origin URL.
    let candidates = proxy_body["plan"]["candidates"]
        .as_array()
        .ok_or("missing candidates")?;
    let kinds: Vec<&str> = candidates
        .iter()
        .filter_map(|candidate| candidate["kind"].as_str())
        .collect();
    assert_eq!(kinds, ["proxy", "lux"]);
    assert_eq!(candidates[1]["url"], proxy_body["plan"]["url"]);
    let proxy_candidate = candidates[0]["url"]
        .as_str()
        .ok_or("missing proxy candidate url")?;
    assert!(
        proxy_candidate.starts_with(&format!(
            "/Videos/{emby_proxy_item_id}/stream?MediaSourceId={}&",
            public_source_id(&proxy_source_id)
        )),
        "{proxy_candidate}"
    );
    let play_session_id = proxy_body["playSessionId"]
        .as_str()
        .ok_or("missing play session id")?;
    assert!(proxy_candidate.contains(&format!(
        "PlaySessionId={}",
        play_session_id.replace(':', "%3A")
    )));
    let proxy_token = proxy_candidate
        .split('&')
        .find_map(|part| part.strip_prefix("api_key="))
        .ok_or("proxy candidate has no api_key")?;
    assert!(proxy_token.starts_with(&format!("luxpb_{emby_proxy_item_id}_")));
    // The token is what a 302 proxy sends back to resolve the item: it works for this item only.
    let own_item = client
        .get(format!("{base_url}/emby/Items/{emby_proxy_item_id}"))
        .query(&[("api_key", proxy_token)])
        .send()
        .await?;
    assert_eq!(own_item.status(), reqwest::StatusCode::OK);
    let other_item = client
        .get(format!("{base_url}/emby/Items/{emby_strm_item_id}"))
        .query(&[("api_key", proxy_token)])
        .send()
        .await?;
    assert_eq!(other_item.status(), reqwest::StatusCode::UNAUTHORIZED);
    let proxy_session_id = proxy_body["sessionId"]
        .as_str()
        .ok_or("missing proxy session id")?;
    let stop_proxy = client
        .delete(format!(
            "{base_url}/api/v1/playback/sessions/{proxy_session_id}"
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf_cookie)
        .send()
        .await?;
    assert_eq!(stop_proxy.status(), reqwest::StatusCode::NO_CONTENT);
    let after_stop = client
        .get(format!("{base_url}/emby/Items/{emby_proxy_item_id}"))
        .query(&[("api_key", proxy_token)])
        .send()
        .await?;
    assert_eq!(after_stop.status(), reqwest::StatusCode::UNAUTHORIZED);

    let stopped = client
        .delete(format!("{base_url}/api/v1/playback/sessions/{session_id}"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf_cookie)
        .send()
        .await?;
    assert_eq!(stopped.status(), reqwest::StatusCode::NO_CONTENT);
    let stopped_admission: (String, String) = sqlx::query_as(
        "SELECT admission.state, playback.state
         FROM playback_admissions AS admission
         JOIN playback_sessions AS playback
           ON playback.play_session_id = admission.play_session_id
         WHERE admission.user_id = ? AND admission.play_session_id = ?",
    )
    .bind(&admin_id)
    .bind(format!("lux-web:{session_id}"))
    .fetch_one(database.pool())
    .await?;
    assert_eq!(stopped_admission.0, "RELEASED");
    assert_eq!(stopped_admission.1, "STOPPED");
    let after_stop = client.get(format!("{base_url}{direct_url}")).send().await?;
    assert_eq!(after_stop.status(), reqwest::StatusCode::GONE);

    sqlx::query("UPDATE users SET max_simultaneous_playbacks = 1 WHERE id = ?")
        .bind(&admin_id)
        .execute(database.pool())
        .await?;
    sqlx::query(
        "UPDATE users SET allow_remux = 0, allow_audio_transcode = 1,
             allow_video_transcode = 0 WHERE id = ?",
    )
    .bind(&admin_id)
    .execute(database.pool())
    .await?;
    let hls_create = client
        .post(format!("{base_url}/api/v1/playback/sessions"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf_cookie)
        .json(&json!({
            "itemId": item_id,
            "sourceId": source_id,
            "capabilities": {
                "directPlay": false,
                "hls": true,
                "videoCopyToFmp4": true,
                "audioCopyToFmp4": true,
                "softwareTranscode": true
            }
        }))
        .send()
        .await?;
    assert_eq!(hls_create.status(), reqwest::StatusCode::OK);
    let hls_body = hls_create.json::<Value>().await?;
    assert_eq!(hls_body["plan"]["type"], "SERVER_HLS");
    assert_eq!(
        hls_body["tier"], 2,
        "audio transcode is allowed when Remux is denied"
    );
    let hls_session_id = hls_body["sessionId"]
        .as_str()
        .ok_or("missing HLS session id")?;
    let hls_play_session_id = format!("lux-web:{hls_session_id}");
    let manifest_url = hls_body["plan"]["manifestUrl"]
        .as_str()
        .ok_or("missing HLS manifest url")?;
    assert_eq!(
        active_admission_count(&database, &admin_id, &hls_play_session_id).await?,
        0,
        "HLS link issue is only a precheck"
    );
    let hls_head = client
        .head(format!("{base_url}{manifest_url}"))
        .send()
        .await?;
    assert_eq!(hls_head.status(), reqwest::StatusCode::OK);
    assert_eq!(
        active_admission_count(&database, &admin_id, &hls_play_session_id).await?,
        0,
        "HLS HEAD must not reserve a slot"
    );
    let manifest = client
        .get(format!("{base_url}{manifest_url}"))
        .send()
        .await?;
    assert_eq!(manifest.status(), reqwest::StatusCode::OK);
    let manifest = manifest.text().await?;
    assert_eq!(
        active_admission_count(&database, &admin_id, &hls_play_session_id).await?,
        1,
        "HLS manifest GET admits the session"
    );
    let segment_url = manifest
        .lines()
        .find(|line| line.contains("segment_000000.m4s"))
        .ok_or("missing rewritten HLS segment url")?;
    let segment = client
        .get(format!("{base_url}{segment_url}"))
        .send()
        .await?;
    assert_eq!(segment.status(), reqwest::StatusCode::OK);
    assert_eq!(segment.bytes().await?.as_ref(), b"segment");
    assert_eq!(
        active_admission_count(&database, &admin_id, &hls_play_session_id).await?,
        1,
        "HLS segment reuses the same slot"
    );
    sqlx::query("UPDATE users SET allow_audio_transcode = 0 WHERE id = ?")
        .bind(&admin_id)
        .execute(database.pool())
        .await?;
    let revoked_hls_head = client
        .head(format!("{base_url}{segment_url}"))
        .send()
        .await?;
    assert_eq!(revoked_hls_head.status(), reqwest::StatusCode::FORBIDDEN);
    let revoked_hls_segment = client
        .get(format!("{base_url}{segment_url}"))
        .send()
        .await?;
    assert_eq!(revoked_hls_segment.status(), reqwest::StatusCode::GONE);
    let revoked_hls_state: String =
        sqlx::query_scalar("SELECT state FROM web_playback_sessions WHERE id = ?")
            .bind(hls_session_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(revoked_hls_state, "STOPPED");
    assert_eq!(
        active_admission_count(&database, &admin_id, &hls_play_session_id).await?,
        0,
        "revoking the HLS tier releases its admission"
    );
    assert!(
        !temp_dir
            .path()
            .join("config/web-playback")
            .join(hls_session_id)
            .exists(),
        "revoking the HLS tier removes temporary resources"
    );
    let hls_stopped = client
        .delete(format!(
            "{base_url}/api/v1/playback/sessions/{hls_session_id}"
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf_cookie)
        .send()
        .await?;
    assert_eq!(hls_stopped.status(), reqwest::StatusCode::NO_CONTENT);
    let hls_admission_state: String = sqlx::query_scalar(
        "SELECT state FROM playback_admissions
         WHERE user_id = ? AND play_session_id = ?",
    )
    .bind(&admin_id)
    .bind(&hls_play_session_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(hls_admission_state, "RELEASED");

    sqlx::query(
        "UPDATE users SET allow_remux = 0, allow_audio_transcode = 0,
             allow_video_transcode = 0 WHERE id = ?",
    )
    .bind(&admin_id)
    .execute(database.pool())
    .await?;
    let denied_hls = client
        .post(format!("{base_url}/api/v1/playback/sessions"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf_cookie)
        .json(&json!({
            "itemId": item_id,
            "sourceId": source_id,
            "capabilities": {
                "directPlay": false,
                "hls": true,
                "videoCopyToFmp4": true,
                "audioCopyToFmp4": true,
                "hardwareTranscode": true,
                "softwareTranscode": true
            }
        }))
        .send()
        .await?;
    assert_eq!(denied_hls.status(), reqwest::StatusCode::FORBIDDEN);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM web_playback_sessions WHERE plan = 'SERVER_HLS' AND state = 'ACTIVE'",
        )
        .fetch_one(database.pool())
        .await?,
        0,
        "a policy-denied Web request must not leave an HLS session"
    );

    sqlx::query("UPDATE users SET can_play = 0 WHERE id = ?")
        .bind(&admin_id)
        .execute(database.pool())
        .await?;
    let disabled_create = client
        .post(format!("{base_url}/api/v1/playback/sessions"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf_cookie)
        .json(&json!({
            "itemId": item_id,
            "sourceId": source_id,
            "capabilities": {
                "directPlay": true,
                "hls": true,
                "videoCopyToFmp4": true,
                "audioCopyToFmp4": true,
                "softwareTranscode": true
            }
        }))
        .send()
        .await?;
    assert_eq!(disabled_create.status(), reqwest::StatusCode::FORBIDDEN);
    let disabled_error = disabled_create.json::<Value>().await?;
    assert!(
        disabled_error["error"]["message"]
            .as_str()
            .is_some_and(
                |message| message.contains("当前 0 路") && message.contains("策略上限 1 路")
            )
    );

    sqlx::query("UPDATE users SET can_play = 1, max_simultaneous_playbacks = 0 WHERE id = ?")
        .bind(&admin_id)
        .execute(database.pool())
        .await?;
    let device_ids = (0..6)
        .map(|_| uuid::Uuid::new_v4().to_string())
        .collect::<Vec<_>>();
    let mut tickets = Vec::new();
    for device_id in &device_ids {
        tickets.push(
            create_direct_ticket(
                &client,
                &base_url,
                &cookies,
                &csrf_cookie,
                &item_id,
                &source_id,
                Some(device_id),
            )
            .await?,
        );
    }
    for (index, (session_id, _)) in tickets.iter().enumerate() {
        let access_context: (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT authorization_device_id, authorization_client_name
             FROM web_playback_sessions WHERE id = ?",
        )
        .bind(session_id)
        .fetch_one(database.pool())
        .await?;
        assert_eq!(
            access_context.0.as_deref(),
            Some(device_ids[index].as_str())
        );
        assert_eq!(access_context.1.as_deref(), Some("Lux Web"));
    }
    let missing_device_ticket = create_direct_ticket(
        &client,
        &base_url,
        &cookies,
        &csrf_cookie,
        &item_id,
        &source_id,
        None,
    )
    .await?;
    let missing_client_ticket = create_direct_ticket(
        &client,
        &base_url,
        &cookies,
        &csrf_cookie,
        &item_id,
        &source_id,
        Some(&uuid::Uuid::new_v4().to_string()),
    )
    .await?;
    sqlx::query(
        "UPDATE web_playback_sessions SET authorization_device_id = NULL
         WHERE id = ?",
    )
    .bind(&missing_device_ticket.0)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "UPDATE web_playback_sessions
         SET authorization_device_id = NULL, authorization_client_name = NULL
         WHERE id = ?",
    )
    .bind(&missing_client_ticket.0)
    .execute(database.pool())
    .await?;

    insert_global_access_rule(
        &database,
        "web-playback-device-rule",
        "DEVICE",
        &device_ids[0],
        &admin_id,
    )
    .await?;
    let blocked_device = client
        .get(format!("{base_url}{}", tickets[0].1))
        .send()
        .await?;
    assert_eq!(blocked_device.status(), reqwest::StatusCode::GONE);
    let blocked_device_state: String =
        sqlx::query_scalar("SELECT state FROM web_playback_sessions WHERE id = ?")
            .bind(&tickets[0].0)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(blocked_device_state, "STOPPED");
    sqlx::query("DELETE FROM user_access_blacklist_rules WHERE id = ?")
        .bind("web-playback-device-rule")
        .execute(database.pool())
        .await?;

    insert_global_access_rule(
        &database,
        "web-playback-unknown-device-rule",
        "DEVICE",
        "unknown-device-that-cannot-match",
        &admin_id,
    )
    .await?;
    let unknown_device = client
        .get(format!("{base_url}{}", missing_device_ticket.1))
        .send()
        .await?;
    assert_eq!(unknown_device.status(), reqwest::StatusCode::GONE);
    sqlx::query("DELETE FROM user_access_blacklist_rules WHERE id = ?")
        .bind("web-playback-unknown-device-rule")
        .execute(database.pool())
        .await?;

    insert_global_access_rule(
        &database,
        "web-playback-client-rule",
        "CLIENT",
        "lux web",
        &admin_id,
    )
    .await?;
    let blocked_client = client
        .get(format!("{base_url}{}", tickets[1].1))
        .send()
        .await?;
    assert_eq!(blocked_client.status(), reqwest::StatusCode::GONE);
    let unknown_client = client
        .get(format!("{base_url}{}", missing_client_ticket.1))
        .send()
        .await?;
    assert_eq!(unknown_client.status(), reqwest::StatusCode::GONE);
    sqlx::query("DELETE FROM user_access_blacklist_rules WHERE id = ?")
        .bind("web-playback-client-rule")
        .execute(database.pool())
        .await?;

    insert_global_access_rule(
        &database,
        "web-playback-ip-rule",
        "IP_CIDR",
        "127.0.0.1/32",
        &admin_id,
    )
    .await?;
    let blocked_ip = peer_client
        .get(format!("{peer_base_url}{}", tickets[2].1))
        .send()
        .await?;
    assert_eq!(blocked_ip.status(), reqwest::StatusCode::GONE);
    sqlx::query("DELETE FROM user_access_blacklist_rules WHERE id = ?")
        .bind("web-playback-ip-rule")
        .execute(database.pool())
        .await?;

    insert_global_access_rule(
        &database,
        "web-playback-missing-peer-rule",
        "IP_CIDR",
        "0.0.0.0/0",
        &admin_id,
    )
    .await?;
    let missing_peer = client
        .get(format!("{base_url}{}", tickets[3].1))
        .send()
        .await?;
    assert_eq!(
        missing_peer.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE
    );
    sqlx::query("DELETE FROM user_access_blacklist_rules WHERE id = ?")
        .bind("web-playback-missing-peer-rule")
        .execute(database.pool())
        .await?;

    insert_global_access_rule(
        &database,
        "web-playback-corrupt-ip-rule",
        "IP_CIDR",
        "not-a-cidr",
        &admin_id,
    )
    .await?;
    let corrupt_rule = peer_client
        .get(format!("{peer_base_url}{}", tickets[4].1))
        .send()
        .await?;
    assert_eq!(
        corrupt_rule.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE
    );
    sqlx::query("DELETE FROM user_access_blacklist_rules WHERE id = ?")
        .bind("web-playback-corrupt-ip-rule")
        .execute(database.pool())
        .await?;

    for index in 0..201_u32 {
        let address = std::net::Ipv4Addr::from(0x0a00_0000_u32 + index);
        insert_global_access_rule(
            &database,
            &format!("web-playback-overflow-ip-rule-{index}"),
            "IP_CIDR",
            &format!("{address}/32"),
            &admin_id,
        )
        .await?;
    }
    let overflow_rules = peer_client
        .get(format!("{peer_base_url}{}", tickets[5].1))
        .send()
        .await?;
    assert_eq!(
        overflow_rules.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE
    );
    sqlx::query(
        "DELETE FROM user_access_blacklist_rules
         WHERE id LIKE 'web-playback-overflow-ip-rule-%'",
    )
    .execute(database.pool())
    .await?;

    sqlx::query(
        "UPDATE users SET can_play = 1, max_simultaneous_playbacks = 0,
             allow_remux = 0, allow_audio_transcode = 1, allow_video_transcode = 0
         WHERE id = ?",
    )
    .bind(&admin_id)
    .execute(database.pool())
    .await?;
    let hls_device_id = uuid::Uuid::new_v4().to_string();
    let hls_ticket = client
        .post(format!("{base_url}/api/v1/playback/sessions"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf_cookie)
        .header("x-lux-device-id", &hls_device_id)
        .json(&json!({
            "itemId": item_id,
            "sourceId": source_id,
            "capabilities": {
                "directPlay": false,
                "hls": true,
                "videoCopyToFmp4": true,
                "audioCopyToFmp4": true,
                "softwareTranscode": true
            }
        }))
        .send()
        .await?;
    assert_eq!(hls_ticket.status(), reqwest::StatusCode::OK);
    let hls_ticket = hls_ticket.json::<Value>().await?;
    let hls_ticket_id = hls_ticket["sessionId"]
        .as_str()
        .ok_or("missing blacklisted HLS session id")?;
    let hls_manifest_url = hls_ticket["plan"]["manifestUrl"]
        .as_str()
        .ok_or("missing blacklisted HLS manifest URL")?;
    insert_global_access_rule(
        &database,
        "web-playback-hls-client-rule",
        "CLIENT",
        "lux web",
        &admin_id,
    )
    .await?;
    let blocked_hls = peer_client
        .get(format!("{peer_base_url}{hls_manifest_url}"))
        .send()
        .await?;
    assert_eq!(blocked_hls.status(), reqwest::StatusCode::GONE);
    let blocked_hls_state: String =
        sqlx::query_scalar("SELECT state FROM web_playback_sessions WHERE id = ?")
            .bind(hls_ticket_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(blocked_hls_state, "STOPPED");
    sqlx::query("DELETE FROM user_access_blacklist_rules WHERE id = ?")
        .bind("web-playback-hls-client-rule")
        .execute(database.pool())
        .await?;

    let range_device_id = uuid::Uuid::new_v4().to_string();
    let range_ticket = client
        .post(format!("{base_url}/api/v1/playback/sessions"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf_cookie)
        .header("x-lux-device-id", &range_device_id)
        .json(&json!({
            "itemId": strm_item_id,
            "sourceId": strm_source_id,
            "capabilities": {
                "directPlay": true,
                "hls": true,
                "videoCopyToFmp4": true,
                "audioCopyToFmp4": true,
                "softwareTranscode": true
            }
        }))
        .send()
        .await?;
    assert_eq!(range_ticket.status(), reqwest::StatusCode::OK);
    let range_ticket = range_ticket.json::<Value>().await?;
    let range_session_id = range_ticket["sessionId"]
        .as_str()
        .ok_or("missing blacklisted range session id")?;
    let range_url = range_ticket["plan"]["rangeUrl"]
        .as_str()
        .ok_or("missing blacklisted range URL")?;
    insert_global_access_rule(
        &database,
        "web-playback-range-client-rule",
        "CLIENT",
        "lux web",
        &admin_id,
    )
    .await?;
    let blocked_range = peer_client
        .get(format!("{peer_base_url}{range_url}"))
        .header("Range", "bytes=0-3")
        .send()
        .await?;
    assert_eq!(blocked_range.status(), reqwest::StatusCode::GONE);
    let blocked_range_state: String =
        sqlx::query_scalar("SELECT state FROM web_playback_sessions WHERE id = ?")
            .bind(range_session_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(blocked_range_state, "STOPPED");
    sqlx::query("DELETE FROM user_access_blacklist_rules WHERE id = ?")
        .bind("web-playback-range-client-rule")
        .execute(database.pool())
        .await?;

    let bound_device_id = uuid::Uuid::new_v4().to_string();
    let bound_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .header("x-lux-device-id", &bound_device_id)
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    assert_eq!(bound_login.status(), reqwest::StatusCode::OK);
    let bound_session_cookie =
        cookie_value(bound_login.headers(), "lux_session").ok_or("missing bound session")?;
    let bound_csrf_cookie =
        cookie_value(bound_login.headers(), "lux_csrf").ok_or("missing bound csrf")?;
    let bound_cookies = format!("lux_session={bound_session_cookie}; lux_csrf={bound_csrf_cookie}");
    let spoofed_device_id = uuid::Uuid::new_v4().to_string();
    let bound_ticket = create_direct_ticket(
        &client,
        &base_url,
        &bound_cookies,
        &bound_csrf_cookie,
        &item_id,
        &source_id,
        Some(&spoofed_device_id),
    )
    .await?;
    let bound_context: (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT authorization_device_id, authorization_client_name
         FROM web_playback_sessions WHERE id = ?",
    )
    .bind(&bound_ticket.0)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(bound_context.0.as_deref(), Some(bound_device_id.as_str()));
    assert_eq!(bound_context.1.as_deref(), Some("Lux Web"));
    insert_global_access_rule(
        &database,
        "web-playback-bound-device-rule",
        "DEVICE",
        &bound_device_id,
        &admin_id,
    )
    .await?;
    let bound_device_rejected = client
        .get(format!("{base_url}{}", bound_ticket.1))
        .send()
        .await?;
    assert_eq!(bound_device_rejected.status(), reqwest::StatusCode::GONE);
    sqlx::query("DELETE FROM user_access_blacklist_rules WHERE id = ?")
        .bind("web-playback-bound-device-rule")
        .execute(database.pool())
        .await?;

    let proxy_context_ticket = create_direct_ticket(
        &client,
        &base_url,
        &cookies,
        &csrf_cookie,
        &item_id,
        &source_id,
        Some(&uuid::Uuid::new_v4().to_string()),
    )
    .await?;
    database
        .set_trusted_proxy_cidrs_json("invalid-json")
        .await?;
    // Without a forwarded chain the peer is the client and the trusted-proxy settings are never
    // read. Send a chain so the resolver must consult the (now invalid) configuration.
    let proxy_settings_error = peer_client
        .get(format!("{peer_base_url}{}", proxy_context_ticket.1))
        .header("x-forwarded-for", "198.51.100.7")
        .send()
        .await?;
    assert_eq!(
        proxy_settings_error.status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE
    );

    server.abort();
    peer_server.abort();
    receiver_server.abort();
    strm_proxy.await?;
    Ok(())
}

#[tokio::test]
async fn web_playback_stats_report_the_plan_and_live_transcode_progress()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Stats Playback Movie 2026.mp4"), b"stats-bytes").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE'")
            .fetch_one(database.pool())
            .await?;
    let source_id: String = sqlx::query_scalar("SELECT id FROM media_sources WHERE item_id = ?")
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?;

    let fake_ffmpeg = temp_dir.path().join("fake-ffmpeg");
    let fake_ffmpeg_source = [
        "#!/bin/sh",
        "set -eu",
        "manifest=\"\"",
        "segment=\"\"",
        "init=\"init.mp4\"",
        "while [ \"$#\" -gt 0 ]; do",
        "  case \"$1\" in",
        "    -hls_segment_filename) segment=\"$2\"; shift 2 ;;",
        "    -hls_fmp4_init_filename) init=\"$2\"; shift 2 ;;",
        "    *.m3u8) manifest=\"$1\"; shift ;;",
        "    *) shift ;;",
        "  esac",
        "done",
        "directory=$(dirname \"$manifest\")",
        "mkdir -p \"$directory\"",
        "printf '#EXTM3U\\n#EXT-X-MAP:URI=\"%s\"\\n#EXTINF:1,\\nsegment_000000.m4s\\n' \"$init\" > \"$manifest\"",
        "printf init > \"$directory/$init\"",
        "printf segment > \"$(printf '%s' \"$segment\" | sed 's/%06d/000000/')\"",
        "printf 'frame=240\\nfps=24.00\\nbitrate=2048.5kbits/s\\ntotal_size=1048576\\n'",
        "printf 'out_time_us=10000000\\nspeed=2.00x\\ndup_frames=0\\ndrop_frames=3\\n'",
        "printf 'progress=continue\\n'",
        "sleep 2",
    ]
    .join("\n");
    tokio::fs::write(&fake_ffmpeg, fake_ffmpeg_source).await?;
    let mut permissions = tokio::fs::metadata(&fake_ffmpeg).await?.permissions();
    permissions.set_mode(0o700);
    tokio::fs::set_permissions(&fake_ffmpeg, permissions).await?;

    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let state = AppState::ready(config, database.clone(), setup, auth, emby_auth)
        .with_hls_executable(fake_ffmpeg);
    let app = app_with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");

    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let session_cookie = cookie_value(login.headers(), "lux_session").ok_or("missing session")?;
    let csrf_cookie = cookie_value(login.headers(), "lux_csrf").ok_or("missing csrf")?;
    let cookies = format!("lux_session={session_cookie}; lux_csrf={csrf_cookie}");

    let direct = client
        .post(format!("{base_url}/api/v1/playback/sessions"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf_cookie)
        .json(&json!({
            "itemId": item_id,
            "sourceId": source_id,
            "capabilities": {
                "directPlay": true,
                "hls": true,
                "softwareTranscode": true
            }
        }))
        .send()
        .await?;
    assert_eq!(direct.status(), reqwest::StatusCode::OK);
    let direct = direct.json::<Value>().await?;
    let direct_id = direct["sessionId"]
        .as_str()
        .ok_or("missing direct session id")?;
    let direct_stats = client
        .get(format!(
            "{base_url}/api/v1/playback/sessions/{direct_id}/stats"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(direct_stats.status(), reqwest::StatusCode::OK);
    let direct_stats = direct_stats.json::<Value>().await?;
    assert_eq!(direct_stats["plan"], json!("DIRECT"));
    assert_eq!(direct_stats["tier"], json!(0));
    assert_eq!(direct_stats["output"]["container"], Value::Null);
    assert_eq!(direct_stats["progress"], Value::Null);

    let transcode = client
        .post(format!("{base_url}/api/v1/playback/sessions"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf_cookie)
        .json(&json!({
            "itemId": item_id,
            "sourceId": source_id,
            "capabilities": {
                "directPlay": false,
                "hls": true,
                "videoCopyToFmp4": false,
                "audioCopyToFmp4": false,
                "softwareTranscode": true
            }
        }))
        .send()
        .await?;
    assert_eq!(transcode.status(), reqwest::StatusCode::OK);
    let transcode = transcode.json::<Value>().await?;
    assert_eq!(transcode["plan"]["type"], json!("SERVER_HLS"));
    assert_eq!(transcode["plan"]["tier"], json!(4));
    let transcode_id = transcode["sessionId"]
        .as_str()
        .ok_or("missing transcode session id")?;

    let mut stats = Value::Null;
    for _ in 0..100 {
        let response = client
            .get(format!(
                "{base_url}/api/v1/playback/sessions/{transcode_id}/stats"
            ))
            .header(COOKIE, &cookies)
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        stats = response.json::<Value>().await?;
        if stats["progress"].is_object() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(stats["plan"], json!("SERVER_HLS"));
    assert_eq!(stats["tier"], json!(4));
    assert_eq!(stats["state"], json!("ACTIVE"));
    assert_eq!(stats["output"]["container"], json!("mp4"));
    assert_eq!(stats["output"]["videoCodec"], json!("h264"));
    assert_eq!(stats["output"]["audioCodec"], json!("aac"));
    assert_eq!(stats["output"]["audioBitrate"], json!(192_000));
    assert_eq!(stats["progress"]["fps"], json!(24.0));
    assert_eq!(stats["progress"]["bitrateKbps"], json!(2048.5));
    assert_eq!(stats["progress"]["totalSize"], json!(1_048_576));
    assert_eq!(stats["progress"]["outTimeSeconds"], json!(10.0));
    assert_eq!(stats["progress"]["speed"], json!(2.0));
    assert_eq!(stats["progress"]["droppedFrames"], json!(3));
    assert_eq!(stats["progress"]["finished"], json!(false));
    // Real HLS muxing reports `bitrate=N/A`, so the server also measures the newest segment.
    assert!(
        stats["progress"]["latestSegmentBitrateKbps"]
            .as_f64()
            .is_some_and(|kbps| kbps > 0.0),
        "segment bitrate must be measured from the segment file: {stats}"
    );
    assert!(
        stats["progress"]["directoryBytes"]
            .as_u64()
            .is_some_and(|bytes| bytes > 0),
        "session output size must be measured: {stats}"
    );
    assert!(
        stats["progress"]["averageBitrateKbps"]
            .as_f64()
            .is_some_and(|kbps| kbps > 0.0)
    );

    let listing = client
        .get(format!("{base_url}/api/v1/admin/transcoding/sessions"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(listing.status(), reqwest::StatusCode::OK);
    let listing = listing.json::<Value>().await?;
    assert!(
        listing["summary"]["activeCount"]
            .as_i64()
            .is_some_and(|count| count >= 1),
        "{listing}"
    );
    let listed = listing["sessions"]
        .as_array()
        .and_then(|sessions| {
            sessions
                .iter()
                .find(|session| session["sessionId"] == json!(transcode_id))
        })
        .ok_or("active transcoding session missing from the admin listing")?;
    assert_eq!(listed["tier"], json!(4));
    assert_eq!(listed["userName"], json!("Admin"));
    assert_eq!(listed["title"], json!("Stats Playback Movie"));
    assert_eq!(listed["progress"]["fps"], json!(24.0));
    assert_eq!(listed["progress"]["latestSegmentBitrateKbps"], json!(0.014));
    assert_eq!(listing["summary"]["softwareCount"], json!(1));
    assert_eq!(listing["summary"]["hardwareCount"], json!(0));
    assert_eq!(listing["summary"]["streamCopyCount"], json!(0));

    let stop_without_csrf = client
        .delete(format!(
            "{base_url}/api/v1/admin/transcoding/sessions/{transcode_id}"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(stop_without_csrf.status(), reqwest::StatusCode::FORBIDDEN);
    let stopped = client
        .delete(format!(
            "{base_url}/api/v1/admin/transcoding/sessions/{transcode_id}"
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf_cookie)
        .send()
        .await?;
    assert_eq!(stopped.status(), reqwest::StatusCode::NO_CONTENT);
    let second_stop = client
        .delete(format!(
            "{base_url}/api/v1/admin/transcoding/sessions/{transcode_id}"
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf_cookie)
        .send()
        .await?;
    assert_eq!(second_stop.status(), reqwest::StatusCode::NOT_FOUND);
    let after_stop = client
        .get(format!("{base_url}/api/v1/admin/transcoding/sessions"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(after_stop.status(), reqwest::StatusCode::OK);
    let after_stop = after_stop.json::<Value>().await?;
    assert!(
        !after_stop["sessions"]
            .as_array()
            .is_some_and(|sessions| sessions
                .iter()
                .any(|session| session["sessionId"] == json!(transcode_id))),
        "a stopped session must leave the live list: {after_stop}"
    );

    let history = client
        .get(format!(
            "{base_url}/api/v1/admin/transcoding/history?page=1&pageSize=10"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(history.status(), reqwest::StatusCode::OK);
    let history = history.json::<Value>().await?;
    assert!(history["total"].as_i64().is_some_and(|total| total >= 1));
    let entry = history["items"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| item["sessionId"] == json!(transcode_id))
        })
        .ok_or("stopped session missing from the transcoding history")?;
    assert_eq!(entry["outcome"], json!("COMPLETED"));
    assert_eq!(entry["tier"], json!(4));
    assert_eq!(entry["title"], json!("Stats Playback Movie"));
    assert_eq!(entry["userName"], json!("Admin"));
    assert!(
        entry["averageSpeed"]
            .as_f64()
            .is_some_and(|speed| speed > 0.0)
    );
    assert!(entry["outputBytes"].as_i64().is_some_and(|bytes| bytes > 0));
    assert_eq!(entry["lastFps"], json!(24.0));

    let unknown = client
        .get(format!(
            "{base_url}/api/v1/playback/sessions/unknown-session/stats"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(unknown.status(), reqwest::StatusCode::NOT_FOUND);
    let anonymous = client
        .get(format!(
            "{base_url}/api/v1/playback/sessions/{transcode_id}/stats"
        ))
        .send()
        .await?;
    assert_eq!(anonymous.status(), reqwest::StatusCode::UNAUTHORIZED);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn broken_hardware_encoder_falls_back_to_software_transcoding()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config_dir = temp_dir.path().join("config");
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Fallback Movie 2026.mkv"), b"fallback-bytes").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE'")
            .fetch_one(database.pool())
            .await?;
    let source_id: String = sqlx::query_scalar("SELECT id FROM media_sources WHERE item_id = ?")
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO server_settings (key, value)
         VALUES ('transcoding', '{\"hardwareEncoder\":\"h264_videotoolbox\"}')
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .execute(database.pool())
    .await?;

    // The self-test succeeds (probe args contain the lavfi source) but the real encode with the
    // hardware encoder fails, which is exactly the "configured but unusable GPU" case. Detect the
    // self-test by its lavfi input instead of the exact testsrc string, which the argument builder
    // may extend with a pixel-format suffix.
    let fake_ffmpeg = temp_dir.path().join("fake-ffmpeg");
    let fake_ffmpeg_source = [
        "#!/bin/sh",
        "set -eu",
        "is_probe=0",
        "is_hardware=0",
        "manifest=\"\"",
        "segment=\"\"",
        "init=\"init.mp4\"",
        "for arg in \"$@\"; do",
        "  [ \"$arg\" = \"lavfi\" ] && is_probe=1",
        "  [ \"$arg\" = \"h264_videotoolbox\" ] && is_hardware=1",
        "done",
        "if [ \"$is_hardware\" = \"1\" ] && [ \"$is_probe\" = \"0\" ]; then exit 1; fi",
        "while [ \"$#\" -gt 0 ]; do",
        "  case \"$1\" in",
        "    -hls_segment_filename) segment=\"$2\"; shift 2 ;;",
        "    -hls_fmp4_init_filename) init=\"$2\"; shift 2 ;;",
        "    *.m3u8) manifest=\"$1\"; shift ;;",
        "    *) shift ;;",
        "  esac",
        "done",
        "[ -n \"$manifest\" ] || exit 0",
        "directory=$(dirname \"$manifest\")",
        "mkdir -p \"$directory\"",
        "printf '#EXTM3U\\n#EXT-X-MAP:URI=\"%s\"\\n#EXTINF:1,\\nsegment_000000.m4s\\n' \"$init\" > \"$manifest\"",
        "printf init > \"$directory/$init\"",
        "printf segment > \"$(printf '%s' \"$segment\" | sed 's/%06d/000000/')\"",
        "printf 'frame=24\\nfps=24.00\\nspeed=1.00x\\nprogress=continue\\n'",
        "sleep 2",
    ]
    .join("\n");
    tokio::fs::write(&fake_ffmpeg, fake_ffmpeg_source).await?;
    let mut permissions = tokio::fs::metadata(&fake_ffmpeg).await?.permissions();
    permissions.set_mode(0o700);
    tokio::fs::set_permissions(&fake_ffmpeg, permissions).await?;

    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let state = AppState::ready(config, database.clone(), setup, auth, emby_auth)
        .with_hls_executable(fake_ffmpeg);
    let app = app_with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");
    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let session_cookie = cookie_value(login.headers(), "lux_session").ok_or("missing session")?;
    let csrf_cookie = cookie_value(login.headers(), "lux_csrf").ok_or("missing csrf")?;
    let cookies = format!("lux_session={session_cookie}; lux_csrf={csrf_cookie}");

    let probe = client
        .post(format!(
            "{base_url}/api/v1/admin/settings/transcoding/probe"
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf_cookie)
        .send()
        .await?;
    assert_eq!(probe.status(), reqwest::StatusCode::OK);
    let probe = probe.json::<Value>().await?;
    assert_eq!(probe["transcodingStatus"]["available"], json!(true));
    assert_eq!(
        probe["transcodingStatus"]["encoder"],
        json!("h264_videotoolbox")
    );

    let created = client
        .post(format!("{base_url}/api/v1/playback/sessions"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf_cookie)
        .json(&json!({
            "itemId": item_id,
            "sourceId": source_id,
            "capabilities": {
                "directPlay": false,
                "hls": true,
                "videoCopyToFmp4": false,
                "audioCopyToFmp4": false,
                "softwareTranscode": true
            }
        }))
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::OK);
    let created_body = created.json::<Value>().await?;
    assert_eq!(created_body["plan"]["type"], json!("SERVER_HLS"));
    assert_eq!(
        created_body["plan"]["tier"],
        json!(4),
        "the failed hardware start must fall back to software: {created_body}"
    );
    assert_eq!(created_body["tier"], json!(4));
    let session_id = created_body["sessionId"]
        .as_str()
        .ok_or("missing playback session id")?;
    let manifest_url = created_body["plan"]["manifestUrl"]
        .as_str()
        .ok_or("missing manifest url")?;
    let manifest = client
        .get(format!("{base_url}{manifest_url}"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(manifest.status(), reqwest::StatusCode::OK);
    let manifest_body = manifest.text().await?;
    assert!(manifest_body.contains("#EXTINF"), "{manifest_body}");
    let stored_tier: i64 =
        sqlx::query_scalar("SELECT tier FROM web_playback_sessions WHERE id = ?")
            .bind(session_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(stored_tier, 4, "the session row must record the real tier");

    let stats = client
        .get(format!(
            "{base_url}/api/v1/playback/sessions/{session_id}/stats"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(stats.status(), reqwest::StatusCode::OK);
    let stats = stats.json::<Value>().await?;
    assert_eq!(stats["tier"], json!(4));

    server.abort();
    Ok(())
}

/// A probed movie plus a fake FFmpeg that records its command line, so a ceiling test can assert
/// both the plan the API returned and the `-b:v` FFmpeg was actually started with.
struct MaxBitrateFixture {
    _temp_dir: tempfile::TempDir,
    database: Database,
    library_id: luxd::domain::ids::LibraryId,
    base_url: String,
    client: reqwest::Client,
    cookies: String,
    csrf_cookie: String,
    item_id: String,
    source_id: String,
    args_path: std::path::PathBuf,
    server: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl MaxBitrateFixture {
    async fn new(source_bitrate: Option<i64>) -> Result<Self, Box<dyn std::error::Error>> {
        Self::with_media(source_bitrate, None).await
    }

    /// The same fixture with a planted video dimension, so a resolution ceiling test can assert the
    /// scale filter and the reported heights without running ffprobe.
    async fn with_media(
        source_bitrate: Option<i64>,
        source_height: Option<i64>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config_dir = temp_dir.path().join("config");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: config_dir.clone(),
        };
        let database = Database::connect(&config).await?;
        let setup = SetupService::new(database.clone())?;
        setup.complete("Admin", "Admin", "correct password").await?;
        let libraries = LibraryService::new(database.clone());
        let library = libraries
            .create_library("Movies", LibraryKind::Movie, false)
            .await?;
        let root = temp_dir.path().join("Movies");
        tokio::fs::create_dir_all(&root).await?;
        tokio::fs::write(root.join("Ceiling Movie 2026.mp4"), b"ceiling-bytes").await?;
        libraries
            .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
            .await?;
        LibraryScanner::new(database.clone())
            .scan_movie_library(library.id)
            .await?;
        let item_id: String =
            sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE'")
                .fetch_one(database.pool())
                .await?;
        let source_id: String =
            sqlx::query_scalar("SELECT id FROM media_sources WHERE item_id = ?")
                .bind(&item_id)
                .fetch_one(database.pool())
                .await?;
        if let Some(bitrate) = source_bitrate {
            // The scanner does not probe in tests, so the measured container bitrate is planted.
            sqlx::query("UPDATE media_sources SET bitrate = ? WHERE id = ?")
                .bind(bitrate)
                .bind(&source_id)
                .execute(database.pool())
                .await?;
        }
        if let Some(height) = source_height {
            // A stored video stream stands in for a completed probe: playback reads only the
            // persisted details, never ffprobe.
            let width = (height * 16 / 9) / 2 * 2;
            sqlx::query(
                "INSERT INTO media_streams (
                     id, media_source_id, stream_index, stream_type, codec, details_json
                 ) VALUES (?, ?, 0, 'VIDEO', 'hevc', ?)",
            )
            .bind(format!("{source_id}-video"))
            .bind(&source_id)
            .bind(json!({ "Height": height, "Width": width }).to_string())
            .execute(database.pool())
            .await?;
        }

        let args_path = temp_dir.path().join("ffmpeg-args.txt");
        let fake_ffmpeg = temp_dir.path().join("fake-ffmpeg");
        let fake_ffmpeg_source = [
            "#!/bin/sh".to_owned(),
            "set -eu".to_owned(),
            format!("printf '%s\\n' \"$@\" > '{}'", args_path.display()),
            "manifest=\"\"".to_owned(),
            "segment=\"\"".to_owned(),
            "init=\"init.mp4\"".to_owned(),
            "while [ \"$#\" -gt 0 ]; do".to_owned(),
            "  case \"$1\" in".to_owned(),
            "    -hls_segment_filename) segment=\"$2\"; shift 2 ;;".to_owned(),
            "    -hls_fmp4_init_filename) init=\"$2\"; shift 2 ;;".to_owned(),
            "    *.m3u8) manifest=\"$1\"; shift ;;".to_owned(),
            "    *) shift ;;".to_owned(),
            "  esac".to_owned(),
            "done".to_owned(),
            "[ -n \"$manifest\" ] || exit 0".to_owned(),
            "directory=$(dirname \"$manifest\")".to_owned(),
            "mkdir -p \"$directory\"".to_owned(),
            "printf '#EXTM3U\\n#EXT-X-MAP:URI=\"%s\"\\n#EXTINF:1,\\nsegment_000000.m4s\\n' \"$init\" > \"$manifest\"".to_owned(),
            "printf init > \"$directory/$init\"".to_owned(),
            "printf segment > \"$(printf '%s' \"$segment\" | sed 's/%06d/000000/')\"".to_owned(),
            "sleep 2".to_owned(),
        ]
        .join("\n");
        tokio::fs::write(&fake_ffmpeg, fake_ffmpeg_source).await?;
        let mut permissions = tokio::fs::metadata(&fake_ffmpeg).await?.permissions();
        permissions.set_mode(0o700);
        tokio::fs::set_permissions(&fake_ffmpeg, permissions).await?;

        let auth = WebAuthService::new(database.clone())?;
        let emby_auth = EmbyAuthService::new(database.clone())?;
        let state = AppState::ready(config, database.clone(), setup, auth, emby_auth)
            .with_hls_executable(fake_ffmpeg);
        let app = app_with_state(state);
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let base_url = format!("http://{address}");
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("test client");
        let login = client
            .post(format!("{base_url}/api/v1/auth/login"))
            .json(&json!({ "username": "admin", "password": "correct password" }))
            .send()
            .await?;
        assert_eq!(login.status(), reqwest::StatusCode::OK);
        let session_cookie =
            cookie_value(login.headers(), "lux_session").ok_or("missing session")?;
        let csrf_cookie = cookie_value(login.headers(), "lux_csrf").ok_or("missing csrf")?;
        Ok(Self {
            cookies: format!("lux_session={session_cookie}; lux_csrf={csrf_cookie}"),
            csrf_cookie,
            library_id: library.id,
            _temp_dir: temp_dir,
            database,
            base_url,
            client,
            item_id,
            source_id,
            args_path,
            server,
        })
    }

    /// Replaces the administrator's transcoding policy.
    async fn set_transcoding_settings(
        &self,
        settings: Value,
    ) -> Result<(), Box<dyn std::error::Error>> {
        sqlx::query(
            "INSERT INTO server_settings (key, value) VALUES ('transcoding', ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(settings.to_string())
        .execute(self.database.pool())
        .await?;
        Ok(())
    }

    /// Stores the administrator's transcoding policy; `maxVideoBitrateKbps` caps every encode.
    async fn set_max_video_bitrate_kbps(
        &self,
        kbps: i64,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.set_transcoding_settings(json!({ "maxVideoBitrateKbps": kbps }))
            .await
    }

    /// Stores the administrator's global output height cap, which every encode is scaled to at
    /// most.
    async fn set_max_output_height(&self, height: i64) -> Result<(), Box<dyn std::error::Error>> {
        self.set_transcoding_settings(json!({ "maxOutputHeight": height }))
            .await
    }

    /// The command line FFmpeg was started with, once the (asynchronous) child recorded it.
    async fn ffmpeg_arguments(&self) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let mut arguments = String::new();
        for _ in 0..250 {
            if let Ok(captured) = tokio::fs::read_to_string(&self.args_path).await
                && !captured.is_empty()
            {
                arguments = captured;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            !arguments.is_empty(),
            "FFmpeg arguments were not captured before the timeout"
        );
        Ok(arguments.lines().map(str::to_owned).collect())
    }

    /// The value that follows `flag` on the captured command line.
    async fn ffmpeg_argument(&self, flag: &str) -> Result<String, Box<dyn std::error::Error>> {
        let arguments = self.ffmpeg_arguments().await?;
        arguments
            .iter()
            .position(|argument| argument == flag)
            .and_then(|index| arguments.get(index + 1))
            .cloned()
            .ok_or_else(|| format!("FFmpeg was not started with {flag}").into())
    }

    /// The `-b:v` FFmpeg was started with, once the (asynchronous) child recorded its arguments.
    async fn ffmpeg_video_bitrate(&self) -> Result<String, Box<dyn std::error::Error>> {
        self.ffmpeg_argument("-b:v").await
    }

    async fn stats(&self, session_id: &str) -> Result<Value, Box<dyn std::error::Error>> {
        let response = self
            .client
            .get(format!(
                "{}/api/v1/playback/sessions/{session_id}/stats",
                self.base_url
            ))
            .header(COOKIE, &self.cookies)
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        Ok(response.json::<Value>().await?)
    }

    async fn stop_session(&self, session_id: &str) -> Result<(), Box<dyn std::error::Error>> {
        let response = self
            .client
            .delete(format!(
                "{}/api/v1/playback/sessions/{session_id}",
                self.base_url
            ))
            .header(COOKIE, &self.cookies)
            .header("x-csrf-token", &self.csrf_cookie)
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
        Ok(())
    }

    async fn create_bootstrap(
        &self,
        body: Value,
    ) -> Result<(reqwest::StatusCode, Value), Box<dyn std::error::Error>> {
        let response = self
            .client
            .post(format!("{}/api/v1/playback/bootstrap", self.base_url))
            .header(COOKIE, &self.cookies)
            .header("x-csrf-token", &self.csrf_cookie)
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        Ok((status, response.json::<Value>().await?))
    }

    async fn create_session(
        &self,
        body: Value,
    ) -> Result<(reqwest::StatusCode, Value), Box<dyn std::error::Error>> {
        let response = self
            .client
            .post(format!("{}/api/v1/playback/sessions", self.base_url))
            .header(COOKIE, &self.cookies)
            .header("x-csrf-token", &self.csrf_cookie)
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        Ok((status, response.json::<Value>().await?))
    }
}

/// The browser can play the original file natively: without a ceiling the plan stays Direct, and a
/// ceiling the source already meets must not change it either.
#[tokio::test]
async fn web_playback_keeps_direct_play_when_the_source_already_meets_the_ceiling()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = MaxBitrateFixture::new(Some(2_000_000)).await?;

    let (status, without_ceiling) = fixture
        .create_session(json!({
            "itemId": fixture.item_id,
            "sourceId": fixture.source_id,
            "capabilities": {
                "directPlay": true,
                "hls": true,
                "softwareTranscode": true
            }
        }))
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(without_ceiling["plan"]["type"], json!("DIRECT"));
    assert_eq!(
        without_ceiling["bitrate"]["sourceBitrate"],
        json!(2_000_000)
    );
    assert_eq!(without_ceiling["bitrate"]["maxBitrate"], Value::Null);
    assert_eq!(without_ceiling["bitrate"]["targetBitrate"], Value::Null);
    assert_eq!(without_ceiling["bitrate"]["limited"], json!(false));

    let (status, within_ceiling) = fixture
        .create_session(json!({
            "itemId": fixture.item_id,
            "sourceId": fixture.source_id,
            "maxBitrate": 4_000_000,
            "capabilities": {
                "directPlay": true,
                "hls": true,
                "softwareTranscode": true
            }
        }))
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(
        within_ceiling["plan"]["type"],
        json!("DIRECT"),
        "a source inside its ceiling keeps Direct Play"
    );
    assert_eq!(within_ceiling["bitrate"]["maxBitrate"], json!(4_000_000));
    assert_eq!(within_ceiling["bitrate"]["targetBitrate"], Value::Null);
    assert_eq!(within_ceiling["bitrate"]["limited"], json!(false));
    assert!(
        !fixture.args_path.exists(),
        "a direct plan must not start FFmpeg"
    );

    fixture.server.abort();
    Ok(())
}

/// A source above the requested ceiling is re-encoded to it: the plan leaves Direct Play, FFmpeg is
/// started with that bitrate, and the session reports the target back to the client.
#[tokio::test]
async fn web_playback_limits_a_source_above_the_requested_max_bitrate()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = MaxBitrateFixture::new(Some(20_000_000)).await?;

    let (status, created) = fixture
        .create_session(json!({
            "itemId": fixture.item_id,
            "sourceId": fixture.source_id,
            "maxBitrate": 4_000_000,
            "capabilities": {
                "directPlay": true,
                "hls": true,
                "videoCopyToFmp4": true,
                "audioCopyToFmp4": true,
                "softwareTranscode": true
            }
        }))
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(
        created["plan"]["type"],
        json!("SERVER_HLS"),
        "Direct Play and Remux would keep the original bitrate: {created}"
    );
    assert_eq!(created["tier"], json!(4));
    assert_eq!(created["bitrate"]["sourceBitrate"], json!(20_000_000));
    assert_eq!(created["bitrate"]["maxBitrate"], json!(4_000_000));
    assert_eq!(created["bitrate"]["targetBitrate"], json!(4_000_000));
    assert_eq!(created["bitrate"]["limited"], json!(true));
    let session_id = created["sessionId"]
        .as_str()
        .ok_or("missing limited session id")?
        .to_owned();

    // The session starts FFmpeg and answers immediately, so wait for the child to record its
    // command line instead of racing the spawn.
    let mut arguments = String::new();
    for _ in 0..250 {
        if let Ok(captured) = tokio::fs::read_to_string(&fixture.args_path).await
            && captured.contains("-b:v")
        {
            arguments = captured;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(
        !arguments.is_empty(),
        "FFmpeg arguments were not captured before the timeout"
    );
    let arguments: Vec<&str> = arguments.lines().collect();
    let video_bitrate = arguments
        .iter()
        .position(|argument| *argument == "-b:v")
        .and_then(|index| arguments.get(index + 1))
        .ok_or("the transcode was not started with a video bitrate")?;
    assert_eq!(
        *video_bitrate, "4000000",
        "the ceiling must bound the encoder: {arguments:?}"
    );

    let stats = fixture
        .client
        .get(format!(
            "{}/api/v1/playback/sessions/{session_id}/stats",
            fixture.base_url
        ))
        .header(COOKIE, &fixture.cookies)
        .send()
        .await?;
    assert_eq!(stats.status(), reqwest::StatusCode::OK);
    let stats = stats.json::<Value>().await?;
    assert_eq!(stats["plan"], json!("SERVER_HLS"));
    assert_eq!(stats["tier"], json!(4));
    assert_eq!(
        stats["output"]["videoBitrate"],
        json!(4_000_000),
        "the stored output description must match what the player was told"
    );

    // Only one software transcode may run at a time by default, so release the first session.
    let stopped = fixture
        .client
        .delete(format!(
            "{}/api/v1/playback/sessions/{session_id}",
            fixture.base_url
        ))
        .header(COOKIE, &fixture.cookies)
        .header("x-csrf-token", &fixture.csrf_cookie)
        .send()
        .await?;
    assert_eq!(stopped.status(), reqwest::StatusCode::NO_CONTENT);

    // A browser that could otherwise be served by Remux must be re-encoded instead: a copy would
    // keep the original bitrate the caller asked to stay below.
    let (status, remux_upgraded) = fixture
        .create_session(json!({
            "itemId": fixture.item_id,
            "sourceId": fixture.source_id,
            "maxBitrate": 4_000_000,
            "capabilities": {
                "directPlay": false,
                "hls": true,
                "videoCopyToFmp4": true,
                "audioCopyToFmp4": true,
                "softwareTranscode": true
            }
        }))
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(remux_upgraded["plan"]["type"], json!("SERVER_HLS"));
    assert_eq!(
        remux_upgraded["tier"],
        json!(4),
        "Remux is tier 1; a ceiling the source exceeds rules it out: {remux_upgraded}"
    );
    assert_eq!(remux_upgraded["bitrate"]["targetBitrate"], json!(4_000_000));

    fixture.server.abort();
    Ok(())
}

/// `.strm` keeps its Direct-only rule: the ceiling is ignored instead of moving it to HLS, and a
/// value outside the accepted range is a plain malformed request.
#[tokio::test]
async fn web_playback_ignores_the_max_bitrate_for_strm_and_rejects_an_invalid_value()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = MaxBitrateFixture::new(Some(20_000_000)).await?;
    let root = fixture._temp_dir.path().join("Movies");
    tokio::fs::write(
        root.join("Remote Ceiling Movie 2026.strm"),
        "http://media.example.test/media.mp4\n",
    )
    .await?;
    LibraryScanner::new(fixture.database.clone())
        .scan_movie_library(fixture.library_id)
        .await?;
    let (strm_item_id, strm_source_id): (String, String) = sqlx::query_as(
        "SELECT mi.id, ms.id
         FROM media_items mi
         JOIN media_sources ms ON ms.item_id = mi.id
         WHERE ms.source_kind = 'STRM_URL'
         LIMIT 1",
    )
    .fetch_one(fixture.database.pool())
    .await?;

    let (status, strm_direct) = fixture
        .create_session(json!({
            "itemId": strm_item_id,
            "sourceId": strm_source_id,
            "maxBitrate": 4_000_000,
            "capabilities": {
                "directPlay": true,
                "hls": true,
                "softwareTranscode": true
            }
        }))
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(
        strm_direct["plan"]["type"],
        json!("DIRECT"),
        "a .strm never leaves Direct Play for a ceiling"
    );
    assert_eq!(strm_direct["bitrate"]["targetBitrate"], Value::Null);
    assert_eq!(strm_direct["bitrate"]["limited"], json!(false));

    let (status, strm_unplayable) = fixture
        .create_session(json!({
            "itemId": strm_item_id,
            "sourceId": strm_source_id,
            "maxBitrate": 4_000_000,
            "capabilities": {
                "directPlay": false,
                "hls": true,
                "softwareTranscode": true
            }
        }))
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(strm_unplayable["plan"]["type"], json!("UNSUPPORTED"));
    assert_eq!(
        strm_unplayable["plan"]["reason"],
        json!("STRM_REQUIRES_DIRECT_PLAY")
    );
    let hls_sessions: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM web_playback_sessions WHERE plan = 'SERVER_HLS'")
            .fetch_one(fixture.database.pool())
            .await?;
    assert_eq!(hls_sessions, 0, "no .strm HLS session may be created");

    for invalid in [50_000, 200_000_001, 0] {
        let (status, body) = fixture
            .create_session(json!({
                "itemId": fixture.item_id,
                "sourceId": fixture.source_id,
                "maxBitrate": invalid,
                "capabilities": { "directPlay": true, "hls": true }
            }))
            .await?;
        assert_eq!(
            status,
            reqwest::StatusCode::BAD_REQUEST,
            "{invalid} bps must be refused as a malformed request"
        );
        assert_eq!(body["error"]["code"], json!("INVALID_REQUEST"));
    }

    fixture.server.abort();
    Ok(())
}

/// The reported target must be the bitrate FFmpeg is really started with: the administrator's cap
/// applies on top of the user's ceiling, so a 4 Mbit/s request under a 2 Mbit/s policy becomes 2.
#[tokio::test]
async fn web_playback_reports_the_applied_video_bitrate_below_the_administrator_cap()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = MaxBitrateFixture::new(Some(20_000_000)).await?;
    fixture.set_max_video_bitrate_kbps(2_000).await?;

    let (status, created) = fixture
        .create_session(json!({
            "itemId": fixture.item_id,
            "sourceId": fixture.source_id,
            "maxBitrate": 4_000_000,
            "capabilities": {
                "directPlay": true,
                "hls": true,
                "videoCopyToFmp4": true,
                "audioCopyToFmp4": true,
                "softwareTranscode": true
            }
        }))
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(created["plan"]["type"], json!("SERVER_HLS"));
    assert_eq!(created["tier"], json!(4));
    assert_eq!(
        created["bitrate"]["maxBitrate"],
        json!(4_000_000),
        "the request the client sent is still reported as-is"
    );
    assert_eq!(
        created["bitrate"]["targetBitrate"],
        json!(2_000_000),
        "the target must be the administrator-capped bitrate: {created}"
    );
    assert_eq!(created["bitrate"]["limited"], json!(true));
    let session_id = created["sessionId"]
        .as_str()
        .ok_or("missing capped session id")?
        .to_owned();
    assert_eq!(
        fixture.ffmpeg_video_bitrate().await?,
        "2000000",
        "the reported target must equal the -b:v FFmpeg was started with"
    );
    let stats = fixture.stats(&session_id).await?;
    assert_eq!(
        stats["output"]["videoBitrate"],
        json!(2_000_000),
        "the session output must describe the same bitrate as the response"
    );

    // Only one software transcode runs by default, so release the session before the next case.
    fixture.stop_session(&session_id).await?;

    // A transcode forced by the browser (no user ceiling at all) still reports the cap FFmpeg
    // applies, so the player never claims an uncapped output it is not getting.
    let (status, capped_without_ceiling) = fixture
        .create_session(json!({
            "itemId": fixture.item_id,
            "sourceId": fixture.source_id,
            "capabilities": {
                "directPlay": false,
                "hls": true,
                "videoCopyToFmp4": false,
                "audioCopyToFmp4": false,
                "softwareTranscode": true
            }
        }))
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(capped_without_ceiling["plan"]["type"], json!("SERVER_HLS"));
    assert_eq!(capped_without_ceiling["bitrate"]["maxBitrate"], Value::Null);
    assert_eq!(
        capped_without_ceiling["bitrate"]["targetBitrate"],
        json!(2_000_000),
        "the administrator cap alone bounds the encoder"
    );
    assert_eq!(capped_without_ceiling["bitrate"]["limited"], json!(true));
    assert_eq!(fixture.ffmpeg_video_bitrate().await?, "2000000");

    fixture.server.abort();
    Ok(())
}

/// A source taller than the requested ceiling is re-encoded and scaled: the plan leaves Direct
/// Play, FFmpeg is started with the `scale=-2:min(ih,1080)` filter and the height-ladder bitrate,
/// and the response reports the heights the player will actually see.
#[tokio::test]
async fn web_playback_scales_a_taller_source_to_the_requested_max_height()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = MaxBitrateFixture::with_media(Some(20_000_000), Some(2_160)).await?;

    let (status, created) = fixture
        .create_session(json!({
            "itemId": fixture.item_id,
            "sourceId": fixture.source_id,
            "maxHeight": 1080,
            "capabilities": {
                "directPlay": true,
                "hls": true,
                "videoCopyToFmp4": true,
                "audioCopyToFmp4": true,
                "softwareTranscode": true
            }
        }))
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(
        created["plan"]["type"],
        json!("SERVER_HLS"),
        "Direct Play and Remux would hand the browser the original 2160p picture: {created}"
    );
    assert_eq!(created["tier"], json!(4));
    assert_eq!(created["resolution"]["sourceHeight"], json!(2160));
    assert_eq!(created["resolution"]["maxHeight"], json!(1080));
    assert_eq!(created["resolution"]["targetHeight"], json!(1080));
    assert_eq!(created["resolution"]["limited"], json!(true));
    assert_eq!(
        created["bitrate"]["targetBitrate"],
        json!(8_000_000),
        "a 1080p output derives 8 Mbit/s, below the 20 Mbit/s source"
    );
    let session_id = created["sessionId"]
        .as_str()
        .ok_or("missing scaled session id")?
        .to_owned();

    assert_eq!(
        fixture.ffmpeg_argument("-vf").await?,
        "scale=-2:min(ih\\,1080)",
        "the encoder must scale to the requested height"
    );
    assert_eq!(
        fixture.ffmpeg_video_bitrate().await?,
        "8000000",
        "the reported target must equal the -b:v FFmpeg was started with"
    );

    let stats = fixture.stats(&session_id).await?;
    assert_eq!(stats["plan"], json!("SERVER_HLS"));
    assert_eq!(stats["output"]["videoHeight"], json!(1080));
    assert_eq!(
        stats["output"]["videoWidth"],
        json!(1920),
        "the width follows FFmpeg's proportional -2 scaling"
    );
    assert_eq!(stats["output"]["videoBitrate"], json!(8_000_000));

    fixture.stop_session(&session_id).await?;
    fixture.server.abort();
    Ok(())
}

/// Both ceilings can be sent together: the stricter one bounds the encoder, and the response still
/// describes each request value separately.
#[tokio::test]
async fn web_playback_applies_the_stricter_of_both_ceilings()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = MaxBitrateFixture::with_media(Some(20_000_000), Some(2_160)).await?;

    let (status, created) = fixture
        .create_session(json!({
            "itemId": fixture.item_id,
            "sourceId": fixture.source_id,
            "maxHeight": 1080,
            "maxBitrate": 4_000_000,
            "capabilities": {
                "directPlay": true,
                "hls": true,
                "softwareTranscode": true
            }
        }))
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(created["plan"]["type"], json!("SERVER_HLS"));
    assert_eq!(created["resolution"]["targetHeight"], json!(1080));
    assert_eq!(created["resolution"]["limited"], json!(true));
    assert_eq!(created["bitrate"]["maxBitrate"], json!(4_000_000));
    assert_eq!(
        created["bitrate"]["targetBitrate"],
        json!(4_000_000),
        "4 Mbit/s is stricter than the 8 Mbit/s the height ladder derives"
    );
    assert_eq!(
        fixture.ffmpeg_argument("-vf").await?,
        "scale=-2:min(ih\\,1080)"
    );
    assert_eq!(fixture.ffmpeg_video_bitrate().await?, "4000000");

    let session_id = created["sessionId"]
        .as_str()
        .ok_or("missing limited session id")?
        .to_owned();
    fixture.stop_session(&session_id).await?;
    fixture.server.abort();
    Ok(())
}

/// A source already inside the ceiling keeps Direct Play: a resolution ceiling never upgrades or
/// downscales a picture that already fits.
#[tokio::test]
async fn web_playback_keeps_direct_play_when_the_source_fits_the_resolution()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = MaxBitrateFixture::with_media(Some(4_000_000), Some(720)).await?;

    let (status, within) = fixture
        .create_session(json!({
            "itemId": fixture.item_id,
            "sourceId": fixture.source_id,
            "maxHeight": 1080,
            "capabilities": {
                "directPlay": true,
                "hls": true,
                "softwareTranscode": true
            }
        }))
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(
        within["plan"]["type"],
        json!("DIRECT"),
        "a 720p source under a 1080p ceiling plays as-is: {within}"
    );
    assert_eq!(within["resolution"]["sourceHeight"], json!(720));
    assert_eq!(within["resolution"]["maxHeight"], json!(1080));
    assert_eq!(
        within["resolution"]["targetHeight"],
        json!(720),
        "the output keeps the source height"
    );
    assert_eq!(within["resolution"]["limited"], json!(false));
    assert_eq!(within["bitrate"]["targetBitrate"], Value::Null);
    assert!(
        !fixture.args_path.exists(),
        "a direct plan must not start FFmpeg"
    );

    fixture.server.abort();
    Ok(())
}

/// An unprobed source has no known height, so the ceiling cannot be honoured and the decision stays
/// exactly as it was: the server never transcodes a file it cannot prove is too tall.
#[tokio::test]
async fn web_playback_ignores_the_resolution_of_an_unprobed_source()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = MaxBitrateFixture::with_media(None, None).await?;

    let (status, created) = fixture
        .create_session(json!({
            "itemId": fixture.item_id,
            "sourceId": fixture.source_id,
            "maxHeight": 480,
            "capabilities": {
                "directPlay": true,
                "hls": true,
                "softwareTranscode": true
            }
        }))
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(created["plan"]["type"], json!("DIRECT"));
    assert_eq!(created["resolution"]["sourceHeight"], Value::Null);
    assert_eq!(created["resolution"]["maxHeight"], json!(480));
    assert_eq!(created["resolution"]["limited"], json!(false));
    assert_eq!(created["bitrate"]["targetBitrate"], Value::Null);
    assert!(!fixture.args_path.exists());

    fixture.server.abort();
    Ok(())
}

/// `.strm` keeps its Direct-only rule and a height outside the accepted range is a malformed
/// request, exactly like the bitrate ceiling.
#[tokio::test]
async fn web_playback_ignores_the_max_height_for_strm_and_rejects_an_invalid_value()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = MaxBitrateFixture::with_media(Some(20_000_000), Some(2_160)).await?;
    let root = fixture._temp_dir.path().join("Movies");
    tokio::fs::write(
        root.join("Remote Height Movie 2026.strm"),
        "http://media.example.test/media.mp4\n",
    )
    .await?;
    LibraryScanner::new(fixture.database.clone())
        .scan_movie_library(fixture.library_id)
        .await?;
    let (strm_item_id, strm_source_id): (String, String) = sqlx::query_as(
        "SELECT mi.id, ms.id
         FROM media_items mi
         JOIN media_sources ms ON ms.item_id = mi.id
         WHERE ms.source_kind = 'STRM_URL'
         LIMIT 1",
    )
    .fetch_one(fixture.database.pool())
    .await?;

    let (status, strm_direct) = fixture
        .create_session(json!({
            "itemId": strm_item_id,
            "sourceId": strm_source_id,
            "maxHeight": 480,
            "capabilities": {
                "directPlay": true,
                "hls": true,
                "softwareTranscode": true
            }
        }))
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(
        strm_direct["plan"]["type"],
        json!("DIRECT"),
        "a .strm never leaves Direct Play for a resolution ceiling"
    );
    assert_eq!(strm_direct["resolution"]["limited"], json!(false));
    assert_eq!(
        strm_direct["resolution"]["targetHeight"],
        Value::Null,
        "a .strm probe recorded no height, so no output height is claimed"
    );
    assert_eq!(strm_direct["bitrate"]["targetBitrate"], Value::Null);

    // A browser that cannot consume HLS gets the existing unsupported reason instead of a plan
    // that would silently keep the original picture.
    let (status, unsupported) = fixture
        .create_session(json!({
            "itemId": fixture.item_id,
            "sourceId": fixture.source_id,
            "maxHeight": 480,
            "capabilities": {
                "directPlay": true,
                "hls": false,
                "softwareTranscode": true
            }
        }))
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(unsupported["plan"]["type"], json!("UNSUPPORTED"));
    assert_eq!(
        unsupported["plan"]["reason"],
        json!("BROWSER_CANNOT_CONSUME_HLS")
    );
    assert_eq!(unsupported["sessionId"], Value::Null);
    assert_eq!(unsupported["resolution"]["limited"], json!(false));
    let hls_sessions: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM web_playback_sessions WHERE plan = 'SERVER_HLS'")
            .fetch_one(fixture.database.pool())
            .await?;
    assert_eq!(hls_sessions, 0, "an unsupported plan starts no session");

    for invalid in [143, 4_321, 0, -720] {
        let (status, body) = fixture
            .create_session(json!({
                "itemId": fixture.item_id,
                "sourceId": fixture.source_id,
                "maxHeight": invalid,
                "capabilities": { "directPlay": true, "hls": true }
            }))
            .await?;
        assert_eq!(
            status,
            reqwest::StatusCode::BAD_REQUEST,
            "{invalid}p must be refused as a malformed request"
        );
        assert_eq!(body["error"]["code"], json!("INVALID_REQUEST"));
    }

    fixture.server.abort();
    Ok(())
}

/// `/playback/bootstrap` accepts the same ceiling, so the detail page can start a session with the
/// quality the player already picked.
#[tokio::test]
async fn web_playback_bootstrap_accepts_the_max_height() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = MaxBitrateFixture::with_media(Some(20_000_000), Some(2_160)).await?;

    let (status, body) = fixture
        .create_bootstrap(json!({
            "itemId": fixture.item_id,
            "sourceId": fixture.source_id,
            "maxHeight": 480,
            "capabilities": {
                "directPlay": true,
                "hls": true,
                "softwareTranscode": true
            }
        }))
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    let session = &body["session"];
    assert_eq!(session["plan"]["type"], json!("SERVER_HLS"));
    assert_eq!(session["resolution"]["sourceHeight"], json!(2160));
    assert_eq!(session["resolution"]["maxHeight"], json!(480));
    assert_eq!(session["resolution"]["targetHeight"], json!(480));
    assert_eq!(session["resolution"]["limited"], json!(true));
    assert_eq!(
        session["bitrate"]["targetBitrate"],
        json!(2_000_000),
        "a 480p output derives 2 Mbit/s"
    );
    assert_eq!(
        fixture.ffmpeg_argument("-vf").await?,
        "scale=-2:min(ih\\,480)"
    );

    let session_id = session["sessionId"]
        .as_str()
        .ok_or("missing bootstrap session id")?
        .to_owned();
    fixture.stop_session(&session_id).await?;
    fixture.server.abort();
    Ok(())
}

/// An odd ceiling cannot reach the encoder as-is: H.264 would refuse it with "height not divisible
/// by 2", so the height is rounded down before the scale filter and reported rounded as well.
#[tokio::test]
async fn web_playback_rounds_an_odd_max_height_down_to_even()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = MaxBitrateFixture::with_media(Some(20_000_000), Some(2_160)).await?;

    let (status, created) = fixture
        .create_session(json!({
            "itemId": fixture.item_id,
            "sourceId": fixture.source_id,
            "maxHeight": 1081,
            "capabilities": {
                "directPlay": true,
                "hls": true,
                "softwareTranscode": true
            }
        }))
        .await?;
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "an odd ceiling must still produce a playable plan, not an encode failure: {created}"
    );
    assert_eq!(created["plan"]["type"], json!("SERVER_HLS"));
    assert_eq!(created["tier"], json!(4));
    assert_eq!(
        created["resolution"]["maxHeight"],
        json!(1081),
        "the request is reported as the client sent it"
    );
    assert_eq!(
        created["resolution"]["targetHeight"],
        json!(1080),
        "the output height is the rounded ceiling"
    );
    assert_eq!(created["resolution"]["limited"], json!(true));
    assert_eq!(
        created["bitrate"]["targetBitrate"],
        json!(8_000_000),
        "the bitrate ladder follows the rounded 1080p output"
    );
    let session_id = created["sessionId"]
        .as_str()
        .ok_or("missing odd-height session id")?
        .to_owned();

    assert_eq!(
        fixture.ffmpeg_argument("-vf").await?,
        "scale=-2:min(ih\\,1080)",
        "the encoder must never receive an odd height"
    );
    assert_eq!(fixture.ffmpeg_video_bitrate().await?, "8000000");

    fixture.stop_session(&session_id).await?;
    fixture.server.abort();
    Ok(())
}

/// The administrator's global height cap is part of the real output height, so it decides which
/// bitrate rung the derived `-b:v` comes from.
#[tokio::test]
async fn web_playback_derives_the_bitrate_from_the_administrator_capped_height()
-> Result<(), Box<dyn std::error::Error>> {
    let fixture = MaxBitrateFixture::with_media(Some(20_000_000), Some(2_160)).await?;
    fixture.set_max_output_height(480).await?;

    let (status, created) = fixture
        .create_session(json!({
            "itemId": fixture.item_id,
            "sourceId": fixture.source_id,
            "maxHeight": 1080,
            "capabilities": {
                "directPlay": true,
                "hls": true,
                "softwareTranscode": true
            }
        }))
        .await?;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(created["plan"]["type"], json!("SERVER_HLS"));
    assert_eq!(created["resolution"]["maxHeight"], json!(1080));
    assert_eq!(
        created["resolution"]["targetHeight"],
        json!(480),
        "the administrator cap is the smallest known ceiling: {created}"
    );
    assert_eq!(created["resolution"]["limited"], json!(true));
    assert_eq!(
        created["bitrate"]["targetBitrate"],
        json!(2_000_000),
        "480p derives 2 Mbit/s, not the 8 Mbit/s the requested 1080p would"
    );
    let session_id = created["sessionId"]
        .as_str()
        .ok_or("missing capped-height session id")?
        .to_owned();

    assert_eq!(
        fixture.ffmpeg_argument("-vf").await?,
        "scale=-2:min(ih\\,480)"
    );
    assert_eq!(fixture.ffmpeg_video_bitrate().await?, "2000000");
    let stats = fixture.stats(&session_id).await?;
    assert_eq!(stats["output"]["videoHeight"], json!(480));
    assert_eq!(stats["output"]["videoBitrate"], json!(2_000_000));

    fixture.stop_session(&session_id).await?;
    fixture.server.abort();
    Ok(())
}

/// Emby media source ids are 32 lowercase hex characters; STRM sources publish the stored UUID
/// without hyphens so external 302 proxies can parse them.
fn public_source_id(id: &str) -> String {
    uuid::Uuid::parse_str(id)
        .map(|uuid| uuid.simple().to_string())
        .unwrap_or_else(|_| id.to_owned())
}
