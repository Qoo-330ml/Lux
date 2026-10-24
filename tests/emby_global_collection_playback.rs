use std::{fs, os::unix::fs::PermissionsExt, path::Path, time::Duration};

use luxd::{
    api::{AppState, app_with_state},
    application::{
        libraries::LibraryService,
        probe::{FfprobeRunner, MediaProbeService},
        scanner::LibraryScanner,
        setup::SetupService,
    },
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::AUTHORIZATION;
use serde_json::{Value, json};
use tokio::net::TcpListener;

const GLOBAL_COLLECTION_LIBRARY_ID: &str = "00000000-0000-7000-8000-000000000538";

fn emby_public_id(id: &str) -> String {
    uuid::Uuid::parse_str(id)
        .map(|uuid| uuid.as_u128().to_string())
        .unwrap_or_else(|_| id.to_owned())
}

#[tokio::test]
async fn global_collection_playback_uses_visible_original_copy_for_video_and_subtitles()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let viewer = UserStore::new(database.clone())?
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?;
    let libraries = LibraryService::new(database.clone());
    let visible_library = libraries
        .create_library("Visible Movies", LibraryKind::Movie, false)
        .await?;
    let hidden_library = libraries
        .create_library("Hidden Movies", LibraryKind::Movie, false)
        .await?;
    sqlx::query(
        "INSERT INTO user_library_access (user_id, library_id, can_view)
         VALUES (?, ?, 1)",
    )
    .bind(viewer.id.to_string())
    .bind(visible_library.id.to_string())
    .execute(database.pool())
    .await?;
    sqlx::query("UPDATE users SET library_access_mode = 'SELECTED' WHERE id = ?")
        .bind(viewer.id.to_string())
        .execute(database.pool())
        .await?;

    let visible_root = temp_dir.path().join("visible");
    let hidden_root = temp_dir.path().join("hidden");
    tokio::fs::create_dir_all(&visible_root).await?;
    tokio::fs::create_dir_all(&hidden_root).await?;
    tokio::fs::write(
        visible_root.join("Collection Movie 2024.mkv"),
        b"visible collection copy",
    )
    .await?;
    tokio::fs::write(
        visible_root.join("Collection Movie 2024.en.srt"),
        b"1\n00:00:00,000 --> 00:00:01,000\nVisible subtitle\n",
    )
    .await?;
    tokio::fs::write(
        hidden_root.join("Collection Movie 2024.mkv"),
        b"hidden collection copy",
    )
    .await?;
    libraries
        .add_root(
            visible_library.id,
            visible_root.to_str().ok_or("non-utf8 path")?,
        )
        .await?;
    libraries
        .add_root(
            hidden_library.id,
            hidden_root.to_str().ok_or("non-utf8 path")?,
        )
        .await?;
    let scanner = LibraryScanner::new(database.clone());
    scanner.scan_movie_library(visible_library.id).await?;
    scanner.scan_movie_library(hidden_library.id).await?;
    let fake_ffprobe = executable_script(
        temp_dir.path(),
        r#"#!/bin/sh
printf '%s' '{"format":{"format_name":"matroska","duration":"10","bit_rate":"1000"},"streams":[{"index":0,"codec_type":"video","codec_name":"h264"},{"index":1,"codec_type":"audio","codec_name":"aac"}]}'
"#,
    )?;
    MediaProbeService::new(
        database.clone(),
        FfprobeRunner::new(fake_ffprobe, Duration::from_secs(5)),
    )
    .probe_movie_library(visible_library.id)
    .await?;
    let visible_item_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE'",
    )
    .bind(visible_library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    let hidden_item_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE library_id = ? AND item_type = 'MOVIE'",
    )
    .bind(hidden_library.id.to_string())
    .fetch_one(database.pool())
    .await?;
    let visible_source_id: String =
        sqlx::query_scalar("SELECT id FROM media_sources WHERE item_id = ?")
            .bind(&visible_item_id)
            .fetch_one(database.pool())
            .await?;
    let hidden_source_id: String =
        sqlx::query_scalar("SELECT id FROM media_sources WHERE item_id = ?")
            .bind(&hidden_item_id)
            .fetch_one(database.pool())
            .await?;
    let visible_subtitle_index: i64 = sqlx::query_scalar(
        "SELECT stream_index FROM media_streams
         WHERE media_source_id = ? AND is_external = 1 AND stream_type = 'SUBTITLE'",
    )
    .bind(&visible_source_id)
    .fetch_one(database.pool())
    .await?;

    let work_item_id = uuid::Uuid::now_v7().to_string();
    let collection_item_id = uuid::Uuid::now_v7().to_string();
    let collection_id = uuid::Uuid::now_v7().to_string();
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status,
             identity_key, has_available_source
         ) VALUES (?, ?, 'MOVIE', 'Collection Movie', 'collection movie',
                   'ONLINE_CONFIRMED', ?, 1)",
    )
    .bind(&work_item_id)
    .bind(GLOBAL_COLLECTION_LIBRARY_ID)
    .bind(format!("collection-playback:{work_item_id}"))
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO collection_work_items (work_item_id, movie_provider, movie_provider_id)
         VALUES (?, 'tmdb', 'collection-playback-movie')",
    )
    .bind(&work_item_id)
    .execute(database.pool())
    .await?;
    for (source_item_id, sort_order) in [(&visible_item_id, 0), (&hidden_item_id, 1)] {
        sqlx::query(
            "INSERT INTO collection_work_sources (work_item_id, source_item_id, sort_order)
             VALUES (?, ?, ?)",
        )
        .bind(&work_item_id)
        .bind(source_item_id)
        .bind(sort_order)
        .execute(database.pool())
        .await?;
    }
    sqlx::query(
        "INSERT INTO media_items (
             id, library_id, item_type, title, sort_title, identification_status,
             identity_key, has_available_source
         ) VALUES (?, ?, 'BOX_SET', 'Playback Collection', 'playback collection',
                   'ONLINE_CONFIRMED', ?, 1)",
    )
    .bind(&collection_item_id)
    .bind(GLOBAL_COLLECTION_LIBRARY_ID)
    .bind(format!("collection-playback:boxset:{collection_item_id}"))
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO collections (id, item_id, library_id, provider, provider_id, title)
         VALUES (?, ?, ?, 'tmdb', 'playback-collection', 'Playback Collection')",
    )
    .bind(&collection_id)
    .bind(&collection_item_id)
    .bind(GLOBAL_COLLECTION_LIBRARY_ID)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO collection_items (collection_id, item_id, sort_order)
         VALUES (?, ?, 1)",
    )
    .bind(&collection_id)
    .bind(&work_item_id)
    .execute(database.pool())
    .await?;

    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database.clone())?,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");
    let login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="CollectionPlayback", Device="Mac", DeviceId="collection-playback-visible", Version="1""#,
        )
        .json(&json!({ "Username": "viewer", "Pw": "viewer password" }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let token = login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing viewer token")?
        .to_owned();
    let work_public_id = emby_public_id(&work_item_id);
    let visible_item_public_id = emby_public_id(&visible_item_id);

    let playback_info = client
        .get(format!("{base_url}/Items/{work_public_id}/PlaybackInfo"))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(playback_info.status(), reqwest::StatusCode::OK);
    let playback_info: Value = playback_info.json().await?;
    assert_eq!(
        playback_info["MediaSources"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(playback_info["MediaSources"][0]["Id"], visible_source_id);
    assert_eq!(
        playback_info["MediaSources"][0]["ItemId"],
        visible_item_public_id
    );
    let direct_stream_url = playback_info["MediaSources"][0]["DirectStreamUrl"]
        .as_str()
        .ok_or("missing original-copy stream URL")?;
    assert!(direct_stream_url.starts_with(&format!(
        "/Videos/{visible_item_public_id}/stream.mkv?MediaSourceId={visible_source_id}"
    )));
    let stream = client
        .get(format!("{base_url}{direct_stream_url}"))
        .send()
        .await?;
    assert_eq!(stream.status(), reqwest::StatusCode::OK);
    assert_eq!(stream.bytes().await?.as_ref(), b"visible collection copy");

    let streams = playback_info["MediaSources"][0]["MediaStreams"]
        .as_array()
        .ok_or("missing original-copy media streams")?;
    assert!(streams.iter().any(|stream| {
        stream["Type"] == "Subtitle" && stream["Index"] == visible_subtitle_index
    }));
    let subtitle = client
        .get(format!(
            "{base_url}/Videos/{visible_item_public_id}/{visible_source_id}/Subtitles/{visible_subtitle_index}/Stream"
        ))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(subtitle.status(), reqwest::StatusCode::OK);
    assert!(String::from_utf8(subtitle.bytes().await?.to_vec())?.contains("Visible subtitle"));

    sqlx::query(
        "INSERT INTO user_library_access (user_id, library_id, can_view)
         VALUES (?, ?, 1)",
    )
    .bind(viewer.id.to_string())
    .bind(hidden_library.id.to_string())
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO user_collection_source_preferences
             (user_id, work_item_id, source_item_id, media_source_id)
         VALUES (?, ?, ?, ?)",
    )
    .bind(viewer.id.to_string())
    .bind(&work_item_id)
    .bind(&hidden_item_id)
    .bind(&hidden_source_id)
    .execute(database.pool())
    .await?;
    let hidden_item_public_id = emby_public_id(&hidden_item_id);
    let preferred_playback = client
        .get(format!("{base_url}/Items/{work_public_id}/PlaybackInfo"))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(preferred_playback.status(), reqwest::StatusCode::OK);
    let preferred_playback: Value = preferred_playback.json().await?;
    assert_eq!(
        preferred_playback["MediaSources"][0]["Id"],
        hidden_source_id
    );
    let selected_hidden_playback = client
        .get(format!("{base_url}/Items/{work_public_id}/PlaybackInfo"))
        .query(&[("MediaSourceId", hidden_source_id.as_str())])
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(selected_hidden_playback.status(), reqwest::StatusCode::OK);
    let selected_hidden_playback: Value = selected_hidden_playback.json().await?;
    assert_eq!(
        selected_hidden_playback["MediaSources"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    assert_eq!(
        selected_hidden_playback["MediaSources"][0]["Id"],
        hidden_source_id
    );
    assert_eq!(
        selected_hidden_playback["MediaSources"][0]["ItemId"],
        hidden_item_public_id
    );
    let hidden_stream_url = selected_hidden_playback["MediaSources"][0]["DirectStreamUrl"]
        .as_str()
        .ok_or("missing selected hidden-copy stream URL")?;
    assert!(hidden_stream_url.starts_with(&format!(
        "/Videos/{hidden_item_public_id}/stream.mkv?MediaSourceId={hidden_source_id}"
    )));
    let hidden_stream = client
        .get(format!("{base_url}{hidden_stream_url}"))
        .send()
        .await?;
    assert_eq!(hidden_stream.status(), reqwest::StatusCode::OK);
    assert_eq!(
        hidden_stream.bytes().await?.as_ref(),
        b"hidden collection copy"
    );

    let playback_events = format!("{base_url}/Sessions/Playing");
    let hidden_event = json!({
        "ItemId": work_public_id,
        "MediaSourceId": hidden_source_id,
        "PlaySessionId": "global-collection-hidden-session",
        "PositionTicks": 40_000_000,
        "RunTimeTicks": 100_000_000,
        "DeviceId": "collection-device-one",
    });
    let playing = client
        .post(&playback_events)
        .header("X-Emby-Token", &token)
        .json(&hidden_event)
        .send()
        .await?;
    assert_eq!(playing.status(), reqwest::StatusCode::NO_CONTENT);

    let mut wrong_device_event = hidden_event.clone();
    wrong_device_event["DeviceId"] = json!("collection-device-two");
    wrong_device_event["PositionTicks"] = json!(80_000_000);
    let wrong_device = client
        .post(format!("{playback_events}/Progress"))
        .header("X-Emby-Token", &token)
        .json(&wrong_device_event)
        .send()
        .await?;
    assert_eq!(wrong_device.status(), reqwest::StatusCode::NOT_FOUND);

    let mut wrong_source_event = hidden_event.clone();
    wrong_source_event["MediaSourceId"] = json!(visible_source_id);
    wrong_source_event["PositionTicks"] = json!(80_000_000);
    let wrong_source = client
        .post(format!("{playback_events}/Progress"))
        .header("X-Emby-Token", &token)
        .json(&wrong_source_event)
        .send()
        .await?;
    assert_eq!(wrong_source.status(), reqwest::StatusCode::NOT_FOUND);

    let mut progress_event = hidden_event.clone();
    progress_event["PositionTicks"] = json!(50_000_000);
    progress_event["IsPaused"] = json!(true);
    let progress = client
        .post(format!("{playback_events}/Progress"))
        .header("X-Emby-Token", &token)
        .json(&progress_event)
        .send()
        .await?;
    assert_eq!(progress.status(), reqwest::StatusCode::NO_CONTENT);
    let stored_hidden_session: (String, String, String, i64, i64) = sqlx::query_as(
        "SELECT item_id, media_source_id, device_id, position_ticks, is_paused
         FROM playback_sessions WHERE play_session_id = ?",
    )
    .bind("global-collection-hidden-session")
    .fetch_one(database.pool())
    .await?;
    assert_eq!(
        stored_hidden_session,
        (
            hidden_item_id.clone(),
            hidden_source_id.clone(),
            "collection-device-one".to_owned(),
            50_000_000,
            1,
        )
    );

    let visible_session = client
        .post(&playback_events)
        .header("X-Emby-Token", &token)
        .json(&json!({
            "ItemId": work_public_id,
            "MediaSourceId": visible_source_id,
            "PlaySessionId": "global-collection-visible-session",
            "PositionTicks": 30_000_000,
            "RunTimeTicks": 100_000_000,
            "DeviceId": "collection-device-two",
        }))
        .send()
        .await?;
    assert_eq!(visible_session.status(), reqwest::StatusCode::NO_CONTENT);
    let visible_session_item: String =
        sqlx::query_scalar("SELECT item_id FROM playback_sessions WHERE play_session_id = ?")
            .bind("global-collection-visible-session")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(visible_session_item, visible_item_id);

    let ambiguous_callback = client
        .post(&playback_events)
        .header("X-Emby-Token", &token)
        .json(&json!({
            "ItemId": work_public_id,
            "PlaySessionId": "global-collection-ambiguous-session",
            "PositionTicks": 10_000_000,
            "DeviceId": "collection-device-three",
        }))
        .send()
        .await?;
    assert_eq!(
        ambiguous_callback.status(),
        reqwest::StatusCode::BAD_REQUEST
    );
    let hidden_stopped = client
        .post(format!("{playback_events}/Stopped"))
        .header("X-Emby-Token", &token)
        .json(&json!({
            "ItemId": work_public_id,
            "MediaSourceId": hidden_source_id,
            "PlaySessionId": "global-collection-hidden-session",
            "PositionTicks": 60_000_000,
            "DeviceId": "collection-device-one",
        }))
        .send()
        .await?;
    assert_eq!(hidden_stopped.status(), reqwest::StatusCode::NO_CONTENT);
    let stopped_state: String =
        sqlx::query_scalar("SELECT state FROM playback_sessions WHERE play_session_id = ?")
            .bind("global-collection-hidden-session")
            .fetch_one(database.pool())
            .await?;
    assert_eq!(stopped_state, "STOPPED");

    sqlx::query("DELETE FROM user_library_access WHERE user_id = ? AND library_id = ?")
        .bind(viewer.id.to_string())
        .bind(hidden_library.id.to_string())
        .execute(database.pool())
        .await?;
    let revoked_playback = client
        .get(format!("{base_url}/Items/{work_public_id}/PlaybackInfo"))
        .query(&[("MediaSourceId", hidden_source_id.as_str())])
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(revoked_playback.status(), reqwest::StatusCode::NOT_FOUND);
    let revoked_stream = client
        .get(format!("{base_url}{hidden_stream_url}"))
        .send()
        .await?;
    assert_eq!(revoked_stream.status(), reqwest::StatusCode::NOT_FOUND);
    let old_callback = client
        .post(format!("{playback_events}/Progress"))
        .header("X-Emby-Token", &token)
        .json(&json!({
            "ItemId": work_public_id,
            "PlaySessionId": "global-collection-hidden-session",
            "PositionTicks": 90_000_000,
            "DeviceId": "collection-device-one",
        }))
        .send()
        .await?;
    assert_eq!(old_callback.status(), reqwest::StatusCode::NOT_FOUND);

    let unique_copy_callback = client
        .post(&playback_events)
        .header("X-Emby-Token", &token)
        .json(&json!({
            "ItemId": work_public_id,
            "PlaySessionId": "global-collection-unique-copy-session",
            "PositionTicks": 40_000_000,
            "RunTimeTicks": 100_000_000,
            "DeviceId": "collection-device-three",
        }))
        .send()
        .await?;
    assert_eq!(
        unique_copy_callback.status(),
        reqwest::StatusCode::NO_CONTENT
    );
    let visible_position: i64 = sqlx::query_scalar(
        "SELECT position_ticks FROM user_item_state WHERE user_id = ? AND item_id = ?",
    )
    .bind(viewer.id.to_string())
    .bind(&visible_item_id)
    .fetch_one(database.pool())
    .await?;
    let hidden_position: i64 = sqlx::query_scalar(
        "SELECT position_ticks FROM user_item_state WHERE user_id = ? AND item_id = ?",
    )
    .bind(viewer.id.to_string())
    .bind(&hidden_item_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(visible_position, 40_000_000);
    assert_eq!(hidden_position, 60_000_000);

    server.abort();
    assert_ne!(viewer.id.to_string(), visible_library.id.to_string());
    Ok(())
}

fn executable_script(
    directory: &Path,
    contents: &str,
) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let path = directory.join(format!("fake-ffprobe-{}", uuid::Uuid::now_v7()));
    fs::write(&path, contents)?;
    let mut permissions = fs::metadata(&path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions)?;
    Ok(path)
}
