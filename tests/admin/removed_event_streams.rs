//! LUX-897: the SSE endpoints are gone for good.
//!
//! `GET /api/v1/events` and `GET /api/v1/admin/events` were replaced by the
//! single WebSocket endpoint `GET /api/v1/ws` in the same release (ADR-054 D2).
//! These tests pin the hard cut from the outside: both paths answer `404` for an
//! anonymous client and for a signed-in administrator, instead of `401`/`200`.
//! A `401` would mean the old route still exists behind an auth check; a `200`
//! would mean a second realtime transport is still running.

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

const RETIRED_EVENT_STREAM_PATHS: [&str; 2] = ["/api/v1/events", "/api/v1/admin/events"];

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

async fn spawn_app() -> Result<(String, reqwest::Client), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:0".parse()?,
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
    // The temp dir must outlive the server, so it is leaked into the spawned
    // task and released when the runtime drops it with the process.
    tokio::spawn(async move {
        let _temp = temp_dir;
        axum::serve(listener, app).await
    });
    let client = reqwest::Client::builder().no_proxy().build()?;
    Ok((format!("http://{address}"), client))
}

#[tokio::test]
async fn retired_event_stream_routes_answer_404_without_a_session()
-> Result<(), Box<dyn std::error::Error>> {
    let (base, client) = spawn_app().await?;

    for path in RETIRED_EVENT_STREAM_PATHS {
        let response = client.get(format!("{base}{path}")).send().await?;
        assert_eq!(
            response.status(),
            reqwest::StatusCode::NOT_FOUND,
            "{path} was deleted with LUX-897; an unauthenticated request must not reach a handler"
        );
    }
    Ok(())
}

#[tokio::test]
async fn retired_event_stream_routes_answer_404_for_an_authenticated_administrator()
-> Result<(), Box<dyn std::error::Error>> {
    let (base, client) = spawn_app().await?;
    let login = client
        .post(format!("{base}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let cookies = format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(login.headers(), "lux_session"),
        cookie_value(login.headers(), "lux_csrf")
    );

    for path in RETIRED_EVENT_STREAM_PATHS {
        let response = client
            .get(format!("{base}{path}"))
            .header(COOKIE, &cookies)
            .send()
            .await?;
        assert_eq!(
            response.status(),
            reqwest::StatusCode::NOT_FOUND,
            "{path} must stay deleted for a signed-in administrator, not stream or 401"
        );
    }
    Ok(())
}
