use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{Json, Router, extract::State as AxumState, routing::any};
use luxd::{
    api::{AppState, app_with_state},
    application::{
        candidates::MetadataSelectionService,
        images::ImageWriteService,
        libraries::LibraryService,
        metadata::MetadataEnricher,
        reidentify::{MetadataRefreshMode, MetadataReidentifyService},
        scanner::LibraryScanner,
        setup::SetupService,
        tmdb::{TmdbClient, TmdbClientConfig},
        tmdb_plugin::TmdbProvider,
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

#[derive(Clone)]
struct RequestTracker {
    requests: Arc<AtomicUsize>,
}

async fn complete_movie_tmdb_stub(AxumState(tracker): AxumState<RequestTracker>) -> Json<Value> {
    tracker.requests.fetch_add(1, Ordering::SeqCst);
    Json(json!({
        "page": 1,
        "total_pages": 1,
        "total_results": 1,
        "results": [{
            "id": 999,
            "title": "Complete Movie",
            "original_title": "Complete Movie Original",
            "overview": "Complete movie overview.",
            "release_date": "2024-04-01",
            "original_language": "en",
            "vote_average": 8.0
        }]
    }))
}

#[derive(Clone)]
struct ConcurrencyTracker {
    active: Arc<AtomicUsize>,
    maximum: Arc<AtomicUsize>,
}

async fn delayed_tmdb_stub(AxumState(tracker): AxumState<ConcurrencyTracker>) -> Json<Value> {
    let active = tracker.active.fetch_add(1, Ordering::SeqCst) + 1;
    let mut maximum = tracker.maximum.load(Ordering::SeqCst);
    while active > maximum {
        match tracker
            .maximum
            .compare_exchange(maximum, active, Ordering::SeqCst, Ordering::SeqCst)
        {
            Ok(_) => break,
            Err(observed) => maximum = observed,
        }
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    tracker.active.fetch_sub(1, Ordering::SeqCst);
    tmdb_search_stub().await
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
async fn admin_can_start_and_poll_metadata_reidentify() -> Result<(), Box<dyn std::error::Error>> {
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
        proxy_url: None,
        api_key: None,
        read_access_token: Some("stub-token".to_owned()),
        timeout: Duration::from_secs(1),
        max_retries: 0,
        initial_backoff: Duration::ZERO,
        max_backoff: Duration::ZERO,
        retry_jitter: Duration::ZERO,
        requests_per_second: 0,
    })?;
    let low_confidence_metadata = MetadataReidentifyService::with_selection(
        database.clone(),
        tmdb.clone(),
        Some(MetadataSelectionService::new(
            database.clone(),
            ImageWriteService::new(database.clone())?,
        )),
    );
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
    assert_eq!(started_body["job"]["libraryId"], library.id.to_string());

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

    let item_refresh_started = client
        .post(format!(
            "{base_url}/api/v1/admin/items/{item_id}/metadata/refresh"
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .json(&json!({ "mode": "FILL_MISSING" }))
        .send()
        .await?;
    assert_eq!(item_refresh_started.status(), reqwest::StatusCode::ACCEPTED);
    let item_refresh_body: Value = item_refresh_started.json().await?;
    assert_eq!(item_refresh_body["mode"], "FILL_MISSING");
    assert_eq!(item_refresh_body["totalCount"], 1);
    assert_eq!(item_refresh_body["job"]["mode"], "FILL_MISSING");

    let item_refresh_job_id = item_refresh_body["job"]["id"]
        .as_str()
        .ok_or("missing item metadata refresh job ID")?
        .to_owned();
    let mut item_refresh_job = Value::Null;
    for _ in 0..80 {
        let response = client
            .get(format!(
                "{base_url}/api/v1/admin/metadata/reidentify/{item_refresh_job_id}"
            ))
            .header(COOKIE, &cookies)
            .send()
            .await?;
        item_refresh_job = response.json().await?;
        if item_refresh_job["job"]["status"] == "COMPLETED"
            || item_refresh_job["job"]["status"] == "FAILED"
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(item_refresh_job["job"]["status"], "COMPLETED");

    sqlx::query(
        "UPDATE media_items SET title = 'Unrelated Local Title', sort_title = 'unrelated local title'
         WHERE id = ?",
    )
    .bind(&item_id)
    .execute(database.pool())
    .await?;
    sqlx::query("DELETE FROM metadata_candidates WHERE item_id = ?")
        .bind(&item_id)
        .execute(database.pool())
        .await?;
    let low_confidence_job = low_confidence_metadata
        .create_item_refresh_job(&item_id, MetadataRefreshMode::FullRefresh)
        .await?;
    low_confidence_metadata.run(&low_confidence_job.id).await;
    let low_confidence_job = low_confidence_metadata
        .get_job(&low_confidence_job.id)
        .await?;
    assert_eq!(low_confidence_job.status, "COMPLETED");
    assert_eq!(low_confidence_job.items[0].status, "COMPLETED");
    assert_eq!(
        low_confidence_job.items[0].error.as_deref(),
        Some("LOW_CONFIDENCE")
    );

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
    assert!(library_body["job"].is_object());
    assert!(library_body["jobs"].is_null());
    let library_job_id = library_body["job"]["id"]
        .as_str()
        .ok_or("missing library metadata job ID")?
        .to_owned();
    let mut library_job = Value::Null;
    for _ in 0..80 {
        let response = client
            .get(format!(
                "{base_url}/api/v1/admin/metadata/reidentify/{library_job_id}"
            ))
            .header(COOKIE, &cookies)
            .send()
            .await?;
        library_job = response.json().await?;
        if library_job["job"]["status"] == "COMPLETED" || library_job["job"]["status"] == "FAILED" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(library_job["job"]["mode"], "FILL_MISSING");
    assert_eq!(library_job["job"]["status"], "COMPLETED");

    let listed = client
        .get(format!(
            "{base_url}/api/v1/admin/metadata/reidentify?page=1&pageSize=50"
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    assert_eq!(listed.status(), reqwest::StatusCode::OK);
    let listed_body: Value = listed.json().await?;
    assert_eq!(listed_body["page"], 1);
    assert_eq!(listed_body["pageSize"], 50);
    assert!(listed_body["jobs"].as_array().is_some_and(|jobs| {
        jobs.iter().any(|job| {
            job["mode"] == "REIDENTIFY"
                && job["status"] == "COMPLETED"
                && job["processedCount"] == 1
                && job["libraryId"] == library.id.to_string()
        })
    }));

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
    assert_eq!(refresh_body["job"]["mode"], "FULL_REFRESH");
    assert!(refresh_body["jobs"].is_null());
    let refresh_job_id = refresh_body["job"]["id"]
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

#[tokio::test]
async fn item_metadata_refresh_includes_series_children() -> Result<(), Box<dyn std::error::Error>>
{
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Shows");
    tokio::fs::create_dir_all(root.join("Example Show/Season 01")).await?;
    tokio::fs::write(
        root.join("Example Show/Season 01/Example.Show.S01E01.mkv"),
        b"fixture",
    )
    .await?;
    tokio::fs::write(
        root.join("Example Show/Season 01/Example.Show.S01E02.mkv"),
        b"fixture",
    )
    .await?;

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Shows", LibraryKind::Series, false)
        .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    luxd::application::scanner::LibraryScanner::new(database.clone())
        .scan_series_library(library.id)
        .await?;
    let series_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE item_type = 'SERIES' AND removed_at IS NULL",
    )
    .fetch_one(database.pool())
    .await?;

    let tmdb = TmdbClient::new(TmdbClientConfig {
        base_url: "http://127.0.0.1:1".to_owned(),
        proxy_url: None,
        api_key: None,
        read_access_token: Some("stub-token".to_owned()),
        timeout: Duration::from_secs(1),
        max_retries: 0,
        initial_backoff: Duration::ZERO,
        max_backoff: Duration::ZERO,
        retry_jitter: Duration::ZERO,
        requests_per_second: 0,
    })?;
    let metadata = MetadataReidentifyService::new(database.clone(), TmdbProvider::from(tmdb));
    let job = metadata
        .create_item_refresh_job(&series_id, MetadataRefreshMode::FillMissing)
        .await?;

    assert_eq!(job.total_count, 4);
    let refreshed_types: Vec<String> = sqlx::query_scalar(
        "SELECT mi.item_type
         FROM metadata_reidentify_job_items ji
         JOIN media_items mi ON mi.id = ji.item_id
         WHERE ji.job_id = ?
         ORDER BY CASE mi.item_type WHEN 'SERIES' THEN 0 WHEN 'SEASON' THEN 1 ELSE 2 END,
                  mi.episode_number",
    )
    .bind(&job.id)
    .fetch_all(database.pool())
    .await?;
    assert_eq!(refreshed_types, ["SERIES", "SEASON", "EPISODE", "EPISODE"]);
    Ok(())
}

#[tokio::test]
async fn fill_missing_skips_complete_movie_without_scraper_request()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let root = temp_dir.path().join("Movies");
    let movie_dir = root.join("Complete Movie (2024)");
    tokio::fs::create_dir_all(&movie_dir).await?;
    tokio::fs::write(movie_dir.join("Complete.Movie.2024.mkv"), b"fixture").await?;
    tokio::fs::write(
        movie_dir.join("movie.nfo"),
        "<movie><title>Complete Movie</title><originaltitle>Complete Movie Original</originaltitle><year>2024</year><plot>Complete movie overview.</plot></movie>",
    )
    .await?;
    for image in ["poster", "fanart", "logo", "thumb"] {
        tokio::fs::write(movie_dir.join(format!("{image}.png")), b"fixture").await?;
    }

    let database = Database::connect(&config).await?;
    let libraries = LibraryService::new(database.clone());
    let library = libraries
        .create_library("Movies", LibraryKind::Movie, false)
        .await?;
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 root")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;
    MetadataEnricher::new(database.clone())
        .enrich_movie_library(library.id)
        .await?;
    let item_id: String = sqlx::query_scalar(
        "SELECT id FROM media_items WHERE item_type = 'MOVIE' AND removed_at IS NULL",
    )
    .fetch_one(database.pool())
    .await?;
    sqlx::query(
        "UPDATE media_items
         SET premiere_date = '2024-04-01', original_language = 'en', rating = 8.0,
             provider_ids_json = '{\"tmdb\":\"999\"}', identification_status = 'ONLINE_CONFIRMED'
         WHERE id = ?",
    )
    .bind(&item_id)
    .execute(database.pool())
    .await?;

    let images = ImageWriteService::new(database.clone())?;
    let selection =
        luxd::application::candidates::MetadataSelectionService::new(database.clone(), images);
    let tracker = RequestTracker {
        requests: Arc::new(AtomicUsize::new(0)),
    };
    let tmdb_app = Router::new()
        .fallback(any(complete_movie_tmdb_stub))
        .with_state(tracker.clone());
    let tmdb_listener = TcpListener::bind("127.0.0.1:0").await?;
    let tmdb_address = tmdb_listener.local_addr()?;
    let tmdb_server = tokio::spawn(async move { axum::serve(tmdb_listener, tmdb_app).await });
    let tmdb = TmdbClient::new(TmdbClientConfig {
        base_url: format!("http://{tmdb_address}"),
        proxy_url: None,
        api_key: None,
        read_access_token: Some("stub-token".to_owned()),
        timeout: Duration::from_secs(1),
        max_retries: 0,
        initial_backoff: Duration::ZERO,
        max_backoff: Duration::ZERO,
        retry_jitter: Duration::ZERO,
        requests_per_second: 0,
    })?;
    let metadata = MetadataReidentifyService::with_selection(
        database.clone(),
        TmdbProvider::from(tmdb),
        Some(selection),
    );
    let job = metadata
        .create_item_refresh_job(&item_id, MetadataRefreshMode::FillMissing)
        .await?;
    metadata.run(&job.id).await;

    let completed = metadata.get_job(&job.id).await?;
    assert_eq!(completed.status, "COMPLETED");
    assert_eq!(completed.items[0].candidate_count, 0);
    assert_eq!(tracker.requests.load(Ordering::SeqCst), 0);

    tokio::fs::remove_file(movie_dir.join("poster.png")).await?;
    let incomplete_job = metadata
        .create_item_refresh_job(&item_id, MetadataRefreshMode::FillMissing)
        .await?;
    metadata.run(&incomplete_job.id).await;
    let requests_after_incomplete = tracker.requests.load(Ordering::SeqCst);
    assert!(requests_after_incomplete > 0);

    let full_refresh_job = metadata
        .create_item_refresh_job(&item_id, MetadataRefreshMode::FullRefresh)
        .await?;
    metadata.run(&full_refresh_job.id).await;
    assert!(tracker.requests.load(Ordering::SeqCst) > requests_after_incomplete);

    tmdb_server.abort();
    Ok(())
}

#[tokio::test]
async fn library_metadata_job_processes_items_concurrently()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8098".parse()?,
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
    for index in 0..24 {
        let movie_dir = root.join(format!("Movie {index} (2024)"));
        tokio::fs::create_dir_all(&movie_dir).await?;
        tokio::fs::write(
            movie_dir.join(format!("Movie.{index}.2024.mkv")),
            b"fixture",
        )
        .await?;
    }
    libraries
        .add_root(library.id, root.to_str().ok_or("non-utf8 path")?)
        .await?;
    LibraryScanner::new(database.clone())
        .scan_movie_library(library.id)
        .await?;

    let tracker = ConcurrencyTracker {
        active: Arc::new(AtomicUsize::new(0)),
        maximum: Arc::new(AtomicUsize::new(0)),
    };
    let tmdb_app = Router::new()
        .fallback(any(delayed_tmdb_stub))
        .with_state(tracker.clone());
    let tmdb_listener = TcpListener::bind("127.0.0.1:0").await?;
    let tmdb_address = tmdb_listener.local_addr()?;
    let tmdb_server = tokio::spawn(async move { axum::serve(tmdb_listener, tmdb_app).await });
    let tmdb = TmdbClient::new(TmdbClientConfig {
        base_url: format!("http://{tmdb_address}"),
        proxy_url: None,
        api_key: None,
        read_access_token: Some("stub-token".to_owned()),
        timeout: Duration::from_secs(1),
        max_retries: 0,
        initial_backoff: Duration::ZERO,
        max_backoff: Duration::ZERO,
        retry_jitter: Duration::ZERO,
        requests_per_second: 0,
    })?;
    let metadata = MetadataReidentifyService::new(database.clone(), TmdbProvider::from(tmdb));
    let job = metadata.create_library_job(&library.id.to_string()).await?;
    metadata.run(&job.id).await;

    let completed = metadata.get_job(&job.id).await?;
    assert_eq!(completed.total_count, 24);
    assert_eq!(completed.status, "COMPLETED");
    assert!(tracker.maximum.load(Ordering::SeqCst) > 1);
    tmdb_server.abort();
    Ok(())
}
