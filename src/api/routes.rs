use super::*;

pub(super) fn app_with_state(state: AppState) -> Router {
    let web_root = web_root();
    let resources = state.resources.clone();
    let catalog_request_slots =
        Arc::new(tokio::sync::Semaphore::new(MAX_IN_FLIGHT_CATALOG_REQUESTS));
    let catalog_workers = Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_CATALOG_REQUESTS));
    Router::new()
        .route("/logo.svg", get(web_logo))
        .merge(users::api_routes())
        .merge(admin::api_routes())
        .merge(lux_api::api_routes())
        .merge(emby::api_routes())
        .nest("/emby", emby::api_routes())
        .fallback_service(
            ServeDir::new(web_root.clone())
                .append_index_html_on_directories(true)
                .fallback(ServeFile::new(web_root.join("index.html"))),
        )
        .with_state(state)
        .layer(middleware::from_fn(
            move |request: Request<Body>, next: Next| {
                let catalog_request_slots = catalog_request_slots.clone();
                let catalog_workers = catalog_workers.clone();
                async move {
                    let request_slot = if is_catalog_aggregation_path(request.uri().path()) {
                        match catalog_request_slots.try_acquire_owned() {
                            Ok(permit) => Some(permit),
                            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
                        }
                    } else {
                        None
                    };
                    let worker_permit = if request_slot.is_some() {
                        match catalog_workers.acquire_owned().await {
                            Ok(permit) => Some(permit),
                            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
                        }
                    } else {
                        None
                    };
                    let response = next.run(request).await;
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
        .layer(middleware::from_fn(normalize_empty_api_service_unavailable))
        .layer(middleware::from_fn(
            move |request: Request<Body>, next: Next| {
                let resources = resources.clone();
                async move {
                    let is_home = request.uri().path() == "/api/v1/home";
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
}
