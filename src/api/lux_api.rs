use super::*;

pub(super) fn api_routes() -> Router<AppState> {
    Router::new()
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
            "/api/v1/playback/bootstrap",
            post(lux_create_web_playback_bootstrap),
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
}
