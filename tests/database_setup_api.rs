use luxd::{
    api::{AppState, app_with_state},
    application::setup::SetupService,
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    storage::Database,
};
use reqwest::StatusCode;
use serde_json::{Value, json};
use tokio::net::TcpListener;

async fn test_server(
    config: Config,
) -> Result<(String, tokio::task::JoinHandle<Result<(), std::io::Error>>), Box<dyn std::error::Error>>
{
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(
        AppState::ready(config.clone(), database.clone(), setup, auth, emby_auth)
            .require_database_selection(),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    Ok((format!("http://{address}"), server))
}

#[tokio::test]
async fn setup_selects_sqlite_before_creating_the_first_admin()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (base_url, server) = test_server(config.clone()).await?;
    let client = reqwest::Client::new();

    let status: Value = client
        .get(format!("{base_url}/api/v1/setup/database"))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(status["configured"], false);
    assert_eq!(status["currentBackend"], "SQLITE");

    let selected = client
        .post(format!("{base_url}/api/v1/setup/database/select"))
        .json(&json!({ "backend": "SQLITE" }))
        .send()
        .await?;
    assert_eq!(selected.status(), StatusCode::OK);
    assert_eq!(selected.json::<Value>().await?["restartRequired"], false);

    let complete = client
        .post(format!("{base_url}/api/v1/setup/complete"))
        .json(&json!({
            "username": "Admin",
            "displayName": "Administrator",
            "password": "correct horse battery staple"
        }))
        .send()
        .await?;
    assert_eq!(complete.status(), StatusCode::CREATED);
    assert!(config.config_dir.join("database.json").is_file());
    server.abort();
    Ok(())
}

#[tokio::test]
async fn setup_rejects_invalid_postgres_configuration_without_persisting_it()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (base_url, server) = test_server(config.clone()).await?;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base_url}/api/v1/setup/database/select"))
        .json(&json!({
            "backend": "POSTGRESQL",
            "host": "",
            "port": 5432,
            "database": "lux",
            "username": "lux",
            "password": "test-only-password",
            "sslMode": "disable"
        }))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response.text().await?;
    assert!(!body.contains("test-only-password"));
    assert!(!config.config_dir.join("database.json").exists());
    server.abort();
    Ok(())
}

#[tokio::test]
async fn setup_requires_database_selection_before_admin_creation()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let (base_url, server) = test_server(config).await?;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{base_url}/api/v1/setup/complete"))
        .json(&json!({
            "username": "Admin",
            "displayName": "Administrator",
            "password": "correct horse battery staple"
        }))
        .send()
        .await?;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        response.json::<Value>().await?["error"]["code"],
        "DATABASE_CONFIGURATION_REQUIRED"
    );
    server.abort();
    Ok(())
}
