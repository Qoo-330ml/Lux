use super::*;

pub(super) fn api_routes() -> Router<AppState> {
    Router::new()
        .route("/System/Info/Public", get(emby_public_system_info))
        .route("/System/Info", get(emby_system_info))
        .route("/System/Ping", get(emby_ping).post(emby_ping))
        .route(
            "/DisplayPreferences/{display_preferences_id}",
            get(emby_display_preferences),
        )
        .route("/Users/Public", get(emby_public_users))
        .route("/Users", get(emby_users))
        .route("/Users/AuthenticateByName", post(emby_authenticate))
        .route("/Users/authenticatebyname", post(emby_authenticate))
        .route("/Library/VirtualFolders", get(emby_library_virtual_folders))
        .route("/Persons", get(emby_persons))
        .route("/Persons/{person_id}", get(emby_person))
        .route("/Users/{user_id}", get(emby_user))
        .route("/Users/{user_id}", post(emby_update_user).put(emby_update_user))
        .route("/Users/{user_id}/Views", get(emby_user_views))
        .route("/Users/{user_id}/Items/Root", get(emby_user_root))
        .route("/Users/{user_id}/Items/Resume", get(emby_user_resume))
        .route("/Users/{user_id}/Items/Latest", get(emby_user_latest))
        .route("/Users/{user_id}/Items/NextUp", get(emby_user_next_up))
        .route("/Users/{user_id}/Items", get(emby_user_items))
        .route("/Users/{user_id}/Items/{item_id}", get(emby_user_item))
        .route("/Users/{user_id}/Policy", post(emby_update_user_policy).put(emby_update_user_policy))
        .route("/Users/{user_id}/Password", post(emby_update_user_password).put(emby_update_user_password))
        .route(
            "/Users/{user_id}/Images/Primary",
            get(emby_user_avatar)
                .post(emby_update_user_avatar)
                .put(emby_update_user_avatar),
        )
        .route(
            "/Persons/{person_id}/Images/{image_type}",
            get(emby_person_image).head(emby_person_image),
        )
        .route(
            "/Persons/{person_id}/Images/{image_type}/{image_index}",
            get(emby_person_image_at_index).head(emby_person_image_at_index),
        )
        .route("/Shows/NextUp", get(emby_shows_next_up))
        .route("/Shows/{series_id}/Seasons", get(emby_show_seasons))
        .route("/Shows/{series_id}/Episodes", get(emby_show_episodes))
        .route("/Items", get(emby_items))
        .route("/Items/Counts", get(emby_items_counts))
        .route("/Items/Root", get(emby_items_root))
        .route("/Search/Hints", get(emby_search_hints))
        .route(
            "/Items/{item_id}",
            get(emby_item).head(emby_item).post(emby_update_item),
        )
        .route("/Items/{item_id}/Children", get(emby_collection_children))
        .route("/api/danmu/{item_id}", get(emby_danmaku_info))
        .route("/api/danmu/{item_id}/raw", get(emby_danmaku_raw))
        .route(
            "/Items/{item_id}/Images/{image_type}",
            get(emby_image)
                .head(emby_image)
                .post(emby_update_person_image),
        )
        .route(
            "/Items/{item_id}/Images/{image_type}/{image_index}",
            get(emby_image_at_index).head(emby_image_at_index),
        )
        .route(
            "/Videos/{item_id}/{media_source_id}/Subtitles/{stream_index}/Stream",
            get(emby_subtitle_with_source).head(emby_subtitle_with_source),
        )
        .route(
            "/Videos/{item_id}/original.strm",
            get(emby_stream).head(emby_stream),
        )
        .route(
            "/Items/{item_id}/Subtitles/{stream_index}/Stream",
            get(emby_subtitle_without_source).head(emby_subtitle_without_source),
        )
        .route(
            "/Videos/{item_id}/stream",
            get(emby_stream).head(emby_stream),
        )
        .route(
            "/Videos/{item_id}/stream.{container}",
            get(emby_stream_with_container).head(emby_stream_with_container),
        )
        .route(
            "/Videos/{item_id}/{media_source_id}/stream",
            get(emby_stream_with_source).head(emby_stream_with_source),
        )
        .route(
            "/Videos/{item_id}/{media_source_id}/stream.{container}",
            get(emby_stream_with_source_and_container).head(emby_stream_with_source_and_container),
        )
        .route(
            "/videos/{item_id}/stream",
            get(emby_stream).head(emby_stream),
        )
        .route(
            "/videos/{item_id}/original.strm",
            get(emby_stream).head(emby_stream),
        )
        .route(
            "/videos/{item_id}/stream.{container}",
            get(emby_stream_with_container).head(emby_stream_with_container),
        )
        .route(
            "/videos/{item_id}/{media_source_id}/stream",
            get(emby_stream_with_source).head(emby_stream_with_source),
        )
        .route(
            "/videos/{item_id}/{media_source_id}/stream.{container}",
            get(emby_stream_with_source_and_container).head(emby_stream_with_source_and_container),
        )
        .route(
            "/Items/{item_id}/PlaybackInfo",
            get(emby_playback_info).post(emby_playback_info),
        )
        .route(
            "/Items/{item_id}/Download",
            get(emby_download).head(emby_download),
        )
        .route("/Sessions", get(emby_sessions))
        .route("/Sessions/Playing", post(emby_playing))
        .route("/Sessions/Playing/Progress", post(emby_playing_progress))
        .route("/Sessions/Playing/Stopped", post(emby_playing_stopped))
        .route(
            "/Users/{user_id}/PlayedItems/{item_id}",
            post(emby_mark_played).delete(emby_unmark_played),
        )
        .route("/Users/{user_id}/FavoriteItems", get(emby_user_favorites))
        .route(
            "/Users/{user_id}/FavoriteItems/{item_id}",
            post(emby_mark_favorite).delete(emby_unmark_favorite),
        )
        .route("/Sessions/Logout", post(emby_logout))
}
