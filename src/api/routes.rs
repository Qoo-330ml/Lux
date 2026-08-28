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
        .route(
            "/api/v1/admin/libraries",
            get(admin_list_libraries).post(admin_create_library),
        )
        .route("/api/v1/admin/directories", get(admin_list_directories))
        .route(
            "/api/v1/admin/libraries/{library_id}",
            patch(admin_update_library).delete(admin_delete_library),
        )
        .route(
            "/api/v1/admin/libraries/{library_id}/cover",
            put(admin_update_library_cover)
                .layer(DefaultBodyLimit::max(MAX_LIBRARY_COVER_BYTES as usize)),
        )
        .route(
            "/api/v1/admin/libraries/{library_id}/cover/auto",
            post(admin_run_auto_library_cover),
        )
        .route(
            "/api/v1/admin/library-cover-jobs",
            get(admin_list_library_cover_jobs),
        )
        .route(
            "/api/v1/admin/library-cover-jobs/{job_id}",
            get(admin_get_library_cover_job),
        )
        .route(
            "/api/v1/admin/people/index-rebuild",
            get(admin_list_people_index_rebuild),
        )
        .route(
            "/api/v1/admin/people/index-rebuild/{library_id}",
            post(admin_queue_people_index_rebuild),
        )
        .route(
            "/api/v1/admin/people/index-rebuild/{library_id}/cancel",
            post(admin_cancel_people_index_rebuild),
        )
        .route("/api/v1/admin/plugins", get(admin_list_plugins))
        .route(
            "/api/v1/admin/notification-providers",
            get(admin_list_notification_providers),
        )
        .route(
            "/api/v1/admin/chapter-sources",
            get(admin_list_chapter_sources),
        )
        .route(
            "/api/v1/admin/plugins/installed",
            get(admin_list_installed_plugins),
        )
        .route(
            "/api/v1/admin/plugins/{plugin_id}/install",
            post(admin_install_plugin),
        )
        .route(
            "/api/v1/admin/plugins/{plugin_id}/update",
            post(admin_update_plugin),
        )
        .route(
            "/api/v1/admin/plugins/{plugin_id}",
            delete(admin_uninstall_plugin),
        )
        .route(
            "/api/v1/admin/plugins/{plugin_id}/enabled",
            patch(admin_update_plugin_enabled),
        )
        .route(
            "/api/v1/admin/plugins/{plugin_id}/config",
            put(admin_update_plugin_config),
        )
        .route(
            "/api/v1/admin/plugins/{plugin_id}/run",
            post(admin_run_plugin),
        )
        .route(
            "/api/v1/admin/plugin-store",
            get(admin_plugin_store).put(admin_update_plugin_store),
        )
        .route(
            "/api/v1/admin/emby-migration/test",
            post(admin_test_emby_migration),
        )
        .route(
            "/api/v1/admin/emby-migration",
            get(admin_list_emby_migrations).post(admin_create_emby_migration),
        )
        .route(
            "/api/v1/admin/emby-migration/{job_id}",
            get(admin_get_emby_migration),
        )
        .route(
            "/api/v1/admin/emby-migration/{job_id}/cancel",
            post(admin_cancel_emby_migration),
        )
        .route(
            "/api/v1/admin/emby-migration/{job_id}/retry",
            post(admin_retry_emby_migration),
        )
        .route(
            "/api/v1/admin/emby-migration/{job_id}/users",
            get(admin_list_emby_migration_users),
        )
        .route(
            "/api/v1/admin/emby-migration/{job_id}/matches",
            get(admin_list_emby_migration_matches),
        )
        .route(
            "/api/v1/admin/emby-migration/{job_id}/imports",
            get(admin_list_emby_migration_imports),
        )
        .route(
            "/api/v1/admin/emby-migration/{job_id}/person-favorites",
            get(admin_list_emby_migration_person_favorites),
        )
        .route(
            "/api/v1/admin/users",
            get(admin_list_users).post(admin_create_user),
        )
        .route(
            "/api/v1/admin/users/{user_id}",
            patch(admin_update_user).delete(admin_disable_user),
        )
        .route(
            "/api/v1/admin/users/{user_id}/libraries",
            get(admin_list_user_library_access),
        )
        .route(
            "/api/v1/admin/metadata/pending",
            get(admin_list_pending_metadata),
        )
        .route(
            "/api/v1/admin/people/matches",
            get(admin_list_pending_person_matches),
        )
        .route(
            "/api/v1/admin/people/matches/{candidate_id}/confirm",
            post(admin_confirm_person_match),
        )
        .route(
            "/api/v1/admin/people/matches/{candidate_id}/reject",
            post(admin_reject_person_match),
        )
        .route(
            "/api/v1/admin/people/matches/{candidate_id}/undo",
            post(admin_undo_person_match),
        )
        .route(
            "/api/v1/admin/people/{person_id}/split",
            post(admin_split_person_identity),
        )
        .route(
            "/api/v1/admin/people/{person_id}/locks",
            post(admin_set_person_field_locks),
        )
        .route(
            "/api/v1/admin/metadata/reidentify",
            get(admin_list_metadata_reidentify).post(admin_start_metadata_reidentify),
        )
        .route(
            "/api/v1/admin/metadata/confirm",
            post(admin_confirm_metadata),
        )
        .route(
            "/api/v1/admin/metadata/reidentify/{job_id}",
            get(admin_get_metadata_reidentify).post(admin_retry_metadata_reidentify),
        )
        .route(
            "/api/v1/admin/metadata/reidentify/{job_id}/cancel",
            post(admin_cancel_metadata_reidentify),
        )
        .route(
            "/api/v1/admin/items/{item_id}/identify/candidates",
            get(admin_list_item_candidates).post(admin_search_item_candidates),
        )
        .route(
            "/api/v1/admin/items/{item_id}/identify/candidates/{candidate_id}/select",
            post(admin_select_candidate),
        )
        .route(
            "/api/v1/admin/items/{item_id}/images",
            get(admin_list_item_images),
        )
        .route(
            "/api/v1/admin/items/{item_id}/images/{image_id}",
            delete(admin_delete_item_image),
        )
        .route(
            "/api/v1/admin/items/{item_id}/scan",
            post(admin_start_item_scan),
        )
        .route(
            "/api/v1/admin/items/{item_id}/metadata/refresh",
            post(admin_start_item_metadata_refresh),
        )
        .route(
            "/api/v1/admin/items/{item_id}/subtitles/{stream_index}",
            patch(admin_update_item_subtitle),
        )
        .route("/api/v1/admin/items/{item_id}", delete(admin_delete_item))
        .route(
            "/api/v1/admin/items/{item_id}/collection/refresh",
            post(admin_refresh_collection),
        )
        .route(
            "/api/v1/admin/libraries/{library_id}/roots",
            post(admin_add_library_root),
        )
        .route(
            "/api/v1/admin/libraries/{library_id}/roots/{root_id}",
            delete(admin_delete_library_root),
        )
        .route(
            "/api/v1/admin/users/{user_id}/libraries/{library_id}",
            patch(admin_set_library_access),
        )
        .route(
            "/api/v1/admin/libraries/{library_id}/scan",
            post(admin_start_scan),
        )
        .route(
            "/api/v1/admin/libraries/{library_id}/reidentify",
            post(admin_start_library_reidentify),
        )
        .route(
            "/api/v1/admin/libraries/{library_id}/metadata/refresh",
            post(admin_start_library_metadata_refresh),
        )
        .route(
            "/api/v1/admin/libraries/{library_id}/reconcile",
            post(admin_start_scan),
        )
        .route(
            "/api/v1/admin/jobs/{job_id}/cancel",
            post(admin_cancel_scan),
        )
        .route("/api/v1/admin/jobs/{job_id}/retry", post(admin_retry_scan))
        .route("/api/v1/admin/jobs/{job_id}", get(admin_get_job))
        .route(
            "/api/v1/admin/strm-probe-jobs",
            get(admin_list_strm_probe_jobs).post(admin_start_strm_probe),
        )
        .route(
            "/api/v1/admin/strm-probe-jobs/{job_id}",
            get(admin_get_strm_probe_job),
        )
        .route(
            "/api/v1/admin/strm-probe-jobs/{job_id}/cancel",
            post(admin_cancel_strm_probe),
        )
        .route(
            "/api/v1/admin/strm-probe-jobs/{job_id}/retry",
            post(admin_retry_strm_probe),
        )
        .route(
            "/api/v1/admin/libraries/{library_id}/chapter-detection",
            post(admin_start_chapter_detection),
        )
        .route(
            "/api/v1/admin/chapter-detection-jobs",
            get(admin_list_chapter_detection_jobs),
        )
        .route(
            "/api/v1/admin/chapter-detection-jobs/{job_id}",
            get(admin_get_chapter_detection_job),
        )
        .route(
            "/api/v1/admin/chapter-detection-jobs/{job_id}/cancel",
            post(admin_cancel_chapter_detection),
        )
        .route(
            "/api/v1/admin/chapter-detection-jobs/{job_id}/retry",
            post(admin_retry_chapter_detection),
        )
        .route(
            "/api/v1/admin/libraries/{library_id}/danmaku/match",
            post(admin_start_danmaku_match),
        )
        .route(
            "/api/v1/admin/danmaku/match-jobs",
            get(admin_list_danmaku_match_jobs),
        )
        .route(
            "/api/v1/admin/danmaku/match-jobs/{job_id}",
            get(admin_get_danmaku_match_job),
        )
        .route(
            "/api/v1/admin/danmaku/match-jobs/{job_id}/cancel",
            post(admin_cancel_danmaku_match),
        )
        .route(
            "/api/v1/admin/danmaku/match-jobs/{job_id}/retry",
            post(admin_retry_danmaku_match),
        )
        .route(
            "/api/v1/admin/jobs/{job_id}/events",
            get(admin_list_job_events),
        )
        .route("/api/v1/admin/jobs", get(admin_list_jobs))
        .route(
            "/api/v1/admin/scheduled-tasks",
            get(admin_list_scheduled_tasks).put(admin_upsert_scheduled_task),
        )
        .route(
            "/api/v1/admin/scheduled-tasks/run",
            post(admin_run_scheduled_task),
        )
        .route("/api/v1/admin/task-activity", get(admin_list_task_activity))
        .route(
            "/api/v1/admin/settings",
            get(admin_settings).patch(admin_update_settings),
        )
        .route(
            "/api/v1/admin/api-key",
            get(admin_get_api_key).delete(admin_revoke_api_key),
        )
        .route("/api/v1/admin/api-key/rotate", post(admin_rotate_api_key))
        .route(
            "/api/v1/admin/settings/network-proxy/test",
            post(admin_test_network_proxy),
        )
        .route(
            "/api/v1/admin/notification-destinations",
            get(admin_list_webhook_destinations).post(admin_create_webhook_destination),
        )
        .route(
            "/api/v1/admin/notification-destinations/{destination_id}",
            get(admin_get_webhook_destination)
                .patch(admin_update_webhook_destination)
                .delete(admin_delete_webhook_destination),
        )
        .route(
            "/api/v1/admin/notification-destinations/{destination_id}/test",
            post(admin_test_webhook_destination),
        )
        .route(
            "/api/v1/admin/notification-destinations/{destination_id}/rotate-secret",
            post(admin_rotate_webhook_secret),
        )
        .route(
            "/api/v1/admin/notification-deliveries",
            get(admin_list_webhook_deliveries),
        )
        .route(
            "/api/v1/admin/notification-deliveries/{delivery_id}/retry",
            post(admin_retry_webhook_delivery),
        )
        .route("/api/v1/admin/health", get(admin_health))
        .route("/api/v1/admin/dashboard", get(admin_dashboard))
        .route("/api/v1/admin/events", get(admin_events))
        .route("/api/v1/events", get(user_events))
        .route("/api/v1/admin/audit", get(admin_list_audit))
        .route("/api/v1/admin/logs/export", get(admin_export_logs))
        .route("/api/v1/admin/logs", get(admin_list_logs))
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
