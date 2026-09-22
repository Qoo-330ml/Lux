use std::os::unix::fs::{PermissionsExt, symlink};

use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, scanner::LibraryScanner, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE};
use serde_json::{Value, json};
use tokio::net::TcpListener;

fn emby_public_id(id: &str) -> String {
    uuid::Uuid::parse_str(id)
        .map(|uuid| uuid.as_u128().to_string())
        .unwrap_or_else(|_| id.to_owned())
}

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
    let emby_item_id = emby_public_id(&item_id);
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
    let stream_url = format!("{base_url}/Videos/{emby_item_id}/stream");

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
    let generic_direct_url = playback_body["MediaSources"][0]["DirectStreamUrl"]
        .as_str()
        .ok_or("missing generic direct stream URL")?;
    assert!(generic_direct_url.starts_with(&format!(
        "/Videos/{emby_item_id}/stream.mkv?MediaSourceId={source_id}"
    )));
    assert!(generic_direct_url.contains("&PlaySessionId="));
    assert!(generic_direct_url.contains("&static=true"));
    assert!(generic_direct_url.contains("&UserId="));
    assert!(generic_direct_url.contains("&luxPlayback"));
    assert!(!generic_direct_url.contains(&token));
    assert_eq!(
        playback_body["MediaSources"][0]["AddApiKeyToDirectStreamUrl"],
        false
    );
    let generic_stream = client
        .get(format!("{base_url}{generic_direct_url}"))
        .header("User-Agent", "Hills/1.8.0 (android; 17)")
        .send()
        .await?;
    assert_eq!(generic_stream.status(), reqwest::StatusCode::OK);
    assert_eq!(generic_stream.bytes().await?.as_ref(), b"0123456789");
    let yamby_playback = client
        .get(format!("{base_url}/Items/{emby_item_id}/PlaybackInfo"))
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
        .get(format!("{base_url}/Videos/{emby_item_id}/stream.mkv"))
        .query(&[
            ("MediaSourceId", source_id.as_str()),
            ("api_key", token.as_str()),
        ])
        .send()
        .await?;
    assert_eq!(standard_stream.status(), reqwest::StatusCode::OK);
    assert_eq!(standard_stream.bytes().await?.as_ref(), b"0123456789");
    let selected_playback = client
        .get(format!("{base_url}/Items/{emby_item_id}/PlaybackInfo"))
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
        let url = format!("{base_url}/Videos/{emby_item_id}/{high_source_id}/stream.mkv");
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
        .post(format!("{base_url}/Items/{emby_item_id}/PlaybackInfo"))
        .query(&[("api_key", token.as_str())])
        .send()
        .await?;
    assert_eq!(playback_post.status(), reqwest::StatusCode::OK);

    let source_route = client
        .get(format!(
            "{base_url}/Videos/{emby_item_id}/{source_id}/stream.mkv"
        ))
        .query(&[("api_key", token.as_str())])
        .send()
        .await?;
    assert_eq!(source_route.status(), reqwest::StatusCode::OK);
    assert_eq!(source_route.bytes().await?.as_ref(), b"0123456789");

    let legacy_container_route = client
        .get(format!(
            "{base_url}/Videos/{emby_item_id}/{source_id}/stream.matroska,webm"
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
    let emby_item_id = emby_public_id(&item_id);
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
        "mediaServerItemId": emby_item_id,
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
        .get(format!(
            "{base_url}/Users/{}/Items/{emby_item_id}",
            admin.id
        ))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(item_response.status(), reqwest::StatusCode::OK);
    let item_body = item_response.json::<Value>().await?;
    assert_eq!(item_body["UserData"]["PlaybackPositionTicks"], 300);
    assert_eq!(item_body["UserData"]["PlayedPercentage"], 30.0);
    let library_id = emby_public_id(&library.id.to_string());
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
            "mediaServerItemId": emby_item_id,
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

    let hills_completed = client
        .post(format!("{playback_base}/Stopped"))
        .header("X-Emby-Token", &token)
        .json(&json!({
            "ItemId": emby_item_id,
            "MediaSourceId": source_id,
            "PlaySessionId": "hills-completed-session",
            "PositionTicks": 950,
        }))
        .send()
        .await?;
    assert_eq!(hills_completed.status(), reqwest::StatusCode::NO_CONTENT);
    let hills_completed_state = sqlx::query_as::<_, (i64, i64)>(
        "SELECT position_ticks, is_played FROM user_item_state WHERE item_id = ?",
    )
    .bind(&item_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(hills_completed_state, (950, 1));

    server.abort();
    assert!(!admin.id.to_string().is_empty());
    Ok(())
}

#[tokio::test]
async fn emby_playback_info_negotiates_server_transcoding_and_cleans_hls()
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
    tokio::fs::write(
        root.join("Emby Transcode Movie 2026.mkv"),
        b"not-a-real-video",
    )
    .await?;
    tokio::fs::write(
        root.join("Emby Transcode Remote 2026.strm"),
        "https://example.invalid/media/movie.mp4\n",
    )
    .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let (item_id, source_id): (String, String) = sqlx::query_as(
        "SELECT mi.id, ms.id
         FROM media_items mi
         JOIN media_sources ms ON ms.item_id = mi.id
         JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
         WHERE fe.relative_path = 'Emby Transcode Movie 2026.mkv'",
    )
    .fetch_one(database.pool())
    .await?;
    let expected_runtime_ticks = 58_943_360_000_i64;
    sqlx::query("UPDATE media_items SET runtime_ticks = ? WHERE id = ?")
        .bind(expected_runtime_ticks)
        .bind(&item_id)
        .execute(database.pool())
        .await?;
    sqlx::query("UPDATE media_sources SET bitrate = ? WHERE id = ?")
        .bind(4_000_000_i64)
        .bind(&source_id)
        .execute(database.pool())
        .await?;
    let emby_item_id = emby_public_id(&item_id);

    let fake_ffmpeg = temp_dir.path().join("fake-ffmpeg");
    tokio::fs::write(
        &fake_ffmpeg,
        "#!/bin/sh
set -eu
manifest=\"\"
segment=\"\"
init=\"init.mp4\"
extension=\"ts\"
start_number=0
while [ \"$#\" -gt 0 ]; do
  case \"$1\" in
    -hls_segment_filename) segment=\"$2\"; shift 2 ;;
    -hls_fmp4_init_filename) init=\"$2\"; shift 2 ;;
    -start_number) start_number=\"$2\"; shift 2 ;;
    *.m3u8) manifest=\"$1\"; shift ;;
    *) shift ;;
  esac
done
directory=$(dirname \"$manifest\")
mkdir -p \"$directory\"
printf '%s\\n' \"$start_number\" >> \"$directory/starts.log\"
case \"$segment\" in
  *.m4s)
    extension=\"m4s\"
    printf '#EXTM3U\\n#EXT-X-MAP:URI=\\\"%s\\\"\\n#EXTINF:1,\\nsegment_%06d.%s\\n' \"$init\" \"$start_number\" \"$extension\" > \"$manifest\"
    printf init > \"$directory/$init\"
    ;;
  *)
    printf '#EXTM3U\\n#EXTINF:1,\\nsegment_%06d.%s\\n' \"$start_number\" \"$extension\" > \"$manifest\"
    ;;
esac
segment_path=$(printf '%s' \"$segment\" | sed \"s/%06d/$(printf '%06d' \"$start_number\")/\")
printf 'segment-%s' \"$start_number\" > \"$segment_path\"
sleep 0.2
next_number=$((start_number + 1))
next_segment_path=$(printf '%s' \"$segment\" | sed \"s/%06d/$(printf '%06d' \"$next_number\")/\")
printf 'segment-%s' \"$next_number\" > \"$next_segment_path\"
",
    )
    .await?;
    let mut permissions = tokio::fs::metadata(&fake_ffmpeg).await?.permissions();
    permissions.set_mode(0o700);
    tokio::fs::set_permissions(&fake_ffmpeg, permissions).await?;

    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(
        AppState::ready(config.clone(), database.clone(), setup, auth, emby_auth)
            .with_hls_executable(fake_ffmpeg),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();
    let login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="PlaybackTest", Device="Mac", DeviceId="transcode-test", Version="1""#,
        )
        .json(&json!({ "Username": "admin", "Pw": "correct password" }))
        .send()
        .await?;
    let login_body = login.json::<Value>().await?;
    let token = login_body["AccessToken"]
        .as_str()
        .ok_or("missing admin token")?
        .to_owned();
    let user_id = login_body["User"]["Id"]
        .as_str()
        .ok_or("missing authenticated user id")?
        .to_owned();

    let direct = client
        .post(format!("{base_url}/Items/{emby_item_id}/PlaybackInfo"))
        .query(&[("api_key", token.as_str())])
        .json(&json!({
            "MediaSourceId": source_id,
            "EnableDirectPlay": true,
            "EnableDirectStream": true,
            "EnableTranscoding": true
        }))
        .send()
        .await?;
    assert_eq!(direct.status(), reqwest::StatusCode::OK);
    let direct_body = direct.json::<Value>().await?;
    assert_eq!(direct_body["MediaSources"][0]["SupportsTranscoding"], true);
    assert!(
        direct_body["MediaSources"][0]
            .get("TranscodingUrl")
            .is_none()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM web_playback_sessions WHERE plan = 'SERVER_HLS' AND state = 'ACTIVE'",
        )
        .fetch_one(database.pool())
        .await?,
        0
    );

    let profile_direct = client
        .post(format!("{base_url}/Items/{emby_item_id}/PlaybackInfo"))
        .query(&[("api_key", token.as_str())])
        .json(&json!({
            "MediaSourceId": source_id,
            "DeviceProfile": {
                "DirectPlayProfiles": [{
                    "Container": "mkv",
                    "VideoCodec": "h264,hevc",
                    "AudioCodec": "aac",
                    "Type": "Video"
                }],
                "TranscodingProfiles": [{
                    "Container": "mp4",
                    "VideoCodec": "h264",
                    "AudioCodec": "aac",
                    "Protocol": "hls",
                    "Type": "Video"
                }]
            }
        }))
        .send()
        .await?;
    assert_eq!(profile_direct.status(), reqwest::StatusCode::OK);
    let profile_direct_body = profile_direct.json::<Value>().await?;
    assert_eq!(
        profile_direct_body["MediaSources"][0]["SupportsDirectPlay"],
        true
    );
    assert_eq!(
        profile_direct_body["MediaSources"][0]["SupportsTranscoding"],
        true
    );
    assert!(
        profile_direct_body["MediaSources"][0]
            .get("TranscodingUrl")
            .is_none()
    );

    sqlx::query("DELETE FROM media_streams WHERE media_source_id = ?")
        .bind(&source_id)
        .execute(database.pool())
        .await?;
    sqlx::query(
        "UPDATE media_sources SET probe_status = 'PENDING', probe_error = NULL WHERE id = ?",
    )
    .bind(&source_id)
    .execute(database.pool())
    .await?;
    let unknown_stream_profile = client
        .post(format!("{base_url}/Items/{emby_item_id}/PlaybackInfo"))
        .query(&[("api_key", token.as_str())])
        .json(&json!({
            "MediaSourceId": source_id,
            "DeviceProfile": {
                "DirectPlayProfiles": [{
                    "Container": "mkv",
                    "VideoCodec": "h264",
                    "AudioCodec": "aac",
                    "Type": "Video"
                }],
                "TranscodingProfiles": [{
                    "Container": "mp4",
                    "VideoCodec": "h264",
                    "AudioCodec": "aac",
                    "Protocol": "hls",
                    "Type": "Video"
                }]
            }
        }))
        .send()
        .await?;
    assert_eq!(unknown_stream_profile.status(), reqwest::StatusCode::OK);
    let unknown_stream_profile_body = unknown_stream_profile.json::<Value>().await?;
    assert!(unknown_stream_profile_body["MediaSources"][0]["DirectStreamUrl"].is_string());
    assert!(
        unknown_stream_profile_body["MediaSources"][0]
            .get("TranscodingUrl")
            .is_none()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM web_playback_sessions WHERE plan = 'SERVER_HLS' AND state = 'ACTIVE'",
        )
        .fetch_one(database.pool())
        .await?,
        0
    );

    sqlx::query("UPDATE media_sources SET probe_status = 'READY', probe_error = NULL WHERE id = ?")
        .bind(&source_id)
        .execute(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO media_streams
         (id, media_source_id, stream_index, stream_type, codec, is_default)
         VALUES (?, ?, 0, 'VIDEO', 'hevc', 1), (?, ?, 1, 'AUDIO', 'aac', 1)",
    )
    .bind(format!("{source_id}-video"))
    .bind(&source_id)
    .bind(format!("{source_id}-audio"))
    .bind(&source_id)
    .execute(database.pool())
    .await?;

    sqlx::query(
        "INSERT INTO user_item_state (user_id, item_id, position_ticks, is_played)
         VALUES (?, ?, 49200000000, 0)
         ON CONFLICT(user_id, item_id) DO UPDATE SET
             position_ticks = excluded.position_ticks,
             is_played = excluded.is_played",
    )
    .bind(&user_id)
    .bind(&item_id)
    .execute(database.pool())
    .await?;

    let device_profile_transcoding = client
        .post(format!("{base_url}/Items/{emby_item_id}/PlaybackInfo"))
        .header(
            AUTHORIZATION,
            format!(
                r#"Emby Client="Harbor", Device="Desktop", DeviceId="harbor-device", Version="1", Token="{token}""#
            ),
        )
        .query(&[("api_key", token.as_str())])
        .json(&json!({
            "MediaSourceId": source_id,
            "AudioStreamIndex": 1,
            "SubtitleStreamIndex": -1,
            "MaxAudioChannels": 2,
            "DeviceProfile": {
                "DirectPlayProfiles": [{
                    "Container": "mp4",
                    "VideoCodec": "h264",
                    "AudioCodec": "aac",
                    "Type": "Video"
                }],
                "TranscodingProfiles": [{
                    "Container": "mp4",
                    "VideoCodec": "h264",
                    "AudioCodec": "aac",
                    "Protocol": "hls",
                    "Type": "Video"
                }]
            }
        }))
        .send()
        .await?;
    assert_eq!(device_profile_transcoding.status(), reqwest::StatusCode::OK);
    let device_profile_body = device_profile_transcoding.json::<Value>().await?;
    assert_eq!(
        device_profile_body["MediaSources"][0]["SupportsTranscoding"],
        true
    );
    assert_eq!(
        device_profile_body["MediaSources"][0]["SupportsDirectPlay"],
        false
    );
    assert_eq!(
        device_profile_body["MediaSources"][0]["SupportsDirectStream"],
        false
    );
    assert_eq!(
        device_profile_body["MediaSources"][0]["TranscodingContainer"],
        "mp4"
    );
    assert_eq!(
        device_profile_body["MediaSources"][0]["TranscodingMimeType"],
        "video/mp4"
    );
    assert_eq!(device_profile_body["RunTimeTicks"], expected_runtime_ticks);
    assert_eq!(
        device_profile_body["MediaSources"][0]["RunTimeTicks"],
        expected_runtime_ticks
    );
    assert_eq!(
        device_profile_body["MediaSources"][0]["DirectStreamUrl"],
        device_profile_body["MediaSources"][0]["TranscodingUrl"],
        "Emby keeps DirectStreamUrl aligned with the HLS transcoding URL"
    );
    let device_profile_url = device_profile_body["MediaSources"][0]["TranscodingUrl"]
        .as_str()
        .ok_or("missing DeviceProfile transcoding URL")?;
    assert!(device_profile_url.starts_with(&format!("/Videos/{emby_item_id}/master.m3u8?")));
    assert!(device_profile_url.contains("SegmentContainer=mp4"));
    for expected in [
        "DeviceId=harbor-device",
        "MediaSourceId=",
        "PlaySessionId=lux-emby%3A",
        "VideoCodec=h264",
        "AudioCodec=aac",
        "AudioStreamIndex=1",
        "SubtitleStreamIndex=-1",
        "TranscodingMaxAudioChannels=2",
        "StartTimeTicks=49200000000",
        "SegmentContainer=mp4",
        "MinSegments=1",
        "BreakOnNonKeyFrames=True",
    ] {
        assert!(
            device_profile_url.contains(expected),
            "missing standard Emby transcoding parameter {expected}: {device_profile_url}"
        );
    }
    assert!(!device_profile_url.contains("api_key="));
    let device_profile_play_session_id = device_profile_body["PlaySessionId"]
        .as_str()
        .ok_or("missing DeviceProfile play session")?
        .to_owned();
    let device_profile_hls_session_id: String = sqlx::query_scalar(
        "SELECT id FROM web_playback_sessions WHERE play_session_id = ? AND state = 'ACTIVE'",
    )
    .bind(&device_profile_play_session_id)
    .fetch_one(database.pool())
    .await?;
    let device_profile_starts = config
        .config_dir
        .join("web-playback")
        .join(device_profile_hls_session_id)
        .join("starts.log");
    assert!(
        !device_profile_starts.exists(),
        "PlaybackInfo must reserve the Emby HLS session without starting FFmpeg"
    );
    let device_profile_manifest = client
        .get(format!("{base_url}{device_profile_url}"))
        .send()
        .await?;
    assert_eq!(device_profile_manifest.status(), reqwest::StatusCode::OK);
    let device_profile_manifest = device_profile_manifest.text().await?;
    assert!(
        !device_profile_starts.exists(),
        "the synthetic Emby VOD manifest must not start FFmpeg"
    );
    assert!(device_profile_manifest.contains("#EXT-X-PLAYLIST-TYPE:VOD\n"));
    assert!(device_profile_manifest.contains("#EXT-X-ENDLIST\n"));
    assert!(device_profile_manifest.contains("#EXTINF:2.336000,"));
    let resumed_seek_segment_url = device_profile_manifest
        .lines()
        .find(|line| line.contains("segment_001230.m4s?"))
        .ok_or("missing signed resumed seek segment URL")?;
    let resumed_seek_segment = client
        .get(format!("{base_url}{resumed_seek_segment_url}"))
        .send()
        .await?;
    assert_eq!(resumed_seek_segment.status(), reqwest::StatusCode::OK);
    assert_eq!(
        resumed_seek_segment.bytes().await?.as_ref(),
        b"segment-1230"
    );
    let beginning_segment_url = device_profile_manifest
        .lines()
        .find(|line| line.contains("segment_000000.m4s?"))
        .ok_or("missing signed beginning segment URL")?;
    let restarted_from_beginning = client
        .get(format!("{base_url}{beginning_segment_url}"))
        .send()
        .await?;
    assert_eq!(
        restarted_from_beginning.bytes().await?.as_ref(),
        b"segment-0"
    );
    assert_eq!(
        tokio::fs::read_to_string(&device_profile_starts).await?,
        "1230\n0\n"
    );
    let device_profile_stopped = client
        .post(format!("{base_url}/Sessions/Playing/Stopped"))
        .query(&[("api_key", token.as_str())])
        .json(&json!({
            "ItemId": emby_item_id,
            "MediaSourceId": source_id,
            "PlaySessionId": device_profile_play_session_id
        }))
        .send()
        .await?;
    assert_eq!(
        device_profile_stopped.status(),
        reqwest::StatusCode::NO_CONTENT
    );

    let get_with_device_profile = client
        .get(format!("{base_url}/Items/{emby_item_id}/PlaybackInfo"))
        .query(&[("api_key", token.as_str())])
        .json(&json!({
            "MediaSourceId": source_id,
            "DeviceProfile": {
                "DirectPlayProfiles": [{
                    "Container": "mp4",
                    "VideoCodec": "h264",
                    "AudioCodec": "aac",
                    "Type": "Video"
                }],
                "TranscodingProfiles": [{
                    "Container": "mp4",
                    "VideoCodec": "h264",
                    "AudioCodec": "aac",
                    "Protocol": "hls",
                    "Type": "Video"
                }]
            }
        }))
        .send()
        .await?;
    assert_eq!(get_with_device_profile.status(), reqwest::StatusCode::OK);
    let get_with_device_profile_body = get_with_device_profile.json::<Value>().await?;
    assert!(
        get_with_device_profile_body["MediaSources"][0]
            .get("TranscodingUrl")
            .is_none()
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM web_playback_sessions WHERE plan = 'SERVER_HLS' AND state = 'ACTIVE'",
        )
        .fetch_one(database.pool())
        .await?,
        0
    );

    let get_with_force_transcoding = client
        .get(format!("{base_url}/Items/{emby_item_id}/PlaybackInfo"))
        .query(&[("api_key", token.as_str()), ("forceTranscode", "true")])
        .send()
        .await?;
    assert_eq!(get_with_force_transcoding.status(), reqwest::StatusCode::OK);
    let get_body = get_with_force_transcoding.json::<Value>().await?;
    assert!(get_body["MediaSources"][0].get("TranscodingUrl").is_none());
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM web_playback_sessions WHERE plan = 'SERVER_HLS' AND state = 'ACTIVE'",
        )
        .fetch_one(database.pool())
        .await?,
        0
    );

    let forced_transcoding = client
        .post(format!("{base_url}/Items/{emby_item_id}/PlaybackInfo"))
        .query(&[("api_key", token.as_str()), ("forceTranscode", "true")])
        .json(&json!({
            "MediaSourceId": source_id,
            "EnableDirectPlay": true,
            "EnableDirectStream": false
        }))
        .send()
        .await?;
    assert_eq!(forced_transcoding.status(), reqwest::StatusCode::OK);
    let forced_body = forced_transcoding.json::<Value>().await?;
    assert_eq!(forced_body["MediaSources"][0]["SupportsTranscoding"], true);
    let forced_url = forced_body["MediaSources"][0]["TranscodingUrl"]
        .as_str()
        .ok_or("missing forced transcoding URL")?;
    assert!(forced_url.starts_with(&format!("/Videos/{emby_item_id}/master.m3u8?")));
    let forced_play_session_id = forced_body["PlaySessionId"]
        .as_str()
        .ok_or("missing forced transcoding play session")?
        .to_owned();
    assert!(forced_play_session_id.starts_with("lux-emby:"));
    let forced_session_id = forced_play_session_id
        .strip_prefix("lux-emby:")
        .ok_or("invalid forced transcoding play session")?;
    let forced_stopped = client
        .post(format!("{base_url}/Sessions/Playing/Stopped"))
        .query(&[("api_key", token.as_str())])
        .json(&json!({
            "ItemId": emby_item_id,
            "MediaSourceId": source_id,
            "PlaySessionId": forced_play_session_id
        }))
        .send()
        .await?;
    assert_eq!(forced_stopped.status(), reqwest::StatusCode::NO_CONTENT);
    assert!(
        !config
            .config_dir
            .join("web-playback")
            .join(forced_session_id)
            .exists()
    );

    let transcoding = client
        .post(format!("{base_url}/Items/{emby_item_id}/PlaybackInfo"))
        .query(&[("api_key", token.as_str())])
        .json(&json!({
            "MediaSourceId": source_id,
            "MaxStreamingBitrate": 1_000_000,
            "EnableDirectStream": false,
            "EnableTranscoding": true,
            "AllowVideoStreamCopy": false,
            "AllowAudioStreamCopy": false
        }))
        .send()
        .await?;
    assert_eq!(transcoding.status(), reqwest::StatusCode::OK);
    let body = transcoding.json::<Value>().await?;
    assert_eq!(body["MediaSources"][0]["SupportsTranscoding"], true);
    assert_eq!(body["MediaSources"][0]["TranscodingSubProtocol"], "hls");
    assert_eq!(body["MediaSources"][0]["TranscodingContainer"], "ts");
    assert_eq!(body["MediaSources"][0]["TranscodingMimeType"], "video/mp2t");
    let transcoding_url = body["MediaSources"][0]["TranscodingUrl"]
        .as_str()
        .ok_or("missing transcoding URL")?;
    assert!(transcoding_url.starts_with(&format!("/Videos/{emby_item_id}/master.m3u8?")));
    assert!(transcoding_url.contains("SegmentContainer=ts"));
    assert!(!transcoding_url.contains(&token));
    let play_session_id = body["PlaySessionId"]
        .as_str()
        .ok_or("missing transcoding play session")?;
    assert!(play_session_id.starts_with("lux-emby:"));
    let session_id = play_session_id
        .strip_prefix("lux-emby:")
        .ok_or("invalid transcoding play session")?;
    let stored_output = sqlx::query_as::<
        _,
        (
            Option<String>,
            Option<String>,
            Option<i64>,
            Option<i64>,
            Option<String>,
        ),
    >(
        "SELECT video_codec, audio_codec, video_bitrate, audio_bitrate, transcoding_container
         FROM web_playback_sessions WHERE id = ?",
    )
    .bind(session_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(stored_output.0.as_deref(), Some("h264"));
    assert_eq!(stored_output.1.as_deref(), Some("aac"));
    assert_eq!(stored_output.2, Some(1_000_000));
    assert_eq!(stored_output.3, Some(192_000));
    assert_eq!(stored_output.4.as_deref(), Some("ts"));

    let stale_stop_without_session_id = client
        .post(format!("{base_url}/Sessions/Playing/Stopped"))
        .query(&[("api_key", token.as_str())])
        .json(&json!({
            "ItemId": emby_item_id,
            "MediaSourceId": source_id,
            "PositionTicks": 0
        }))
        .send()
        .await?;
    assert_eq!(
        stale_stop_without_session_id.status(),
        reqwest::StatusCode::NO_CONTENT
    );

    let playing = client
        .post(format!("{base_url}/Sessions/Playing"))
        .query(&[("api_key", token.as_str())])
        .json(&json!({
            "ItemId": emby_item_id,
            "MediaSourceId": source_id,
            "PlaySessionId": play_session_id,
            "PositionTicks": 0,
            "RunTimeTicks": expected_runtime_ticks,
        }))
        .send()
        .await?;
    assert_eq!(playing.status(), reqwest::StatusCode::NO_CONTENT);
    let sessions = client
        .get(format!("{base_url}/Sessions"))
        .query(&[("api_key", token.as_str())])
        .send()
        .await?
        .json::<Value>()
        .await?;
    let current_session = sessions
        .as_array()
        .and_then(|sessions| {
            sessions
                .iter()
                .find(|session| session["PlaySessionId"] == play_session_id)
        })
        .ok_or("missing transcoding session")?;
    assert_eq!(current_session["PlayState"]["PlayMethod"], "Transcode");
    assert_eq!(current_session["TranscodingInfo"]["Container"], "ts");
    assert_eq!(current_session["TranscodingInfo"]["VideoCodec"], "h264");
    assert_eq!(current_session["TranscodingInfo"]["AudioCodec"], "aac");
    assert_eq!(
        current_session["TranscodingInfo"]["VideoBitrate"],
        1_000_000
    );
    assert_eq!(current_session["TranscodingInfo"]["AudioBitrate"], 192_000);

    let manifest = client
        .get(format!("{base_url}{transcoding_url}"))
        .send()
        .await?;
    assert_eq!(manifest.status(), reqwest::StatusCode::OK);
    let manifest = manifest.text().await?;
    assert!(manifest.contains("#EXT-X-PLAYLIST-TYPE:VOD\n"));
    assert!(manifest.contains("#EXT-X-ENDLIST\n"));
    assert_eq!(manifest.matches("segment_").count(), 1_474);
    assert_eq!(manifest.matches("#EXT-X-MAP:").count(), 0);
    assert!(manifest.contains("/segment_000000.ts?"));
    assert!(manifest.contains("/segment_001230.ts?"));
    assert!(manifest.contains("#EXTINF:4.000000,"));
    assert!(manifest.contains("#EXTINF:2.336000,"));
    let segment_url = manifest
        .lines()
        .find(|line| line.contains("segment_000001.ts?"))
        .ok_or("missing signed segment URL")?;
    let second_segment_url = manifest
        .lines()
        .filter(|line| line.contains("/transcoding/") && line.contains(".ts?"))
        .nth(1)
        .ok_or("missing signed second segment URL")?;
    let segment = client
        .get(format!("{base_url}{segment_url}"))
        .send()
        .await?;
    assert_eq!(segment.status(), reqwest::StatusCode::OK);
    assert_eq!(segment.headers()[CONTENT_TYPE], "video/mp2t");
    assert_eq!(segment.bytes().await?.as_ref(), b"segment-1");
    let tampered_container = segment_url.replace("SegmentContainer=ts", "SegmentContainer=mp4");
    assert_eq!(
        client
            .get(format!("{base_url}{tampered_container}"))
            .send()
            .await?
            .status(),
        reqwest::StatusCode::NOT_FOUND
    );
    let second_segment = client
        .get(format!("{base_url}{second_segment_url}"))
        .send()
        .await?;
    assert_eq!(second_segment.status(), reqwest::StatusCode::OK);
    assert_eq!(second_segment.bytes().await?.as_ref(), b"segment-1");

    let before_heartbeat: i64 =
        sqlx::query_scalar("SELECT expires_at FROM web_playback_sessions WHERE id = ?")
            .bind(session_id)
            .fetch_one(database.pool())
            .await?;
    let playing = client
        .post(format!("{base_url}/Sessions/Playing"))
        .query(&[("api_key", token.as_str())])
        .json(&json!({
            "ItemId": emby_item_id,
            "MediaSourceId": source_id,
            "PlaySessionId": play_session_id,
            "PositionTicks": 0
        }))
        .send()
        .await?;
    assert_eq!(playing.status(), reqwest::StatusCode::NO_CONTENT);
    let after_heartbeat: i64 =
        sqlx::query_scalar("SELECT expires_at FROM web_playback_sessions WHERE id = ?")
            .bind(session_id)
            .fetch_one(database.pool())
            .await?;
    assert!(after_heartbeat >= before_heartbeat);

    let wrong_item = transcoding_url.replace(&format!("/Videos/{emby_item_id}/"), "/Videos/0/");
    assert_eq!(
        client
            .get(format!("{base_url}{wrong_item}"))
            .send()
            .await?
            .status(),
        reqwest::StatusCode::NOT_FOUND
    );
    let tampered = transcoding_url.replacen("luxPlaybackSignature=", "luxPlaybackSignature=x", 1);
    assert_eq!(
        client
            .get(format!("{base_url}{tampered}"))
            .send()
            .await?
            .status(),
        reqwest::StatusCode::NOT_FOUND
    );

    let stopped = client
        .post(format!("{base_url}/Sessions/Playing/Stopped"))
        .query(&[("api_key", token.as_str())])
        .json(&json!({
            "ItemId": emby_item_id,
            "MediaSourceId": source_id,
            "PlaySessionId": play_session_id,
            "PositionTicks": 0
        }))
        .send()
        .await?;
    assert_eq!(stopped.status(), reqwest::StatusCode::NO_CONTENT);
    assert!(
        !config
            .config_dir
            .join("web-playback")
            .join(session_id)
            .exists()
    );

    let (strm_item_id, strm_source_id): (String, String) = sqlx::query_as(
        "SELECT mi.id, ms.id
         FROM media_items mi
         JOIN media_sources ms ON ms.item_id = mi.id
         JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
         WHERE fe.relative_path = 'Emby Transcode Remote 2026.strm'",
    )
    .fetch_one(database.pool())
    .await?;
    let strm_response = client
        .post(format!(
            "{base_url}/Items/{}/PlaybackInfo",
            emby_public_id(&strm_item_id)
        ))
        .query(&[("api_key", token.as_str())])
        .json(&json!({
            "MediaSourceId": strm_source_id,
            "EnableTranscoding": true
        }))
        .send()
        .await?;
    assert_eq!(strm_response.status(), reqwest::StatusCode::OK);
    let strm_body = strm_response.json::<Value>().await?;
    assert_eq!(strm_body["MediaSources"][0]["SupportsTranscoding"], true);
    assert!(strm_body["MediaSources"][0].get("TranscodingUrl").is_none());
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM web_playback_sessions WHERE plan = 'SERVER_HLS' AND state = 'ACTIVE'",
        )
        .fetch_one(database.pool())
        .await?,
        0
    );

    server.abort();
    Ok(())
}
