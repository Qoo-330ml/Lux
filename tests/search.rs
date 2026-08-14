use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, scanner::LibraryScanner, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::AUTHORIZATION;
use serde_json::{Value, json};
use tokio::net::TcpListener;

#[tokio::test]
async fn fts_search_matches_chinese_titles_and_aliases_with_acl()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    let admin = setup.complete("Admin", "Admin", "correct password").await?;
    let viewer = UserStore::new(database.clone())?
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    tokio::fs::write(root.join("银河护卫队.2024.mkv"), b"one").await?;
    tokio::fs::write(root.join("Hidden.Movie.2025.mkv"), b"two").await?;
    tokio::fs::write(root.join("Movie.2026.mkv"), b"three").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let chinese_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE title = '银河护卫队'")
            .fetch_one(database.pool())
            .await?;
    let hidden_movie_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE title = 'Hidden Movie'")
            .fetch_one(database.pool())
            .await?;
    sqlx::query(
        "INSERT INTO item_aliases (id, item_id, alias, language, alias_normalized)
         VALUES (?, ?, '星际守护者', 'zh-CN', '星际守护者')",
    )
    .bind(uuid::Uuid::now_v7().to_string())
    .bind(&chinese_id)
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
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="SearchTest", Device="Mac", DeviceId="search-admin", Version="1""#,
        )
        .json(&json!({ "Username": "admin", "Pw": "correct password" }))
        .send()
        .await?;
    let token = login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing token")?
        .to_owned();
    let hints = client
        .get(format!("{base_url}/Search/Hints"))
        .query(&[("api_key", token.as_str()), ("SearchTerm", "星际守护者")])
        .send()
        .await?;
    assert_eq!(hints.status(), reqwest::StatusCode::OK);
    let hints_body = hints.json::<Value>().await?;
    assert_eq!(hints_body["TotalRecordCount"], 1);
    assert_eq!(hints_body["SearchHints"][0]["Id"], chinese_id);

    let item_search = client
        .get(format!("{base_url}/Users/{}/Items", admin.id))
        .query(&[
            ("api_key", token.as_str()),
            ("SearchTerm", "星际守护者"),
            ("IncludeItemTypes", "Movie,Series"),
            ("Recursive", "true"),
        ])
        .send()
        .await?;
    assert_eq!(item_search.status(), reqwest::StatusCode::OK);
    let item_search_body = item_search.json::<Value>().await?;
    assert_eq!(item_search_body["TotalRecordCount"], 1);
    assert_eq!(item_search_body["Items"][0]["Id"], chinese_id);

    let lux_login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let session = lux_login
        .headers()
        .get_all("set-cookie")
        .iter()
        .find_map(|value| {
            let value = value.to_str().ok()?;
            value.strip_prefix("lux_session=")?.split(';').next()
        })
        .ok_or("missing session")?;
    let lux = client
        .get(format!("{base_url}/api/v1/search?q=银河"))
        .header("Cookie", format!("lux_session={session}"))
        .send()
        .await?;
    assert_eq!(lux.status(), reqwest::StatusCode::OK);
    let lux_body = lux.json::<Value>().await?;
    assert_eq!(lux_body["total"], 1);
    assert_eq!(lux_body["items"][0]["id"], chinese_id);

    let multi_word = client
        .get(format!("{base_url}/api/v1/search?q=Hidden%20Movie"))
        .header("Cookie", format!("lux_session={session}"))
        .send()
        .await?;
    assert_eq!(multi_word.status(), reqwest::StatusCode::OK);
    let multi_word_body = multi_word.json::<Value>().await?;
    assert_eq!(multi_word_body["total"], 1);
    assert_eq!(multi_word_body["items"][0]["id"], hidden_movie_id);

    let substring = client
        .get(format!("{base_url}/api/v1/search?q=idden"))
        .header("Cookie", format!("lux_session={session}"))
        .send()
        .await?;
    assert_eq!(substring.status(), reqwest::StatusCode::OK);
    let substring_body = substring.json::<Value>().await?;
    assert_eq!(substring_body["total"], 1);
    assert_eq!(substring_body["items"][0]["id"], hidden_movie_id);

    let viewer_login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            AUTHORIZATION,
            r#"Emby Client="SearchTest", Device="Mac", DeviceId="search-viewer", Version="1""#,
        )
        .json(&json!({ "Username": "viewer", "Pw": "viewer password" }))
        .send()
        .await?;
    let viewer_token = viewer_login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing viewer token")?
        .to_owned();
    let denied = client
        .get(format!("{base_url}/Search/Hints"))
        .query(&[("api_key", viewer_token.as_str()), ("SearchTerm", "银河")])
        .send()
        .await?;
    assert_eq!(denied.status(), reqwest::StatusCode::OK);
    assert_eq!(denied.json::<Value>().await?["TotalRecordCount"], 0);
    assert_ne!(admin.id, viewer.id);
    server.abort();
    Ok(())
}
