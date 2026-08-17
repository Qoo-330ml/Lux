use luxd::network::RemoteAccessPolicy;
use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, scanner::LibraryScanner, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::COOKIE;
use serde_json::json;
use std::net::SocketAddr;
use tokio::net::TcpListener;

#[test]
fn remote_policy_uses_forwarded_client_without_proxy_allowlist() {
    let policy = RemoteAccessPolicy;

    assert_eq!(
        policy.client_ip(Some("192.168.1.2"), Some("8.8.8.8")),
        Some("8.8.8.8".parse().expect("valid client IP"))
    );
    assert!(!policy.is_remote(Some("192.168.1.2"), Some("8.8.8.8")));
}

#[test]
fn remote_policy_reports_forwarded_client_without_proxy_allowlist() {
    let policy = RemoteAccessPolicy;

    assert_eq!(
        policy.reported_client_ip(Some("192.168.1.2"), Some("8.8.8.8")),
        Some("8.8.8.8".parse().expect("valid client IP"))
    );
    assert_eq!(policy.reported_client_ip(Some("192.168.1.20"), None), None);
}

#[tokio::test]
async fn remote_policy_blocks_auth_and_media_until_user_is_allowed()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let viewer = UserStore::new(database.clone())?
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Remote.Movie.2024.mkv"), b"movie").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String = sqlx::query_scalar("SELECT id FROM media_items LIMIT 1")
        .fetch_one(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO user_library_access (user_id, library_id, can_view)
         VALUES (?, ?, 1)",
    )
    .bind(viewer.id.to_string())
    .bind(library.id.to_string())
    .execute(database.pool())
    .await?;

    let web_auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let state = AppState::ready(config, database.clone(), setup, web_auth, emby_auth);
    let app = app_with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
    });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();
    let denied_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .header("x-forwarded-for", "8.8.8.8")
        .json(&json!({ "username": "viewer", "password": "viewer password" }))
        .send()
        .await?;
    assert_eq!(denied_login.status(), reqwest::StatusCode::OK);

    sqlx::query("UPDATE users SET can_remote_access = 1 WHERE id = ?")
        .bind(viewer.id.to_string())
        .execute(database.pool())
        .await?;
    let allowed_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .header("x-forwarded-for", "8.8.8.8")
        .json(&json!({ "username": "viewer", "password": "viewer password" }))
        .send()
        .await?;
    assert_eq!(allowed_login.status(), reqwest::StatusCode::OK);
    let cookie = cookie_pair(allowed_login.headers());
    let visible_item = client
        .get(format!("{base_url}/api/v1/items/{item_id}"))
        .header(COOKIE, &cookie)
        .header("x-forwarded-for", "8.8.8.8")
        .send()
        .await?;
    assert_eq!(visible_item.status(), reqwest::StatusCode::OK);

    sqlx::query("UPDATE users SET can_remote_access = 0 WHERE id = ?")
        .bind(viewer.id.to_string())
        .execute(database.pool())
        .await?;
    let blocked_item = client
        .get(format!("{base_url}/api/v1/items/{item_id}"))
        .header(COOKIE, &cookie)
        .header("x-forwarded-for", "8.8.8.8")
        .send()
        .await?;
    assert_eq!(blocked_item.status(), reqwest::StatusCode::OK);

    server.abort();
    Ok(())
}

fn cookie_pair(headers: &reqwest::header::HeaderMap) -> String {
    format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(headers, "lux_session"),
        cookie_value(headers, "lux_csrf")
    )
}

fn cookie_value(headers: &reqwest::header::HeaderMap, name: &str) -> String {
    headers
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| {
            value
                .strip_prefix(&format!("{name}="))
                .and_then(|value| value.split(';').next())
                .map(str::to_owned)
        })
        .unwrap_or_default()
}
