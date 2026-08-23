use luxd::{
    api::{AppState, app_with_state},
    application::setup::SetupService,
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    storage::Database,
};
use tokio::net::TcpListener;

struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

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
    sqlx::query(
        "INSERT INTO server_settings (key, value) VALUES ('server_name', ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind("客厅 Lux")
    .execute(database.pool())
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
    let _server = AbortOnDrop(tokio::spawn(
        async move { axum::serve(listener, app).await },
    ));
    let client = reqwest::Client::new();

    for path in ["/System/Ping", "/emby/System/Ping"] {
        let response = client.get(format!("http://{address}{path}")).send().await?;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert!(response.text().await?.is_empty());
    }
    for path in ["/System/Ping", "/emby/System/Ping"] {
        let response = client
            .post(format!("http://{address}{path}"))
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert!(response.text().await?.is_empty());
    }

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
    assert_eq!(public_body["ServerName"], "客厅 Lux");
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
    assert_eq!(info_body["ServerName"], "客厅 Lux");
    assert_eq!(info_body["Id"], public_body["Id"]);
    assert!(info_body.get("ProgramDataPath").is_none());
    assert!(info_body.get("InternalMetadataPath").is_none());

    let info_with_query_token = client
        .get(format!("http://{address}/emby/System/Info?api_key={token}"))
        .send()
        .await?;
    assert_eq!(info_with_query_token.status(), reqwest::StatusCode::OK);

    let user_id = login_body["User"]["Id"]
        .as_str()
        .ok_or("missing logged-in user id")?;
    for path in [
        "/DisplayPreferences/usersettings",
        "/emby/DisplayPreferences/usersettings",
    ] {
        let response = client
            .get(format!("http://{address}{path}"))
            .query(&[("userId", user_id), ("client", "Infuse")])
            .header("X-Emby-Token", &token)
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::OK, "{path}");
        assert_eq!(response.headers()["content-type"], "application/json");
        let preferences: serde_json::Value = response.json().await?;
        assert_eq!(preferences["Id"], "usersettings");
        assert_eq!(preferences["Client"], "Infuse");
        assert_eq!(preferences["SortBy"], "SortName");
        assert_eq!(preferences["SortOrder"], "Ascending");
        assert_eq!(preferences["ShowBackdrop"], true);
        assert!(preferences["CustomPrefs"].is_object());
    }

    assert_eq!(login_body["User"]["ServerName"], "客厅 Lux");
    let public_users = client
        .get(format!("http://{address}/Users/Public"))
        .send()
        .await?;
    assert_eq!(public_users.status(), reqwest::StatusCode::OK);
    let public_users_body: serde_json::Value = public_users.json().await?;
    assert_eq!(public_users_body[0]["ServerName"], "客厅 Lux");

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
