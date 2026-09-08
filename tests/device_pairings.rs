use axum::Router;
use luxd::{
    api::{AppState, app_with_state},
    application::setup::SetupService,
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    storage::Database,
};
use reqwest::header::{COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use sqlx::Row;
use tokio::net::TcpListener;

struct TestServer {
    base_url: String,
    database: Database,
    _temp_dir: tempfile::TempDir,
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
        config_dir: temp_dir.path().join("config"),
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
        _temp_dir: temp_dir,
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

    login(client, base_url, "admin", "correct password").await
}

async fn login(
    client: &reqwest::Client,
    base_url: &str,
    username: &str,
    password: &str,
) -> Result<(String, String), Box<dyn std::error::Error>> {
    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": username, "password": password }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let session = cookie_value(login.headers(), "lux_session");
    let csrf = cookie_value(login.headers(), "lux_csrf");
    Ok((format!("lux_session={session}; lux_csrf={csrf}"), csrf))
}

async fn create_pairing(
    client: &reqwest::Client,
    base_url: &str,
    cookies: &str,
    csrf: &str,
) -> Result<Value, Box<dyn std::error::Error>> {
    let response = client
        .post(format!("{base_url}/api/v1/auth/device-pairings"))
        .header(COOKIE, cookies)
        .header("x-csrf-token", csrf)
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::CREATED);
    Ok(response.json().await?)
}

fn redeem_payload(secret: &str) -> Value {
    json!({
        "secret": secret,
        "deviceId": "prism-device-1",
        "deviceName": "Test Desktop",
        "platform": "macOS",
        "version": "0.1.0"
    })
}

#[tokio::test]
async fn creating_pairing_requires_web_session_and_csrf() -> Result<(), Box<dyn std::error::Error>>
{
    let server = test_server().await?;
    let client = reqwest::Client::new();
    let url = format!("{}/api/v1/auth/device-pairings", server.base_url);

    let anonymous = client.post(&url).send().await?;
    assert_eq!(anonymous.status(), reqwest::StatusCode::UNAUTHORIZED);
    let api_key_only = client
        .post(&url)
        .header("x-lux-api-key", "shared-key-is-not-a-web-session")
        .send()
        .await?;
    assert_eq!(api_key_only.status(), reqwest::StatusCode::UNAUTHORIZED);

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
    let expires_at = body["expiresAt"].as_i64().ok_or("missing expiry")?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs() as i64;
    assert!((299..=300).contains(&(expires_at - now)));
    Ok(())
}

#[tokio::test]
async fn pairing_redeem_returns_an_emby_access_token_once() -> Result<(), Box<dyn std::error::Error>>
{
    let server = test_server().await?;
    let client = reqwest::Client::new();
    let (cookies, csrf) = setup_and_login(&client, &server.base_url).await?;
    let created_body = create_pairing(&client, &server.base_url, &cookies, &csrf).await?;
    let pairing_id = created_body["pairingId"].as_str().unwrap_or_default();
    let secret = created_body["secret"].as_str().unwrap_or_default();
    let redeem_url = format!(
        "{}/api/v1/device-pairings/{pairing_id}/redeem",
        server.base_url
    );
    let payload = redeem_payload(secret);

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

#[tokio::test]
async fn pairing_secret_is_hashed_and_invalid_secret_does_not_consume_it()
-> Result<(), Box<dyn std::error::Error>> {
    let server = test_server().await?;
    let client = reqwest::Client::new();
    let (cookies, csrf) = setup_and_login(&client, &server.base_url).await?;
    let created = create_pairing(&client, &server.base_url, &cookies, &csrf).await?;
    let pairing_id = created["pairingId"].as_str().ok_or("missing pairing ID")?;
    let secret = created["secret"].as_str().ok_or("missing secret")?;

    let stored_hash: Vec<u8> =
        sqlx::query_scalar("SELECT secret_hash FROM device_pairings WHERE id = ?")
            .bind(pairing_id)
            .fetch_one(server.database.pool())
            .await?;
    assert_eq!(stored_hash.len(), 32);
    assert_ne!(stored_hash, secret.as_bytes());

    let redeem_url = format!(
        "{}/api/v1/device-pairings/{pairing_id}/redeem",
        server.base_url
    );
    let invalid = client
        .post(&redeem_url)
        .json(&redeem_payload("not-the-secret"))
        .send()
        .await?;
    assert_eq!(invalid.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(
        invalid.json::<Value>().await?["error"]["code"],
        "DEVICE_PAIRING_INVALID_SECRET"
    );
    let consumed_at: Option<i64> =
        sqlx::query_scalar("SELECT consumed_at FROM device_pairings WHERE id = ?")
            .bind(pairing_id)
            .fetch_one(server.database.pool())
            .await?;
    assert!(consumed_at.is_none());

    let redeemed = client
        .post(&redeem_url)
        .json(&redeem_payload(secret))
        .send()
        .await?;
    assert_eq!(redeemed.status(), reqwest::StatusCode::OK);
    let body = redeemed.json::<Value>().await?;
    let access_token = body["accessToken"].as_str().ok_or("missing access token")?;
    let token_hash = {
        use sha2::{Digest, Sha256};
        Sha256::digest(access_token.as_bytes()).to_vec()
    };
    let token_row = sqlx::query(
        "SELECT token_hash, client_name, device_name, client_version, device_type
         FROM access_tokens WHERE token_hash = ?",
    )
    .bind(token_hash)
    .fetch_one(server.database.pool())
    .await?;
    let stored_token_hash: Vec<u8> = token_row.try_get("token_hash")?;
    assert_eq!(stored_token_hash.len(), 32);
    assert_eq!(token_row.try_get::<String, _>("client_name")?, "Lux Prism");
    assert_eq!(
        token_row.try_get::<String, _>("device_name")?,
        "Test Desktop"
    );
    assert_eq!(token_row.try_get::<String, _>("client_version")?, "0.1.0");
    assert_eq!(
        token_row
            .try_get::<Option<String>, _>("device_type")?
            .as_deref(),
        Some("macOS")
    );
    Ok(())
}

#[tokio::test]
async fn pairing_expiry_and_cancellation_are_enforced() -> Result<(), Box<dyn std::error::Error>> {
    let server = test_server().await?;
    let client = reqwest::Client::new();
    let (cookies, csrf) = setup_and_login(&client, &server.base_url).await?;

    let expired = create_pairing(&client, &server.base_url, &cookies, &csrf).await?;
    let expired_id = expired["pairingId"].as_str().ok_or("missing pairing ID")?;
    let expired_secret = expired["secret"].as_str().ok_or("missing secret")?;
    sqlx::query("UPDATE device_pairings SET expires_at = 0 WHERE id = ?")
        .bind(expired_id)
        .execute(server.database.pool())
        .await?;
    let expired_response = client
        .post(format!(
            "{}/api/v1/device-pairings/{expired_id}/redeem",
            server.base_url
        ))
        .json(&redeem_payload(expired_secret))
        .send()
        .await?;
    assert_eq!(expired_response.status(), reqwest::StatusCode::GONE);
    assert_eq!(
        expired_response.json::<Value>().await?["error"]["code"],
        "DEVICE_PAIRING_EXPIRED"
    );
    let missing_id = uuid::Uuid::now_v7().to_string();
    let missing_response = client
        .post(format!(
            "{}/api/v1/device-pairings/{missing_id}/redeem",
            server.base_url
        ))
        .json(&redeem_payload("missing-secret"))
        .send()
        .await?;
    assert_eq!(missing_response.status(), reqwest::StatusCode::NOT_FOUND);
    assert_eq!(
        missing_response.json::<Value>().await?["error"]["code"],
        "DEVICE_PAIRING_NOT_FOUND"
    );

    let cancelled = create_pairing(&client, &server.base_url, &cookies, &csrf).await?;
    let cancelled_id = cancelled["pairingId"]
        .as_str()
        .ok_or("missing pairing ID")?;
    let cancelled_secret = cancelled["secret"].as_str().ok_or("missing secret")?;
    let cancel_url = format!(
        "{}/api/v1/auth/device-pairings/{cancelled_id}",
        server.base_url
    );
    let missing_csrf = client
        .delete(&cancel_url)
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(missing_csrf.status(), reqwest::StatusCode::FORBIDDEN);
    let cancel = client
        .delete(&cancel_url)
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(cancel.status(), reqwest::StatusCode::NO_CONTENT);
    let repeated_cancel = client
        .delete(&cancel_url)
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(repeated_cancel.status(), reqwest::StatusCode::NOT_FOUND);
    let cancelled_redeem = client
        .post(format!(
            "{}/api/v1/device-pairings/{cancelled_id}/redeem",
            server.base_url
        ))
        .json(&redeem_payload(cancelled_secret))
        .send()
        .await?;
    assert_eq!(cancelled_redeem.status(), reqwest::StatusCode::GONE);
    assert_eq!(
        cancelled_redeem.json::<Value>().await?["error"]["code"],
        "DEVICE_PAIRING_CANCELLED"
    );
    Ok(())
}

#[tokio::test]
async fn pairing_cancellation_is_isolated_to_the_creator() -> Result<(), Box<dyn std::error::Error>>
{
    let server = test_server().await?;
    let client = reqwest::Client::new();
    let (admin_cookies, admin_csrf) = setup_and_login(&client, &server.base_url).await?;
    let users = UserStore::new(server.database.clone())?;
    users
        .create_user("Viewer", "Viewer", "viewer password", false)
        .await?;
    let (viewer_cookies, viewer_csrf) =
        login(&client, &server.base_url, "viewer", "viewer password").await?;
    let pairing = create_pairing(&client, &server.base_url, &admin_cookies, &admin_csrf).await?;
    let pairing_id = pairing["pairingId"].as_str().ok_or("missing pairing ID")?;
    let cancel_url = format!(
        "{}/api/v1/auth/device-pairings/{pairing_id}",
        server.base_url
    );

    let viewer_cancel = client
        .delete(&cancel_url)
        .header(COOKIE, viewer_cookies)
        .header("x-csrf-token", viewer_csrf)
        .send()
        .await?;
    assert_eq!(viewer_cancel.status(), reqwest::StatusCode::NOT_FOUND);
    let owner_cancel = client
        .delete(&cancel_url)
        .header(COOKIE, admin_cookies)
        .header("x-csrf-token", admin_csrf)
        .send()
        .await?;
    assert_eq!(owner_cancel.status(), reqwest::StatusCode::NO_CONTENT);
    Ok(())
}

#[tokio::test]
async fn pairing_redeem_validates_device_fields_and_body_size()
-> Result<(), Box<dyn std::error::Error>> {
    let server = test_server().await?;
    let client = reqwest::Client::new();
    let (cookies, csrf) = setup_and_login(&client, &server.base_url).await?;
    let pairing = create_pairing(&client, &server.base_url, &cookies, &csrf).await?;
    let pairing_id = pairing["pairingId"].as_str().ok_or("missing pairing ID")?;
    let secret = pairing["secret"].as_str().ok_or("missing secret")?;
    let redeem_url = format!(
        "{}/api/v1/device-pairings/{pairing_id}/redeem",
        server.base_url
    );
    let invalid = client
        .post(&redeem_url)
        .json(&json!({
            "secret": secret,
            "deviceId": "",
            "deviceName": "Test Desktop",
            "platform": "macOS",
            "version": "0.1.0"
        }))
        .send()
        .await?;
    assert_eq!(invalid.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(
        invalid.json::<Value>().await?["error"]["code"],
        "INVALID_REQUEST"
    );

    let oversized = client
        .post(&redeem_url)
        .json(&json!({
            "secret": secret,
            "deviceId": "prism-device-1",
            "deviceName": "x".repeat(20_000),
            "platform": "macOS",
            "version": "0.1.0"
        }))
        .send()
        .await?;
    assert_eq!(oversized.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);
    Ok(())
}

#[tokio::test]
async fn pairing_endpoints_are_rate_limited() -> Result<(), Box<dyn std::error::Error>> {
    let server = test_server().await?;
    let client = reqwest::Client::new();
    let (cookies, csrf) = setup_and_login(&client, &server.base_url).await?;
    let create_url = format!("{}/api/v1/auth/device-pairings", server.base_url);
    let pairing = create_pairing(&client, &server.base_url, &cookies, &csrf).await?;
    for _ in 0..9 {
        let response = client
            .post(&create_url)
            .header(COOKIE, &cookies)
            .header("x-csrf-token", &csrf)
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::CREATED);
    }
    let limited_create = client
        .post(&create_url)
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(
        limited_create.status(),
        reqwest::StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        limited_create
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok()),
        Some("60")
    );
    assert_eq!(
        limited_create.json::<Value>().await?["error"]["code"],
        "RATE_LIMITED"
    );

    let pairing_id = pairing["pairingId"].as_str().ok_or("missing pairing ID")?;
    let redeem_url = format!(
        "{}/api/v1/device-pairings/{pairing_id}/redeem",
        server.base_url
    );
    for _ in 0..10 {
        let response = client
            .post(&redeem_url)
            .json(&redeem_payload("wrong-secret"))
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    }
    let limited_redeem = client
        .post(&redeem_url)
        .json(&redeem_payload("wrong-secret"))
        .send()
        .await?;
    assert_eq!(
        limited_redeem.status(),
        reqwest::StatusCode::TOO_MANY_REQUESTS
    );
    let limited_body = limited_redeem.text().await?;
    assert!(limited_body.contains("RATE_LIMITED"));
    assert!(!limited_body.contains("wrong-secret"));
    Ok(())
}

#[tokio::test]
async fn pairing_redeem_allows_only_one_concurrent_consumer()
-> Result<(), Box<dyn std::error::Error>> {
    let server = test_server().await?;
    let client = reqwest::Client::new();
    let (cookies, csrf) = setup_and_login(&client, &server.base_url).await?;
    let pairing = create_pairing(&client, &server.base_url, &cookies, &csrf).await?;
    let pairing_id = pairing["pairingId"].as_str().ok_or("missing pairing ID")?;
    let secret = pairing["secret"].as_str().ok_or("missing secret")?;
    let redeem_url = format!(
        "{}/api/v1/device-pairings/{pairing_id}/redeem",
        server.base_url
    );
    let request_one = client
        .clone()
        .post(&redeem_url)
        .json(&redeem_payload(secret))
        .send();
    let request_two = client
        .clone()
        .post(&redeem_url)
        .json(&redeem_payload(secret))
        .send();
    let (one, two) = tokio::join!(request_one, request_two);
    let statuses = [one?.status(), two?.status()];
    assert!(statuses.contains(&reqwest::StatusCode::OK));
    assert!(statuses.contains(&reqwest::StatusCode::CONFLICT));
    let token_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM access_tokens WHERE client_name = 'Lux Prism'")
            .fetch_one(server.database.pool())
            .await?;
    assert_eq!(token_count, 1);
    Ok(())
}
