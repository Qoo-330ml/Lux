use axum::Router;
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

struct TestServer {
    base_url: String,
    database: Database,
    server: tokio::task::JoinHandle<Result<(), std::io::Error>>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn test_server() -> Result<TestServer, Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.keep().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app: Router = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        auth,
        emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    Ok(TestServer {
        base_url: format!("http://{address}"),
        database,
        server,
    })
}

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
        .expect("expected cookie in test response")
}

async fn setup_and_login(
    client: &reqwest::Client,
    base_url: &str,
) -> Result<(String, String), Box<dyn std::error::Error>> {
    let setup = client
        .post(format!("{base_url}/api/v1/setup/complete"))
        .json(&json!({
            "username": "Admin",
            "displayName": "Administrator",
            "password": "correct password"
        }))
        .send()
        .await?;
    assert_eq!(setup.status(), reqwest::StatusCode::CREATED);

    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({
            "username": "admin",
            "password": "correct password"
        }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let session = cookie_value(login.headers(), "lux_session");
    let csrf = cookie_value(login.headers(), "lux_csrf");
    Ok((format!("lux_session={session}; lux_csrf={csrf}"), csrf))
}

#[tokio::test]
async fn creating_pairing_requires_web_session_and_csrf() -> Result<(), Box<dyn std::error::Error>>
{
    let server = test_server().await?;
    let client = reqwest::Client::new();
    let url = format!("{}/api/v1/auth/device-pairings", server.base_url);

    let anonymous = client.post(&url).send().await?;
    assert_eq!(anonymous.status(), reqwest::StatusCode::UNAUTHORIZED);

    let (cookies, csrf) = setup_and_login(&client, &server.base_url).await?;
    let missing_csrf = client.post(&url).header(COOKIE, &cookies).send().await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);
    assert_eq!(
        missing_csrf.json::<Value>().await?["error"]["code"],
        "CSRF_FAILED"
    );

    let created = client
        .post(&url)
        .header(COOKIE, cookies)
        .header("x-csrf-token", csrf)
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let body = created.json::<Value>().await?;
    assert!(body["pairingId"].as_str().is_some());
    assert!(body["secret"].as_str().is_some());
    assert!(body["expiresAt"].as_i64().is_some());
    Ok(())
}

#[tokio::test]
async fn pairing_redeem_returns_an_emby_access_token_once() -> Result<(), Box<dyn std::error::Error>>
{
    let server = test_server().await?;
    let client = reqwest::Client::new();
    let (cookies, csrf) = setup_and_login(&client, &server.base_url).await?;
    let created = client
        .post(format!("{}/api/v1/auth/device-pairings", server.base_url))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let created_body = created.json::<Value>().await?;
    let pairing_id = created_body["pairingId"].as_str().unwrap_or_default();
    let secret = created_body["secret"].as_str().unwrap_or_default();
    let redeem_url = format!(
        "{}/api/v1/device-pairings/{pairing_id}/redeem",
        server.base_url
    );
    let payload = json!({
        "secret": secret,
        "deviceId": "prism-device-1",
        "deviceName": "Test Desktop",
        "platform": "macOS",
        "version": "0.1.0"
    });

    let redeemed = client.post(&redeem_url).json(&payload).send().await?;
    let redeemed_status = redeemed.status();
    let redeemed_text = redeemed.text().await?;
    assert_eq!(redeemed_status, reqwest::StatusCode::OK, "{redeemed_text}");
    let redeemed_body = serde_json::from_str::<Value>(&redeemed_text)?;
    let access_token = redeemed_body["accessToken"].as_str().unwrap_or_default();
    assert!(!access_token.is_empty());
    assert_eq!(redeemed_body["serverId"], server.database.server_id());
    let user_id = redeemed_body["userId"].as_str().unwrap_or_default();

    let emby_me = client
        .get(format!("{}/Users/{user_id}", server.base_url))
        .header("X-Emby-Token", access_token)
        .send()
        .await?;
    assert_eq!(emby_me.status(), reqwest::StatusCode::OK);

    let replay = client.post(&redeem_url).json(&payload).send().await?;
    assert_eq!(replay.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(
        replay.json::<Value>().await?["error"]["code"],
        "DEVICE_PAIRING_CONSUMED"
    );
    Ok(())
}
