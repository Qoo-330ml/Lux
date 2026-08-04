use std::time::Duration;

use axum::{Json, Router, routing::any};
use luxd::{
    api::{AppState, app_with_state},
    application::{
        libraries::LibraryService,
        scanner::LibraryScanner,
        setup::SetupService,
        tmdb::{TmdbClient, TmdbClientConfig},
    },
    auth::{emby::EmbyAuthService, sessions::WebAuthService},
    config::Config,
    library::LibraryKind,
    storage::Database,
};
use reqwest::header::{COOKIE, SET_COOKIE};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use uuid::Uuid;

async fn tmdb_search_stub() -> Json<Value> {
    Json(json!({
        "page": 1,
        "total_pages": 1,
        "total_results": 1,
        "results": [{
            "id": 999,
            "title": "Batch Movie",
            "original_title": "Batch Movie",
            "overview": "A local batch reidentify result.",
            "release_date": "2024-04-01",
            "original_language": "en"
        }]
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
async fn admin_can_start_and_poll_batch_metadata_reidentify()
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
    let movie_dir = root.join("Batch Movie (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Batch.Movie.2024.mkv"), b"fixture").await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    let item_id: String = sqlx::query_scalar("SELECT id FROM media_items LIMIT 1")
        .fetch_one(database.pool())
        .await?;

    let tmdb_app = Router::new().fallback(any(tmdb_search_stub));
    let tmdb_listener = TcpListener::bind("127.0.0.1:0").await?;
    let tmdb_address = tmdb_listener.local_addr()?;
    let tmdb_server = tokio::spawn(async move { axum::serve(tmdb_listener, tmdb_app).await });
    let tmdb = TmdbClient::new(TmdbClientConfig {
        base_url: format!("http://{tmdb_address}"),
        api_key: None,
        read_access_token: Some("stub-token".to_owned()),
        timeout: Duration::from_secs(1),
        max_retries: 0,
        initial_backoff: Duration::ZERO,
        max_backoff: Duration::ZERO,
        retry_jitter: Duration::ZERO,
        requests_per_second: 0,
    })?;
    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(
        AppState::ready(config, database.clone(), setup, auth, emby_auth).with_tmdb_client(tmdb),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let base_url = format!("http://{address}");
    let client = reqwest::Client::new();

    let login = client
        .post(format!("{base_url}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let csrf = cookie_value(login.headers(), "lux_csrf");
    let cookies = format!(
        "lux_session={}; lux_csrf={csrf}",
        cookie_value(login.headers(), "lux_session")
    );

    let csrf_required = client
        .post(format!("{base_url}/api/v1/admin/metadata/reidentify"))
        .header(COOKIE, &cookies)
        .json(&json!({ "itemIds": [item_id.clone()] }))
        .send()
        .await?;
    assert_eq!(csrf_required.status(), reqwest::StatusCode::FORBIDDEN);

    let empty = client
        .post(format!("{base_url}/api/v1/admin/metadata/reidentify"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "itemIds": [] }))
        .send()
        .await?;
    assert_eq!(empty.status(), reqwest::StatusCode::BAD_REQUEST);

    let missing = client
        .post(format!("{base_url}/api/v1/admin/metadata/reidentify"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "itemIds": [Uuid::now_v7().to_string()] }))
        .send()
        .await?;
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

    let started = client
        .post(format!("{base_url}/api/v1/admin/metadata/reidentify"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "itemIds": [item_id.clone()] }))
        .send()
        .await?;
    assert_eq!(started.status(), reqwest::StatusCode::ACCEPTED);
    let started_body: Value = started.json().await?;
    let job_id = started_body["job"]["id"]
        .as_str()
        .ok_or("missing metadata reidentify job ID")?
        .to_owned();
    assert_eq!(started_body["job"]["totalCount"], 1);
    assert_eq!(started_body["job"]["mode"], "REIDENTIFY");

    let mut job = Value::Null;
    for _ in 0..80 {
        let response = client
            .get(format!(
                "{base_url}/api/v1/admin/metadata/reidentify/{job_id}"
            ))
            .header(COOKIE, &cookies)
            .send()
            .await?;
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        job = response.json().await?;
        if job["job"]["status"] == "COMPLETED" || job["job"]["status"] == "FAILED" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(job["job"]["status"], "COMPLETED");
    assert_eq!(job["job"]["processedCount"], 1);
    assert_eq!(job["job"]["items"][0]["itemId"], item_id);
    assert_eq!(job["job"]["items"][0]["status"], "COMPLETED");
    assert_eq!(job["job"]["items"][0]["candidateCount"], 1);

    let candidate_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM metadata_candidates WHERE item_id = ?")
            .bind(&item_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(candidate_count, 1);

    sqlx::query("UPDATE media_items SET title = '', sort_title = '' WHERE id = ?")
        .bind(&item_id)
        .execute(database.pool())
        .await?;
    let failed = client
        .post(format!("{base_url}/api/v1/admin/metadata/reidentify"))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "itemIds": [item_id.clone()] }))
        .send()
        .await?;
    assert_eq!(failed.status(), reqwest::StatusCode::ACCEPTED);
    let failed_job_id = failed.json::<Value>().await?["job"]["id"]
        .as_str()
        .ok_or("missing failed job ID")?
        .to_owned();
    let mut failed_job = Value::Null;
    for _ in 0..80 {
        let response = client
            .get(format!(
                "{base_url}/api/v1/admin/metadata/reidentify/{failed_job_id}"
            ))
            .header(COOKIE, &cookies)
            .send()
            .await?;
        failed_job = response.json().await?;
        if failed_job["job"]["status"] == "FAILED" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(failed_job["job"]["status"], "FAILED");
    assert_eq!(failed_job["job"]["items"][0]["error"], "INVALID_SEARCH");

    sqlx::query(
        "UPDATE media_items SET title = 'Batch Movie', sort_title = 'batch movie' WHERE id = ?",
    )
    .bind(&item_id)
    .execute(database.pool())
    .await?;
    let retry = client
        .post(format!(
            "{base_url}/api/v1/admin/metadata/reidentify/{failed_job_id}"
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(retry.status(), reqwest::StatusCode::ACCEPTED);
    let mut retried_job = Value::Null;
    for _ in 0..80 {
        let response = client
            .get(format!(
                "{base_url}/api/v1/admin/metadata/reidentify/{failed_job_id}"
            ))
            .header(COOKIE, &cookies)
            .send()
            .await?;
        retried_job = response.json().await?;
        if retried_job["job"]["status"] == "COMPLETED" || retried_job["job"]["status"] == "FAILED" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(retried_job["job"]["status"], "COMPLETED");
    assert_eq!(retried_job["job"]["items"][0]["status"], "COMPLETED");

    let unknown_job = client
        .get(format!(
            "{base_url}/api/v1/admin/metadata/reidentify/{}",
            Uuid::now_v7()
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(unknown_job.status(), reqwest::StatusCode::NOT_FOUND);

    let library_started = client
        .post(format!(
            "{base_url}/api/v1/admin/libraries/{}/reidentify",
            library.id
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(library_started.status(), reqwest::StatusCode::ACCEPTED);
    let library_body: Value = library_started.json().await?;
    assert_eq!(library_body["totalCount"], 1);
    assert_eq!(library_body["jobs"].as_array().map(Vec::len), Some(1));

    let refresh_started = client
        .post(format!(
            "{base_url}/api/v1/admin/libraries/{}/metadata/refresh",
            library.id
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "mode": "FULL_REFRESH" }))
        .send()
        .await?;
    assert_eq!(refresh_started.status(), reqwest::StatusCode::ACCEPTED);
    let refresh_body: Value = refresh_started.json().await?;
    assert_eq!(refresh_body["mode"], "FULL_REFRESH");
    assert_eq!(refresh_body["totalCount"], 1);
    assert_eq!(refresh_body["jobs"][0]["mode"], "FULL_REFRESH");
    let refresh_job_id = refresh_body["jobs"][0]["id"]
        .as_str()
        .ok_or("missing metadata refresh job ID")?
        .to_owned();
    let mut refresh_job = Value::Null;
    for _ in 0..80 {
        let response = client
            .get(format!(
                "{base_url}/api/v1/admin/metadata/reidentify/{refresh_job_id}"
            ))
            .header(COOKIE, &cookies)
            .send()
            .await?;
        refresh_job = response.json().await?;
        if refresh_job["job"]["status"] == "COMPLETED" || refresh_job["job"]["status"] == "FAILED" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(refresh_job["job"]["mode"], "FULL_REFRESH");
    assert_eq!(refresh_job["job"]["status"], "COMPLETED");
    assert_eq!(refresh_job["job"]["items"][0]["candidateCount"], 0);

    server.abort();
    tmdb_server.abort();
    Ok(())
}
