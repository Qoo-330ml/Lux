use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

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
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
    started_at: Arc<Mutex<Vec<Instant>>>,
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
        active: Arc::new(AtomicUsize::new(0)),
        max_active: Arc::new(AtomicUsize::new(0)),
        started_at: Arc::new(Mutex::new(Vec::new())),
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

struct ActiveRequestGuard(Arc<AtomicUsize>);

impl Drop for ActiveRequestGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

async fn stub_response(State(state): State<StubState>, request: Request<Body>) -> Response {
    let active = state.active.fetch_add(1, Ordering::Relaxed) + 1;
    state.max_active.fetch_max(active, Ordering::Relaxed);
    state.started_at.lock().await.push(Instant::now());
    let _active_request = ActiveRequestGuard(state.active.clone());
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
    if request.uri().path().contains("/3/movie/7")
        && !request.uri().path().contains("/external_ids")
        && !request.uri().path().contains("/videos")
        && !request.uri().path().contains("/images")
        && !request.uri().path().contains("/credits")
    {
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
    if request.uri().path().contains("/3/movie/7/credits") {
        return axum::Json(json!({
            "cast": [{
                "id": 9,
                "name": "Stub Person",
                "character": "主角",
                "profile_path": "/profile.jpg",
                "order": 0
            }]
        }))
        .into_response();
    }
    if request.uri().path().contains("/3/search/tv") {
        return axum::Json(json!({
            "page": 1,
            "total_pages": 1,
            "total_results": 1,
            "results": [{
                "id": 8,
                "name": "Stub Series",
                "original_name": "Original Series",
                "overview": "Series overview",
                "first_air_date": "2021-01-02",
                "original_language": "en",
                "poster_path": "/series-poster.jpg",
                "backdrop_path": "/series-backdrop.jpg"
            }]
        }))
        .into_response();
    }
    if request.uri().path().contains("/3/search/person") {
        return axum::Json(json!({
            "page": 1,
            "total_pages": 1,
            "total_results": 1,
            "results": [{
                "id": 9,
                "name": "Stub Person",
                "known_for_department": "Acting",
                "profile_path": "/profile.jpg"
            }]
        }))
        .into_response();
    }
    if request.uri().path().contains("/3/search/collection") {
        return axum::Json(json!({
            "page": 1,
            "total_pages": 1,
            "total_results": 1,
            "results": [{
                "id": 10,
                "name": "Stub Collection",
                "overview": "Collection overview",
                "poster_path": "/poster.jpg",
                "backdrop_path": "/backdrop.jpg"
            }]
        }))
        .into_response();
    }
    if request.uri().path().contains("/3/tv/8/season/1/episode/2") {
        return axum::Json(json!({
            "id": 802,
            "name": "Stub Episode",
            "overview": "Episode overview",
            "air_date": "2021-01-03",
            "episode_number": 2,
            "season_number": 1,
            "still_path": "/still.jpg",
            "runtime": 45
        }))
        .into_response();
    }
    if request.uri().path().contains("/3/tv/8/season/1") {
        return axum::Json(json!({
            "id": 801,
            "name": "Season 1",
            "overview": "Season overview",
            "air_date": "2021-01-02",
            "season_number": 1,
            "poster_path": "/season-poster.jpg",
            "episodes": [{
                "id": 802,
                "name": "Stub Episode",
                "overview": "Episode overview",
                "air_date": "2021-01-03",
                "episode_number": 2,
                "season_number": 1,
                "still_path": "/still.jpg",
                "runtime": 45
            }]
        }))
        .into_response();
    }
    if request.uri().path().contains("/3/tv/8") {
        return axum::Json(json!({
            "id": 8,
            "name": "Stub Series",
            "original_name": "Original Series",
            "overview": "Series overview",
            "first_air_date": "2021-01-02",
            "last_air_date": "2021-02-03",
            "original_language": "en",
            "number_of_seasons": 1,
            "number_of_episodes": 2,
            "poster_path": "/series-poster.jpg",
            "backdrop_path": "/series-backdrop.jpg",
            "seasons": []
        }))
        .into_response();
    }
    if request.uri().path().contains("/3/person/9/external_ids") {
        return axum::Json(json!({
            "imdb_id": "nm0000009",
            "tvdb_id": 9009,
            "wikidata_id": "Q9"
        }))
        .into_response();
    }
    if request.uri().path().contains("/3/person/9") {
        return axum::Json(json!({
            "id": 9,
            "name": "Stub Person",
            "biography": "Biography",
            "birthday": "1970-01-01",
            "known_for_department": "Acting",
            "place_of_birth": "Test City",
            "profile_path": "/profile.jpg"
        }))
        .into_response();
    }
    if request.uri().path().contains("/3/movie/7/videos") {
        return axum::Json(json!({
            "results": [{
                "id": "video-1",
                "key": "abc123",
                "name": "Official Trailer",
                "site": "YouTube",
                "type": "Trailer",
                "official": true,
                "published_at": "2020-01-01T00:00:00.000Z"
            }]
        }))
        .into_response();
    }
    if request.uri().path().contains("/3/movie/7/images") {
        return axum::Json(json!({
            "posters": [{"file_path": "/poster.jpg", "iso_639_1": "zh", "width": 100, "height": 150}],
            "backdrops": [{"file_path": "/backdrop.jpg", "iso_639_1": null, "width": 1920, "height": 1080}],
            "profiles": []
        }))
        .into_response();
    }
    if request.uri().path().contains("/3/movie/7/external_ids") {
        return axum::Json(json!({
            "imdb_id": "tt0000007",
            "tvdb_id": 7007,
            "wikidata_id": "Q7"
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
        proxy_url: None,
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

#[tokio::test]
async fn tmdb_client_routes_requests_through_configured_network_proxy()
-> Result<(), Box<dyn std::error::Error>> {
    let (proxy_url, state, proxy_server) =
        start_stub(vec![StatusCode::OK], None, false, false).await;
    let mut config = client_config("http://tmdb.invalid".to_owned(), Duration::from_secs(1), 0);
    config.proxy_url = Some(proxy_url);
    let client = TmdbClient::new(config)?;

    let response = client.search_movies("stub", Some(2020), "en-US").await?;

    assert_eq!(response.results[0].id, 7);
    assert_eq!(state.attempts.load(Ordering::Relaxed), 1);
    proxy_server.abort();
    Ok(())
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

    assert!(matches!(
        TmdbClient::new(TmdbClientConfig {
            proxy_url: Some("ftp://proxy.invalid:7890".to_owned()),
            read_access_token: Some("stub-token".to_owned()),
            ..TmdbClientConfig::default()
        }),
        Err(TmdbError::InvalidProxyUrl(_))
    ));
}

#[tokio::test]
async fn tmdb_client_sends_v3_api_key_as_a_query_parameter()
-> Result<(), Box<dyn std::error::Error>> {
    let (base_url, state, server) = start_stub(vec![StatusCode::OK], None, false, false).await;
    let client = TmdbClient::new(TmdbClientConfig {
        base_url,
        proxy_url: None,
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

#[tokio::test]
async fn tmdb_client_reads_tv_people_images_external_ids_and_videos()
-> Result<(), Box<dyn std::error::Error>> {
    let (base_url, _, server) = start_stub(vec![StatusCode::OK; 12], None, false, false).await;
    let client = TmdbClient::new(client_config(base_url, Duration::from_secs(1), 0))?;

    let series_search = client.search_tv("stub", Some(2021), "zh-CN").await?;
    assert_eq!(series_search.results[0].id, 8);
    let person_search = client.search_people("stub", "zh-CN").await?;
    assert_eq!(person_search.results[0].id, 9);
    let collection_search = client.search_collections("stub", "zh-CN").await?;
    assert_eq!(collection_search.results[0].id, 10);
    let series = client.series_details(8, "zh-CN").await?;
    assert_eq!(series.number_of_episodes, Some(2));
    let season = client.season_details(8, 1, "zh-CN").await?;
    assert_eq!(season.episodes[0].episode_number, Some(2));
    let episode = client.episode_details(8, 1, 2, "zh-CN").await?;
    assert_eq!(episode.id, 802);
    let person = client.person_details(9, "zh-CN").await?;
    assert_eq!(person.name.as_deref(), Some("Stub Person"));
    let external_ids = client.movie_external_ids(7).await?;
    assert_eq!(external_ids.imdb_id.as_deref(), Some("tt0000007"));
    let images = client.movie_images(7, "zh-CN").await?;
    assert_eq!(images.posters[0].file_path.as_deref(), Some("/poster.jpg"));
    let credits = client.movie_credits(7, "zh-CN").await?;
    assert_eq!(credits.cast[0].id, 9);
    assert_eq!(credits.cast[0].character.as_deref(), Some("主角"));
    let videos = client.movie_videos(7, "zh-CN").await?;
    assert_eq!(videos.results[0].key.as_deref(), Some("abc123"));

    server.abort();
    Ok(())
}

#[tokio::test]
async fn tmdb_client_caps_upstream_concurrency_at_sixteen() -> Result<(), Box<dyn std::error::Error>> {
    let (base_url, state, server) = start_stub(
        vec![StatusCode::OK; 40],
        Some(Duration::from_millis(80)),
        false,
        false,
    )
    .await;
    let client = TmdbClient::new(client_config(base_url, Duration::from_secs(1), 0))?;
    let mut requests = Vec::new();
    for index in 0..40 {
        let client = client.clone();
        requests.push(tokio::spawn(async move {
            client
                .search_movies(&format!("stub-{index}"), None, "en-US")
                .await
        }));
    }
    for request in requests {
        request.await??;
    }

    assert!(state.max_active.load(Ordering::Relaxed) <= 16);
    server.abort();
    Ok(())
}

#[tokio::test]
async fn tmdb_client_default_rate_limit_starts_no_more_than_thirty_two_requests_per_second()
-> Result<(), Box<dyn std::error::Error>> {
    let (base_url, state, server) = start_stub(vec![StatusCode::OK; 4], None, false, false).await;
    let client = TmdbClient::new(TmdbClientConfig {
        base_url,
        read_access_token: Some("stub-token".to_owned()),
        ..TmdbClientConfig::default()
    })?;
    for index in 0..4 {
        client
            .search_movies(&format!("stub-{index}"), None, "en-US")
            .await?;
    }

    let started_at = state.started_at.lock().await.clone();
    assert!(
        started_at
            .windows(2)
            .all(|pair| { pair[1].duration_since(pair[0]) >= Duration::from_millis(30) })
    );
    server.abort();
    Ok(())
}

#[tokio::test]
async fn tmdb_client_clamps_configured_rate_to_thirty_two_requests_per_second()
-> Result<(), Box<dyn std::error::Error>> {
    let (base_url, state, server) = start_stub(vec![StatusCode::OK; 2], None, false, false).await;
    let client = TmdbClient::new(TmdbClientConfig {
        base_url,
        read_access_token: Some("stub-token".to_owned()),
        requests_per_second: 128,
        ..TmdbClientConfig::default()
    })?;
    for index in 0..2 {
        client
            .search_movies(&format!("stub-{index}"), None, "en-US")
            .await?;
    }

    let started_at = state.started_at.lock().await.clone();
    assert!(
        started_at
            .windows(2)
            .all(|pair| { pair[1].duration_since(pair[0]) >= Duration::from_millis(30) })
    );
    server.abort();
    Ok(())
}
