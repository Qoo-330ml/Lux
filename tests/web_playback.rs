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
        for request_number in 0..3 {
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
        format!("/Videos/{emby_strm_item_id}/stream?MediaSourceId={strm_source_id}")
    );
    assert!(strm_direct_body["sessionId"].is_string());
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
    let web_stopped: Value = serde_json::from_slice(
        &receiver
            .recv()
            .await
            .ok_or("missing Web playback stop notification")?,
    )?;
    assert_eq!(web_stopped["eventType"], "PLAYBACK_STOPPED");
    let strm_started: Value = serde_json::from_slice(
        &receiver
            .recv()
            .await
            .ok_or("missing STRM playback notification")?,
    )?;
    assert_eq!(strm_started["eventType"], "PLAYBACK_STARTED");
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
        format!("/Videos/{emby_proxy_item_id}/stream?MediaSourceId={proxy_source_id}")
    );

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
    sqlx::query(
        "INSERT INTO server_settings (key, value)
         VALUES ('trusted_proxy_cidrs', 'invalid-json')
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .execute(database.pool())
    .await?;
    let proxy_settings_error = peer_client
        .get(format!("{peer_base_url}{}", proxy_context_ticket.1))
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
    // hardware encoder fails, which is exactly the "configured but unusable GPU" case.
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
        "  [ \"$arg\" = \"testsrc=size=320x240:rate=30:duration=0.3\" ] && is_probe=1",
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
