use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, scanner::LibraryScanner, setup::SetupService},
    auth::{
        admin_api_key::AdminApiKeyService, emby::EmbyAuthService, sessions::WebAuthService,
        users::UserStore,
    },
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{AUTHORIZATION, COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use std::net::SocketAddr;
use tokio::net::TcpListener;

fn emby_public_id(id: &str) -> String {
    uuid::Uuid::parse_str(id)
        .map(|uuid| uuid.as_u128().to_string())
        .unwrap_or_else(|_| id.to_owned())
}

#[tokio::test]
async fn playback_events_are_idempotent_and_positions_never_regress()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "correct password").await?;
    sqlx::query("UPDATE users SET can_remote_access = 1 WHERE id = ?")
        .bind(admin.id.to_string())
        .execute(database.pool())
        .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Session.Movie.2024.mkv"), b"video").await?;
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
    // Some Lux items legitimately have no year metadata. Emby session
    // consumers still expect the numeric field to be present.
    sqlx::query("UPDATE media_items SET production_year = NULL WHERE id = ?")
        .bind(&item_id)
        .execute(database.pool())
        .await?;
    sqlx::query(
        "UPDATE media_items
         SET original_title = ?, overview = ?, provider_ids_json = ?
         WHERE id = ?",
    )
    .bind("Session Movie Original")
    .bind("A session-card metadata fixture.")
    .bind(r#"{"tmdb":"12345"}"#)
    .bind(&item_id)
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO item_images (id, item_id, image_type, image_index, local_path, source)
         VALUES (?, ?, ?, ?, ?, ?), (?, ?, ?, ?, ?, ?)",
    )
    .bind("session-primary-image")
    .bind(&item_id)
    .bind("POSTER")
    .bind(0_i64)
    .bind("poster.jpg")
    .bind("LOCAL")
    .bind("session-backdrop-image")
    .bind(&item_id)
    .bind("FANART")
    .bind(0_i64)
    .bind("backdrop.jpg")
    .bind("LOCAL")
    .execute(database.pool())
    .await?;
    let emby_item_id = emby_public_id(&item_id);

    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let admin_api_key = AdminApiKeyService::new(config.config_dir.clone(), database.clone())
        .rotate()
        .await?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        auth,
        emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
    });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();
    let login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="SessionTest", Device="Mac", DeviceId="session-device", Version="1""#,
        )
        .json(&json!({ "Username": "admin", "Pw": "correct password" }))
        .send()
        .await?;
    let token = login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing token")?
        .to_owned();
    let event_url = format!("{base_url}/Sessions/Playing");
    let event = json!({
        "ItemId": emby_item_id,
        "MediaSourceId": source_id,
        "PlaySessionId": "lux-emby:session-1",
        "PositionTicks": 100,
        "RunTimeTicks": 5000,
    });
    let playing = client
        .post(&event_url)
        .header("X-Emby-Token", &token)
        .header("x-lux-peer-ip", "203.0.113.9")
        .header("x-forwarded-for", "203.0.113.9")
        .header("Content-Type", "text/plain")
        .body(serde_json::to_vec(&event)?)
        .send()
        .await?;
    assert_eq!(playing.status(), reqwest::StatusCode::NO_CONTENT);
    let invalid_playing = client
        .post(&event_url)
        .header("X-Emby-Token", &token)
        .header("Content-Type", "text/plain")
        .body("not-json")
        .send()
        .await?;
    assert_eq!(invalid_playing.status(), reqwest::StatusCode::BAD_REQUEST);
    let duplicate = client
        .post(&event_url)
        .header("X-Emby-Token", &token)
        .json(&event)
        .send()
        .await?;
    assert_eq!(duplicate.status(), reqwest::StatusCode::NO_CONTENT);

    let progress_url = format!("{base_url}/Sessions/Playing/Progress");
    let high = client.clone();
    let high_token = token.clone();
    let high_item_id = event["ItemId"].clone();
    let high_source_id = event["MediaSourceId"].clone();
    let high_request = tokio::spawn(async move {
        high.post(progress_url)
            .header("X-Emby-Token", high_token)
            .header("Content-Type", "text/plain")
            .body(
                serde_json::to_vec(&json!({
                    "ItemId": high_item_id,
                    "MediaSourceId": high_source_id,
                    "PlaySessionId": "lux-emby:session-1",
                    "PositionTicks": 900,
                    "RunTimeTicks": 5000,
                }))
                .expect("playback progress fixture should serialize"),
            )
            .send()
            .await
    });
    let low = client
        .post(format!("{base_url}/Sessions/Playing/Progress"))
        .header("X-Emby-Token", &token)
        .json(&json!({
            "ItemId": event["ItemId"],
            "MediaSourceId": event["MediaSourceId"],
            "PlaySessionId": "lux-emby:session-1",
            "PositionTicks": 300,
            "RunTimeTicks": 5000,
        }))
        .send();
    let (high, low) = tokio::join!(high_request, low);
    assert_eq!(high??.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(low?.status(), reqwest::StatusCode::NO_CONTENT);

    let sessions = client
        .get(format!("{base_url}/Sessions"))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(sessions.status(), reqwest::StatusCode::OK);
    let sessions_body = sessions.json::<Value>().await?;
    assert_eq!(sessions_body.as_array().map(Vec::len), Some(1));
    assert_eq!(sessions_body[0]["PlayState"]["PositionTicks"], 900);
    assert_eq!(sessions_body[0]["PlayState"]["PlayMethod"], "Transcode");
    assert_eq!(sessions_body[0]["NowPlayingItem"]["Id"], emby_item_id);
    assert_eq!(sessions_body[0]["UserName"], "Admin");
    assert_eq!(sessions_body[0]["NowPlayingItem"]["Name"], "Session Movie");
    assert_eq!(sessions_body[0]["NowPlayingItem"]["ProductionYear"], 0);
    assert_eq!(
        sessions_body[0]["NowPlayingItem"]["OriginalTitle"],
        "Session Movie Original"
    );
    assert_eq!(
        sessions_body[0]["NowPlayingItem"]["Overview"],
        "A session-card metadata fixture."
    );
    assert_eq!(
        sessions_body[0]["NowPlayingItem"]["ImageTags"]["Primary"],
        "session-primary-image"
    );
    assert_eq!(
        sessions_body[0]["NowPlayingItem"]["BackdropImageTags"],
        json!(["session-backdrop-image"])
    );
    assert_eq!(
        sessions_body[0]["NowPlayingItem"]["ProviderIds"]["Tmdb"],
        "12345"
    );
    assert_eq!(sessions_body[0]["NowPlayingItem"]["SeriesId"], "");
    assert_eq!(sessions_body[0]["NowPlayingItem"]["SeasonName"], "");
    assert!(
        sessions_body[0]["LastActivityDate"]
            .as_str()
            .is_some_and(|value| value.ends_with('Z') && value.contains('T'))
    );
    assert_eq!(sessions_body[0]["UserPrimaryImageTag"], Value::Null);
    assert_eq!(sessions_body[0]["NowPlayingItem"]["RunTimeTicks"], 1000);
    assert_eq!(sessions_body[0]["RunTimeTicks"], 1000);
    assert_eq!(sessions_body[0]["DeviceId"], "session-device");
    assert_eq!(sessions_body[0]["Client"], "SessionTest");
    assert_eq!(sessions_body[0]["DeviceName"], "Mac");
    assert_eq!(sessions_body[0]["DeviceType"], "Mac");
    assert_eq!(sessions_body[0]["ApplicationVersion"], "1");
    assert_eq!(sessions_body[0]["RemoteEndPoint"], "203.0.113.9");

    sqlx::query("UPDATE playback_sessions SET duration_ticks = 5000 WHERE play_session_id = ?")
        .bind("lux-emby:session-1")
        .execute(database.pool())
        .await?;
    let sessions_after_legacy_duration = client
        .get(format!("{base_url}/Sessions"))
        .header("X-Emby-Token", &token)
        .send()
        .await?
        .json::<Value>()
        .await?;
    let legacy_duration_session = sessions_after_legacy_duration
        .as_array()
        .and_then(|sessions| {
            sessions
                .iter()
                .find(|session| session["PlaySessionId"] == "lux-emby:session-1")
        })
        .ok_or("missing session with legacy duration")?;
    assert_eq!(legacy_duration_session["RunTimeTicks"], 1000);
    assert_eq!(
        legacy_duration_session["NowPlayingItem"]["RunTimeTicks"],
        1000
    );

    sqlx::query("UPDATE media_sources SET duration_ticks = NULL WHERE id = ?")
        .bind(&source_id)
        .execute(database.pool())
        .await?;
    sqlx::query("UPDATE media_items SET runtime_ticks = ? WHERE id = ?")
        .bind(8_000_i64)
        .bind(&item_id)
        .execute(database.pool())
        .await?;
    let missing_duration = client
        .post(&event_url)
        .header("X-Emby-Token", &token)
        .json(&json!({
            "ItemId": emby_item_id,
            "MediaSourceId": source_id,
            "PlaySessionId": "session-without-duration",
            "PositionTicks": 100,
        }))
        .send()
        .await?;
    assert_eq!(missing_duration.status(), reqwest::StatusCode::NO_CONTENT);
    let sessions_with_fallback = client
        .get(format!("{base_url}/Sessions"))
        .header("X-Emby-Token", &token)
        .send()
        .await?
        .json::<Value>()
        .await?;
    let fallback_session = sessions_with_fallback
        .as_array()
        .and_then(|sessions| {
            sessions
                .iter()
                .find(|session| session["PlaySessionId"] == "session-without-duration")
        })
        .ok_or("missing session without duration")?;
    assert_eq!(fallback_session["NowPlayingItem"]["RunTimeTicks"], 8_000);
    assert_eq!(fallback_session["RunTimeTicks"], 8_000);
    let stop_fallback = client
        .post(format!("{event_url}/Stopped"))
        .header("X-Emby-Token", &token)
        .json(&json!({
            "ItemId": emby_item_id,
            "MediaSourceId": source_id,
            "PlaySessionId": "session-without-duration",
            "PositionTicks": 100,
        }))
        .send()
        .await?;
    assert_eq!(stop_fallback.status(), reqwest::StatusCode::NO_CONTENT);

    sqlx::query(
        "UPDATE playback_sessions
         SET last_event_at = unixepoch() - 3600
         WHERE play_session_id = ?",
    )
    .bind("lux-emby:session-1")
    .execute(database.pool())
    .await?;
    let stale_sessions = client
        .get(format!("{base_url}/Sessions"))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(stale_sessions.status(), reqwest::StatusCode::OK);
    assert_eq!(
        stale_sessions
            .json::<Value>()
            .await?
            .as_array()
            .map(Vec::len),
        Some(0)
    );

    sqlx::query(
        "UPDATE playback_sessions
         SET last_event_at = unixepoch() - 120
         WHERE play_session_id = ?",
    )
    .bind("lux-emby:session-1")
    .execute(database.pool())
    .await?;
    let sessions_with_explicit_window = client
        .get(format!("{base_url}/Sessions?ActiveWithinSeconds=300"))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(
        sessions_with_explicit_window.status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        sessions_with_explicit_window
            .json::<Value>()
            .await?
            .as_array()
            .map(Vec::len),
        Some(0)
    );

    let stopped = client
        .post(format!("{base_url}/Sessions/Playing/Stopped"))
        .header("X-Emby-Token", &token)
        .header("Content-Type", "text/plain")
        .body(serde_json::to_vec(&json!({
            "ItemId": event["ItemId"],
            "MediaSourceId": event["MediaSourceId"],
            "PlaySessionId": "lux-emby:session-1",
            "PositionTicks": 900,
        }))?)
        .send()
        .await?;
    assert_eq!(stopped.status(), reqwest::StatusCode::NO_CONTENT);
    let stopped_again = client
        .post(format!("{base_url}/Sessions/Playing/Stopped"))
        .header("X-Emby-Token", &token)
        .json(&json!({
            "ItemId": event["ItemId"],
            "PlaySessionId": "lux-emby:session-1",
            "PositionTicks": 800,
        }))
        .send()
        .await?;
    assert_eq!(stopped_again.status(), reqwest::StatusCode::NO_CONTENT);
    let sessions_after_stop = client
        .get(format!("{base_url}/Sessions"))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(
        sessions_after_stop
            .json::<Value>()
            .await?
            .as_array()
            .map(Vec::len),
        Some(0)
    );

    let direct_playing = client
        .post(&event_url)
        .header("X-Emby-Token", &token)
        .json(&json!({
            "ItemId": emby_item_id,
            "MediaSourceId": source_id,
            "PlaySessionId": "direct-playback-session",
            "PositionTicks": 100,
            "RunTimeTicks": 1000,
        }))
        .send()
        .await?;
    assert_eq!(direct_playing.status(), reqwest::StatusCode::NO_CONTENT);
    let direct_sessions = client
        .get(format!("{base_url}/Sessions"))
        .header("X-Emby-Token", &token)
        .send()
        .await?
        .json::<Value>()
        .await?;
    let direct_session = direct_sessions
        .as_array()
        .and_then(|sessions| {
            sessions
                .iter()
                .find(|session| session["PlaySessionId"] == "direct-playback-session")
        })
        .ok_or("missing direct playback session")?;
    assert_eq!(direct_session["UserName"], "Admin");
    assert_eq!(direct_session["NowPlayingItem"]["Name"], "Session Movie");
    assert_eq!(direct_session["TranscodingInfo"]["Bitrate"], 0);
    assert_eq!(direct_session["TranscodingInfo"]["VideoBitrate"], 0);
    assert_eq!(direct_session["TranscodingInfo"]["AudioBitrate"], 0);
    let direct_stopped = client
        .post(format!("{event_url}/Stopped"))
        .header("X-Emby-Token", &token)
        .json(&json!({
            "ItemId": emby_item_id,
            "MediaSourceId": source_id,
            "PlaySessionId": "direct-playback-session",
            "PositionTicks": 100,
        }))
        .send()
        .await?;
    assert_eq!(direct_stopped.status(), reqwest::StatusCode::NO_CONTENT);

    let explicit_stop_playing = client
        .post(&event_url)
        .header("X-Emby-Token", &token)
        .json(&json!({
            "ItemId": emby_item_id,
            "MediaSourceId": source_id,
            "PlaySessionId": "explicit-stop-session",
            "DeviceId": "stop-device",
            "PositionTicks": 100,
            "RunTimeTicks": 1000,
        }))
        .send()
        .await?;
    assert_eq!(
        explicit_stop_playing.status(),
        reqwest::StatusCode::NO_CONTENT
    );
    let implicit_stopped = client
        .post(format!("{event_url}/Stopped"))
        .header("X-Lux-Api-Key", &admin_api_key)
        .json(&json!({
            "ItemId": emby_item_id,
            "MediaSourceId": source_id,
            "PositionTicks": 100,
        }))
        .send()
        .await?;
    assert_eq!(implicit_stopped.status(), reqwest::StatusCode::NO_CONTENT);
    let sessions_after_implicit_stop = client
        .get(format!("{base_url}/Sessions"))
        .header("X-Emby-Token", &token)
        .send()
        .await?
        .json::<Value>()
        .await?;
    assert!(
        !sessions_after_implicit_stop
            .as_array()
            .is_some_and(|sessions| {
                sessions
                    .iter()
                    .any(|session| session["PlaySessionId"] == "explicit-stop-session")
            })
    );

    let position: i64 = sqlx::query_scalar(
        "SELECT position_ticks FROM user_item_state
         WHERE item_id = ? LIMIT 1",
    )
    .bind(&item_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(position, 900);

    let web_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    assert_eq!(web_login.status(), reqwest::StatusCode::OK);
    let web_session = cookie_value(web_login.headers(), "lux_session")?;
    let web_csrf = cookie_value(web_login.headers(), "lux_csrf")?;
    let web_cookie = format!("lux_session={web_session}; lux_csrf={web_csrf}");
    let web_progress_url = format!("{base_url}/api/v1/items/{item_id}/progress");

    let web_playing = client
        .post(&web_progress_url)
        .header(COOKIE, &web_cookie)
        .header("X-CSRF-Token", &web_csrf)
        .json(&json!({
            "positionTicks": 1_000,
            "durationTicks": 2_000,
            "state": "PLAYING",
        }))
        .send()
        .await?;
    assert_eq!(web_playing.status(), reqwest::StatusCode::NO_CONTENT);

    let web_paused = client
        .post(&web_progress_url)
        .header(COOKIE, &web_cookie)
        .header("X-CSRF-Token", &web_csrf)
        .json(&json!({
            "positionTicks": 1_200,
            "durationTicks": 2_000,
            "state": "PAUSED",
        }))
        .send()
        .await?;
    assert_eq!(web_paused.status(), reqwest::StatusCode::NO_CONTENT);

    let web_playback = client
        .get(format!("{base_url}/api/v1/items/{item_id}/playback"))
        .header(COOKIE, &web_cookie)
        .send()
        .await?;
    assert_eq!(web_playback.status(), reqwest::StatusCode::OK);
    let web_playback_body = web_playback.json::<Value>().await?;
    assert_eq!(web_playback_body["positionTicks"], 1_200);
    assert_eq!(web_playback_body["state"], "PAUSED");
    assert_eq!(web_playback_body["isPaused"], true);

    let web_sessions = client
        .get(format!("{base_url}/Sessions"))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    let web_session_body = web_sessions
        .json::<Value>()
        .await?
        .as_array()
        .and_then(|sessions| {
            sessions.iter().find(|session| {
                session["PlaySessionId"]
                    .as_str()
                    .is_some_and(|value| value.starts_with("lux-web:"))
            })
        })
        .cloned()
        .ok_or("missing web playback session")?;
    assert_eq!(web_session_body["PlayState"]["IsPaused"], true);

    sqlx::query(
        "UPDATE playback_sessions
         SET last_event_at = unixepoch() - 3600
         WHERE item_id = ? AND device_id = 'lux-web'",
    )
    .bind(&item_id)
    .execute(database.pool())
    .await?;
    let stale_web_playback = client
        .get(format!("{base_url}/api/v1/items/{item_id}/playback"))
        .header(COOKIE, &web_cookie)
        .send()
        .await?;
    let stale_web_playback_body = stale_web_playback.json::<Value>().await?;
    assert_eq!(stale_web_playback_body["state"], Value::Null);

    let web_stopped = client
        .post(&web_progress_url)
        .header(COOKIE, &web_cookie)
        .header("X-CSRF-Token", &web_csrf)
        .json(&json!({
            "positionTicks": 1_200,
            "durationTicks": 2_000,
            "state": "STOPPED",
        }))
        .send()
        .await?;
    assert_eq!(web_stopped.status(), reqwest::StatusCode::NO_CONTENT);

    let web_playback_after_stop = client
        .get(format!("{base_url}/api/v1/items/{item_id}/playback"))
        .header(COOKIE, &web_cookie)
        .send()
        .await?;
    let web_playback_after_stop_body = web_playback_after_stop.json::<Value>().await?;
    assert_eq!(web_playback_after_stop_body["positionTicks"], 1_200);
    assert_eq!(web_playback_after_stop_body["state"], Value::Null);

    let header_playing = client
        .post(&event_url)
        .header(
            "X-Emby-Authorization",
            format!(
                r##"MediaBrowser Client="HeaderClient", Device="AppleTV", DeviceId="header-device", Version="2", Token="{token}""##
            ),
        )
        .json(&json!({
            "ItemId": event["ItemId"],
            "PlaySessionId": "header-session",
            "PositionTicks": 100,
        }))
        .send()
        .await?;
    assert_eq!(header_playing.status(), reqwest::StatusCode::NO_CONTENT);
    let header_sessions = client
        .get(format!("{base_url}/Sessions"))
        .header("X-Emby-Token", &token)
        .send()
        .await?
        .json::<Value>()
        .await?;
    let header_session = header_sessions
        .as_array()
        .and_then(|sessions| {
            sessions
                .iter()
                .find(|session| session["PlaySessionId"].as_str() == Some("header-session"))
        })
        .ok_or("missing header playback session")?;
    assert_eq!(header_session["Client"], "HeaderClient");
    assert_eq!(header_session["DeviceName"], "AppleTV");
    assert_eq!(header_session["DeviceId"], "header-device");
    assert_eq!(header_session["DeviceType"], "AppleTV");
    assert_eq!(header_session["ApplicationVersion"], "2");

    for (play_session_id, device_id) in [
        ("ambiguous-stop-session-a", "ambiguous-device-a"),
        ("ambiguous-stop-session-b", "ambiguous-device-b"),
    ] {
        let ambiguous_playing = client
            .post(&event_url)
            .header("X-Emby-Token", &token)
            .json(&json!({
                "ItemId": emby_item_id,
                "MediaSourceId": source_id,
                "PlaySessionId": play_session_id,
                "DeviceId": device_id,
                "PositionTicks": 100,
                "RunTimeTicks": 1000,
            }))
            .send()
            .await?;
        assert_eq!(ambiguous_playing.status(), reqwest::StatusCode::NO_CONTENT);
    }
    let ambiguous_stopped = client
        .post(format!("{event_url}/Stopped"))
        .header("X-Lux-Api-Key", &admin_api_key)
        .json(&json!({
            "ItemId": emby_item_id,
            "MediaSourceId": source_id,
            "PositionTicks": 100,
        }))
        .send()
        .await?;
    assert_eq!(ambiguous_stopped.status(), reqwest::StatusCode::NO_CONTENT);
    let sessions_after_ambiguous_stop = client
        .get(format!("{base_url}/Sessions"))
        .header("X-Emby-Token", &token)
        .send()
        .await?
        .json::<Value>()
        .await?;
    let active_session_ids = sessions_after_ambiguous_stop
        .as_array()
        .ok_or("sessions response is not an array")?;
    assert!(
        active_session_ids
            .iter()
            .any(|session| { session["PlaySessionId"] == "ambiguous-stop-session-a" })
    );
    assert!(
        active_session_ids
            .iter()
            .any(|session| { session["PlaySessionId"] == "ambiguous-stop-session-b" })
    );

    server.abort();
    Ok(())
}

#[tokio::test]
async fn emby_session_stop_by_session_id_stops_only_owned_sessions_and_is_idempotent()
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
    tokio::fs::write(root.join("Session.Stop.2024.mkv"), b"video").await?;
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
    let admin_api_key = AdminApiKeyService::new(config.config_dir.clone(), database.clone())
        .rotate()
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
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
    });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();
    let item_id = emby_public_id(&item_id);
    let playing = client
        .post(format!("{base_url}/Sessions/Playing"))
        .header("X-Emby-Token", &admin_api_key)
        .json(&json!({
            "ItemId": item_id,
            "MediaSourceId": source_id,
            "PlaySessionId": "session-command-stop",
            "PositionTicks": 100,
            "RunTimeTicks": 1_000,
        }))
        .send()
        .await?;
    assert_eq!(playing.status(), reqwest::StatusCode::NO_CONTENT);

    let sessions = client
        .get(format!("{base_url}/Sessions"))
        .header("X-Emby-Token", &admin_api_key)
        .send()
        .await?
        .json::<Value>()
        .await?;
    let session_id = sessions
        .as_array()
        .and_then(|sessions| {
            sessions.iter().find_map(|session| {
                (session["PlaySessionId"] == "session-command-stop")
                    .then(|| session["Id"].as_str())
                    .flatten()
            })
        })
        .ok_or("missing session id")?
        .to_owned();

    let viewer = UserStore::new(database.clone())?
        .create_user("Viewer", "Viewer", "viewer password", false)
        .await?;
    let viewer_login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .json(&json!({
            "Username": "viewer",
            "Pw": "viewer password"
        }))
        .send()
        .await?;
    assert_eq!(viewer_login.status(), reqwest::StatusCode::OK);
    let viewer_token = viewer_login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing viewer token")?
        .to_owned();
    let forbidden = client
        .post(format!("{base_url}/Sessions/{session_id}/Playing/Stop"))
        .header("X-Emby-Token", &viewer_token)
        .send()
        .await?;
    assert_eq!(forbidden.status(), reqwest::StatusCode::NOT_FOUND);

    let stopped = client
        .post(format!(
            "{base_url}/emby/Sessions/{session_id}/Playing/Stop"
        ))
        .header("X-Emby-Token", &admin_api_key)
        .send()
        .await?;
    assert_eq!(stopped.status(), reqwest::StatusCode::NO_CONTENT);

    let sessions_after_stop = client
        .get(format!("{base_url}/Sessions"))
        .header("X-Emby-Token", &admin_api_key)
        .send()
        .await?
        .json::<Value>()
        .await?;
    assert!(
        !sessions_after_stop
            .as_array()
            .is_some_and(|sessions| sessions.iter().any(|session| session["Id"] == session_id))
    );
    let stopped_again = client
        .post(format!("{base_url}/Sessions/{session_id}/Playing/Stop"))
        .header("X-Emby-Token", &admin_api_key)
        .send()
        .await?;
    assert_eq!(stopped_again.status(), reqwest::StatusCode::NO_CONTENT);

    let unknown = client
        .post(format!("{base_url}/Sessions/unknown-session/Playing/Stop"))
        .header("X-Emby-Token", &admin_api_key)
        .send()
        .await?;
    assert_eq!(unknown.status(), reqwest::StatusCode::NOT_FOUND);
    let state: String = sqlx::query_scalar("SELECT state FROM playback_sessions WHERE id = ?")
        .bind(&session_id)
        .fetch_one(database.pool())
        .await?;
    assert_eq!(state, "STOPPED");
    assert_eq!(viewer.id.to_string().len(), 36);
    server.abort();
    assert_eq!(admin.id.to_string().len(), 36);
    Ok(())
}

fn cookie_value(
    headers: &reqwest::header::HeaderMap,
    name: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| {
            let (pair, _) = value.split_once(';')?;
            let (cookie_name, cookie_value) = pair.split_once('=')?;
            (cookie_name == name).then(|| cookie_value.to_owned())
        })
        .ok_or_else(|| format!("missing {name} cookie").into())
}
