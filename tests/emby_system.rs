use luxd::{
    api::{AppState, app_with_state},
    application::setup::SetupService,
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    storage::Database,
};
use tokio::net::TcpListener;

#[tokio::test]
async fn emby_system_routes_work_with_both_prefixes_without_paths()
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
    setup
        .complete("Admin", "Administrator", "correct password")
        .await?;
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

    let login = client
        .post(format!("http://{address}/Users/AuthenticateByName"))
        .header(
            "Authorization",
            r#"Emby Client="Infuse", Device="iPhone", DeviceId="device-1", Version="1.2.3""#,
        )
        .json(&serde_json::json!({ "Username": "Admin", "Pw": "correct password" }))
        .send()
        .await?;
    let login_body: serde_json::Value = login.json().await?;
    let token = login_body["AccessToken"]
        .as_str()
        .ok_or("missing token")?
        .to_owned();

    let public = client
        .get(format!("http://{address}/System/Info/Public"))
        .send()
        .await?;
    assert_eq!(public.status(), reqwest::StatusCode::OK);
    let public_body: serde_json::Value = public.json().await?;
    assert_eq!(public_body["ServerName"], "Lux");
    assert_eq!(public_body["Version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(public_body["Id"].as_str(), Some(database.server_id()));
    assert!(
        !public_body
            .to_string()
            .contains(&config.config_dir.to_string_lossy().to_string())
    );

    let prefixed = client
        .get(format!("http://{address}/emby/System/Info/Public"))
        .send()
        .await?;
    assert_eq!(prefixed.status(), reqwest::StatusCode::OK);
    let prefixed_body: serde_json::Value = prefixed.json().await?;
    assert_eq!(prefixed_body["Id"], public_body["Id"]);

    let info = client
        .get(format!("http://{address}/System/Info"))
        .header("X-Emby-Token", &token)
        .send()
        .await?;
    assert_eq!(info.status(), reqwest::StatusCode::OK);
    let info_body: serde_json::Value = info.json().await?;
    assert_eq!(info_body["Id"], public_body["Id"]);
    assert!(info_body.get("ProgramDataPath").is_none());
    assert!(info_body.get("InternalMetadataPath").is_none());

    for path in ["/System/Ping", "/emby/System/Ping"] {
        let response = client
            .get(format!("http://{address}{path}"))
            .header("X-Emby-Token", &token)
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert!(response.text().await?.is_empty());
    }
    for path in ["/System/Ping", "/emby/System/Ping"] {
        let response = client
            .post(format!("http://{address}{path}"))
            .header("X-Emby-Token", &token)
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
    }

    server.abort();
    Ok(())
}

#[tokio::test]
async fn server_id_survives_database_reopen() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let server_id = database.server_id().to_owned();
    database.close().await;

    let reopened = Database::connect(&config).await?;
    assert_eq!(reopened.server_id(), server_id);
    reopened.close().await;
    Ok(())
}
