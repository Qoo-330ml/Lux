use std::time::Duration;

use luxd::{
    api::{AppState, app_with_state},
    application::setup::SetupService,
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    storage::Database,
};
use reqwest::header::{COOKIE, SET_COOKIE};
use serde_json::json;
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
async fn admin_event_stream_requires_an_authenticated_admin()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(config, database, setup, auth, emby_auth));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    let response = reqwest::get(format!("http://{address}/api/v1/admin/events")).await?;

    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_event_stream_sends_ready_frame_to_an_admin() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(config, database, setup, auth, emby_auth));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;

    let login = client
        .post(format!("http://{address}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let cookies = format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(login.headers(), "lux_session"),
        cookie_value(login.headers(), "lux_csrf")
    );
    let csrf = cookie_value(login.headers(), "lux_csrf");
    let mut response = client
        .get(format!("http://{address}/api/v1/admin/events"))
        .header(COOKIE, cookies)
        .send()
        .await?;

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream; charset=utf-8")
    );
    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .and_then(|value| value.to_str().ok()),
        Some("no-cache, no-store, must-revalidate")
    );
    assert_eq!(
        response
            .headers()
            .get("x-accel-buffering")
            .and_then(|value| value.to_str().ok()),
        Some("no")
    );
    let first_chunk = response.chunk().await?.ok_or("missing ready frame")?;
    let first_chunk = String::from_utf8(first_chunk.to_vec())?;
    assert!(first_chunk.contains("event: ready"));
    assert!(first_chunk.contains("{\"version\":1}"));

    let update = client
        .patch(format!("http://{address}/api/v1/admin/settings"))
        .header(
            COOKIE,
            format!(
                "lux_session={}; lux_csrf={csrf}",
                cookie_value(login.headers(), "lux_session")
            ),
        )
        .header("X-CSRF-Token", &csrf)
        .json(&json!({ "serverName": "Updated Lux" }))
        .send()
        .await?;
    assert_eq!(update.status(), reqwest::StatusCode::OK);
    let event_chunk = tokio::time::timeout(Duration::from_secs(1), response.chunk())
        .await??
        .ok_or("missing settings invalidation")?;
    let event_chunk = String::from_utf8(event_chunk.to_vec())?;
    assert!(event_chunk.contains("event: invalidate"));
    assert!(event_chunk.contains("\"scope\":\"settings\""));

    server.abort();
    Ok(())
}

#[tokio::test]
async fn user_event_stream_is_authenticated_and_does_not_expose_admin_events()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(config, database, setup, auth, emby_auth));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;

    let unauthenticated = client
        .get(format!("http://{address}/api/v1/events"))
        .send()
        .await?;
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);

    let login = client
        .post(format!("http://{address}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let cookies = format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(login.headers(), "lux_session"),
        cookie_value(login.headers(), "lux_csrf")
    );
    let mut response = client
        .get(format!("http://{address}/api/v1/events"))
        .header(COOKIE, cookies)
        .send()
        .await?;

    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream; charset=utf-8")
    );
    let first_chunk = response.chunk().await?.ok_or("missing ready frame")?;
    let first_chunk = String::from_utf8(first_chunk.to_vec())?;
    assert!(first_chunk.contains("event: ready"));
    assert!(!first_chunk.contains("jobs"));
    assert!(!first_chunk.contains("currentItem"));

    server.abort();
    Ok(())
}
