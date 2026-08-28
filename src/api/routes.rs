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
        .route("/api/v1/libraries", get(lux_list_libraries))
        .route(
            "/api/v1/libraries/{library_id}/cover",
            get(lux_library_cover).head(lux_library_cover),
        )
        .route("/api/v1/favorites", get(lux_list_favorites))
        .route("/api/v1/search", get(lux_search))
        .route("/api/v1/home", get(lux_home))
        .route(
            "/api/v1/libraries/{library_id}/items",
            get(lux_list_library_items),
        )
        .route("/api/v1/items/{item_id}", get(lux_get_item))
        .route("/api/v1/people", get(lux_search_people))
        .route(
            "/api/v1/people/{person_id}/items",
            get(lux_get_person_items),
        )
        .route(
            "/api/v1/people/{person_id}",
            get(lux_get_person).patch(lux_update_person),
        )
        .route(
            "/api/v1/people/{person_id}/favorite",
            put(lux_set_person_favorite),
        )
        .route(
            "/api/v1/people/{person_id}/image",
            get(lux_get_person_image),
        )
        .route(
            "/api/v1/people/{provider}/{person_id}/image",
            get(lux_get_person_image_for_provider),
        )
        .route("/api/v1/items/{item_id}/children", get(lux_get_children))
        .route(
            "/api/v1/collections/{collection_id}",
            get(lux_get_collection),
        )
        .route(
            "/api/v1/items/{item_id}/images/{image_type}",
            get(lux_image).head(lux_image),
        )
        .route(
            "/api/v1/items/{item_id}/images/{image_type}/{image_index}",
            get(lux_image_at_index).head(lux_image_at_index),
        )
        .route("/api/v1/items/{item_id}/images", get(lux_list_item_images))
        .route(
            "/api/v1/items/{item_id}/images/search",
            post(lux_search_item_images),
        )
        .route(
            "/api/v1/items/{item_id}/images/select",
            post(lux_select_item_image),
        )
        .route(
            "/api/v1/items/{item_id}/subtitles/{stream_index}",
            get(lux_subtitle).head(lux_subtitle),
        )
        .route("/api/v1/items/{item_id}/danmaku", get(lux_danmaku_info))
        .route("/api/v1/items/{item_id}/danmaku/raw", get(lux_danmaku_raw))
        .route(
            "/api/v1/items/{item_id}/stream",
            get(lux_stream).head(lux_stream),
        )
        .route("/api/v1/items/{item_id}/playback", get(lux_get_playback))
        .route("/api/v1/items/{item_id}/progress", post(lux_post_progress))
        .route(
            "/api/v1/playback/sessions",
            post(lux_create_web_playback_session),
        )
        .route(
            "/api/v1/playback/sessions/{session_id}/events",
            post(lux_web_playback_event),
        )
        .route(
            "/api/v1/playback/sessions/{session_id}/heartbeat",
            post(lux_web_playback_heartbeat),
        )
        .route(
            "/api/v1/playback/sessions/{session_id}/direct",
            get(lux_web_playback_direct).head(lux_web_playback_direct),
        )
        .route(
            "/api/v1/playback/sessions/{session_id}/hls/{*asset}",
            get(lux_web_playback_hls).head(lux_web_playback_hls),
        )
        .route(
            "/api/v1/playback/sessions/{session_id}",
            delete(lux_delete_web_playback_session),
        )
        .route("/api/v1/items/{item_id}/favorite", put(lux_set_favorite))
        .route("/api/v1/items/{item_id}/played", put(lux_set_played))
        .route("/api/v1/playback-history", get(lux_list_playback_history))
        .route(
            "/api/v1/items/{item_id}/metadata",
            get(lux_get_metadata).patch(lux_update_metadata),
        )
        .route(
            "/api/v1/items/{item_id}/download",
            get(lux_download).head(lux_download),
        )
        .merge(emby_routes())
        .nest("/emby", emby_routes())
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
