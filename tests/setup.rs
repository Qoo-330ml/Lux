use luxd::{
    api::{AppState, app_with_state},
    application::setup::SetupService,
    auth::sessions::WebAuthService,
    config::Config,
    storage::Database,
};
use serde_json::json;
use tokio::net::TcpListener;

async fn test_server(
    config: Config,
) -> Result<(String, tokio::task::JoinHandle<Result<(), std::io::Error>>), Box<dyn std::error::Error>>
{
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let auth = WebAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(config, database.clone(), setup, auth));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    Ok((format!("http://{address}"), server))
}

#[tokio::test]
async fn setup_can_create_one_admin_and_then_closes() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (base_url, server) = test_server(config).await?;
    let client = reqwest::Client::new();

    let status = client
        .get(format!("{base_url}/api/v1/setup/status"))
        .send()
        .await?;
    assert_eq!(status.status(), reqwest::StatusCode::OK);
    assert_eq!(
        status.json::<serde_json::Value>().await?["initialized"],
        false
    );

    let complete = client
        .post(format!("{base_url}/api/v1/setup/complete"))
        .json(&json!({
            "username": "Admin",
            "displayName": "Administrator",
            "password": "correct horse battery staple"
        }))
        .send()
        .await?;
    assert_eq!(complete.status(), reqwest::StatusCode::CREATED);
    let complete_body: serde_json::Value = complete.json().await?;
    assert_eq!(complete_body["initialized"], true);
    assert_eq!(complete_body["user"]["usernameNormalized"], "admin");
    assert!(complete_body.get("passwordHash").is_none());

    let status = client
        .get(format!("{base_url}/api/v1/setup/status"))
        .send()
        .await?;
    assert_eq!(
        status.json::<serde_json::Value>().await?["initialized"],
        true
    );

    let repeated = client
        .post(format!("{base_url}/api/v1/setup/complete"))
        .json(&json!({
            "username": "Second",
            "displayName": "Second",
            "password": "another password"
        }))
        .send()
        .await?;
    assert_eq!(repeated.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(
        repeated.json::<serde_json::Value>().await?["error"]["code"],
        "SETUP_ALREADY_COMPLETED"
    );

    server.abort();
    Ok(())
}

#[tokio::test]
async fn concurrent_setup_requests_only_allow_one_admin() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (base_url, server) = test_server(config).await?;
    let client = reqwest::Client::new();
    let first = client
        .post(format!("{base_url}/api/v1/setup/complete"))
        .json(
            &json!({ "username": "First", "displayName": "First", "password": "first password" }),
        );
    let second = client
        .post(format!("{base_url}/api/v1/setup/complete"))
        .json(&json!({ "username": "Second", "displayName": "Second", "password": "second password" }));
    let (first, second) = tokio::join!(first.send(), second.send());
    let statuses = [first?.status(), second?.status()];

    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == reqwest::StatusCode::CREATED)
            .count(),
        1
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == reqwest::StatusCode::CONFLICT)
            .count(),
        1
    );

    server.abort();
    Ok(())
}
