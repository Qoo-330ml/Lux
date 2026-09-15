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
    let client = reqwest::Client::new();

    let unauthenticated = client
        .get(format!("http://{address}/api/v1/home"))
        .send()
        .await?;
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);

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
        .bearer_auth(token)
        .send()
        .await?;
    assert_eq!(bearer.status(), reqwest::StatusCode::OK);

    Ok(())
}
