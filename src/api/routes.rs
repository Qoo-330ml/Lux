use super::*;
use tower::{ServiceExt, service_fn};

const CATALOG_WORKER_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
const PERFORMANCE_PROBE_HEADER: &str = "x-lux-performance-probe";
const PERFORMANCE_PROBE_VALUE: &str = "v1";
// Keep the existing per-pool concurrency for Home while isolating it from
// unrelated catalog traffic that could otherwise hold every shared worker.
const MAX_CONCURRENT_HOME_REQUESTS: usize = MAX_CONCURRENT_CATALOG_REQUESTS;

pub(super) fn app_with_state(state: AppState) -> Router {
    let web_root = web_root();
    let resources = state.resources.clone();
    let (emby_routes, emby_path_templates) = emby::api_routes();
    let catalog_request_slots =
        Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT_CATALOG_REQUESTS));
    let catalog_workers = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_CATALOG_REQUESTS));
    let home_workers = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_HOME_REQUESTS));
    let app = Router::new()
        .route("/logo.svg", get(web_logo))
        .merge(users::api_routes())
        .merge(admin::api_routes())
        .merge(lux_api::api_routes())
        .merge(emby_routes.clone())
        .nest("/emby", emby_routes)
        .fallback_service(
            ServeDir::new(web_root.clone())
                .append_index_html_on_directories(true)
                .precompressed_gzip()
                .fallback(ServeFile::new(web_root.join("index.html")).precompressed_gzip()),
        )
        .with_state(state)
        .layer(middleware::from_fn(
            move |request: Request<Body>, next: Next| {
                let catalog_request_slots = catalog_request_slots.clone();
                let catalog_workers = catalog_workers.clone();
                let home_workers = home_workers.clone();
                async move {
                    let path = request.uri().path().to_owned();
                    let request_headers = request.headers().clone();
                    let request_slot = if is_catalog_aggregation_path(&path) {
                        match catalog_request_slots.try_acquire_owned() {
                            Ok(permit) => Some(permit),
                            Err(_) => return catalog_busy_response(&path, &request_headers),
                        }
                    } else {
                        None
                    };
                    let mut worker_wait_us = 0_u64;
                    let worker_permit = if request_slot.is_some() {
                        let workers =
                            select_catalog_worker_pool(&path, catalog_workers, home_workers);
                        let (acquire_result, wait_duration) =
                            acquire_catalog_worker_with_wait(workers, CATALOG_WORKER_WAIT_TIMEOUT)
                                .await;
                        worker_wait_us =
                            u64::try_from(wait_duration.as_micros()).unwrap_or(u64::MAX);
                        match acquire_result {
                            Ok(permit) => Some(permit),
                            Err(CatalogWorkerWaitError::TimedOut) => {
                                tracing::warn!(
                                    request_id = request_headers
                                        .get("x-request-id")
                                        .and_then(|value| value.to_str().ok())
                                        .unwrap_or("unknown"),
                                    route_class = catalog_route_class(&path),
                                    wait_ms = u64::try_from(wait_duration.as_millis())
                                        .unwrap_or(u64::MAX),
                                    "catalog worker queue wait timed out"
                                );
                                return catalog_busy_response(&path, &request_headers);
                            }
                            Err(CatalogWorkerWaitError::Closed) => {
                                return StatusCode::SERVICE_UNAVAILABLE.into_response();
                            }
                        }
                    } else {
                        None
                    };
                    let handler_started = Instant::now();
                    let mut response = next.run(request).await;
                    if request_slot.is_some() {
                        add_performance_probe_headers(
                            &mut response,
                            &request_headers,
                            &path,
                            worker_wait_us,
                            u64::try_from(handler_started.elapsed().as_micros())
                                .unwrap_or(u64::MAX),
                        );
                    }
                    drop(worker_permit);
                    drop(request_slot);
                    response
                }
            },
        ))
        .layer(middleware::from_fn(attach_peer_address))
        .layer(middleware::from_fn(normalize_lux_api_key_query))
        .layer(middleware::from_fn(trace_emby_playback_callback))
        .layer(middleware::from_fn(trace_emby_playback_info))
        .layer(middleware::from_fn(trace_emby_media_stream_failure))
        .layer(middleware::from_fn(reject_unmatched_emby_video_path))
        .layer(middleware::from_fn(reject_unmatched_api_path))
        .layer(middleware::from_fn(normalize_empty_api_service_unavailable))
        .layer(middleware::from_fn(
            move |request: Request<Body>, next: Next| {
                let resources = resources.clone();
                async move {
                    let is_home = is_home_request_path(request.uri().path());
                    let started = Instant::now();
                    let response = next.run(request).await;
                    if is_home {
                        resources.record_home_latency(started.elapsed());
                    }
                    response
                }
            },
        ))
        .layer(
            tower::ServiceBuilder::new()
                // A client-supplied request id must not become the server's own correlation
                // id; keep it aside for diagnostics and always mint a fresh one.
                .layer(middleware::from_fn(strip_client_request_id))
                .set_x_request_id(MakeRequestUuid)
                .layer(
                    TraceLayer::new_for_http()
                        .make_span_with(|request: &axum::http::Request<_>| {
                            let request_id = request
                                .headers()
                                .get("x-request-id")
                                .and_then(|value| value.to_str().ok())
                                .unwrap_or("unknown");
                            tracing::info_span!(
                                "request",
                                method = %request.method(),
                                path = %safe_trace_path(request.uri()),
                                version = ?request.version(),
                                "requestId" = %request_id,
                                "durationMs" = tracing::field::Empty,
                                "statusCode" = tracing::field::Empty,
                                "errorCode" = tracing::field::Empty,
                            )
                        })
                        .on_response(
                            |response: &Response, latency: Duration, span: &tracing::Span| {
                                let duration_ms =
                                    u64::try_from(latency.as_millis()).unwrap_or(u64::MAX);
                                span.record("durationMs", duration_ms);
                                span.record("statusCode", response.status().as_u16());
                                tracing::debug!(
                                    latency = ?latency,
                                    status = %response.status(),
                                    "finished processing request"
                                );
                            },
                        ),
                )
                .propagate_x_request_id(),
        )
        .layer(middleware::from_fn(web_asset_cache_policy));
    case_insensitive_emby_router(app, emby_path_templates)
}

struct EmbyRoutePatterns {
    path_templates: Vec<String>,
    static_prefixes: Vec<String>,
}

impl EmbyRoutePatterns {
    fn new(path_templates: Vec<String>) -> Self {
        let mut path_templates = path_templates;
        path_templates.sort_by(|left, right| {
            left.matches('{')
                .count()
                .cmp(&right.matches('{').count())
                .then_with(|| right.len().cmp(&left.len()))
        });
        let mut static_prefixes = Vec::new();
        for template in &path_templates {
            let mut prefix = String::new();
            for segment in template.split('/').skip(1) {
                if segment.contains('{') {
                    break;
                }
                prefix.push('/');
                prefix.push_str(segment);
            }
            if !prefix.is_empty() && !static_prefixes.contains(&prefix) {
                static_prefixes.push(prefix);
            }
        }
        Self {
            path_templates,
            static_prefixes,
        }
    }

    fn matches_static_prefix(&self, path: &str) -> bool {
        self.static_prefixes.iter().any(|prefix| {
            path.get(..prefix.len())
                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
                && path
                    .get(prefix.len()..)
                    .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with('/'))
        })
    }
}

fn case_insensitive_emby_router(router: Router, path_templates: Vec<String>) -> Router {
    let path_patterns = Arc::new(EmbyRoutePatterns::new(path_templates));
    Router::new().fallback_service(service_fn(move |mut request: Request<Body>| {
        let router = router.clone();
        let path_patterns = path_patterns.clone();
        async move {
            if !is_media_detail_document_navigation(&request) {
                normalize_emby_request_path(request.uri_mut(), &path_patterns);
            }
            router.oneshot(request).await
        }
    }))
}

fn is_media_detail_document_navigation(request: &Request<Body>) -> bool {
    if request.method() != Method::GET {
        return false;
    }
    let Some(item_id) = request.uri().path().strip_prefix("/items/") else {
        return false;
    };
    if item_id.is_empty() || item_id.contains('/') {
        return false;
    }
    request
        .headers()
        .get(axum::http::header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|accept| {
            accept.split(',').any(|media_range| {
                media_range
                    .trim()
                    .split(';')
                    .next()
                    .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case("text/html"))
            })
        })
}

fn normalize_emby_request_path(uri: &mut axum::http::Uri, path_patterns: &EmbyRoutePatterns) {
    let path = uri.path();
    let emby_prefix = path
        .get(..5)
        .filter(|prefix| prefix.eq_ignore_ascii_case("/emby"))
        .filter(|_| {
            path.get(5..)
                .is_some_and(|remaining| remaining.is_empty() || remaining.starts_with('/'))
        });
    let (prefix, route_path) = if emby_prefix.is_some() {
        let Some(route_path) = path.get(5..) else {
            return;
        };
        ("/emby", route_path)
    } else {
        ("", path)
    };
    if !path_patterns.matches_static_prefix(route_path) {
        return;
    }
    let Some(normalized_route_path) = path_patterns
        .path_templates
        .iter()
        .find_map(|template| normalize_route_path(route_path, template))
    else {
        return;
    };
    let normalized_path = format!("{prefix}{normalized_route_path}");
    if normalized_path == path {
        return;
    }

    let path_and_query = match uri.query() {
        Some(query) => format!("{normalized_path}?{query}"),
        None => normalized_path,
    };
    let Ok(path_and_query) = path_and_query.parse::<axum::http::uri::PathAndQuery>() else {
        return;
    };
    let mut parts = uri.clone().into_parts();
    parts.path_and_query = Some(path_and_query);
    if let Ok(normalized_uri) = axum::http::Uri::from_parts(parts) {
        *uri = normalized_uri;
    }
}

fn normalize_route_path(path: &str, template: &str) -> Option<String> {
    let mut path_segments = path.split('/');
    let mut template_segments = template.split('/');
    let mut normalized_path = String::new();
    let mut index = 0;
    loop {
        match (path_segments.next(), template_segments.next()) {
            (Some(path_segment), Some(template_segment)) => {
                if index > 0 {
                    normalized_path.push('/');
                }
                normalized_path.push_str(&normalize_route_segment(path_segment, template_segment)?);
                index += 1;
            }
            (None, None) => return Some(normalized_path),
            _ => return None,
        }
    }
}

fn normalize_route_segment(path: &str, template: &str) -> Option<String> {
    let mut normalized = String::new();
    let mut path_offset = 0;
    let mut template_offset = 0;

    while template_offset < template.len() {
        let template_remaining = &template[template_offset..];
        let Some(parameter_start) = template_remaining.find('{') else {
            let static_suffix = template_remaining;
            let path_suffix = path.get(path_offset..)?;
            if !path_suffix.eq_ignore_ascii_case(static_suffix) {
                return None;
            }
            normalized.push_str(static_suffix);
            path_offset = path.len();
            template_offset = template.len();
            continue;
        };

        let static_prefix = &template_remaining[..parameter_start];
        let path_static_prefix = path.get(path_offset..path_offset + static_prefix.len())?;
        if !path_static_prefix.eq_ignore_ascii_case(static_prefix) {
            return None;
        }
        normalized.push_str(static_prefix);
        path_offset += static_prefix.len();
        template_offset += parameter_start;

        let parameter_end = template[template_offset + 1..].find('}')? + template_offset + 1;
        let suffix_start = parameter_end + 1;
        let static_suffix = &template[suffix_start..];
        let path_remaining = path.get(path_offset..)?;
        let parameter_length = if static_suffix.is_empty() {
            path_remaining.len()
        } else {
            find_ascii_case_insensitive(path_remaining, static_suffix)?
        };
        if parameter_length == 0 {
            return None;
        }
        normalized.push_str(path_remaining.get(..parameter_length)?);
        path_offset += parameter_length;
        template_offset = suffix_start;
    }

    (path_offset == path.len()).then_some(normalized)
}

fn find_ascii_case_insensitive(value: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() || needle.len() > value.len() {
        return None;
    }
    (0..=value.len() - needle.len()).find(|start| {
        value
            .get(*start..*start + needle.len())
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(needle))
    })
}

async fn strip_client_request_id(mut request: Request<Body>, next: Next) -> Response {
    if let Some(value) = request.headers_mut().remove("x-request-id") {
        if let Some(value) = value
            .to_str()
            .ok()
            .filter(|value| !value.is_empty() && value.len() <= 128)
            && let Ok(value) = HeaderValue::from_str(value)
        {
            request.headers_mut().insert("x-client-request-id", value);
        }
    }
    next.run(request).await
}

async fn web_asset_cache_policy(request: Request<Body>, next: Next) -> Response {
    let path = request.uri().path().to_owned();
    let mut response = next.run(request).await;
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();

    if let Some(policy) = web_asset_cache_control(&path, content_type) {
        response
            .headers_mut()
            .insert(CACHE_CONTROL, HeaderValue::from_static(policy));
    }
    response
}

fn web_asset_cache_control(path: &str, content_type: &str) -> Option<&'static str> {
    if is_api_or_emby_namespace_path(path) {
        return None;
    }
    if content_type.starts_with("text/html") {
        return Some("no-store");
    }
    if !path.starts_with("/assets/") {
        return None;
    }

    if has_content_hash(path) {
        Some("public, max-age=31536000, immutable")
    } else {
        Some("no-cache")
    }
}

fn has_content_hash(path: &str) -> bool {
    let Some(file_name) = path.rsplit('/').next() else {
        return false;
    };
    let Some((stem, _extension)) = file_name.rsplit_once('.') else {
        return false;
    };

    stem.match_indices('-').any(|(index, _)| {
        let suffix = &stem[index + 1..];
        suffix.len() >= 8
            && suffix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    })
}

fn add_performance_probe_headers(
    response: &mut Response,
    request_headers: &HeaderMap,
    path: &str,
    worker_wait_us: u64,
    handler_us: u64,
) {
    if request_headers
        .get(PERFORMANCE_PROBE_HEADER)
        .is_none_or(|value| value != PERFORMANCE_PROBE_VALUE)
        || !path.starts_with('/')
        || path.contains('?')
        || path.contains('#')
    {
        return;
    }
    let Some(request_id) = request_headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 128
                && value.bytes().all(|byte| byte.is_ascii_graphic())
        })
    else {
        return;
    };
    let Ok(request_id) = HeaderValue::from_str(request_id) else {
        return;
    };
    let Ok(path) = HeaderValue::from_str(path) else {
        return;
    };

    let headers = response.headers_mut();
    headers.insert("x-lux-perf-request-id", request_id);
    headers.insert("x-lux-perf-path", path);
    headers.insert(
        "x-lux-perf-worker-wait-us",
        HeaderValue::from(worker_wait_us),
    );
    headers.insert("x-lux-perf-handler-us", HeaderValue::from(handler_us));
}

fn select_catalog_worker_pool(
    path: &str,
    catalog_workers: Arc<tokio::sync::Semaphore>,
    home_workers: Arc<tokio::sync::Semaphore>,
) -> Arc<tokio::sync::Semaphore> {
    if is_home_request_path(path) {
        home_workers
    } else {
        catalog_workers
    }
}

fn is_home_request_path(path: &str) -> bool {
    matches!(path, "/api/v1/home" | "/api/v1/home/carousel")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CatalogWorkerWaitError {
    Closed,
    TimedOut,
}

async fn acquire_catalog_worker(
    workers: Arc<tokio::sync::Semaphore>,
    wait: Duration,
) -> Result<tokio::sync::OwnedSemaphorePermit, CatalogWorkerWaitError> {
    match tokio::time::timeout(wait, workers.acquire_owned()).await {
        Ok(Ok(permit)) => Ok(permit),
        Ok(Err(_)) => Err(CatalogWorkerWaitError::Closed),
        Err(_) => Err(CatalogWorkerWaitError::TimedOut),
    }
}

async fn acquire_catalog_worker_with_wait(
    workers: Arc<tokio::sync::Semaphore>,
    wait: Duration,
) -> (
    Result<tokio::sync::OwnedSemaphorePermit, CatalogWorkerWaitError>,
    Duration,
) {
    let started = Instant::now();
    let result = acquire_catalog_worker(workers, wait).await;
    (result, started.elapsed())
}

fn catalog_busy_response(path: &str, headers: &HeaderMap) -> Response {
    let mut response = if path.starts_with("/api/v1/") {
        super::api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::CatalogBusy,
            "目录请求繁忙，请稍后重试",
        )
        .into_response()
    } else {
        StatusCode::SERVICE_UNAVAILABLE.into_response()
    };
    response.headers_mut().insert(
        axum::http::header::RETRY_AFTER,
        HeaderValue::from_static("2"),
    );
    response
}

fn catalog_route_class(path: &str) -> &'static str {
    if is_home_request_path(path) {
        "home"
    } else if path == "/Items" || path.starts_with("/emby/Items") {
        "emby_items"
    } else {
        "catalog"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emby_case_normalization_preserves_dynamic_segments_and_query() {
        let path_patterns =
            EmbyRoutePatterns::new(vec!["/Users/{user_id}/Items/{item_id}".to_owned()]);
        let mut uri: axum::http::Uri =
            "/eMbY/uSeRs/AbC-123/iTeMs/IMDB:tt456?api_key=A%2FB&Fields=Id%2CName"
                .parse()
                .expect("valid test URI");

        normalize_emby_request_path(&mut uri, &path_patterns);

        assert_eq!(uri.path(), "/emby/Users/AbC-123/Items/IMDB:tt456");
        assert_eq!(uri.query(), Some("api_key=A%2FB&Fields=Id%2CName"));
    }

    #[test]
    fn emby_case_normalization_leaves_non_emby_paths_unchanged() {
        let path_patterns = EmbyRoutePatterns::new(vec!["/Users/{user_id}".to_owned()]);
        let mut uri: axum::http::Uri = "/API/v1/auth/LoGiN?ReturnUrl=%2FHome"
            .parse()
            .expect("valid test URI");
        let original = uri.clone();

        normalize_emby_request_path(&mut uri, &path_patterns);

        assert_eq!(uri, original);
    }

    #[test]
    fn web_documents_and_assets_receive_version_aware_cache_policy() {
        assert_eq!(
            web_asset_cache_control("/admin/changelog", "text/html; charset=utf-8"),
            Some("no-store")
        );
        assert_eq!(
            web_asset_cache_control("/assets/AdminChangelogPage-a1b2c3d4.js", "text/javascript"),
            Some("public, max-age=31536000, immutable")
        );
        assert_eq!(
            web_asset_cache_control("/assets/lux.js", "text/javascript"),
            Some("no-cache")
        );
        assert_eq!(
            web_asset_cache_control("/api/v1/me", "application/json"),
            None
        );
        assert_eq!(web_asset_cache_control("/emby/Users", "text/html"), None);
    }

    #[test]
    fn homepage_carousel_uses_the_isolated_home_worker_pool() {
        let catalog_workers = Arc::new(tokio::sync::Semaphore::new(1));
        let home_workers = Arc::new(tokio::sync::Semaphore::new(1));

        assert!(Arc::ptr_eq(
            &select_catalog_worker_pool(
                "/api/v1/home/carousel",
                catalog_workers.clone(),
                home_workers.clone(),
            ),
            &home_workers,
        ));
        assert_eq!(catalog_route_class("/api/v1/home/carousel"), "home");
    }

    #[test]
    fn performance_probe_headers_require_opt_in_request_id_and_path() {
        let mut response = StatusCode::OK.into_response();
        let mut request_headers = HeaderMap::new();
        request_headers.insert("x-request-id", HeaderValue::from_static("probe-17"));
        assert!(response.headers().get("x-lux-perf-request-id").is_none());

        add_performance_probe_headers(
            &mut response,
            &request_headers,
            "/api/v1/libraries/1/items",
            19,
            241,
        );
        assert!(response.headers().get("x-lux-perf-request-id").is_none());

        request_headers.insert(
            PERFORMANCE_PROBE_HEADER,
            HeaderValue::from_static(PERFORMANCE_PROBE_VALUE),
        );
        add_performance_probe_headers(
            &mut response,
            &request_headers,
            "/api/v1/libraries/1/items",
            19,
            241,
        );

        assert_eq!(response.headers()["x-lux-perf-request-id"], "probe-17");
        assert_eq!(
            response.headers()["x-lux-perf-path"],
            "/api/v1/libraries/1/items"
        );
        assert_eq!(response.headers()["x-lux-perf-worker-wait-us"], "19");
        assert_eq!(response.headers()["x-lux-perf-handler-us"], "241");

        let mut query_path_response = StatusCode::OK.into_response();
        add_performance_probe_headers(
            &mut query_path_response,
            &request_headers,
            "/api/v1/libraries/1/items?sort=Name",
            19,
            241,
        );
        assert!(
            query_path_response
                .headers()
                .get("x-lux-perf-request-id")
                .is_none()
        );

        let mut missing_request_id_response = StatusCode::OK.into_response();
        request_headers.remove("x-request-id");
        add_performance_probe_headers(
            &mut missing_request_id_response,
            &request_headers,
            "/api/v1/libraries/1/items",
            19,
            241,
        );
        assert!(
            missing_request_id_response
                .headers()
                .get("x-lux-perf-request-id")
                .is_none()
        );
    }

    #[tokio::test]
    async fn catalog_worker_queue_wait_has_a_deadline() {
        let workers = Arc::new(tokio::sync::Semaphore::new(1));
        let held = workers
            .clone()
            .acquire_owned()
            .await
            .expect("worker permit");

        let result = acquire_catalog_worker(workers.clone(), Duration::from_millis(1)).await;
        assert!(matches!(result, Err(CatalogWorkerWaitError::TimedOut)));

        drop(held);
        assert!(
            acquire_catalog_worker(workers, Duration::from_millis(10))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn catalog_worker_timeout_records_elapsed_wait_duration() {
        let workers = Arc::new(tokio::sync::Semaphore::new(1));
        let _held = workers
            .clone()
            .acquire_owned()
            .await
            .expect("worker permit");

        let (result, elapsed) =
            acquire_catalog_worker_with_wait(workers, Duration::from_millis(5)).await;

        assert!(matches!(result, Err(CatalogWorkerWaitError::TimedOut)));
        assert!(elapsed >= Duration::from_millis(5));
    }

    #[tokio::test]
    async fn home_worker_pool_does_not_wait_behind_catalog_workers() {
        let catalog_workers = Arc::new(tokio::sync::Semaphore::new(1));
        let home_workers = Arc::new(tokio::sync::Semaphore::new(1));
        let _held_catalog = catalog_workers
            .clone()
            .acquire_owned()
            .await
            .expect("catalog worker permit");

        let home_pool = select_catalog_worker_pool(
            "/api/v1/home",
            catalog_workers.clone(),
            home_workers.clone(),
        );
        let catalog_pool =
            select_catalog_worker_pool("/api/v1/search", catalog_workers, home_workers);

        assert!(
            acquire_catalog_worker(home_pool, Duration::from_millis(10))
                .await
                .is_ok()
        );
        assert!(matches!(
            acquire_catalog_worker(catalog_pool, Duration::from_millis(1)).await,
            Err(CatalogWorkerWaitError::TimedOut)
        ));
    }

    #[tokio::test]
    async fn catalog_busy_lux_response_has_a_stable_code_and_retry_hint() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-request-id",
            HeaderValue::from_static("catalog-busy-test"),
        );

        let response = catalog_busy_response("/api/v1/home", &headers);

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok()),
            Some("2")
        );
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("catalog busy response body");
        let body: Value = serde_json::from_slice(&body).expect("catalog busy response JSON");
        assert_eq!(body["error"]["code"], "CATALOG_BUSY");
        assert_eq!(body["error"]["requestId"], "catalog-busy-test");
    }

    #[tokio::test]
    async fn catalog_busy_emby_response_keeps_its_empty_503_shape() {
        let mut headers = HeaderMap::new();
        headers.insert("x-request-id", HeaderValue::from_static("emby-busy-test"));

        let response = catalog_busy_response("/Items", &headers);

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok()),
            Some("2")
        );
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("Emby busy response body");
        assert!(body.is_empty());
    }
}
