use luxd::{
    api::{AppState, app, app_with_state},
    application::setup::SetupService,
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    storage::Database,
};
use tokio::net::TcpListener;

#[tokio::test]
async fn ready_requires_a_database() -> Result<(), Box<dyn std::error::Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app()).await });
    let response = reqwest::get(format!("http://{address}/health/ready")).await?;

    assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    assert!(response.text().await?.contains("database_unavailable"));

    server.abort();
    Ok(())
}

#[tokio::test]
async fn ready_and_version_report_migrated_state_without_paths()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config.clone(),
        database.clone(),
        setup,
        auth,
        emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    let client = reqwest::Client::new();
    let ready = client
        .get(format!("http://{address}/health/ready"))
        .send()
        .await?;
    assert_eq!(ready.status(), reqwest::StatusCode::OK);
    let ready_body: serde_json::Value = ready.json().await?;
    assert_eq!(ready_body["status"], "ready");
    assert_eq!(ready_body["schemaVersion"], 98);
    assert_eq!(ready_body["databaseWritable"], true);

    let version = client
        .get(format!("http://{address}/api/v1/version"))
        .send()
        .await?;
    assert_eq!(version.status(), reqwest::StatusCode::OK);
    let version_text = version.text().await?;
    let version_body: serde_json::Value = serde_json::from_str(&version_text)?;
    assert_eq!(version_body["luxVersion"], env!("CARGO_PKG_VERSION"));
    assert_eq!(version_body["schemaVersion"], 98);
    assert!(
        version_body["commit"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert!(!version_text.contains(&config.config_dir.to_string_lossy().to_string()));

    server.abort();
    database.close().await;
    Ok(())
}
