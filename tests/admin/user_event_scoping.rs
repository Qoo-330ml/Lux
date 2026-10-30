use std::time::Duration;

use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, scanner::LibraryScanner, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{COOKIE, HeaderMap, SET_COOKIE};
use serde_json::{Value, json};
use tokio::net::TcpListener;

fn cookie_value(headers: &HeaderMap, name: &str) -> String {
    headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| {
            let (pair, _) = value.split_once(';')?;
            let (cookie_name, cookie_value) = pair.split_once('=')?;
            (cookie_name == name).then(|| cookie_value.to_owned())
        })
        .unwrap_or_default()
}

async fn login(
    client: &reqwest::Client,
    base: &str,
    username: &str,
    password: &str,
) -> Result<(String, String), Box<dyn std::error::Error>> {
    let response = client
        .post(format!("{base}/api/v1/auth/login"))
        .json(&json!({ "username": username, "password": password }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let cookies = format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(response.headers(), "lux_session"),
        cookie_value(response.headers(), "lux_csrf")
    );
    let csrf = cookie_value(response.headers(), "lux_csrf");
    Ok((cookies, csrf))
}

async fn open_events(
    client: &reqwest::Client,
    base: &str,
    cookies: &str,
) -> Result<reqwest::Response, Box<dyn std::error::Error>> {
    let mut response = client
        .get(format!("{base}/api/v1/events"))
        .header(COOKIE, cookies)
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let ready = response.chunk().await?.ok_or("missing ready frame")?;
    assert!(String::from_utf8(ready.to_vec())?.contains("event: ready"));
    Ok(response)
}

async fn next_invalidation_scope(
    response: &mut reqwest::Response,
) -> Result<String, Box<dyn std::error::Error>> {
    let frame = tokio::time::timeout(Duration::from_secs(2), async {
        let mut frame = String::new();
        loop {
            let chunk = response.chunk().await?.ok_or("event stream closed")?;
            frame.push_str(std::str::from_utf8(&chunk)?);
            if frame.contains("event: invalidate") && frame.ends_with("\n\n") {
                let data = frame
                    .lines()
                    .find_map(|line| line.strip_prefix("data: "))
                    .ok_or("missing data")?;
                let value: Value = serde_json::from_str(data)?;
                return Ok::<_, Box<dyn std::error::Error>>(
                    value["scope"].as_str().ok_or("scope")?.to_owned(),
                );
            }
        }
    })
    .await??;
    Ok(frame)
}

#[tokio::test]
async fn user_level_events_stay_private_while_admin_events_reach_everyone()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:0".parse()?,
        config_dir: temp.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    UserStore::new(database.clone())?
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Scoped.Event.2024.mkv"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE' LIMIT 1")
            .fetch_one(database.pool())
            .await?;

    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(config, database, setup, auth, emby_auth));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base = format!("http://{address}");
    let client = reqwest::Client::builder().no_proxy().build()?;

    let (admin_cookies, admin_csrf) = login(&client, &base, "admin", "correct password").await?;
    let (viewer_cookies, _viewer_csrf) = login(&client, &base, "viewer", "viewer password").await?;
    let mut admin_events = open_events(&client, &base, &admin_cookies).await?;
    let mut viewer_events = open_events(&client, &base, &viewer_cookies).await?;

    // Favorites are per-user state: only the acting user's connections react.
    let favorite = client
        .put(format!("{base}/api/v1/items/{item_id}/favorite"))
        .header(COOKIE, &admin_cookies)
        .header("X-CSRF-Token", &admin_csrf)
        .json(&json!({ "favorite": true }))
        .send()
        .await?;
    assert_eq!(favorite.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(
        next_invalidation_scope(&mut admin_events).await?,
        "favorites"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(750), viewer_events.chunk())
            .await
            .is_err(),
        "another user's favorites event must not reach the viewer's stream"
    );

    // Playback progress is per-user as well.
    let progress = client
        .post(format!("{base}/api/v1/items/{item_id}/progress"))
        .header(COOKIE, &admin_cookies)
        .header("X-CSRF-Token", &admin_csrf)
        .json(&json!({
            "state": "PLAYING",
            "positionTicks": 1_000_000_000_i64,
            "durationTicks": 2_000_000_000_i64
        }))
        .send()
        .await?;
    assert_eq!(progress.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(next_invalidation_scope(&mut admin_events).await?, "home");
    assert!(
        tokio::time::timeout(Duration::from_millis(750), viewer_events.chunk())
            .await
            .is_err(),
        "another user's progress event must not reach the viewer's stream"
    );

    // Directory-level administrator changes are still broadcast.
    let settings = client
        .patch(format!("{base}/api/v1/admin/settings"))
        .header(COOKIE, &admin_cookies)
        .header("X-CSRF-Token", &admin_csrf)
        .json(&json!({ "serverName": "Scoped Event Test" }))
        .send()
        .await?;
    assert_eq!(settings.status(), reqwest::StatusCode::OK);
    assert_eq!(next_invalidation_scope(&mut viewer_events).await?, "home");
    server.abort();
    Ok(())
}
