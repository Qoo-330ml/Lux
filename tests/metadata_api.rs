use std::time::Duration;

use luxd::{
    api::{AppState, app_with_state},
    application::{libraries::LibraryService, scanner::LibraryScanner, setup::SetupService},
    auth::{emby::EmbyAuthService, sessions::WebAuthService, users::UserStore},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use uuid::Uuid;

async fn start_server(
    config: Config,
    database: Database,
    setup: SetupService,
) -> Result<(String, tokio::task::JoinHandle<Result<(), std::io::Error>>), Box<dyn std::error::Error>>
{
    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(config, database, setup, auth, emby_auth));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    Ok((
        format!("http://{address}"),
        tokio::spawn(async move { axum::serve(listener, app).await }),
    ))
}

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

async fn login(
    client: &reqwest::Client,
    base_url: &str,
    username: &str,
    password: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let response = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": username, "password": password }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    Ok(format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(response.headers(), "lux_session"),
        cookie_value(response.headers(), "lux_csrf")
    ))
}

#[tokio::test]
async fn admin_can_page_search_and_preview_pending_candidates()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let users = UserStore::new(database.clone())?;
    users
        .create_user("viewer", "Viewer", "viewer password", false)
        .await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Example Movie (2020)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Example.Movie.2020.mkv"), b"fixture").await?;
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
        "INSERT INTO metadata_candidates
         (id, item_id, provider, provider_id, candidate_json, score, status, expires_at)
         VALUES (?, ?, 'TMDB', '603', ?, 82.5, 'PENDING', unixepoch() + 3600)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(&item_id)
    .bind(
        json!({
            "title": "Online Title",
            "overview": "Online overview",
            "productionYear": 2021
        })
        .to_string(),
    )
    .execute(database.pool())
    .await?;
    sqlx::query(
        "INSERT INTO metadata_candidates
         (id, item_id, provider, provider_id, candidate_json, score, status)
         VALUES (?, ?, 'TMDB', '604', '{}', 10, 'REJECTED')",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(&item_id)
    .execute(database.pool())
    .await?;

    let (base_url, server) = start_server(config, database, setup).await?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;
    let admin_cookie = login(&client, &base_url, "admin", "correct password").await?;
    let viewer_cookie = login(&client, &base_url, "viewer", "viewer password").await?;

    let pending = client
        .get(format!(
            "{base_url}/api/v1/admin/metadata/pending?page=1&pageSize=1"
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(pending.status(), reqwest::StatusCode::OK);
    let pending_body: Value = pending.json().await?;
    assert_eq!(pending_body["total"], 1);
    assert_eq!(pending_body["pageSize"], 1);
    assert_eq!(pending_body["items"][0]["itemId"], item_id);
    assert_eq!(pending_body["items"][0]["fieldDiffs"][0]["field"], "title");
    assert_eq!(
        pending_body["items"][0]["fieldDiffs"][0]["current"],
        "Example Movie"
    );

    let searched = client
        .get(format!(
            "{base_url}/api/v1/admin/items/{item_id}/identify/candidates?q=Online&pageSize=1"
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(searched.status(), reqwest::StatusCode::OK);
    let searched_body: Value = searched.json().await?;
    assert_eq!(searched_body["total"], 1);
    assert_eq!(
        searched_body["items"][0]["candidate"]["title"],
        "Online Title"
    );
    assert_eq!(searched_body["items"][0]["score"], 82.5);

    let forbidden = client
        .get(format!("{base_url}/api/v1/admin/metadata/pending"))
        .header(COOKIE, &viewer_cookie)
        .send()
        .await?;
    assert_eq!(forbidden.status(), reqwest::StatusCode::FORBIDDEN);

    let invalid_page = client
        .get(format!(
            "{base_url}/api/v1/admin/metadata/pending?pageSize=101"
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(invalid_page.status(), reqwest::StatusCode::BAD_REQUEST);

    let missing_item = client
        .get(format!(
            "{base_url}/api/v1/admin/items/{}/identify/candidates",
            Uuid::now_v7()
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(missing_item.status(), reqwest::StatusCode::NOT_FOUND);

    server.abort();
    Ok(())
}
