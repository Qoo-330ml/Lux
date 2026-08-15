use luxd::{
    api::{AppState, app_with_state},
    application::setup::SetupService,
    auth::{
        admin_api_key::AdminApiKeyService, emby::EmbyAuthService, sessions::WebAuthService,
        users::UserStore,
    },
    config::Config,
    storage::Database,
};
use serde_json::json;
use tokio::net::TcpListener;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[tokio::test]
async fn shared_admin_key_survives_restart_and_can_be_revoked()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let users = UserStore::new(database.clone())?;
    let admin = users
        .create_initial_admin("Admin", "Administrator", "correct horse battery staple")
        .await?;
    let service = AdminApiKeyService::new(config.config_dir.clone(), database.clone());

    assert!(service.current().await?.is_none());

    let key = service.rotate().await?;
    assert!(key.starts_with("lux_"));
    assert_eq!(service.current().await?.as_deref(), Some(key.as_str()));
    #[cfg(unix)]
    assert_eq!(
        std::fs::metadata(config.config_dir.join("lux_admin_api_key"))?
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        service.resolve(&key).await?.map(|user| user.id),
        Some(admin.id)
    );

    let restarted = AdminApiKeyService::new(config.config_dir.clone(), database.clone());
    assert_eq!(
        restarted.resolve(&key).await?.map(|user| user.id),
        Some(admin.id)
    );

    service.revoke().await?;
    assert!(service.current().await?.is_none());
    assert!(service.resolve(&key).await?.is_none());

    database.close().await;
    Ok(())
}

#[tokio::test]
async fn shared_admin_key_authenticates_lux_and_emby_requests_without_csrf()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let users = UserStore::new(database.clone())?;
    users
        .create_initial_admin("Admin", "Administrator", "correct horse battery staple")
        .await?;
    let key_service = AdminApiKeyService::new(config.config_dir.clone(), database.clone());
    let key = key_service.rotate().await?;
    let setup = SetupService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database.clone())?,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = reqwest::Client::new();

    let me = client
        .get(format!("http://{address}/api/v1/auth/me"))
        .header("X-Lux-Api-Key", &key)
        .send()
        .await?;
    assert_eq!(me.status(), reqwest::StatusCode::OK);
    assert_eq!(
        me.json::<serde_json::Value>().await?["user"]["isAdmin"],
        true
    );

    for request in [
        client
            .get(format!("http://{address}/api/v1/auth/me?api_key={key}"))
            .build()?,
        client
            .get(format!("http://{address}/api/v1/auth/me"))
            .header("Authorization", format!("Bearer {key}"))
            .build()?,
        client
            .get(format!("http://{address}/api/v1/auth/me"))
            .header("X-Emby-Token", &key)
            .build()?,
    ] {
        assert_eq!(
            client.execute(request).await?.status(),
            reqwest::StatusCode::OK
        );
    }

    let settings = client
        .patch(format!("http://{address}/api/v1/admin/settings"))
        .header("X-Lux-Api-Key", &key)
        .json(&json!({}))
        .send()
        .await?;
    assert_eq!(settings.status(), reqwest::StatusCode::OK);

    let audit = client
        .get(format!("http://{address}/api/v1/admin/audit"))
        .header("X-Lux-Api-Key", &key)
        .send()
        .await?;
    assert_eq!(audit.status(), reqwest::StatusCode::OK);
    let audit_body = audit.json::<serde_json::Value>().await?;
    assert_eq!(audit_body["events"][0]["metadata"]["auth"], "admin_api_key");
    assert!(!audit_body.to_string().contains(&key));

    let emby = client
        .get(format!("http://{address}/System/Info?api_key={key}"))
        .send()
        .await?;
    assert_eq!(emby.status(), reqwest::StatusCode::OK);

    let emby_with_lux_header = client
        .get(format!("http://{address}/System/Info"))
        .header("X-Lux-Api-Key", &key)
        .send()
        .await?;
    assert_eq!(emby_with_lux_header.status(), reqwest::StatusCode::OK);

    server.abort();
    database.close().await;
    Ok(())
}

#[tokio::test]
async fn only_web_admins_can_manage_the_shared_key() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let users = UserStore::new(database.clone())?;
    users
        .create_initial_admin("Admin", "Administrator", "correct horse battery staple")
        .await?;
    users
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?;
    let setup = SetupService::new(database.clone())?;
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        WebAuthService::new(database.clone())?,
        EmbyAuthService::new(database.clone())?,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = reqwest::Client::new();

    let admin_login = client
        .post(format!("http://{address}/api/v1/auth/login"))
        .json(&json!({
            "username": "admin",
            "password": "correct horse battery staple"
        }))
        .send()
        .await?;
    assert_eq!(admin_login.status(), reqwest::StatusCode::OK);
    let admin_cookies = format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(admin_login.headers(), "lux_session")?,
        cookie_value(admin_login.headers(), "lux_csrf")?
    );
    let csrf = cookie_value(admin_login.headers(), "lux_csrf")?;

    let initial = client
        .get(format!("http://{address}/api/v1/admin/api-key"))
        .header("Cookie", &admin_cookies)
        .send()
        .await?;
    assert_eq!(initial.status(), reqwest::StatusCode::OK);
    let initial_body = initial.json::<serde_json::Value>().await?;
    assert_eq!(initial_body["configured"], false);
    assert!(initial_body["apiKey"].is_null());

    let rotated = client
        .post(format!("http://{address}/api/v1/admin/api-key/rotate"))
        .header("Cookie", &admin_cookies)
        .header("X-CSRF-Token", &csrf)
        .send()
        .await?;
    assert_eq!(rotated.status(), reqwest::StatusCode::OK);
    let key = rotated.json::<serde_json::Value>().await?["apiKey"]
        .as_str()
        .ok_or("missing shared API key")?
        .to_owned();

    let listed = client
        .get(format!("http://{address}/api/v1/admin/api-key"))
        .header("Cookie", &admin_cookies)
        .send()
        .await?;
    assert_eq!(listed.status(), reqwest::StatusCode::OK);
    assert_eq!(listed.json::<serde_json::Value>().await?["apiKey"], key);

    let key_cannot_manage_itself = client
        .get(format!("http://{address}/api/v1/admin/api-key"))
        .header("X-Lux-Api-Key", &key)
        .send()
        .await?;
    assert_eq!(
        key_cannot_manage_itself.status(),
        reqwest::StatusCode::FORBIDDEN
    );

    let viewer_login = client
        .post(format!("http://{address}/api/v1/auth/login"))
        .json(&json!({ "username": "viewer", "password": "viewer password" }))
        .send()
        .await?;
    assert_eq!(viewer_login.status(), reqwest::StatusCode::OK);
    let viewer_cookies = format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(viewer_login.headers(), "lux_session")?,
        cookie_value(viewer_login.headers(), "lux_csrf")?
    );
    let viewer_read = client
        .get(format!("http://{address}/api/v1/admin/api-key"))
        .header("Cookie", viewer_cookies)
        .send()
        .await?;
    assert_eq!(viewer_read.status(), reqwest::StatusCode::FORBIDDEN);

    let revoked = client
        .delete(format!("http://{address}/api/v1/admin/api-key"))
        .header("Cookie", &admin_cookies)
        .header("X-CSRF-Token", &csrf)
        .send()
        .await?;
    assert_eq!(revoked.status(), reqwest::StatusCode::NO_CONTENT);

    server.abort();
    database.close().await;
    Ok(())
}

fn cookie_value(
    headers: &reqwest::header::HeaderMap,
    name: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    headers
        .get_all("set-cookie")
        .iter()
        .find_map(|value| {
            let value = value.to_str().ok()?;
            let prefix = format!("{name}=");
            value
                .strip_prefix(&prefix)
                .and_then(|value| value.split(';').next())
                .map(str::to_owned)
        })
        .ok_or_else(|| format!("missing {name} cookie").into())
}
