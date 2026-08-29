use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, scanner::LibraryScanner, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use tokio::net::TcpListener;

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

#[tokio::test]
async fn web_playback_uses_signed_direct_urls_and_monotonic_events()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
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
    let media_path = root.join("Signed Playback Movie 2026.mp4");
    tokio::fs::write(&media_path, b"web-playback-bytes").await?;
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

    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        auth,
        emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();

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
    let duplicate = event("event-1", 1, "PLAYING", 100).send().await?;
    assert_eq!(duplicate.json::<Value>().await?["duplicate"], true);
    let stale = event("event-2", 0, "PAUSED", 0).send().await?;
    assert_eq!(stale.json::<Value>().await?["stale"], true);
    let session = sqlx::query_as::<_, (String, i64)>(
        "SELECT state, position_ticks
         FROM playback_sessions
         WHERE play_session_id = ?",
    )
    .bind(format!("lux-web:{session_id}"))
    .fetch_one(database.pool())
    .await?;
    assert_eq!(session.0, "PLAYING");
    assert_eq!(session.1, 500);
    let sequence: i64 =
        sqlx::query_scalar("SELECT last_sequence FROM web_playback_sessions WHERE id = ?")
            .bind(session_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(sequence, 1);

    tokio::fs::write(
        root.join("Remote Only Movie 2026.strm"),
        "https://example.invalid/media/movie.mp4\n",
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
    assert!(strm_direct_body["plan"]["proxyUrl"].is_null());
    assert!(strm_direct_body["sessionId"].is_string());
    let strm_direct_session_id = strm_direct_body["sessionId"]
        .as_str()
        .ok_or("missing direct STRM session id")?;
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
        format!("/Videos/{proxy_item_id}/stream?MediaSourceId={proxy_source_id}")
    );

    let stopped = client
        .delete(format!("{base_url}/api/v1/playback/sessions/{session_id}"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf_cookie)
        .send()
        .await?;
    assert_eq!(stopped.status(), reqwest::StatusCode::NO_CONTENT);
    let after_stop = client.get(format!("{base_url}{direct_url}")).send().await?;
    assert_eq!(after_stop.status(), reqwest::StatusCode::GONE);

    server.abort();
    Ok(())
}
