use luxd::{
    api::{AppState, app_with_state},
    application::setup::SetupService,
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    storage::Database,
};
use reqwest::header::AUTHORIZATION;
use serde_json::{Value, json};
use tempfile::tempdir;
use tokio::net::TcpListener;

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[tokio::test]
async fn lux_client_tokens_authenticate_lux_api_without_web_cookies()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup
        .complete("Admin", "Administrator", "correct password")
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
    let _server = AbortOnDrop(tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    }));
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");

    let unauthenticated = client
        .get(format!("http://{address}/api/v1/home"))
        .send()
        .await?;
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);
    let unauthenticated_carousel = client
        .get(format!("http://{address}/api/v1/home/carousel"))
        .send()
        .await?;
    assert_eq!(
        unauthenticated_carousel.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );

    let login = client
        .post(format!("http://{address}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="LuxTest", Device="Mac", DeviceId="lux-client", Version="1""#,
        )
        .json(&json!({ "Username": "admin", "Pw": "correct password" }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let token = login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing access token")?
        .to_owned();

    let home = client
        .get(format!("http://{address}/api/v1/home"))
        .header("X-Lux-Token", &token)
        .send()
        .await?;
    assert_eq!(home.status(), reqwest::StatusCode::OK);
    assert!(home.json::<Value>().await?["libraries"].is_array());

    let carousel = client
        .get(format!("http://{address}/api/v1/home/carousel"))
        .header("X-Lux-Token", &token)
        .send()
        .await?;
    assert_eq!(carousel.status(), reqwest::StatusCode::OK);
    assert!(carousel.json::<Value>().await?["recommended"].is_array());

    let continue_watching = client
        .get(format!("http://{address}/api/v1/continue-watching"))
        .header("X-Lux-Token", &token)
        .send()
        .await?;
    assert_eq!(continue_watching.status(), reqwest::StatusCode::OK);
    assert_eq!(continue_watching.json::<Value>().await?["total"], 0);

    let settings = client
        .patch(format!("http://{address}/api/v1/auth/settings"))
        .header("X-Lux-Token", &token)
        .json(&json!({ "playedPercent": 90 }))
        .send()
        .await?;
    assert_eq!(settings.status(), reqwest::StatusCode::OK);

    let libraries = client
        .get(format!("http://{address}/api/v1/libraries"))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(libraries.status(), reqwest::StatusCode::OK);
    assert!(libraries.json::<Value>().await?["libraries"].is_array());

    let bearer = client
        .get(format!("http://{address}/api/v1/home"))
        .bearer_auth(&token)
        .send()
        .await?;
    assert_eq!(bearer.status(), reqwest::StatusCode::OK);

    // Self-service profile, avatar, preferences, sessions and password updates with X-Lux-Token.
    let profile = client
        .patch(format!("http://{address}/api/v1/auth/profile"))
        .header("X-Lux-Token", &token)
        .json(&json!({ "displayName": "Client Admin" }))
        .send()
        .await?;
    assert_eq!(profile.status(), reqwest::StatusCode::OK);
    assert_eq!(
        profile.json::<Value>().await?["user"]["displayName"],
        "Client Admin"
    );

    const PNG_1X1: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x60,
        0x00, 0x02, 0x00, 0x00, 0x05, 0x00, 0x01, 0xE2, 0x26, 0x05, 0x9B, 0x00, 0x00, 0x00, 0x00,
        0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    let avatar_put = client
        .put(format!("http://{address}/api/v1/auth/avatar"))
        .header("X-Lux-Token", &token)
        .header("Content-Type", "image/png")
        .body(PNG_1X1.to_vec())
        .send()
        .await?;
    assert_eq!(avatar_put.status(), reqwest::StatusCode::OK);
    let avatar_get = client
        .get(format!("http://{address}/api/v1/auth/avatar"))
        .header("X-Lux-Token", &token)
        .send()
        .await?;
    assert_eq!(avatar_get.status(), reqwest::StatusCode::OK);

    let prefs_get: Value = client
        .get(format!("http://{address}/api/v1/auth/account-preferences"))
        .header("X-Lux-Token", &token)
        .send()
        .await?
        .json()
        .await?;
    let prefs_patch = client
        .patch(format!("http://{address}/api/v1/auth/account-preferences"))
        .header("X-Lux-Token", &token)
        .json(&json!({
            "expectedVersion": prefs_get["version"],
            "theme": "dark",
            "accentColor": "ocean",
            "showMediaLibraries": true,
            "showContinueWatching": true,
            "audioLanguage": "日本語",
            "subtitleLanguage": "简体中文",
            "autoPlayNextEpisode": true
        }))
        .send()
        .await?;
    assert_eq!(prefs_patch.status(), reqwest::StatusCode::OK);

    // Login a second client device and a Web session, then list & revoke from token1.
    let login2 = client
        .post(format!("http://{address}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="LuxTest2", Device="iPhone", DeviceId="lux-client-2", Version="1""#,
        )
        .json(&json!({ "Username": "admin", "Pw": "correct password" }))
        .send()
        .await?;
    assert_eq!(login2.status(), reqwest::StatusCode::OK);
    let token2 = login2.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing access token 2")?
        .to_owned();

    let web_login = client
        .post(format!("http://{address}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    assert_eq!(web_login.status(), reqwest::StatusCode::OK);

    let sessions_resp = client
        .get(format!("http://{address}/api/v1/auth/sessions"))
        .header("X-Lux-Token", &token)
        .send()
        .await?;
    assert_eq!(sessions_resp.status(), reqwest::StatusCode::OK);
    let sessions_body: Value = sessions_resp.json().await?;
    let sessions = sessions_body["sessions"]
        .as_array()
        .ok_or("missing sessions")?;
    let current_id = sessions
        .iter()
        .find(|s| s["isCurrent"] == true && s["sessionKind"] == "CLIENT_TOKEN")
        .and_then(|s| s["id"].as_str())
        .ok_or("missing current client token session")?;
    let other_client_id = sessions
        .iter()
        .find(|s| s["isCurrent"] == false && s["sessionKind"] == "CLIENT_TOKEN")
        .and_then(|s| s["id"].as_str())
        .ok_or("missing other client token session")?;
    let web_id = sessions
        .iter()
        .find(|s| s["isCurrent"] == false && s["sessionKind"] == "WEB")
        .and_then(|s| s["id"].as_str())
        .ok_or("missing web session")?;

    let revoke_self = client
        .delete(format!(
            "http://{address}/api/v1/auth/sessions/{current_id}"
        ))
        .header("X-Lux-Token", &token)
        .send()
        .await?;
    assert_eq!(revoke_self.status(), reqwest::StatusCode::BAD_REQUEST);

    let revoke_other_client = client
        .delete(format!(
            "http://{address}/api/v1/auth/sessions/{other_client_id}"
        ))
        .header("X-Lux-Token", &token)
        .send()
        .await?;
    assert_eq!(
        revoke_other_client.status(),
        reqwest::StatusCode::NO_CONTENT
    );
    let token2_after_revoke = client
        .get(format!("http://{address}/api/v1/auth/me"))
        .header("X-Lux-Token", &token2)
        .send()
        .await?;
    assert_eq!(
        token2_after_revoke.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );

    let revoke_web = client
        .delete(format!("http://{address}/api/v1/auth/sessions/{web_id}"))
        .header("X-Lux-Token", &token)
        .send()
        .await?;
    assert_eq!(revoke_web.status(), reqwest::StatusCode::NO_CONTENT);

    // Password change keeps caller's X-Lux-Token alive and revokes other tokens.
    let login3 = client
        .post(format!("http://{address}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="LuxTest3", Device="iPad", DeviceId="lux-client-3", Version="1""#,
        )
        .json(&json!({ "Username": "admin", "Pw": "correct password" }))
        .send()
        .await?;
    let token3 = login3.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing access token 3")?
        .to_owned();

    let bad_pw = client
        .patch(format!("http://{address}/api/v1/auth/password"))
        .header("X-Lux-Token", &token)
        .json(&json!({ "currentPassword": "wrong", "newPassword": "new password 123" }))
        .send()
        .await?;
    assert_eq!(bad_pw.status(), reqwest::StatusCode::UNAUTHORIZED);

    let ok_pw = client
        .patch(format!("http://{address}/api/v1/auth/password"))
        .header("X-Lux-Token", &token)
        .json(&json!({ "currentPassword": "correct password", "newPassword": "new password 123" }))
        .send()
        .await?;
    assert_eq!(ok_pw.status(), reqwest::StatusCode::NO_CONTENT);

    let still_valid = client
        .get(format!("http://{address}/api/v1/auth/me"))
        .header("X-Lux-Token", &token)
        .send()
        .await?;
    assert_eq!(still_valid.status(), reqwest::StatusCode::OK);

    let token3_revoked = client
        .get(format!("http://{address}/api/v1/auth/me"))
        .header("X-Lux-Token", &token3)
        .send()
        .await?;
    assert_eq!(token3_revoked.status(), reqwest::StatusCode::UNAUTHORIZED);

    Ok(())
}
