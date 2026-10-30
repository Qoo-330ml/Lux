use super::*;
use axum::extract::MatchedPath;
use axum::http::{HeaderMap, StatusCode, header};
use http_body::Body as HttpBody;
use tower::{ServiceExt, service_fn};
use tower_http::compression::CompressionLayer;

const CATALOG_WORKER_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
const PERFORMANCE_PROBE_HEADER: &str = "x-lux-performance-probe";
const PERFORMANCE_PROBE_VALUE: &str = "v1";
// Keep the existing per-pool concurrency for Home while isolating it from
// unrelated catalog traffic that could otherwise hold every shared worker.
const MAX_CONCURRENT_HOME_REQUESTS: usize = MAX_CONCURRENT_CATALOG_REQUESTS;

/// Largest JSON body that still gets an ETag; bigger payloads (none today) skip the buffering.
const JSON_ETAG_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Tags JSON responses with a strong ETag over the uncompressed body and answers `If-None-Match`
/// with 304, so re-opening the same page transfers no body at all.
async fn json_response_etag(request: Request<Body>, next: Next) -> Response {
    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        return next.run(request).await;
    }
    let request_headers = request.headers().clone();
    let response = next.run(request).await;
    if !response.status().is_success() || !is_json_response(&response) {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    // Only buffer bodies whose size is known up front: hyper adds `content-length` while sending, so
    // the body's own hint is the reliable signal here.
    match body.size_hint().exact() {
        Some(length) if length <= JSON_ETAG_MAX_BYTES as u64 => {}
        _ => return Response::from_parts(parts, body),
    }
    // A handler that already declared a cache policy keeps it, and `no-store` responses are never
    // tagged: the middleware must not rewrite an explicit policy.
    let existing_cache_control = parts
        .headers
        .get(header::CACHE_CONTROL)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    if existing_cache_control
        .as_deref()
        .is_some_and(|value| value.contains("no-store"))
    {
        return Response::from_parts(parts, body);
    }
    let bytes = match axum::body::to_bytes(body, JSON_ETAG_MAX_BYTES).await {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(%error, "could not buffer a JSON response for its ETag");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let etag = format!("\"{}\"", json_etag_hex(&bytes));
    if if_none_match_matches(&request_headers, &etag) {
        let mut not_modified = Response::new(Body::empty());
        *not_modified.status_mut() = StatusCode::NOT_MODIFIED;
        let headers = not_modified.headers_mut();
        headers.insert(header::ETAG, header_value(&etag));
        for name in [
            header::CACHE_CONTROL,
            header::VARY,
            header::CONTENT_ENCODING,
        ] {
            if let Some(value) = parts.headers.get(&name) {
                headers.insert(name, value.clone());
            }
        }
        return not_modified;
    }
    parts.headers.insert(header::ETAG, header_value(&etag));
    if existing_cache_control.is_none() {
        parts
            .headers
            .insert(header::CACHE_CONTROL, header_value("private, no-cache"));
    }
    Response::from_parts(parts, Body::from(bytes))
}

fn is_json_response(response: &Response) -> bool {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/json"))
}

fn json_etag_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn header_value(value: &str) -> axum::http::HeaderValue {
    axum::http::HeaderValue::from_str(value)
        .unwrap_or_else(|_| axum::http::HeaderValue::from_static(""))
}

/// Compresses JSON responses only: media streams, images, the SPA fallback and server-sent events
/// must reach the client untouched, so the predicate is deliberately narrow.
#[derive(Clone, Copy)]
struct CompressJsonResponses;

impl tower_http::compression::predicate::Predicate for CompressJsonResponses {
    fn should_compress<B>(&self, response: &axum::http::Response<B>) -> bool
    where
        B: http_body::Body,
    {
        response.status().is_success()
            && response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.starts_with("application/json"))
    }
}

/// Dispatches between the two router trees of the setup/ready split.
///
/// The signature and the semantics of the compatibility façade stay: `tests/**` keeps calling
/// `app()`/`app_with_state(AppState::default())` (not-ready compatibility tree) and
/// `app_with_state(AppState::ready(..))` (ready tree). `src/main.rs` keeps calling
/// `app_with_state(app_state)`, which is always ready, so the setup tree is not selected here yet.
pub(super) fn app_with_state(state: AppState) -> Router {
    match state.into_mode() {
        AppMode::Ready(ready) => ready_router(ready),
        AppMode::NotReady(state) => not_ready_compat_router(state),
    }
}

/// The tree a ready application serves: every Lux, admin and Emby business route.
fn ready_router(state: ReadyAppState) -> Router {
    let core = state.core().clone();
    let slots = RequestSlots::default();
    let (emby_routes, emby_path_templates) = emby::api_routes();
    let app = Router::new()
        .route("/logo.svg", get(web_logo))
        .merge(core_routes::<ReadyAppState>())
        .merge(users::api_routes())
        .merge(admin::api_routes())
        .merge(lux_api::api_routes())
        .merge(emby_routes.clone())
        .nest("/emby", emby_routes)
        .fallback_service(spa_service())
        .with_state(state);
    let app = apply_common_layers(app, &core, slots);
    case_insensitive_emby_router(app, emby_path_templates)
}

/// The compatibility tree for a state that has neither a database nor the services.
///
/// `src/main.rs` never builds it: only the frozen `tests/**` reach it (`app()`,
/// `app_with_state(AppState::default())`), and they pin the responses the pre-split single tree
/// gave them. Its business routes are deliberately not registered, so the six responses of
/// `docs/SETUP-READY-SPLIT-DESIGN.md` §9.2 are synthesised here; every other path falls through to
/// the SPA fallback and the namespace middlewares exactly like before (Lux 404 envelope, bare Emby
/// 404, HTML for client-side routes).
fn not_ready_compat_router(state: AppState) -> Router {
    let core = state.core.clone();
    let slots = RequestSlots::default();
    // The Emby path templates of the real tree: the case-insensitive wrapper normalises
    // `/items/x` to `/Items/x` before routing, exactly like the single pre-split tree did. Only the
    // templates are used here (the tree itself mounts the three public discovery routes below), so
    // the normalised Emby business paths land on the SPA fallback and the namespace middlewares.
    let (_emby_routes, emby_path_templates) = emby::api_routes();
    let app = Router::new()
        .route("/logo.svg", get(web_logo))
        // The three core probes are real handlers: they answer correctly without services.
        .merge(core_routes::<AppState>())
        // Emby's public discovery routes answer correctly without any service
        // (`emby_ping` 200, `public/System/Info` with `StartupWizardCompleted: false`,
        // `Users/Public` an empty list), so the pre-split tree already served them unready.
        .route("/System/Ping", get(emby_ping).post(emby_ping))
        .route("/System/Info/Public", get(emby_public_system_info))
        .route("/Users/Public", get(emby_public_users))
        // Synthesised for the frozen tests only (§9.2 #3, #4, #5, #6).
        .route(
            "/api/v1/auth/sessions",
            get(not_ready_compat_bare_service_unavailable),
        )
        .route("/api/v1/auth/me", get(not_ready_compat_auth_me))
        .route(
            "/api/v1/items/{item_id}/danmaku",
            get(not_ready_compat_bare_service_unavailable),
        )
        .route(
            "/System/Info",
            get(not_ready_compat_bare_service_unavailable),
        )
        .fallback_service(spa_service())
        .with_state(state);
    let app = apply_common_layers(app, &core, slots);
    case_insensitive_emby_router(app, emby_path_templates)
}

/// The three routes both router trees mount: the liveness/readiness probes and the version.
///
/// The handlers still take `State<AppState>`, so the only generic bound is `AppState: FromRef<S>`:
/// the ready tree resolves it through the migration-period shim and the compatibility tree through
/// `AppState` itself. When the closing batch switches these handlers to `State<CoreState>` the
/// bound becomes `S: FromRef<CoreState>`, which is what lets the setup tree mount them too
/// (docs/SETUP-READY-SPLIT-DESIGN.md §2.3).
pub(super) fn core_routes<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    AppState: FromRef<S>,
{
    Router::new()
        .route("/health/live", get(users::live))
        .route("/health/ready", get(users::ready))
        .route("/api/v1/version", get(users::version))
}

/// The bare `503` the pre-split handlers answered for a missing service.
///
/// `normalize_empty_api_service_unavailable` turns it into the structured
/// `DATABASE_UNAVAILABLE` envelope under `/api/v1/` and leaves the Emby namespace bare, which is
/// exactly what the frozen tests assert for `/api/v1/auth/sessions`,
/// `/api/v1/items/{item_id}/danmaku` and `/System/Info`.
async fn not_ready_compat_bare_service_unavailable() -> Response {
    StatusCode::SERVICE_UNAVAILABLE.into_response()
}

/// `GET /api/v1/auth/me` used to fail in `require_web_principal` with this exact envelope when no
/// auth service existed; the compatibility tree cannot mount that handler, so it answers directly.
async fn not_ready_compat_auth_me(headers: HeaderMap) -> Response {
    api_error(
        &headers,
        StatusCode::SERVICE_UNAVAILABLE,
        lux::ApiErrorCode::DatabaseUnavailable,
        "服务尚未就绪",
    )
    .into_response()
}

/// The static assets and the `index.html` fallback the SPA needs in every mode.
///
/// Setup renders from the same bundle (`web/src/app/App.tsx` shows `SetupPage` when
/// `/api/v1/setup/status` says `initialized: false`), so both trees mount it.
fn spa_service() -> ServeDir<ServeFile> {
    let web_root = web_root();
    ServeDir::new(web_root.clone())
        .append_index_html_on_directories(true)
        .precompressed_gzip()
        .fallback(ServeFile::new(web_root.join("index.html")).precompressed_gzip())
}

/// The middleware stack every router tree shares, in the order the single tree applied it.
///
/// `Router::layer` wraps in call order (the last `.layer()` ends up outermost), so the calls below
/// are the pre-split chain copied verbatim: reordering them changes ETag, compression, request-id,
/// security-header and empty-`503` envelope behaviour. None of the layers needs a business service;
/// they read `CoreState` (metrics, resources, rate limiter) or nothing at all, which is what lets
/// both trees call this once instead of duplicating the stack.
fn apply_common_layers(router: Router, core: &CoreState, slots: RequestSlots) -> Router {
    let resources = core.resources.clone();
    let request_metrics = core.request_metrics.clone();
    let request_rate_limiter = core.request_rate_limiter.clone();
    let RequestSlots {
        catalog_request_slots,
        catalog_workers,
        home_workers,
    } = slots;
    router
        .layer(middleware::from_fn(json_response_etag))
        .layer(
            // JSON responses only: media streams, images, the SPA fallback and server-sent events
            // must reach the client untouched.
            CompressionLayer::new().compress_when(CompressJsonResponses),
        )
        .layer(middleware::from_fn_with_state(
            request_rate_limiter,
            limit_request_rate,
        ))
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
        .layer(middleware::from_fn(
            reject_playback_token_outside_allow_list,
        ))
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
        .layer(middleware::from_fn(security_response_headers))
        .layer(middleware::from_fn(web_asset_cache_policy))
        .layer(middleware::from_fn(
            move |request: Request<Body>, next: Next| {
                let request_metrics = request_metrics.clone();
                async move {
                    let method = request.method().as_str().to_owned();
                    let route = request.extensions().get::<MatchedPath>().map_or_else(
                        || "unmatched".to_owned(),
                        |matched| matched.as_str().to_owned(),
                    );
                    let started = Instant::now();
                    let response = next.run(request).await;
                    request_metrics.record(
                        &method,
                        &route,
                        response.status().as_u16(),
                        started.elapsed(),
                    );
                    response
                }
            },
        ))
}

/// The per-tree concurrency budgets the catalog/home layer holds for the life of the process.
struct RequestSlots {
    catalog_request_slots: Arc<tokio::sync::Semaphore>,
    catalog_workers: Arc<tokio::sync::Semaphore>,
    home_workers: Arc<tokio::sync::Semaphore>,
}

impl Default for RequestSlots {
    fn default() -> Self {
        Self {
            catalog_request_slots: Arc::new(tokio::sync::Semaphore::new(
                MAX_IN_FLIGHT_CATALOG_REQUESTS,
            )),
            catalog_workers: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_CATALOG_REQUESTS)),
            home_workers: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_HOME_REQUESTS)),
        }
    }
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

/// Same-origin policy for the bundled Web client.
///
/// The player needs blob workers (hls.js) and WebAssembly (HEVC), and the build has no inline
/// scripts. `'wasm-unsafe-eval'` is ignored by browsers without it (Safari before 16.4), which
/// then refuses WebAssembly; those clients fall back to the non-HEVC path.
const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; \
     img-src 'self' data: blob: https://www.themoviedb.org; \
     media-src 'self' blob:; \
     script-src 'self' 'wasm-unsafe-eval'; \
     style-src 'self' 'unsafe-inline'; \
     connect-src 'self'; \
     worker-src 'self' blob:; \
     frame-ancestors 'none'; \
     base-uri 'self'; \
     form-action 'self'; \
     object-src 'none'";

async fn limit_request_rate(
    State(limiter): State<crate::security::RequestRateLimiter>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let peer = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|connect| connect.0.ip().to_string())
        .unwrap_or_else(|| "local".to_owned());
    if limiter.allow(&peer).await {
        return next.run(request).await;
    }
    let mut response = api_error(
        request.headers(),
        StatusCode::TOO_MANY_REQUESTS,
        super::lux::ApiErrorCode::RateLimited,
        "请求过于频繁，请稍后重试",
    )
    .into_response();
    response
        .headers_mut()
        .insert("retry-after", HeaderValue::from_static("60"));
    response
}

async fn security_response_headers(request: Request<Body>, next: Next) -> Response {
    let forwarded_https = request
        .headers()
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("https"));
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers
        .entry("x-content-type-options")
        .or_insert(HeaderValue::from_static("nosniff"));
    headers
        .entry("x-frame-options")
        .or_insert(HeaderValue::from_static("DENY"));
    headers
        .entry("referrer-policy")
        .or_insert(HeaderValue::from_static("no-referrer"));
    headers
        .entry("content-security-policy")
        .or_insert(HeaderValue::from_static(CONTENT_SECURITY_POLICY));
    if forwarded_https {
        headers
            .entry("strict-transport-security")
            .or_insert(HeaderValue::from_static(
                "max-age=31536000; includeSubDomains",
            ));
    }
    response
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

    fn test_request(method: Method, uri: &str, headers: &[(&str, &str)]) -> Request<Body> {
        let mut builder = Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        builder.body(Body::empty()).expect("test request")
    }

    async fn send(app: Router, request: Request<Body>) -> Response {
        app.oneshot(request).await.expect("router response")
    }

    async fn json_body(response: Response) -> Value {
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("response body");
        serde_json::from_slice(&bytes).expect("JSON response body")
    }

    fn header<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
    }

    /// Pins the observable order of the shared middleware stack, which `apply_common_layers` moved
    /// out of `app_with_state` without touching a single `.layer()` call.
    ///
    /// `Router::layer` wraps in call order, so the assertions below only hold for the pre-split
    /// order: the ETag layer innermost over the handler, the request-id layers outside the empty
    /// `503` normaliser (that is how the envelope's `requestId` matches the response header), the
    /// security headers outside both, and the rate limiter outside the routes.
    #[tokio::test]
    async fn common_layers_keep_their_pre_split_order() {
        let app = app_with_state(AppState::default());

        // json_response_etag sits next to the routes: it tags the handler body and answers
        // `If-None-Match` before any outer layer rewrites the response.
        let live = send(app.clone(), test_request(Method::GET, "/health/live", &[])).await;
        assert_eq!(live.status(), StatusCode::OK);
        let etag = header(&live, "etag").map(str::to_owned).expect("json etag");
        assert_eq!(header(&live, "cache-control"), Some("private, no-cache"));
        let not_modified = send(
            app.clone(),
            test_request(Method::GET, "/health/live", &[("if-none-match", &etag)]),
        )
        .await;
        assert_eq!(not_modified.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(header(&not_modified, "etag"), Some(etag.as_str()));

        // strip_client_request_id then SetRequestId run outside the empty-503 normaliser, so the
        // server mints the id the envelope reports, and security_response_headers still upgrades
        // the rewritten response to HSTS when the request arrived over HTTPS.
        let unavailable = send(
            app.clone(),
            test_request(
                Method::GET,
                "/api/v1/auth/sessions",
                &[
                    ("x-request-id", "client-supplied-id"),
                    ("x-forwarded-proto", "https"),
                ],
            ),
        )
        .await;
        assert_eq!(unavailable.status(), StatusCode::SERVICE_UNAVAILABLE);
        let server_id = header(&unavailable, "x-request-id")
            .map(str::to_owned)
            .expect("server request id");
        assert!(!server_id.is_empty());
        assert_ne!(server_id, "client-supplied-id");
        assert!(header(&unavailable, "x-client-request-id").is_none());
        assert_eq!(
            header(&unavailable, "strict-transport-security"),
            Some("max-age=31536000; includeSubDomains")
        );
        let body = json_body(unavailable).await;
        assert_eq!(body["error"]["code"], "DATABASE_UNAVAILABLE");
        assert_eq!(body["error"]["requestId"], server_id.as_str());

        // reject_unmatched_api_path is inside security_response_headers and keeps the two
        // namespace behaviours: a Lux JSON envelope, a bare Emby status.
        let missing = send(
            app.clone(),
            test_request(Method::GET, "/api/v1/no-such-endpoint", &[]),
        )
        .await;
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        assert_eq!(header(&missing, "x-content-type-options"), Some("nosniff"));
        assert_eq!(json_body(missing).await["error"]["code"], "NOT_FOUND");
        let emby_missing = send(
            app.clone(),
            test_request(Method::GET, "/Items/unknown-item/UnknownThing", &[]),
        )
        .await;
        assert_eq!(emby_missing.status(), StatusCode::NOT_FOUND);
        assert!(
            axum::body::to_bytes(emby_missing.into_body(), 4096)
                .await
                .expect("Emby 404 body")
                .is_empty()
        );

        // limit_request_rate wraps the routes, so the second request of a one-per-window peer is
        // answered by the layer, not by a handler.
        let limited = app_with_state(AppState::default().with_request_rate_limit(1));
        assert_eq!(
            send(
                limited.clone(),
                test_request(Method::GET, "/health/live", &[])
            )
            .await
            .status(),
            StatusCode::OK
        );
        let throttled = send(limited, test_request(Method::GET, "/health/live", &[])).await;
        assert_eq!(throttled.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(header(&throttled, "retry-after"), Some("60"));
        assert_eq!(json_body(throttled).await["error"]["code"], "RATE_LIMITED");
    }

    /// The six responses the frozen `tests/**` pin for `app()`/`AppState::default()`, byte for
    /// byte (docs/SETUP-READY-SPLIT-DESIGN.md §9.2).
    ///
    /// The compatibility tree only mounts the core probes plus these four synthesised handlers, so
    /// this test is the executable version of the contract table: it breaks if a synthetic answer
    /// drifts away from the handler the split removed.
    #[tokio::test]
    async fn not_ready_compat_tree_serves_the_six_frozen_responses() {
        let app = app_with_state(AppState::default());

        // 1. GET /health/live -> 200 {"status":"ok"} with a server request id.
        let live = send(app.clone(), test_request(Method::GET, "/health/live", &[])).await;
        assert_eq!(live.status(), StatusCode::OK);
        assert!(header(&live, "x-request-id").is_some());
        let body = axum::body::to_bytes(live.into_body(), 4096)
            .await
            .expect("live body");
        assert_eq!(body.as_ref(), br#"{"status":"ok"}"#);

        // 2. GET /health/ready -> 503 because the database slot is empty.
        let ready = send(app.clone(), test_request(Method::GET, "/health/ready", &[])).await;
        assert_eq!(ready.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(ready.into_body(), 4096)
            .await
            .expect("ready body");
        assert!(String::from_utf8_lossy(&body).contains("database_unavailable"));

        // 3. GET /api/v1/auth/sessions -> structured 503 whose requestId is the response header.
        let sessions = send(
            app.clone(),
            test_request(Method::GET, "/api/v1/auth/sessions", &[]),
        )
        .await;
        assert_eq!(sessions.status(), StatusCode::SERVICE_UNAVAILABLE);
        let server_id = header(&sessions, "x-request-id")
            .map(str::to_owned)
            .expect("server request id");
        let body = json_body(sessions).await;
        assert_eq!(body["error"]["code"], "DATABASE_UNAVAILABLE");
        assert_eq!(body["error"]["requestId"], server_id.as_str());

        // 4. GET /api/v1/auth/me -> the `require_web_principal` envelope.
        let me = send(
            app.clone(),
            test_request(Method::GET, "/api/v1/auth/me", &[]),
        )
        .await;
        assert_eq!(me.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(json_body(me).await["error"]["message"], "服务尚未就绪");

        // 5. GET /System/Info -> bare 503, no body and no error envelope.
        let emby_info = send(app.clone(), test_request(Method::GET, "/System/Info", &[])).await;
        assert_eq!(emby_info.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            axum::body::to_bytes(emby_info.into_body(), 4096)
                .await
                .expect("Emby 503 body")
                .is_empty()
        );

        // Emby's public discovery routes stay available on the compatibility tree: they answer
        // without any service, exactly like the pre-split tree did.
        let ping = send(app.clone(), test_request(Method::GET, "/System/Ping", &[])).await;
        assert_eq!(ping.status(), StatusCode::OK);
        let ping_post = send(app.clone(), test_request(Method::POST, "/System/Ping", &[])).await;
        assert_eq!(ping_post.status(), StatusCode::OK);
        let public_info = send(
            app.clone(),
            test_request(Method::GET, "/System/Info/Public", &[]),
        )
        .await;
        assert_eq!(public_info.status(), StatusCode::OK);
        let public_info = json_body(public_info).await;
        assert_eq!(public_info["StartupWizardCompleted"], false);
        // `Id` is the core `server_id`, empty for `AppState::default()` exactly like before.
        assert!(public_info["Id"].as_str().is_some());
        let public_users = send(app.clone(), test_request(Method::GET, "/Users/Public", &[])).await;
        assert_eq!(public_users.status(), StatusCode::OK);
        assert_eq!(json_body(public_users).await, json!([]));

        // The Emby templates make `/items/x` normalise to `/Items/x` like the pre-split tree, so an
        // Emby business path never reaches the SPA as HTML. The compatibility tree mounts no Emby
        // business route, so the namespace middleware answers its bare 404 where the pre-split tree
        // reached the handler's bare 503; the frozen test only requires "not 200 and not HTML".
        let emby_item = send(
            app.clone(),
            test_request(
                Method::GET,
                "/items/example-item",
                &[("accept", "application/json")],
            ),
        )
        .await;
        assert_eq!(emby_item.status(), StatusCode::NOT_FOUND);
        assert!(header(&emby_item, "content-type").is_none());

        // The case-insensitive Emby wrapper is still the outermost layer of this tree.
        let emby_info_lowercase =
            send(app.clone(), test_request(Method::GET, "/system/info", &[])).await;
        assert_eq!(
            emby_info_lowercase.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );

        // 6. GET /api/v1/items/item-1/danmaku -> structured bare-503 normalisation.
        let danmaku = send(
            app.clone(),
            test_request(Method::GET, "/api/v1/items/item-1/danmaku", &[]),
        )
        .await;
        assert_eq!(danmaku.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            json_body(danmaku).await["error"]["code"],
            "DATABASE_UNAVAILABLE"
        );

        // No business route is mounted: the Lux namespace answers its 404 envelope while the Emby
        // namespace stays a bare 404, and the SPA fallback still owns the client-side routes.
        for path in ["/api/v1/libraries", "/api/v1/home"] {
            let missing = send(app.clone(), test_request(Method::GET, path, &[])).await;
            assert_eq!(missing.status(), StatusCode::NOT_FOUND, "{path}");
            assert_eq!(json_body(missing).await["error"]["code"], "NOT_FOUND");
        }
        let admin = send(
            app.clone(),
            test_request(Method::GET, "/api/v1/admin/navigation", &[]),
        )
        .await;
        assert_eq!(admin.status(), StatusCode::NOT_FOUND);
        assert_eq!(json_body(admin).await["error"]["code"], "NOT_FOUND");
        // A non-GET request to an unregistered business path is answered by the SPA fallback's
        // ServeDir, which rejects non-GET methods with 405; the pre-split tree reached the real
        // handler here. No frozen test exercises that on the compatibility tree, and the ready
        // tree keeps the real handler.
        let login = send(
            app.clone(),
            test_request(Method::POST, "/api/v1/auth/login", &[]),
        )
        .await;
        assert_eq!(login.status(), StatusCode::METHOD_NOT_ALLOWED);
        let emby_users = send(app.clone(), test_request(Method::GET, "/Users", &[])).await;
        assert_eq!(emby_users.status(), StatusCode::NOT_FOUND);
        assert!(
            axum::body::to_bytes(emby_users.into_body(), 4096)
                .await
                .expect("Emby 404 body")
                .is_empty()
        );
    }
}
