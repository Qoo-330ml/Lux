use luxd::{
    api::{AppState, app_with_state},
    application::setup::SetupService,
    auth::{
        emby::{EmbyAuthService, EmbyDeviceInfo},
        sessions::WebAuthService,
    },
    config::Config,
    storage::Database,
};
use reqwest::header::AUTHORIZATION;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;

#[tokio::test]
async fn emby_public_users_login_and_logout_use_hashed_device_tokens()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let admin = setup
        .complete("Admin", "Administrator", "correct password")
        .await?;
    let app = app_with_state(AppState::ready(
        config.clone(),
        database.clone(),
        setup,
        web_auth,
        emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = reqwest::Client::new();

    let public = client
        .get(format!("http://{address}/emby/Users/Public"))
        .send()
        .await?;
    assert_eq!(public.status(), reqwest::StatusCode::OK);
    let public_body: serde_json::Value = public.json().await?;
    assert_eq!(public_body.as_array().map(Vec::len), Some(1));
    assert_eq!(public_body[0]["Id"], admin.id.to_string());
    assert_eq!(public_body[0]["HasPassword"], true);

    let login = client
        .post(format!("http://{address}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="Infuse", Device="iPhone", DeviceId="device-1", Version="1.2.3""#,
        )
        .json(&json!({ "Username": "ADMIN", "Pw": "correct password" }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let login_body: serde_json::Value = login.json().await?;
    let token = login_body["AccessToken"]
        .as_str()
        .ok_or("missing access token")?
        .to_owned();
    assert_eq!(login_body["User"]["Id"], admin.id.to_string());
    assert_eq!(login_body["ServerId"], database.server_id());
    assert_eq!(login_body["SessionInfo"]["Client"], "Infuse");
    assert_eq!(login_body["SessionInfo"]["DeviceId"], "device-1");

    let raw_token_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM access_tokens WHERE token_hash = ?")
            .bind(token.as_bytes())
            .fetch_one(database.pool())
            .await?;
    assert_eq!(raw_token_count, 0);
    let token_hash = Sha256::digest(token.as_bytes()).to_vec();
    let hashed_token_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM access_tokens WHERE token_hash = ?")
            .bind(token_hash)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(hashed_token_count, 1);

    let logout = client
        .post(format!(
            "http://{address}/emby/Sessions/Logout?api_key={token}"
        ))
        .send()
        .await?;
    assert_eq!(logout.status(), reqwest::StatusCode::NO_CONTENT);
    let second_logout = client
        .post(format!("http://{address}/Sessions/Logout"))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(second_logout.status(), reqwest::StatusCode::NO_CONTENT);

    let after_logout = client
        .get(format!("http://{address}/System/Info"))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(after_logout.status(), reqwest::StatusCode::UNAUTHORIZED);

    server.abort();
    Ok(())
}

#[test]
fn emby_device_info_parser_keeps_only_expected_fields() {
    let info = EmbyDeviceInfo::parse(
        r#"Emby Client="Infuse", Device="iPhone", DeviceId="device-1", Version="1.2.3", UserId="user-1""#,
    );
    assert_eq!(info.client, "Infuse");
    assert_eq!(info.device, "iPhone");
    assert_eq!(info.device_id, "device-1");
    assert_eq!(info.version, "1.2.3");
    assert_eq!(info.user_id.as_deref(), Some("user-1"));
    assert!(!format!("{info:?}").contains("token"));
}
