use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{Json, Router, extract::State, routing::any};
use luxd::{
    api::{AppState, app_with_state},
    application::{
        libraries::LibraryService,
        reidentify::MetadataReidentifyService,
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
        .ok_or_else(|| format!("missing {name} cookie"))
        .expect("test response should set auth cookies")
}

async fn delayed_tmdb_search() -> Json<Value> {
    tokio::time::sleep(Duration::from_millis(100)).await;
    Json(json!({
        "page": 1,
        "total_pages": 1,
        "total_results": 1,
        "results": [{
            "id": 999,
            "title": "Batch Movie",
            "original_title": "Batch Movie",
            "overview": "A cancellation fixture.",
            "release_date": "2024-04-01",
            "original_language": "en"
        }]
    }))
}

#[derive(Clone)]
struct RequestCounter {
    active: Arc<AtomicUsize>,
    maximum: Arc<AtomicUsize>,
}

async fn counted_tmdb_search(State(counter): State<RequestCounter>) -> Json<Value> {
    let active = counter.active.fetch_add(1, Ordering::SeqCst) + 1;
    counter.maximum.fetch_max(active, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(50)).await;
    counter.active.fetch_sub(1, Ordering::SeqCst);
    delayed_tmdb_search().await
}

#[tokio::test]
async fn metadata_job_can_be_cancelled_while_running() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
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

    let tmdb_app = Router::new().fallback(any(delayed_tmdb_search));
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
    let job_id = job.id.clone();
    let runner = metadata.clone();
    let runner_job_id = job_id.clone();
    let runner_handle = tokio::spawn(async move {
        runner.run(&runner_job_id).await;
    });

    for _ in 0..100 {
        let status: String =
            sqlx::query_scalar("SELECT status FROM metadata_reidentify_jobs WHERE id = ?")
                .bind(&job_id)
                .fetch_one(database.pool())
                .await?;
        if status == "RUNNING" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let running: String =
        sqlx::query_scalar("SELECT status FROM metadata_reidentify_jobs WHERE id = ?")
            .bind(&job_id)
            .fetch_one(database.pool())
            .await?;
    assert_eq!(running, "RUNNING");

    metadata.cancel(&job_id).await?;
    runner_handle.await?;

    let cancelled = metadata.get_job(&job_id).await?;
    assert_eq!(cancelled.status, "CANCELLED");
    assert!(cancelled.processed_count < cancelled.total_count);
    let retried = metadata.retry_job(&job_id).await?;
    assert_eq!(retried.status, "QUEUED");
    tmdb_server.abort();
    Ok(())
}

#[tokio::test]
async fn metadata_job_reduces_workers_when_home_latency_is_degraded()
-> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = tempfile::tempdir()?;
    let config = Config {
        http_addr: "127.0.0.1:8097".parse()?,
        config_dir: temp_dir.path().join("config"),
    };
    let database = Database::connect(&config).await?;
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

    let counter = RequestCounter {
        active: Arc::new(AtomicUsize::new(0)),
        maximum: Arc::new(AtomicUsize::new(0)),
    };
    let tmdb_app = Router::new()
        .fallback(any(counted_tmdb_search))
        .with_state(counter.clone());
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
    let resources = luxd::observability::resources::ResourceMetrics::new();
    resources.record_home_latency(Duration::from_millis(400));
    let metadata = MetadataReidentifyService::new(database.clone(), TmdbProvider::from(tmdb))
        .with_resource_metrics(resources);
    let job = metadata.create_library_job(&library.id.to_string()).await?;
    metadata.run(&job.id).await;

    assert_eq!(metadata.get_job(&job.id).await?.status, "COMPLETED");
    assert_eq!(counter.maximum.load(Ordering::SeqCst), 1);
    tmdb_server.abort();
    Ok(())
}

#[tokio::test]
async fn admin_can_request_metadata_job_cancellation() -> Result<(), Box<dyn std::error::Error>> {
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
    let job = metadata.create_job(vec![item_id]).await?;
    let setup = SetupService::new(database.clone())?;
    let auth = WebAuthService::new(database.clone())?;
    let emby_auth = EmbyAuthService::new(database.clone())?;
    let app = app_with_state(AppState::ready(config, database, setup, auth, emby_auth));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let client = reqwest::Client::new();
    let login = client
        .post(format!("http://{address}/api/v1/auth/login"))
        .json(&json!({ "username": "admin", "password": "correct password" }))
        .send()
        .await?;
    let csrf = cookie_value(login.headers(), "lux_csrf");
    let cookies = format!(
        "lux_session={}; lux_csrf={csrf}",
        cookie_value(login.headers(), "lux_session")
    );
    let cancelled = client
        .post(format!(
            "http://{address}/api/v1/admin/metadata/reidentify/{}/cancel",
            job.id
        ))
        .header(COOKIE, &cookies)
        .header("x-csrf-token", &csrf)
        .send()
        .await?;
    assert_eq!(cancelled.status(), reqwest::StatusCode::ACCEPTED);
    let detail = client
        .get(format!(
            "http://{address}/api/v1/admin/metadata/reidentify/{}",
            job.id
        ))
        .header(COOKIE, &cookies)
        .send()
        .await?;
    let body: Value = detail.json().await?;
    assert_eq!(body["job"]["status"], "QUEUED");
    assert_eq!(body["job"]["cancelRequested"], true);
    server.abort();
    Ok(())
}
