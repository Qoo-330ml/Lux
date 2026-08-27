use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, scanner::LibraryScanner, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::AUTHORIZATION;
use serde_json::{Value, json};
use tokio::net::TcpListener;

#[tokio::test]
async fn emby_can_read_xml_sidecar_without_danmaku_settings()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let config_dir = directory.path().join("config");
    let media_root = directory.path().join("Movies");
    tokio::fs::create_dir_all(&media_root).await?;
    tokio::fs::write(media_root.join("Demo.Movie.2024.mkv"), b"video").await?;
    tokio::fs::write(
        media_root.join("Demo.Movie.2024.xml"),
        b"<i><d p=\"1,1,25,16777215,0,0,0,0\">hello</d></i>",
    )
    .await?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: config_dir.clone(),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let library = LibraryService::new(database.clone())
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    LibraryService::new(database.clone())
        .add_root(
            library.id,
            media_root.to_str().ok_or("non-utf8 media root")?,
        )
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE title = 'Demo Movie'")
            .fetch_one(database.pool())
            .await?;
    let source_id: String = sqlx::query_scalar(
        "SELECT id FROM media_sources WHERE item_id = ? ORDER BY is_default DESC LIMIT 1",
    )
    .bind(&item_id)
    .fetch_one(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO danmaku_tracks (
            id, media_source_id, relative_path, format, status
         ) VALUES ('danmaku-track-test', ?, 'Demo.Movie.2024.xml', 'XML', 'READY')",
    )
    .bind(&source_id)
    .execute(database.pool())
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
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();

    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({"username": "admin", "password": "correct password"}))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let session_cookie = login
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .ok_or("missing web session cookie")?
        .to_owned();
    let web_info = client
        .get(format!("{base_url}/api/v1/items/{item_id}/danmaku"))
        .header(reqwest::header::COOKIE, &session_cookie)
        .send()
        .await?;
    assert_eq!(web_info.status(), reqwest::StatusCode::OK);
    let web_info_body = web_info.json::<Value>().await?;
    assert_eq!(web_info_body["available"], true);
    assert_eq!(web_info_body["format"], "BILIBILI_XML");
    assert_eq!(web_info_body["sourceId"], Value::Null);
    assert!(
        web_info_body["rawUrl"]
            .as_str()
            .is_some_and(|url| url.starts_with("/api/v1/items/"))
    );
    let web_source_info = client
        .get(format!("{base_url}/api/v1/items/{item_id}/danmaku"))
        .query(&[("sourceId", source_id.as_str())])
        .header(reqwest::header::COOKIE, &session_cookie)
        .send()
        .await?;
    assert_eq!(web_source_info.status(), reqwest::StatusCode::OK);
    let web_source_info_body = web_source_info.json::<Value>().await?;
    assert_eq!(web_source_info_body["sourceId"], source_id);
    let web_raw = client
        .get(format!("{base_url}/api/v1/items/{item_id}/danmaku/raw"))
        .header(reqwest::header::COOKIE, &session_cookie)
        .send()
        .await?;
    assert_eq!(web_raw.status(), reqwest::StatusCode::OK);
    assert_eq!(
        web_raw
            .headers()
            .get(reqwest::header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("private, no-cache")
    );
    assert_eq!(
        web_raw.text().await?,
        r#"<i><d p="1,1,25,16777215,0,0,0,0">hello</d></i>"#
    );
    let web_unauthorized = client
        .get(format!("{base_url}/api/v1/items/{item_id}/danmaku"))
        .send()
        .await?;
    assert_eq!(web_unauthorized.status(), reqwest::StatusCode::UNAUTHORIZED);
    let web_unauthorized_body = web_unauthorized.json::<Value>().await?;
    assert_eq!(
        web_unauthorized_body["error"]["code"],
        "AUTHENTICATION_REQUIRED"
    );
    let web_missing = client
        .get(format!("{base_url}/api/v1/items/missing-item/danmaku"))
        .header(reqwest::header::COOKIE, &session_cookie)
        .send()
        .await?;
    assert_eq!(web_missing.status(), reqwest::StatusCode::NOT_FOUND);
    let web_missing_body = web_missing.json::<Value>().await?;
    assert_eq!(web_missing_body["error"]["code"], "NOT_FOUND");
    sqlx::query("UPDATE danmaku_tracks SET status = 'MISSING' WHERE media_source_id = ?")
        .bind(&source_id)
        .execute(database.pool())
        .await?;
    let web_unregistered = client
        .get(format!("{base_url}/api/v1/items/{item_id}/danmaku"))
        .header(reqwest::header::COOKIE, &session_cookie)
        .send()
        .await?;
    assert_eq!(web_unregistered.status(), reqwest::StatusCode::NOT_FOUND);
    let web_unregistered_body = web_unregistered.json::<Value>().await?;
    assert_eq!(web_unregistered_body["error"]["code"], "NOT_FOUND");
    let emby_login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="DanmakuTest", Device="Mac", DeviceId="danmaku-test", Version="1""#,
        )
        .json(&json!({"Username": "admin", "Pw": "correct password"}))
        .send()
        .await?;
    assert_eq!(emby_login.status(), reqwest::StatusCode::OK);
    let emby_token = emby_login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing Emby token")?
        .to_owned();
    let info = client
        .get(format!("{base_url}/api/danmu/{item_id}"))
        .query(&[("api_key", emby_token.as_str())])
        .send()
        .await?;
    assert_eq!(info.status(), reqwest::StatusCode::OK);
    assert_eq!(info.json::<Value>().await?["hasDanmaku"], true);
    let raw = client
        .get(format!("{base_url}/api/danmu/{item_id}/raw"))
        .query(&[("api_key", emby_token.as_str())])
        .send()
        .await?;
    assert_eq!(raw.status(), reqwest::StatusCode::OK);
    assert_eq!(
        raw.text().await?,
        "<i><d p=\"1,1,25,16777215,0,0,0,0\">hello</d></i>"
    );
    server.abort();
    Ok(())
}

#[tokio::test]
async fn lux_danmaku_returns_a_structured_error_when_the_service_is_unavailable()
-> Result<(), Box<dyn std::error::Error>> {
    let app = app_with_state(AppState::default());
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });

    let response = reqwest::get(format!("http://{address}/api/v1/items/item-1/danmaku")).await?;
    assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let body = response.json::<Value>().await?;
    assert_eq!(body["error"]["code"], "DATABASE_UNAVAILABLE");

    server.abort();
    Ok(())
}
