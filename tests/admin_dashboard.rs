use std::time::Duration;

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

fn cookie_value(headers: &reqwest::header::HeaderMap, name: &str) -> String {
    headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| {
            let (pair, _) = value.split_once(';')?;
            let (cookie_name, cookie_value) = pair.split_once('=')?;
            (cookie_name == name).then(|| cookie_value.to_owned())
        })
        .expect("expected cookie")
}

#[tokio::test]
async fn admin_dashboard_returns_server_playback_and_activity_data()
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
    tokio::fs::write(root.join("Session.Movie.2024.mkv"), b"video").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
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
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;

    let emby_login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            "Authorization",
            r#"Emby Client="DashboardTest", Device="Mac", DeviceId="dashboard-device", Version="1""#,
        )
        .json(&json!({ "Username": "admin", "Pw": "correct password" }))
        .send()
        .await?;
    assert_eq!(emby_login.status(), reqwest::StatusCode::OK);
    let emby_body = emby_login.text().await?;
    let token = serde_json::from_str::<Value>(&emby_body)?["AccessToken"]
        .as_str()
        .ok_or("missing Emby token")?
        .to_owned();
    let item_id: String = sqlx::query_scalar("SELECT id FROM media_items LIMIT 1")
        .fetch_one(database.pool())
        .await?;
    let playing = client
        .post(format!("{base_url}/Sessions/Playing"))
        .header("X-Emby-Token", &token)
        .json(&json!({
            "ItemId": item_id,
            "PlaySessionId": "dashboard-play-session",
            "PositionTicks": 600000000,
            "RunTimeTicks": 36_000_000_000i64,
            "DeviceId": "dashboard-device",
            "Client": "DashboardTest",
            "DeviceName": "Mac",
        }))
        .send()
        .await?;
    assert_eq!(playing.status(), reqwest::StatusCode::NO_CONTENT);

    let web_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let session_cookie = cookie_value(web_login.headers(), "lux_session");
    let csrf_cookie = cookie_value(web_login.headers(), "lux_csrf");
    let cookies = format!("lux_session={session_cookie}; lux_csrf={csrf_cookie}");
    let settings = client
        .patch(format!("{base_url}/api/v1/admin/settings"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf_cookie)
        .json(&json!({ "serverName": "客厅 Lux" }))
        .send()
        .await?;
    assert_eq!(settings.status(), reqwest::StatusCode::OK);

    let dashboard = client
        .get(format!("{base_url}/api/v1/admin/dashboard"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(dashboard.status(), reqwest::StatusCode::OK);
    let body: Value = dashboard.json().await?;
    assert_eq!(body["server"]["name"], "客厅 Lux");
    assert_eq!(body["server"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(body["nowPlaying"][0]["userName"], "Admin");
    assert_eq!(body["nowPlaying"][0]["title"], "Session Movie");
    assert_eq!(body["nowPlaying"][0]["client"], "DashboardTest");
    let events = body["activity"].as_array().ok_or("missing activity")?;
    assert!(
        events
            .iter()
            .any(|event| event["eventType"] == "AUTH_LOGIN")
    );
    assert!(
        events
            .iter()
            .any(|event| event["eventType"] == "PLAYBACK_STARTED")
    );
    assert!(events.len() <= 24);

    let stopped = client
        .post(format!("{base_url}/Sessions/Playing/Stopped"))
        .header("X-Emby-Token", &token)
        .json(&json!({
            "ItemId": body["nowPlaying"][0]["itemId"],
            "PlaySessionId": "dashboard-play-session",
            "PositionTicks": 600000000,
        }))
        .send()
        .await?;
    assert_eq!(stopped.status(), reqwest::StatusCode::NO_CONTENT);
    let after_stop = client
        .get(format!("{base_url}/api/v1/admin/dashboard"))
        .header(COOKIE, &cookies)
        .send()
        .await?
        .json::<Value>()
        .await?;
    assert!(
        after_stop["nowPlaying"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );
    assert!(after_stop["activity"].as_array().is_some_and(|events| {
        events
            .iter()
            .any(|event| event["eventType"] == "PLAYBACK_STOPPED")
    }));

    server.abort();
    Ok(())
}
