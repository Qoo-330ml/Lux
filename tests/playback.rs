use std::os::unix::fs::symlink;

use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, scanner::LibraryScanner, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_RANGE, RANGE};
use serde_json::{Value, json};
use tokio::net::TcpListener;

#[tokio::test]
async fn local_file_stream_supports_full_head_range_acl_and_path_safety()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "correct password").await?;
    let viewer = UserStore::new(database.clone())?
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    let media_path = root.join("Range.Movie.2024.mkv");
    tokio::fs::write(&media_path, b"0123456789").await?;
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
    let high_media_path = root.join("Range.Movie.2024.2160p.mkv");
    tokio::fs::write(&high_media_path, vec![b'X'; 8 * 1024 * 1024]).await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let high_source_id: String = sqlx::query_scalar(
        "SELECT ms.id
         FROM media_sources ms
         JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
         WHERE ms.item_id = ? AND fe.relative_path = ?",
    )
    .bind(&item_id)
    .bind("Range.Movie.2024.2160p.mkv")
    .fetch_one(database.pool())
    .await?;

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
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="PlaybackTest", Device="Mac", DeviceId="playback-admin", Version="1""#,
        )
        .json(&json!({ "Username": "admin", "Pw": "correct password" }))
        .send()
        .await?;
    let token = login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing admin token")?
        .to_owned();
    let stream_url = format!("{base_url}/Videos/{item_id}/stream");

    let full = client
        .get(&stream_url)
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(full.status(), reqwest::StatusCode::OK);
    assert_eq!(full.headers()[CONTENT_LENGTH], "10");
    assert_eq!(full.headers()["accept-ranges"], "bytes");
    assert_eq!(full.headers()["content-type"], "video/x-matroska");
    assert!(full.headers().contains_key("etag"));
    assert!(full.headers().contains_key("last-modified"));
    assert_eq!(full.bytes().await?.as_ref(), b"0123456789");

    let playback_info = client
        .get(format!("{base_url}/Items/{item_id}/PlaybackInfo"))
        .query(&[("api_key", token.as_str())])
        .send()
        .await?;
    assert_eq!(playback_info.status(), reqwest::StatusCode::OK);
    let playback_body = playback_info.json::<Value>().await?;
    assert_eq!(playback_body["MediaSources"][0]["Id"], source_id);
    assert!(playback_body["PlaySessionId"].as_str().is_some());
    assert_eq!(
        playback_body["MediaSources"].as_array().map(Vec::len),
        Some(2)
    );
    assert_eq!(playback_body["MediaSources"][0]["Quality"], Value::Null);
    assert_eq!(playback_body["MediaSources"][1]["Quality"], "2160p");
    assert_eq!(playback_body["MediaSources"][0]["SupportsDirectPlay"], true);
    assert_eq!(
        playback_body["MediaSources"][0]["SupportsDirectStream"],
        true
    );
    assert_eq!(
        playback_body["MediaSources"][0]["SupportsTranscoding"],
        false
    );
    assert_eq!(
        playback_body["MediaSources"][0]["DirectStreamUrl"],
        format!("/Videos/{item_id}/stream.mkv?MediaSourceId={source_id}")
    );
    assert_eq!(
        playback_body["MediaSources"][0]["AddApiKeyToDirectStreamUrl"],
        true
    );
    let yamby_playback = client
        .get(format!("{base_url}/Items/{item_id}/PlaybackInfo"))
        .header(
            "X-Emby-Authorization",
            format!(
                "Emby UserId={},Client=Yamby,Device=Android,DeviceId=yamby-test,Version=2.0.5.5",
                admin.id
            ),
        )
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(yamby_playback.status(), reqwest::StatusCode::OK);
    let yamby_playback_body = yamby_playback.json::<Value>().await?;
    let yamby_direct_url = yamby_playback_body["MediaSources"][0]["DirectStreamUrl"]
        .as_str()
        .ok_or("missing Yamby direct stream URL")?;
    assert!(yamby_direct_url.contains("luxPlayback"));
    assert!(!yamby_direct_url.contains(&token));
    let yamby_stream = client
        .get(format!("{base_url}{yamby_direct_url}"))
        .send()
        .await?;
    assert_eq!(yamby_stream.status(), reqwest::StatusCode::OK);
    assert_eq!(yamby_stream.bytes().await?.as_ref(), b"0123456789");
    let tampered_yamby_url =
        yamby_direct_url.replacen("luxPlaybackSignature=", "luxPlaybackSignature=invalid", 1);
    let tampered_yamby_stream = client
        .get(format!("{base_url}{tampered_yamby_url}"))
        .send()
        .await?;
    assert_eq!(
        tampered_yamby_stream.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    let standard_stream = client
        .get(format!("{base_url}/Videos/{item_id}/stream.mkv"))
        .query(&[
            ("MediaSourceId", source_id.as_str()),
            ("api_key", token.as_str()),
        ])
        .send()
        .await?;
    assert_eq!(standard_stream.status(), reqwest::StatusCode::OK);
    assert_eq!(standard_stream.bytes().await?.as_ref(), b"0123456789");
    let selected_playback = client
        .get(format!("{base_url}/Items/{item_id}/PlaybackInfo"))
        .query(&[
            ("api_key", token.as_str()),
            ("mediaSourceId", high_source_id.as_str()),
        ])
        .send()
        .await?;
    assert_eq!(selected_playback.status(), reqwest::StatusCode::OK);
    let selected_body = selected_playback.json::<Value>().await?;
    assert_eq!(selected_body["MediaSources"][0]["Id"], high_source_id);
    assert_eq!(selected_body["MediaSources"][0]["Quality"], "2160p");

    let range_request = |start: u64, end: u64| {
        let client = client.clone();
        let url = format!("{base_url}/Videos/{item_id}/{high_source_id}/stream.mkv");
        let token = token.clone();
        async move {
            let mut response = client
                .get(url)
                .header("X-Emby-Token", token)
                .header(RANGE, format!("bytes={start}-{end}"))
                .send()
                .await?;
            assert_eq!(response.status(), reqwest::StatusCode::PARTIAL_CONTENT);
            assert_eq!(
                response.headers()[CONTENT_LENGTH],
                (end - start + 1).to_string()
            );
            assert_eq!(
                response.headers()[CONTENT_RANGE],
                format!("bytes {start}-{end}/8388608")
            );
            let mut received = 0_u64;
            while let Some(chunk) = response.chunk().await? {
                received = received.saturating_add(u64::try_from(chunk.len()).unwrap_or(0));
            }
            Ok::<u64, reqwest::Error>(received)
        }
    };
    let (range_one, range_two, range_three, range_four) = tokio::join!(
        range_request(0, 1_048_575),
        range_request(2_097_152, 3_145_727),
        range_request(4_194_304, 5_242_879),
        range_request(6_291_456, 7_340_031),
    );
    for result in [range_one, range_two, range_three, range_four] {
        assert_eq!(result?, 1_048_576);
    }

    let playback_post = client
        .post(format!("{base_url}/Items/{item_id}/PlaybackInfo"))
        .query(&[("api_key", token.as_str())])
        .send()
        .await?;
    assert_eq!(playback_post.status(), reqwest::StatusCode::OK);

    let source_route = client
        .get(format!(
            "{base_url}/Videos/{item_id}/{source_id}/stream.mkv"
        ))
        .query(&[("api_key", token.as_str())])
        .send()
        .await?;
    assert_eq!(source_route.status(), reqwest::StatusCode::OK);
    assert_eq!(source_route.bytes().await?.as_ref(), b"0123456789");

    let legacy_container_route = client
        .get(format!(
            "{base_url}/Videos/{item_id}/{source_id}/stream.matroska,webm"
        ))
        .query(&[("api_key", token.as_str())])
        .send()
        .await?;
    assert_eq!(legacy_container_route.status(), reqwest::StatusCode::OK);
    assert_eq!(
        legacy_container_route.bytes().await?.as_ref(),
        b"0123456789"
    );

    let head = client
        .head(&stream_url)
        .query(&[("api_key", token.as_str())])
        .send()
        .await?;
    assert_eq!(head.status(), reqwest::StatusCode::OK);
    assert_eq!(head.headers()[CONTENT_LENGTH], "10");
    assert!(head.bytes().await?.is_empty());

    let range = client
        .get(&stream_url)
        .header("X-Emby-Token", &token)
        .header("Range", "bytes=2-5")
        .send()
        .await?;
    assert_eq!(range.status(), reqwest::StatusCode::PARTIAL_CONTENT);
    assert_eq!(range.headers()[CONTENT_LENGTH], "4");
    assert_eq!(range.headers()[CONTENT_RANGE], "bytes 2-5/10");
    assert_eq!(range.bytes().await?.as_ref(), b"2345");

    let invalid = client
        .get(&stream_url)
        .header("X-Emby-Token", &token)
        .header("Range", "bytes=0-1,3-4")
        .send()
        .await?;
    assert_eq!(invalid.status(), reqwest::StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(invalid.headers()[CONTENT_RANGE], "bytes */10");

    let viewer_login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="PlaybackTest", Device="Mac", DeviceId="playback-viewer", Version="1""#,
        )
        .json(&json!({ "Username": "viewer", "Pw": "viewer password" }))
        .send()
        .await?;
    let viewer_token = viewer_login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing viewer token")?
        .to_owned();
    let denied = client
        .get(&stream_url)
        .header("X-Emby-Token", &viewer_token)
        .send()
        .await?;
    assert_eq!(denied.status(), reqwest::StatusCode::NOT_FOUND);

    let outside = temp_dir.path().join("outside.mkv");
    tokio::fs::write(&outside, b"outside").await?;
    tokio::fs::remove_file(&media_path).await?;
    symlink(&outside, &media_path)?;
    let escaped = client
        .get(&stream_url)
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(escaped.status(), reqwest::StatusCode::FORBIDDEN);

    server.abort();
    assert_ne!(admin.id, viewer.id);
    Ok(())
}

#[tokio::test]
async fn emby_playback_events_accept_vidhub_field_names_and_persist_progress()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "correct password").await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("VidHub Playback Test 2024.mkv"), b"video").await?;
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
    sqlx::query("UPDATE media_sources SET duration_ticks = 1000 WHERE id = ?")
        .bind(&source_id)
        .execute(database.pool())
        .await?;

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
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="VidHub", Device="Mac", DeviceId="vidhub-device", Version="2.1.8""#,
        )
        .json(&json!({ "Username": "admin", "Pw": "correct password" }))
        .send()
        .await?;
    let token = login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing admin token")?
        .to_owned();
    let playback_base = format!("{base_url}/Sessions/Playing");
    let common_event = json!({
        "mediaServerItemId": item_id,
        "mediaServerMediaSourceId": source_id,
        "mediaServerPlaySessionId": "vidhub-playback-session",
        "RunTimeTicks": 1_000,
        "deviceId": "vidhub-device",
        "client": "VidHub",
        "deviceName": "Mac",
    });

    let mut playing_event = common_event.clone();
    playing_event["playbackPositionTicks"] = json!(100);
    let playing = client
        .post(&playback_base)
        .header("X-Emby-Token", &token)
        .json(&playing_event)
        .send()
        .await?;
    assert_eq!(playing.status(), reqwest::StatusCode::NO_CONTENT);

    let sessions = client
        .get(format!("{base_url}/Sessions"))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(sessions.status(), reqwest::StatusCode::OK);
    let sessions_body = sessions.json::<Value>().await?;
    assert_eq!(sessions_body.as_array().map(Vec::len), Some(1));
    assert_eq!(sessions_body[0]["PlayState"]["PositionTicks"], 100);

    let mut progress_event = common_event.clone();
    progress_event["playbackPositionTicks"] = json!(200);
    progress_event["isPaused"] = json!(true);
    let progress = client
        .post(format!("{playback_base}/Progress"))
        .header("X-Emby-Token", &token)
        .json(&progress_event)
        .send()
        .await?;
    assert_eq!(progress.status(), reqwest::StatusCode::NO_CONTENT);

    let mut stopped_event = common_event;
    stopped_event["playbackPositionTicks"] = json!(300);
    let stopped = client
        .post(format!("{playback_base}/Stopped"))
        .header("X-Emby-Token", &token)
        .json(&stopped_event)
        .send()
        .await?;
    assert_eq!(stopped.status(), reqwest::StatusCode::NO_CONTENT);

    let item_response = client
        .get(format!("{base_url}/Users/{}/Items/{item_id}", admin.id))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(item_response.status(), reqwest::StatusCode::OK);
    let item_body = item_response.json::<Value>().await?;
    assert_eq!(item_body["UserData"]["PlaybackPositionTicks"], 300);
    assert_eq!(item_body["UserData"]["PlayedPercentage"], 30.0);
    let library_id = library.id.to_string();
    let items_response = client
        .get(format!("{base_url}/Users/{}/Items", admin.id))
        .header("X-Emby-Token", &token)
        .query(&[
            ("ParentId", library_id.as_str()),
            ("IncludeItemTypes", "Movie"),
        ])
        .send()
        .await?;
    assert_eq!(items_response.status(), reqwest::StatusCode::OK);
    let items_body = items_response.json::<Value>().await?;
    assert_eq!(
        items_body["Items"][0]["UserData"]["PlaybackPositionTicks"],
        300
    );
    assert_eq!(items_body["Items"][0]["UserData"]["PlayedPercentage"], 30.0);

    let session = sqlx::query_as::<_, (String, i64, Option<i64>, i64)>(
        "SELECT state, position_ticks, duration_ticks, is_paused
         FROM playback_sessions WHERE play_session_id = ?",
    )
    .bind("vidhub-playback-session")
    .fetch_one(database.pool())
    .await?;
    assert_eq!(session, ("STOPPED".to_owned(), 300, Some(1_000), 0));
    let item_state = sqlx::query_as::<_, (i64, i64)>(
        "SELECT position_ticks, is_played FROM user_item_state WHERE item_id = ?",
    )
    .bind(&item_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(item_state, (300, 0));

    sqlx::query(
        "INSERT INTO user_playback_settings (user_id, played_percent)
         VALUES (?, 80)
         ON CONFLICT(user_id) DO UPDATE SET played_percent = excluded.played_percent",
    )
    .bind(admin.id.to_string())
    .execute(database.pool())
    .await?;
    let completed = client
        .post(&playback_base)
        .header("X-Emby-Token", &token)
        .json(&json!({
            "mediaServerItemId": item_id,
            "mediaServerMediaSourceId": source_id,
            "mediaServerPlaySessionId": "vidhub-completed-session",
            "RunTimeTicks": 1_000,
            "playbackPositionTicks": 800,
            "deviceId": "vidhub-device",
            "client": "VidHub",
            "deviceName": "Mac",
        }))
        .send()
        .await?;
    assert_eq!(completed.status(), reqwest::StatusCode::NO_CONTENT);
    let completed_state = sqlx::query_as::<_, (i64, i64)>(
        "SELECT position_ticks, is_played FROM user_item_state WHERE item_id = ?",
    )
    .bind(&item_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(completed_state, (800, 1));

    server.abort();
    assert!(!admin.id.to_string().is_empty());
    Ok(())
}
