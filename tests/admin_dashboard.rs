use std::{net::SocketAddr, time::Duration};

use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, scanner::LibraryScanner, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    network::RemoteAccessPolicy,
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
    let admin = setup.complete("Admin", "Admin", "correct password").await?;
    sqlx::query("UPDATE users SET can_remote_access = 1 WHERE id = ?")
        .bind(admin.id.to_string())
        .execute(database.pool())
        .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    let root = temp_dir.path().join("Shows");
    let season_dir = root.join("九门/Season 01");
    tokio::fs::create_dir_all(&season_dir).await?;
    tokio::fs::write(
        season_dir.join("张启山和吴老狗达成合作.S01E02.mkv"),
        b"video",
    )
    .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_series_library(library.id)
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
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'EPISODE' LIMIT 1")
            .fetch_one(database.pool())
            .await?;
    let playing = client
        .post(format!("{base_url}/Sessions/Playing"))
        .header("X-Emby-Token", &token)
        .header("X-Forwarded-For", "203.0.113.10")
        .json(&json!({
            "ItemId": item_id,
            "PlaySessionId": "dashboard-play-session",
            "PositionTicks": 600000000,
            "RunTimeTicks": 36_000_000_000i64,
            "DeviceId": "dashboard-device",
            "Client": "DashboardTest",
            "DeviceName": "Mac",
            "DeviceType": "Desktop",
            "ApplicationVersion": "4.2.1",
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
    assert_eq!(body["stats"]["movieCount"], 0);
    assert_eq!(body["stats"]["seriesCount"], 1);
    assert_eq!(body["stats"]["userCount"], 1);
    assert!(body["health"]["runtime"]["seconds"].is_number());
    assert_eq!(body["health"]["resources"]["cpu"]["source"], "cgroup");
    assert_eq!(
        body["health"]["resources"]["mediaStorage"]["path"],
        "/media"
    );
    assert_eq!(body["nowPlaying"][0]["userName"], "Admin");
    assert_eq!(body["nowPlaying"][0]["title"], "张启山和吴老狗达成合作");
    assert_eq!(body["nowPlaying"][0]["seriesTitle"], "九门");
    assert!(body["nowPlaying"][0]["seriesId"].is_string());
    assert_eq!(body["nowPlaying"][0]["client"], "DashboardTest");
    assert_eq!(body["nowPlaying"][0]["clientVersion"], "4.2.1");
    assert_eq!(body["nowPlaying"][0]["deviceName"], "Mac");
    assert_eq!(body["nowPlaying"][0]["deviceType"], "Desktop");
    assert_eq!(body["nowPlaying"][0]["deviceId"], "dashboard-device");
    assert_eq!(body["nowPlaying"][0]["remoteIp"], "203.0.113.10");
    assert!(body["nowPlaying"][0]["remoteIpLocation"].is_null());
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

    sqlx::query(
        "UPDATE playback_sessions
         SET last_event_at = unixepoch() - 3600
         WHERE play_session_id = ?",
    )
    .bind("dashboard-play-session")
    .execute(database.pool())
    .await?;
    let stale_dashboard = client
        .get(format!("{base_url}/api/v1/admin/dashboard"))
        .header(COOKIE, &cookies)
        .send()
        .await?
        .json::<Value>()
        .await?;
    assert!(
        stale_dashboard["nowPlaying"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );

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
