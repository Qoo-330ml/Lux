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

    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(config, database, setup, auth, emby_auth));
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
