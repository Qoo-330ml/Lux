mod common;

use std::time::Duration;

use axum::{Json, Router, routing::any};
use common::{TestScraper, TestScraperConfig};
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

async fn image_stub(request: axum::http::Request<axum::body::Body>) -> Json<Value> {
    let query = request.uri().query().unwrap_or_default();
    let posters = if query.is_empty() {
        json!([
            {
                "file_path": "/poster-zh.jpg",
                "iso_639_1": "zh",
                "width": 1000,
                "height": 1500,
                "vote_average": 8.0
            },
            {
                "file_path": "/poster-en.jpg",
                "iso_639_1": "en",
                "width": 1000,
                "height": 1500,
                "vote_average": 8.0
            },
            {
                "file_path": "/poster-ja.jpg",
                "iso_639_1": "ja",
                "width": 1000,
                "height": 1500,
                "vote_average": 8.0
            }
        ])
    } else if query.contains("language=zh-CN") {
        json!([
            {
                "file_path": "/poster.jpg",
                "iso_639_1": "zh",
                "width": 1000,
                "height": 1500,
                "vote_average": 8.0
            }
        ])
    } else {
        json!([
            {
                "file_path": "/poster-en.jpg",
                "iso_639_1": "en",
                "width": 1000,
                "height": 1500,
                "vote_average": 8.0
            }
        ])
    };
    Json(json!({
        "posters": posters,
        "backdrops": [],
        "logos": []
    }))
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

#[tokio::test]
async fn image_search_returns_filtered_scraper_candidates() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let tmdb_app = Router::new().fallback(any(image_stub));
    let tmdb_listener = TcpListener::bind("127.0.0.1:0").await?;
    let tmdb_address = tmdb_listener.local_addr()?;
    let tmdb_server = tokio::spawn(async move { axum::serve(tmdb_listener, tmdb_app).await });

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
    sqlx::query("UPDATE media_items SET provider_ids_json = '{\"tmdb\":\"999\"}' WHERE id = ?")
        .bind(&item_id)
        .execute(database.pool())
        .await?;

    let tmdb = TestScraper::new(TestScraperConfig {
        base_url: format!("http://{tmdb_address}"),
        read_access_token: Some("stub-token".to_owned()),
        requests_per_second: 0,
        max_retries: 0,
        ..TestScraperConfig::default()
    })?;
    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(
        AppState::ready(config, database, setup, auth, emby_auth).with_scraper(tmdb.provider()),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;
    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let cookie = format!(
        "lux_session={}; lux_csrf={}",
        cookie_value(login.headers(), "lux_session"),
        cookie_value(login.headers(), "lux_csrf")
    );
    let response = client
        .post(format!("{base_url}/api/v1/items/{item_id}/images/search"))
        .header(COOKIE, cookie.clone())
        .json(&json!({ "imageType": "POSTER", "language": "zh-CN", "source": "TMDB" }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await?;
    assert_eq!(
        body["images"][0]["url"],
        "https://image.tmdb.org/t/p/w780/poster.jpg"
    );
    assert_eq!(body["images"][0]["language"], "zh");

    let response = client
        .post(format!("{base_url}/api/v1/items/{item_id}/images/search"))
        .header(COOKIE, cookie)
        .json(&json!({ "imageType": "POSTER", "language": "", "source": "TMDB" }))
        .send()
        .await?;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: Value = response.json().await?;
    let languages = body["images"]
        .as_array()
        .ok_or("image result list is missing")?
        .iter()
        .filter_map(|image| image["language"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(languages, vec!["zh", "en", "ja"]);

    server.abort();
    tmdb_server.abort();
    Ok(())
}
