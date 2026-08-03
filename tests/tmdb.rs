use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use luxd::application::tmdb::{TmdbClient, TmdbClientConfig, TmdbError};
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

#[derive(Clone)]
struct StubState {
    statuses: Arc<Mutex<Vec<StatusCode>>>,
    attempts: Arc<AtomicUsize>,
    auth_seen: Arc<AtomicBool>,
    api_key_seen: Arc<AtomicBool>,
    delay: Option<Duration>,
    invalid_schema: bool,
    localized: bool,
}

async fn start_stub(
    statuses: Vec<StatusCode>,
    delay: Option<Duration>,
    invalid_schema: bool,
    localized: bool,
) -> (
    String,
    StubState,
    tokio::task::JoinHandle<Result<(), std::io::Error>>,
) {
    let state = StubState {
        statuses: Arc::new(Mutex::new(statuses)),
        attempts: Arc::new(AtomicUsize::new(0)),
        auth_seen: Arc::new(AtomicBool::new(false)),
        api_key_seen: Arc::new(AtomicBool::new(false)),
        delay,
        invalid_schema,
        localized,
    };
    let app = Router::new()
        .fallback(any(stub_response))
        .with_state(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("stub listener");
    let address = listener.local_addr().expect("stub address");
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    (format!("http://{address}"), state, server)
}

async fn stub_response(State(state): State<StubState>, request: Request<Body>) -> Response {
    if request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        == Some("Bearer stub-token")
    {
        state.auth_seen.store(true, Ordering::Relaxed);
    }
    if request
        .uri()
        .query()
        .is_some_and(|query| query.contains("api_key=custom-api-key"))
    {
        state.api_key_seen.store(true, Ordering::Relaxed);
    }
    state.attempts.fetch_add(1, Ordering::Relaxed);
    if let Some(delay) = state.delay {
        tokio::time::sleep(delay).await;
    }
    let status = state.statuses.lock().await.pop().unwrap_or(StatusCode::OK);
    if status != StatusCode::OK {
        return (
            status,
            axum::Json(json!({ "status_message": "stub failure" })),
        )
            .into_response();
    }
    if state.invalid_schema {
        return axum::Json(json!({ "page": 1 })).into_response();
    }
    if request.uri().path().contains("/3/collection/10") {
        return axum::Json(json!({
            "id": 10,
            "name": "Stub Collection",
            "overview": "Collection overview",
            "poster_path": "/poster.jpg",
            "backdrop_path": "/backdrop.jpg",
            "parts": [{
                "id": 7,
                "title": "Stub title",
                "release_date": "2020-01-01",
                "poster_path": "/part.jpg"
            }]
        }))
        .into_response();
    }
    if request.uri().path().contains("/3/movie/7") {
        return axum::Json(json!({
            "id": 7,
            "title": "Stub title",
            "original_title": "Original title",
            "overview": "Stub overview",
            "release_date": "2020-01-01",
            "original_language": "en",
            "belongs_to_collection": {
                "id": 10,
                "name": "Stub Collection"
            }
        }))
        .into_response();
    }
    let language = request
        .uri()
        .query()
        .and_then(|query| {
            query
                .split('&')
                .find_map(|pair| pair.strip_prefix("language="))
        })
        .unwrap_or("en-US");
    let (title, overview) = if state.localized && language == "zh-CN" {
        ("中文标题", "")
    } else if state.localized {
        ("English title", "English overview")
    } else {
        ("Stub title", "Stub overview")
    };
    axum::Json(json!({
        "page": 1,
        "total_pages": 1,
        "total_results": 1,
        "results": [{
            "id": 7,
            "title": title,
            "original_title": "Original title",
            "overview": overview,
            "release_date": "2020-01-01",
            "original_language": "en"
        }],
        "belongs_to_collection": {
            "id": 10,
            "name": "Stub Collection"
        }
    }))
    .into_response()
}

fn client_config(base_url: String, timeout: Duration, max_retries: u32) -> TmdbClientConfig {
    TmdbClientConfig {
        base_url,
        api_key: None,
        read_access_token: Some("stub-token".to_owned()),
        timeout,
        max_retries,
        initial_backoff: Duration::ZERO,
        max_backoff: Duration::ZERO,
        retry_jitter: Duration::ZERO,
        requests_per_second: 0,
    }
}

#[test]
fn tmdb_client_requires_a_token_and_valid_http_base_url() {
    assert!(matches!(
        TmdbClient::new(TmdbClientConfig::default()),
        Err(TmdbError::MissingToken)
    ));

    assert!(matches!(
        TmdbClient::new(TmdbClientConfig {
            base_url: "file:///tmp/tmdb".to_owned(),
            api_key: None,
            read_access_token: Some("stub-token".to_owned()),
            ..TmdbClientConfig::default()
        }),
        Err(TmdbError::InvalidBaseUrl(_))
    ));
}

#[tokio::test]
async fn tmdb_client_sends_v3_api_key_as_a_query_parameter()
-> Result<(), Box<dyn std::error::Error>> {
    let (base_url, state, server) = start_stub(vec![StatusCode::OK], None, false, false).await;
    let client = TmdbClient::new(TmdbClientConfig {
        base_url,
        api_key: Some("custom-api-key".to_owned()),
        read_access_token: None,
        timeout: Duration::from_secs(1),
        max_retries: 0,
        initial_backoff: Duration::ZERO,
        max_backoff: Duration::ZERO,
        retry_jitter: Duration::ZERO,
        requests_per_second: 0,
    })?;

    client.search_movies("stub", None, "en-US").await?;

    assert!(state.api_key_seen.load(Ordering::Relaxed));
    server.abort();
    Ok(())
}

#[tokio::test]
async fn tmdb_client_retries_429_and_sends_bearer_token() -> Result<(), Box<dyn std::error::Error>>
{
    let (base_url, state, server) = start_stub(
        vec![StatusCode::OK, StatusCode::TOO_MANY_REQUESTS],
        None,
        false,
        false,
    )
    .await;
    let client = TmdbClient::new(client_config(base_url, Duration::from_secs(1), 1))?;

    let response = client.search_movies("stub", Some(2020), "zh-CN").await?;

    assert_eq!(response.results[0].id, 7);
    assert_eq!(state.attempts.load(Ordering::Relaxed), 2);
    assert!(state.auth_seen.load(Ordering::Relaxed));
    server.abort();
    Ok(())
}

#[tokio::test]
async fn tmdb_client_classifies_not_found_and_server_failure()
-> Result<(), Box<dyn std::error::Error>> {
    let (not_found_url, _, not_found_server) =
        start_stub(vec![StatusCode::NOT_FOUND], None, false, false).await;
    let not_found = TmdbClient::new(client_config(not_found_url, Duration::from_secs(1), 0))?
        .search_movies("missing", None, "en-US")
        .await
        .expect_err("404");
    assert!(matches!(not_found, TmdbError::NotFound));
    not_found_server.abort();

    let (server_error_url, state, server_error_server) =
        start_stub(vec![StatusCode::INTERNAL_SERVER_ERROR], None, false, false).await;
    let server_error = TmdbClient::new(client_config(server_error_url, Duration::from_secs(1), 0))?
        .search_movies("error", None, "en-US")
        .await
        .expect_err("500");
    assert!(matches!(server_error, TmdbError::Upstream { status: 500 }));
    assert_eq!(state.attempts.load(Ordering::Relaxed), 1);
    server_error_server.abort();
    Ok(())
}

#[tokio::test]
async fn tmdb_client_rejects_invalid_response_and_classifies_timeout()
-> Result<(), Box<dyn std::error::Error>> {
    let (invalid_url, _, invalid_server) =
        start_stub(vec![StatusCode::OK], None, true, false).await;
    let invalid = TmdbClient::new(client_config(invalid_url, Duration::from_secs(1), 0))?
        .search_movies("invalid", None, "en-US")
        .await
        .expect_err("invalid schema");
    assert!(matches!(invalid, TmdbError::InvalidResponse(_)));
    invalid_server.abort();

    let (timeout_url, _, timeout_server) = start_stub(
        vec![StatusCode::OK],
        Some(Duration::from_millis(50)),
        false,
        false,
    )
    .await;
    let timeout = TmdbClient::new(client_config(timeout_url, Duration::from_millis(5), 0))?
        .search_movies("timeout", None, "en-US")
        .await
        .expect_err("timeout");
    assert!(matches!(timeout, TmdbError::Timeout));
    timeout_server.abort();
    Ok(())
}

#[tokio::test]
async fn tmdb_client_falls_back_from_zh_cn_per_missing_field()
-> Result<(), Box<dyn std::error::Error>> {
    let (base_url, state, server) =
        start_stub(vec![StatusCode::OK, StatusCode::OK], None, false, true).await;
    let client = TmdbClient::new(client_config(base_url, Duration::from_secs(1), 0))?;

    let response = client
        .search_movies_with_english_fallback("stub", Some(2020))
        .await?;

    assert_eq!(response.results[0].title.as_deref(), Some("中文标题"));
    assert_eq!(
        response.results[0].overview.as_deref(),
        Some("English overview")
    );
    assert_eq!(state.attempts.load(Ordering::Relaxed), 2);
    server.abort();
    Ok(())
}

#[tokio::test]
async fn tmdb_client_reads_collection_details() -> Result<(), Box<dyn std::error::Error>> {
    let (base_url, _, server) =
        start_stub(vec![StatusCode::OK, StatusCode::OK], None, false, false).await;
    let client = TmdbClient::new(client_config(base_url, Duration::from_secs(1), 0))?;

    let movie = client.movie_details(7, "zh-CN").await?;
    assert_eq!(
        movie.belongs_to_collection.as_ref().map(|item| item.id),
        Some(10)
    );
    let collection = client.collection_details(10, "zh-CN").await?;
    assert_eq!(collection.id, 10);
    assert_eq!(collection.parts[0].id, 7);
    server.abort();
    Ok(())
}
