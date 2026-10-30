use std::time::Duration;

use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, scanner::LibraryScanner, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use tokio::net::TcpListener;

async fn next_scope(
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
                assert_eq!(value.as_object().ok_or("event object")?.len(), 1);
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
async fn user_mutations_notify_other_web_sessions_after_persistence()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:0".parse()?,
        config_dir: temp.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "correct password").await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("Event.Movie.2024.mkv"), b"fixture").await?;
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
    let app = app_with_state(AppState::ready(
        config,
        database.clone(),
        setup,
        auth,
        emby_auth,
    ));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base = format!("http://{address}");
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .expect("test client");
    let login = client
        .post(format!("{base}/api/v1/auth/login"))
        .json(&json!({"username":"admin", "password":"correct password"}))
        .send()
        .await?;
    let cookies = login
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| v.split(';').next())
        .collect::<Vec<_>>()
        .join("; ");
    let csrf = cookies
        .split("; ")
        .find_map(|v| v.strip_prefix("lux_csrf="))
        .ok_or("csrf")?;
    let mut events = client
        .get(format!("{base}/api/v1/events"))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(events.status(), reqwest::StatusCode::OK);
    assert!(std::str::from_utf8(&events.chunk().await?.ok_or("ready")?)?.contains("event: ready"));

    for (suffix, body, expected) in [
        ("favorite", json!({"favorite":true}), "favorites"),
        ("played", json!({"played":true}), "item"),
        ("favorite", json!({"favorite":false}), "favorites"),
        ("played", json!({"played":false}), "item"),
    ] {
        let response = client
            .put(format!("{base}/api/v1/items/{item_id}/{suffix}"))
            .header(COOKIE, &cookies)
            .header("X-CSRF-Token", csrf)
            .json(&body)
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
        assert_eq!(next_scope(&mut events).await?, expected);
        let (favorite, played): (i64, i64) = sqlx::query_as(
            "SELECT is_favorite, is_played FROM user_item_state WHERE user_id = ? AND item_id = ?",
        )
        .bind(admin.id.to_string())
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?;
        if suffix == "favorite" {
            assert_eq!(favorite != 0, body["favorite"] == true);
        } else {
            assert_eq!(played != 0, body["played"] == true);
        }
    }

    let progress = client.post(format!("{base}/api/v1/items/{item_id}/progress"))
        .header(COOKIE, &cookies).header("X-CSRF-Token", csrf)
        .json(&json!({"state":"PLAYING", "positionTicks":1000000000_i64, "durationTicks":2000000000_i64})).send().await?;
    assert_eq!(progress.status(), reqwest::StatusCode::NO_CONTENT);
    assert_eq!(next_scope(&mut events).await?, "home");

    let response = client
        .patch(format!("{base}/api/v1/auth/library-order"))
        .header(COOKIE, &cookies)
        .header("X-CSRF-Token", csrf)
        .json(&json!({"libraryOrder":[library.id.to_string()]}))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(next_scope(&mut events).await?, "home");
    server.abort();
    Ok(())
}
