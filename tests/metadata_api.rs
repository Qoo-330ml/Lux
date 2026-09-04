mod common;

use std::time::Duration;

use axum::{Json, Router, routing::any};
use common::{TestScraper, TestScraperConfig};
use luxd::{
    api::{AppState, app_with_state},
    application::{
        libraries::LibraryService, scanner::LibraryScanner, setup::SetupService,
        webhooks::WebhookService,
    },
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
    tmdb: Option<TestScraper>,
) -> Result<(String, tokio::task::JoinHandle<Result<(), std::io::Error>>), Box<dyn std::error::Error>>
{
    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let state = AppState::ready(config, database, setup, auth, emby_auth);
    let state = tmdb.map_or(state.clone(), |tmdb| state.with_scraper(tmdb.provider()));
    let app = app_with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    Ok((
        format!("http://{address}"),
        tokio::spawn(async move { axum::serve(listener, app).await }),
    ))
}

async fn tmdb_search_stub(uri: axum::http::Uri) -> Json<Value> {
    if uri.path().ends_with("/search/tv") {
        if !uri
            .query()
            .is_some_and(|query| query.contains("query=%E6%9A%97"))
        {
            return Json(json!({
                "page": 1,
                "total_pages": 1,
                "total_results": 0,
                "results": []
            }));
        }
        return Json(json!({
            "page": 1,
            "total_pages": 1,
            "total_results": 1,
            "results": [{
                "id": 998,
                "name": "暗夜与黎明",
                "original_name": "Dark Night and Dawn",
                "overview": "A series stub.",
                "first_air_date": "2024-09-20",
                "original_language": "zh"
            }]
        }));
    }
    if uri.path().ends_with("/3/tv/998/season/1/episode/1") {
        return Json(json!({
            "id": 1999,
            "name": "第一集",
            "overview": "Episode stub.",
            "air_date": "2024-09-21",
            "episode_number": 1,
            "season_number": 1,
            "runtime": 45
        }));
    }
    if uri.path().ends_with("/3/tv/998/season/1") {
        return Json(json!({
            "id": 1998,
            "name": "第一季",
            "overview": "Season stub.",
            "air_date": "2024-09-20",
            "season_number": 1,
            "episodes": [{
                "id": 1999,
                "name": "第一集",
                "air_date": "2024-09-21",
                "episode_number": 1,
                "season_number": 1
            }]
        }));
    }
    if uri.path().ends_with("/images") {
        return Json(json!({
            "posters": [
                { "file_path": "/poster-first.jpg", "iso_639_1": "zh" },
                { "file_path": "/poster-second.jpg", "iso_639_1": "en" }
            ],
            "backdrops": [
                { "file_path": "/backdrop-first.jpg", "iso_639_1": null }
            ],
            "logos": [
                { "file_path": "/logo-first.png", "iso_639_1": null }
            ]
        }));
    }
    Json(json!({
        "page": 1,
        "total_pages": 1,
        "total_results": 1,
        "results": [{
            "id": 999,
            "title": "Example Movie",
            "original_title": "Example Movie",
            "overview": "A local stub result.",
            "release_date": "2020-04-01",
            "original_language": "en"
        }]
    }))
}

async fn start_tmdb_stub()
-> Result<(String, tokio::task::JoinHandle<Result<(), std::io::Error>>), Box<dyn std::error::Error>>
{
    let app = Router::new().fallback(any(tmdb_search_stub));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    Ok((
        format!("http://{address}"),
        tokio::spawn(async move { axum::serve(listener, app).await }),
    ))
}

fn cookie_from_request(cookie: &str, name: &str) -> String {
    cookie
        .split("; ")
        .find_map(|part| {
            let (key, value) = part.split_once('=')?;
            (key == name).then(|| value.to_owned())
        })
        .expect("expected request cookie")
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
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE' LIMIT 1")
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

    let (tmdb_url, tmdb_server) = start_tmdb_stub().await?;
    let tmdb = TestScraper::new(TestScraperConfig {
        base_url: tmdb_url,
        read_access_token: Some("stub-token".to_owned()),
        requests_per_second: 0,
        max_retries: 0,
        ..TestScraperConfig::default()
    })?;
    let (base_url, server) = start_server(config, database, setup, Some(tmdb)).await?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;
    let admin_cookie = login(&client, &base_url, "admin", "correct password").await?;
    let viewer_cookie = login(&client, &base_url, "viewer", "viewer password").await?;

    let library_items = client
        .get(format!(
            "{base_url}/api/v1/libraries/{}/items?page=1&pageSize=10",
            library.id
        ))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(library_items.status(), reqwest::StatusCode::OK);
    let library_items_body: Value = library_items.json().await?;
    assert_eq!(library_items_body["items"][0]["metadataPending"], true);

    let item_detail = client
        .get(format!("{base_url}/api/v1/items/{item_id}"))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(item_detail.status(), reqwest::StatusCode::OK);
    let item_detail_body: Value = item_detail.json().await?;
    assert_eq!(item_detail_body["metadataPending"], true);

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

    let reidentified = client
        .post(format!(
            "{base_url}/api/v1/admin/items/{item_id}/identify/candidates"
        ))
        .header(COOKIE, &admin_cookie)
        .header(
            "x-csrf-token",
            cookie_from_request(&admin_cookie, "lux_csrf"),
        )
        .json(&json!({ "query": "Example Movie", "year": 2020 }))
        .send()
        .await?;
    assert_eq!(reidentified.status(), reqwest::StatusCode::OK);
    let reidentified_body: Value = reidentified.json().await?;
    assert!(reidentified_body["items"].as_array().is_some_and(|items| {
        items.iter().any(|item| {
            item["providerId"] == "999"
                && item["candidate"]["productionYear"] == 2020
                && item["score"] == 95.0
        })
    }));
    let searched_candidate = reidentified_body["items"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["providerId"] == "999"))
        .ok_or("missing searched candidate")?;
    assert_eq!(
        searched_candidate["candidate"]["images"]["POSTER"][0],
        "https://image.tmdb.org/t/p/w780/poster-first.jpg"
    );
    assert_eq!(
        searched_candidate["candidate"]["images"]["LOGO"][0],
        "https://image.tmdb.org/t/p/w780/logo-first.png"
    );
    assert_eq!(
        searched_candidate["candidate"]["images"]["ART"][0],
        "https://image.tmdb.org/t/p/w780/backdrop-first.jpg"
    );

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
    tmdb_server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_read_and_edit_item_metadata_with_field_locks()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Example Movie (2020)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Example.Movie.2020.mkv"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE' LIMIT 1")
            .fetch_one(database.pool())
            .await?;

    let (base_url, server) = start_server(config, database, setup, None).await?;
    let client = reqwest::Client::new();
    let admin_cookie = login(&client, &base_url, "admin", "correct password").await?;
    let csrf = cookie_from_request(&admin_cookie, "lux_csrf");

    let before = client
        .get(format!("{base_url}/api/v1/items/{item_id}/metadata"))
        .header(COOKIE, &admin_cookie)
        .send()
        .await?;
    assert_eq!(before.status(), reqwest::StatusCode::OK);
    let before_body: Value = before.json().await?;
    assert_eq!(before_body["title"], "Example Movie");
    assert_eq!(before_body["lockedFields"], json!([]));

    let updated = client
        .patch(format!("{base_url}/api/v1/items/{item_id}/metadata"))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", csrf)
        .json(&json!({
            "title": "Edited Movie",
            "originalTitle": "Edited Original",
            "overview": "Edited overview",
            "productionYear": 2021,
            "lockedFields": ["title"]
        }))
        .send()
        .await?;
    assert_eq!(updated.status(), reqwest::StatusCode::OK);
    let updated_body: Value = updated.json().await?;
    assert_eq!(updated_body["title"], "Edited Movie");
    assert_eq!(updated_body["productionYear"], 2021);
    assert_eq!(updated_body["lockedFields"], json!(["title"]));
    let nfo = tokio::fs::read_to_string(movie_dir.join("movie.nfo")).await?;
    assert!(nfo.contains("Edited Movie"));
    assert!(nfo.contains("Edited overview"));

    server.abort();
    Ok(())
}

#[tokio::test]
async fn series_candidate_search_uses_tmdb_tv_endpoint_and_cleaned_year()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    let root = temp_dir.path().join("Shows");
    let episode =
        root.join("暗夜与黎明 (2024)/Season 1/暗夜与黎明 S01E01 - 2160p H.265 AAC CHDWEB.strm");
    if let Some(parent) = episode.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&episode, b"").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_series_library(library.id)
        .await?;
    let series_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE item_type = 'SERIES' AND removed_at IS NULL",
    )
    .fetch_one(database.pool())
    .await?;
    let series: (String, i64) =
        sqlx::query_as("SELECT title, production_year FROM media_items WHERE id = ?")
            .bind(&series_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(series, ("暗夜与黎明".to_owned(), 2024));

    let (tmdb_url, tmdb_server) = start_tmdb_stub().await?;
    let tmdb = TestScraper::new(TestScraperConfig {
        base_url: tmdb_url,
        read_access_token: Some("stub-token".to_owned()),
        requests_per_second: 0,
        max_retries: 0,
        ..TestScraperConfig::default()
    })?;
    let (base_url, server) = start_server(config, database, setup, Some(tmdb)).await?;
    let client = reqwest::Client::new();
    let admin_cookie = login(&client, &base_url, "admin", "correct password").await?;
    let csrf = cookie_from_request(&admin_cookie, "lux_csrf");
    let response = client
        .post(format!(
            "{base_url}/api/v1/admin/items/{series_id}/identify/candidates"
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", csrf)
        .json(&json!({ "query": "暗夜与黎明 S01E01 - 2160p H.265 AAC CHDWEB.strm" }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await?;
    assert_eq!(body["items"][0]["providerId"], "998");
    assert_eq!(body["items"][0]["candidate"]["title"], "暗夜与黎明");

    server.abort();
    tmdb_server.abort();
    Ok(())
}

#[tokio::test]
async fn season_and_episode_candidate_search_uses_parent_series_details()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    let root = temp_dir.path().join("Shows");
    let episode =
        root.join("暗夜与黎明 (2024)/Season 1/暗夜与黎明 S01E01 - 2160p H.265 AAC CHDWEB.strm");
    if let Some(parent) = episode.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&episode, b"").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_series_library(library.id)
        .await?;
    let season_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE item_type = 'SEASON' AND removed_at IS NULL",
    )
    .fetch_one(database.pool())
    .await?;
    let episode_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE item_type = 'EPISODE' AND removed_at IS NULL",
    )
    .fetch_one(database.pool())
    .await?;

    let (tmdb_url, tmdb_server) = start_tmdb_stub().await?;
    let tmdb = TestScraper::new(TestScraperConfig {
        base_url: tmdb_url,
        read_access_token: Some("stub-token".to_owned()),
        requests_per_second: 0,
        max_retries: 0,
        ..TestScraperConfig::default()
    })?;
    let (base_url, server) = start_server(config, database, setup, Some(tmdb)).await?;
    let client = reqwest::Client::new();
    let admin_cookie = login(&client, &base_url, "admin", "correct password").await?;
    let csrf = cookie_from_request(&admin_cookie, "lux_csrf");

    let season = client
        .post(format!(
            "{base_url}/api/v1/admin/items/{season_id}/identify/candidates"
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "query": "第一季" }))
        .send()
        .await?;
    assert_eq!(season.status(), reqwest::StatusCode::OK);
    let season_body: Value = season.json().await?;
    assert_eq!(season_body["items"][0]["providerId"], "1998");
    assert_eq!(season_body["items"][0]["candidate"]["title"], "第一季");
    assert!(
        season_body["items"][0]["candidate"]["images"]["POSTER"]
            .as_array()
            .is_some_and(|images| !images.is_empty())
    );

    let episode = client
        .post(format!(
            "{base_url}/api/v1/admin/items/{episode_id}/identify/candidates"
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "query": "第一集" }))
        .send()
        .await?;
    assert_eq!(episode.status(), reqwest::StatusCode::OK);
    let episode_body: Value = episode.json().await?;
    assert_eq!(episode_body["items"][0]["providerId"], "1999");
    assert_eq!(episode_body["items"][0]["candidate"]["title"], "第一集");
    assert!(
        episode_body["items"][0]["candidate"]["images"]["POSTER"]
            .as_array()
            .is_some_and(|images| !images.is_empty())
    );

    server.abort();
    tmdb_server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_start_a_scan_from_an_item_action() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Example Movie (2020)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Example.Movie.2020.mkv"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE' LIMIT 1")
            .fetch_one(database.pool())
            .await?;

    let (base_url, server) = start_server(config, database, setup, None).await?;
    let client = reqwest::Client::new();
    let admin_cookie = login(&client, &base_url, "admin", "correct password").await?;
    let csrf = cookie_from_request(&admin_cookie, "lux_csrf");
    let response = client
        .post(format!("{base_url}/api/v1/admin/items/{item_id}/scan"))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", csrf)
        .send()
        .await?;

    assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
    let body: Value = response.json().await?;
    assert_eq!(body["job"]["libraryId"], library.id.to_string());

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_edit_an_external_subtitle_stream() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Example Movie (2020)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Example.Movie.2020.mkv"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE' LIMIT 1")
            .fetch_one(database.pool())
            .await?;
    let source_id: String = sqlx::query_scalar("SELECT id FROM media_sources WHERE item_id = ?")
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?;
    sqlx::query(
        "INSERT INTO media_streams
         (id, media_source_id, stream_index, stream_type, language, title,
          external_path, is_external, is_default, is_forced)
         VALUES (?, ?, 2, 'SUBTITLE', 'eng', 'English', 'Example.Movie.2020.en.srt', 1, 0, 0)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(&source_id)
    .execute(database.pool())
    .await?;

    let (base_url, server) = start_server(config, database.clone(), setup, None).await?;
    let client = reqwest::Client::new();
    let admin_cookie = login(&client, &base_url, "admin", "correct password").await?;
    let csrf = cookie_from_request(&admin_cookie, "lux_csrf");
    let response = client
        .patch(format!(
            "{base_url}/api/v1/admin/items/{item_id}/subtitles/2"
        ))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", csrf)
        .json(&json!({
            "sourceId": source_id,
            "title": "简体中文",
            "language": "zho",
            "isDefault": true,
            "isForced": false
        }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await?;
    assert_eq!(body["title"], "简体中文");
    assert_eq!(body["language"], "zho");
    assert_eq!(body["isDefault"], true);

    let flags: (String, String, i64, i64) = sqlx::query_as(
        "SELECT language, title, is_default, is_forced FROM media_streams
         WHERE media_source_id = ? AND stream_index = 2",
    )
    .bind(&source_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(flags, ("zho".to_owned(), "简体中文".to_owned(), 1, 0));

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_delete_a_media_source_and_matching_sidecars()
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
    let media = movie_dir.join("Example.Movie.2020.mkv");
    let subtitle = movie_dir.join("Example.Movie.2020.zh.srt");
    let nfo = movie_dir.join("Example.Movie.2020.nfo");
    tokio::fs::write(&media, b"fixture").await?;
    tokio::fs::write(&subtitle, b"subtitle").await?;
    tokio::fs::write(&nfo, b"nfo").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE' LIMIT 1")
            .fetch_one(database.pool())
            .await?;
    let source_id: String = sqlx::query_scalar("SELECT id FROM media_sources WHERE item_id = ?")
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?;

    let webhooks = WebhookService::new(database.clone(), config.config_dir.clone())?;
    webhooks
        .create_destination(
            "Deletion test receiver",
            "https://example.com/lux-hook",
            true,
            false,
            &["MEDIA_REMOVED".to_owned()],
            Some("webhook-test-secret-1234"),
        )
        .await?;

    let (base_url, server) = start_server(config, database.clone(), setup, None).await?;
    let client = reqwest::Client::new();
    let viewer_login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            reqwest::header::AUTHORIZATION,
            r#"Emby Client="DeletionTest", Device="Mac", DeviceId="deletion-viewer", Version="1""#,
        )
        .json(&json!({ "Username": "viewer", "Pw": "viewer password" }))
        .send()
        .await?;
    assert_eq!(viewer_login.status(), reqwest::StatusCode::OK);
    let viewer_token = viewer_login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing viewer Emby token")?
        .to_owned();
    let forbidden = client
        .delete(format!("{base_url}/Items/{item_id}"))
        .header("X-Emby-Token", &viewer_token)
        .send()
        .await?;
    assert_eq!(forbidden.status(), reqwest::StatusCode::FORBIDDEN);
    assert!(media.exists());

    let emby_login = client
        .post(format!("{base_url}/Users/AuthenticateByName"))
        .header(
            reqwest::header::AUTHORIZATION,
            r#"Emby Client="DeletionTest", Device="Mac", DeviceId="deletion-admin", Version="1""#,
        )
        .json(&json!({ "Username": "admin", "Pw": "correct password" }))
        .send()
        .await?;
    assert_eq!(emby_login.status(), reqwest::StatusCode::OK);
    let emby_token = emby_login.json::<Value>().await?["AccessToken"]
        .as_str()
        .ok_or("missing Emby token")?
        .to_owned();
    let response = client
        .delete(format!("{base_url}/Items/{item_id}"))
        .query(&[("MediaSourceId", source_id.as_str())])
        .header("X-Emby-Token", &emby_token)
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    assert!(!media.exists());
    assert!(!subtitle.exists());
    assert!(!nfo.exists());
    let source_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_sources WHERE item_id = ?")
            .bind(&item_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(source_count, 0);
    let removed_events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notification_events WHERE event_type = 'MEDIA_REMOVED'",
    )
    .fetch_one(database.pool())
    .await?;
    assert_eq!(removed_events, 1);

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_remove_a_missing_media_source_from_lux() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    let root = temp_dir.path().join("Movies");
    tokio::fs::create_dir_all(&root).await?;
    let media = root.join("Missing.Movie.2024.strm");
    tokio::fs::write(&media, "https://media.example.test/missing").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String =
        sqlx::query_scalar("SELECT id FROM media_items WHERE item_type = 'MOVIE' LIMIT 1")
            .fetch_one(database.pool())
            .await?;
    let source_id: String = sqlx::query_scalar("SELECT id FROM media_sources WHERE item_id = ?")
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?;
    tokio::fs::remove_file(&media).await?;

    let (base_url, server) = start_server(config, database.clone(), setup, None).await?;
    let client = reqwest::Client::new();
    let admin_cookie = login(&client, &base_url, "admin", "correct password").await?;
    let csrf = cookie_from_request(&admin_cookie, "lux_csrf");
    let response = client
        .delete(format!("{base_url}/api/v1/admin/items/{item_id}"))
        .query(&[("sourceId", source_id.as_str())])
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", csrf)
        .send()
        .await?;

    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    let source_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM media_sources WHERE item_id = ?")
            .bind(&item_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(source_count, 0);
    let removed_at: Option<i64> =
        sqlx::query_scalar("SELECT removed_at FROM media_items WHERE id = ?")
            .bind(&item_id)
            .fetch_one(database.pool())
            .await?;
    assert!(removed_at.is_some());

    server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_delete_a_series_and_all_episode_media() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
    let setup = SetupService::new(database.clone())?;
    setup.complete("Admin", "Admin", "correct password").await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    let root = temp_dir.path().join("Shows");
    let season_dir = root.join("Example Show (2024)").join("Season 1");
    tokio::fs::create_dir_all(&season_dir).await?;
    let episode_one = season_dir.join("Example.Show.S01E01.mkv");
    let episode_one_subtitle = season_dir.join("Example.Show.S01E01.zh.srt");
    let episode_two = season_dir.join("Example.Show.S01E02.mkv");
    let episode_two_nfo = season_dir.join("Example.Show.S01E02.nfo");
    tokio::fs::write(&episode_one, b"fixture").await?;
    tokio::fs::write(&episode_one_subtitle, b"subtitle").await?;
    tokio::fs::write(&episode_two, b"fixture").await?;
    tokio::fs::write(&episode_two_nfo, b"nfo").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_series_library(library.id)
        .await?;
    let series_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE item_type = 'SERIES' AND removed_at IS NULL",
    )
    .fetch_one(database.pool())
    .await?;
    let source_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_sources ms
         JOIN media_items mi ON mi.id = ms.item_id
         WHERE mi.series_id = ? OR mi.id = ?",
    )
    .bind(&series_id)
    .bind(&series_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(source_count, 2);

    let (base_url, server) = start_server(config, database.clone(), setup, None).await?;
    let client = reqwest::Client::new();
    let admin_cookie = login(&client, &base_url, "admin", "correct password").await?;
    let csrf = cookie_from_request(&admin_cookie, "lux_csrf");
    let response = client
        .delete(format!("{base_url}/api/v1/admin/items/{series_id}"))
        .header(COOKIE, &admin_cookie)
        .header("x-csrf-token", csrf)
        .send()
        .await?;

    assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);
    assert!(!episode_one.exists());
    assert!(!episode_one_subtitle.exists());
    assert!(!episode_two.exists());
    assert!(!episode_two_nfo.exists());
    let remaining_sources: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM media_sources ms
         JOIN media_items mi ON mi.id = ms.item_id
         WHERE mi.series_id = ? OR mi.id = ?",
    )
    .bind(&series_id)
    .bind(&series_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(remaining_sources, 0);
    let visible_items: i64 = sqlx::query_scalar(
        "WITH RECURSIVE descendants(id) AS (
             SELECT id FROM media_items WHERE id = ?
             UNION ALL
             SELECT child.id FROM media_items child
             JOIN descendants parent ON child.parent_id = parent.id
         )
         SELECT COUNT(*) FROM media_items
         WHERE id IN (SELECT id FROM descendants) AND removed_at IS NULL",
    )
    .bind(&series_id)
    .fetch_one(database.pool())
    .await?;
    assert_eq!(visible_items, 0);

    server.abort();
    Ok(())
}
