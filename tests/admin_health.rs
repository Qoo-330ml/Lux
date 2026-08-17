use std::time::Duration;

use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use tokio::net::TcpListener;

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
        .expect("expected cookie")
}

#[tokio::test]
async fn admin_health_reports_safe_runtime_diagnostics_and_enforces_access()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let _viewer = UserStore::new(database.clone())?
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;

    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(config, database, setup, auth, emby_auth));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;

    let unauthenticated = client
        .get(format!("{base_url}/api/v1/admin/health"))
        .send()
        .await?;
    assert_eq!(unauthenticated.status(), reqwest::StatusCode::UNAUTHORIZED);

    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let cookies = format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(login.headers(), "lux_session"),
        cookie_value(login.headers(), "lux_csrf")
    );
    let health = client
        .get(format!("{base_url}/api/v1/admin/health"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(health.status(), reqwest::StatusCode::OK);
    let body: Value = health.json().await?;
    assert_eq!(
        body["status"],
        if body["ffprobe"]["available"] == true {
            "ok"
        } else {
            "degraded"
        }
    );
    assert_eq!(body["schemaVersion"], 67);
    assert_eq!(body["database"]["status"], "ok");
    assert_eq!(body["database"]["backend"], "SQLITE");
    assert_eq!(body["database"]["writable"], true);
    assert_eq!(body["config"]["available"], true);
    assert_eq!(body["config"]["writable"], true);
    assert!(body["ffprobe"]["available"].is_boolean());
    assert_eq!(body["tmdb"]["configured"], true);
    assert!(body["runtime"]["seconds"].is_number());
    assert_eq!(body["resources"]["cpu"]["source"], "cgroup");
    assert_eq!(body["resources"]["memory"]["source"], "cgroup");
    assert_eq!(body["resources"]["mediaStorage"]["path"], "/media");
    assert_eq!(
        body["resources"]["mediaStorage"]["source"],
        "container-filesystem"
    );
    assert_eq!(body["libraries"][0]["rootCount"], 1);
    assert!(body.get("configDir").is_none());
    let body_text = body.to_string();
    let config_path = temp_dir.path().to_string_lossy().into_owned();
    assert!(!body_text.contains(&config_path));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let config_dir = temp_dir.path().join("config");
        let mut permissions = std::fs::metadata(&config_dir)?.permissions();
        permissions.set_mode(0o500);
        std::fs::set_permissions(&config_dir, permissions)?;

        let degraded = client
            .get(format!("{base_url}/api/v1/admin/health"))
            .header(COOKIE, &cookies)
            .send()
            .await?;
        assert_eq!(degraded.status(), reqwest::StatusCode::OK);
        let degraded_body: Value = degraded.json().await?;
        assert_eq!(degraded_body["status"], "degraded");
        assert_eq!(degraded_body["database"]["status"], "ok");
        assert_eq!(degraded_body["database"]["writable"], true);
        assert_eq!(degraded_body["config"]["writable"], false);

        let mut permissions = std::fs::metadata(&config_dir)?.permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&config_dir, permissions)?;
    }

    let logs = client
        .get(format!("{base_url}/api/v1/admin/logs?pageSize=10"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(logs.status(), reqwest::StatusCode::OK);
    assert!(logs.json::<Value>().await?["events"].is_array());

    let viewer_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "viewer", "password": "viewer password" }))
        .send()
        .await?;
    let viewer_cookies = format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(viewer_login.headers(), "lux_session"),
        cookie_value(viewer_login.headers(), "lux_csrf")
    );
    let denied = client
        .get(format!("{base_url}/api/v1/admin/health"))
        .header(COOKIE, viewer_cookies)
        .send()
        .await?;
    assert_eq!(denied.status(), reqwest::StatusCode::FORBIDDEN);
    server.abort();
    Ok(())
}
