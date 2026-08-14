use luxd::{
    api::{AppState, app_with_state},
    application::setup::SetupService,
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    storage::Database,
};
use reqwest::header::{COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use tokio::net::TcpListener;

#[tokio::test]
async fn user_playback_threshold_defaults_to_95_and_is_csrf_protected()
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
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();

    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let session = cookie_value(&login, "lux_session")?;
    let csrf = cookie_value(&login, "lux_csrf")?;
    let cookies = format!("lux_session={session}; lux_csrf={csrf}");

    let defaults = client
        .get(format!("{base_url}/api/v1/auth/settings"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(defaults.status(), reqwest::StatusCode::OK);
    assert_eq!(defaults.json::<Value>().await?["playedPercent"], 95);

    let missing_csrf = client
        .patch(format!("{base_url}/api/v1/auth/settings"))
        .header(COOKIE, &cookies)
        .json(&json!({ "playedPercent": 80 }))
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);

    let updated = client
        .patch(format!("{base_url}/api/v1/auth/settings"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({ "playedPercent": 80 }))
        .send()
        .await?;
    assert_eq!(updated.status(), reqwest::StatusCode::OK);
    assert_eq!(updated.json::<Value>().await?["playedPercent"], 80);

    let invalid = client
        .patch(format!("{base_url}/api/v1/auth/settings"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", &csrf)
        .json(&json!({ "playedPercent": 101 }))
        .send()
        .await?;
    assert_eq!(invalid.status(), reqwest::StatusCode::BAD_REQUEST);

    server.abort();
    Ok(())
}

fn cookie_value(
    response: &reqwest::Response,
    name: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    response
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .find_map(|value| {
            let value = value.to_str().ok()?;
            let value = value.strip_prefix(&format!("{name}="))?;
            Some(value.split(';').next()?.to_owned())
        })
        .ok_or_else(|| format!("missing {name} cookie").into())
}
