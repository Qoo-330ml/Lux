use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, scanner::LibraryScanner, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    network::RemoteAccessPolicy,
    storage::Database,
};
use reqwest::header::{AUTHORIZATION, COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use std::net::SocketAddr;
use tokio::net::TcpListener;

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

    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(
        AppState::ready(config, database.clone(), setup, auth, emby_auth)
            .with_remote_access_policy(RemoteAccessPolicy::from_cidrs(["127.0.0.1/32"])?),
    );
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
        "ItemId": item_id,
        "MediaSourceId": source_id,
        "PlaySessionId": "session-1",
        "PositionTicks": 100,
        "RunTimeTicks": 1000,
    });
    let playing = client
        .post(&event_url)
        .header("X-Emby-Token", &token)
        .header("x-lux-peer-ip", "203.0.113.9")
        .header("x-forwarded-for", "203.0.113.9")
        .json(&event)
        .send()
        .await?;
    assert_eq!(playing.status(), reqwest::StatusCode::NO_CONTENT);
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
            .json(&json!({
                "ItemId": high_item_id,
                "MediaSourceId": high_source_id,
                "PlaySessionId": "session-1",
                "PositionTicks": 900,
                "RunTimeTicks": 1000,
            }))
            .send()
            .await
    });
    let low = client
        .post(format!("{base_url}/Sessions/Playing/Progress"))
        .header("X-Emby-Token", &token)
        .json(&json!({
            "ItemId": event["ItemId"],
            "MediaSourceId": event["MediaSourceId"],
            "PlaySessionId": "session-1",
            "PositionTicks": 300,
            "RunTimeTicks": 1000,
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
    assert_eq!(sessions_body[0]["DeviceId"], "session-device");
    assert_eq!(sessions_body[0]["Client"], "SessionTest");
    assert_eq!(sessions_body[0]["DeviceName"], "Mac");
    assert_eq!(sessions_body[0]["DeviceType"], "Mac");
    assert_eq!(sessions_body[0]["ApplicationVersion"], "1");
    assert_eq!(sessions_body[0]["RemoteEndPoint"], "203.0.113.9");

    sqlx::query(
        "UPDATE playback_sessions
         SET last_event_at = unixepoch() - 3600
         WHERE play_session_id = ?",
    )
    .bind("session-1")
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

    let stopped = client
        .post(format!("{base_url}/Sessions/Playing/Stopped"))
        .header("X-Emby-Token", &token)
        .json(&json!({
            "ItemId": event["ItemId"],
            "MediaSourceId": event["MediaSourceId"],
            "PlaySessionId": "session-1",
            "PositionTicks": 900,
        }))
        .send()
        .await?;
    assert_eq!(stopped.status(), reqwest::StatusCode::NO_CONTENT);
    let stopped_again = client
        .post(format!("{base_url}/Sessions/Playing/Stopped"))
        .header("X-Emby-Token", &token)
        .json(&json!({
            "ItemId": event["ItemId"],
            "PlaySessionId": "session-1",
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

    let position: i64 = sqlx::query_scalar(
        "SELECT position_ticks FROM user_item_state
         WHERE item_id = ? LIMIT 1",
    )
    .bind(event["ItemId"].as_str().ok_or("missing item id")?)
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

    server.abort();
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
