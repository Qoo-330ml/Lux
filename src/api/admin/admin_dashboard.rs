use super::*;

pub(super) fn api_routes() -> Router<AppState> {
    Router::new().route("/api/v1/admin/dashboard", get(admin_dashboard))
}

pub(crate) async fn admin_dashboard(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let database = require_service!(state.database);
    let snapshot = match crate::application::admin_dashboard::load_admin_dashboard(
        database,
        state.config_dir.as_deref(),
        &state.resources,
        crate::application::admin_dashboard::AdminDashboardServices {
            libraries: state.libraries(),
            catalog: state.catalog(),
            ip_location: state.ip_location(),
            health_probe_cache: &state.admin_health_probe_cache,
        },
    )
    .await
    {
        Ok(snapshot) => snapshot,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let health = match serde_json::to_value(&snapshot.health) {
        Ok(health) => health,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let activity = snapshot
        .activity
        .iter()
        .map(|activity| {
            dashboard_activity_json(
                &activity.event,
                activity.remote_ip_location.as_ref(),
                activity.series.as_ref(),
            )
        })
        .collect::<Vec<_>>();
    let now_playing = snapshot
        .now_playing
        .iter()
        .map(|playback| {
            dashboard_playback_item_json(
                &playback.session,
                &playback.item,
                playback.series.as_ref(),
                &playback.user_name,
                playback.remote_ip_location.as_ref(),
                playback.web_session.as_ref(),
            )
        })
        .collect::<Vec<_>>();
    Json(json!({
        "server": {
            "name": snapshot.server_name,
            "version": VERSION,
            "commit": COMMIT,
            "schemaVersion": snapshot.schema_version,
        },
        "stats": dashboard_stats_json(&snapshot.stats),
        "health": health,
        "nowPlaying": now_playing,
        "activity": activity,
    }))
    .into_response()
}

pub(crate) fn dashboard_stats_json(stats: &DashboardStats) -> Value {
    json!({
        "movieCount": stats.movie_count,
        "seriesCount": stats.series_count,
        "userCount": stats.user_count,
    })
}

pub(crate) fn dashboard_playback_item_json(
    session: &StoredPlaybackSession,
    item: &CatalogItem,
    series: Option<&CatalogItem>,
    user_name: &str,
    remote_ip_location: Option<&IpLocation>,
    web_session: Option<&crate::storage::StoredWebPlaybackSession>,
) -> Value {
    let source = session
        .media_source_id
        .as_deref()
        .and_then(|source_id| {
            item.media_sources
                .iter()
                .find(|source| source.id == source_id)
        })
        .or_else(|| item.media_sources.iter().find(|source| source.is_default))
        .or_else(|| item.media_sources.first());
    // Episodes show the series poster, never the season or episode image.
    let poster_owner = if item.item_type == "EPISODE" {
        series.filter(|series| series.poster_image_tag.is_some())
    } else {
        Some(item).filter(|item| item.poster_image_tag.is_some())
    };
    json!({
        "id": session.id,
        "userId": session.user_id,
        "userName": user_name,
        "itemId": item.id,
        "title": item.title,
        "originalTitle": item.original_title,
        "itemType": item.item_type,
        "seriesId": item.series_id,
        "seriesTitle": series.map(|item| item.title.as_str()),
        "productionYear": item.production_year,
        "parentIndexNumber": item.season_number,
        "indexNumber": item.episode_number,
        "posterAvailable": poster_owner.is_some(),
        "posterItemId": poster_owner.map(|owner| owner.id.as_str()),
        "positionTicks": session.position_ticks,
        "durationTicks": session.duration_ticks.or(item.runtime_ticks),
        "state": session.state,
        "isPaused": session.is_paused,
        "lastEventAt": session.last_event_at,
        "client": session.client,
        "clientVersion": session.client_version,
        "deviceId": session.device_id,
        "deviceName": session.device_name,
        "deviceType": session.device_type,
        "remoteIp": session.remote_ip,
        "remoteIpLocation": remote_ip_location.map(dashboard_ip_location_json),
        "playSessionId": session.play_session_id,
        "playMethod": dashboard_play_method(session, web_session),
        "playbackPlan": web_session.map(|session| session.plan.as_str()),
        "serverTier": web_session.map(|session| session.tier),
        "output": web_session.and_then(|web_session| {
            (web_session.plan == "SERVER_HLS").then(|| dashboard_playback_output(web_session, source))
        }),
        "source": source.map(dashboard_source_json),
    })
}

fn dashboard_play_method(
    session: &StoredPlaybackSession,
    web_session: Option<&crate::storage::StoredWebPlaybackSession>,
) -> &'static str {
    let Some(web_session) = web_session.filter(|session| session.plan == "SERVER_HLS") else {
        return if session.play_session_id.starts_with("lux-emby:") {
            "Transcode"
        } else {
            "DirectPlay"
        };
    };
    if web_session.tier <= i64::from(ServerTier::Remux.number()) {
        "DirectStream"
    } else {
        "Transcode"
    }
}

fn dashboard_playback_output(
    web_session: &crate::storage::StoredWebPlaybackSession,
    source: Option<&CatalogSource>,
) -> Value {
    let source_video_codec = source.and_then(|source| {
        source
            .streams
            .iter()
            .find(|stream| stream.stream_type.eq_ignore_ascii_case("VIDEO"))
            .and_then(|stream| stream.codec.clone())
    });
    let source_audio_codec = source.and_then(|source| {
        source
            .streams
            .iter()
            .find(|stream| stream.stream_type.eq_ignore_ascii_case("AUDIO"))
            .and_then(|stream| stream.codec.clone())
    });
    let video_transcoded = web_session.tier >= i64::from(ServerTier::HardwareTranscode.number());
    let audio_transcoded = web_session.tier >= i64::from(ServerTier::AudioTranscode.number());
    json!({
        "container": web_session.transcoding_container,
        "videoCodec": web_session.video_codec.as_deref().or_else(|| {
            video_transcoded.then_some("h264").or(source_video_codec.as_deref())
        }),
        "audioCodec": web_session.audio_codec.as_deref().or_else(|| {
            audio_transcoded.then_some("aac").or(source_audio_codec.as_deref())
        }),
        "videoBitrate": web_session.video_bitrate,
        "audioBitrate": web_session.audio_bitrate,
    })
}

pub(crate) fn dashboard_ip_location_json(location: &IpLocation) -> Value {
    json!({
        "location": location.formatted_location(),
        "district": location.district,
        "street": location.street,
        "isp": location.isp,
    })
}

pub(crate) fn dashboard_source_json(source: &CatalogSource) -> Value {
    let video = source
        .streams
        .iter()
        .find(|stream| stream.stream_type == "VIDEO");
    let audio = source
        .streams
        .iter()
        .find(|stream| stream.stream_type == "AUDIO");
    json!({
        "id": source.id,
        "qualityLabel": source.quality_label,
        "editionName": source.edition_name,
        "container": source.container,
        "bitrate": source.bitrate,
        "durationTicks": source.duration_ticks,
        "video": video.map(|stream| json!({
            "codec": stream.codec,
            "title": stream.title,
            "details": stream.details,
        })),
        "audio": audio.map(|stream| json!({
            "codec": stream.codec,
            "language": stream.language,
            "title": stream.title,
        })),
    })
}

pub(crate) fn dashboard_activity_json(
    event: &crate::storage::StoredActivityEvent,
    remote_ip_location: Option<&IpLocation>,
    series: Option<&crate::application::admin_dashboard::DashboardActivitySeries>,
) -> Value {
    let metadata =
        serde_json::from_str::<Value>(&event.metadata_json).unwrap_or_else(|_| json!({}));
    let remote_ip = metadata.get("remoteIp").and_then(Value::as_str);
    json!({
        "id": event.id,
        "userId": event.actor_user_id,
        "userName": event.actor_username,
        "eventType": event.event_type,
        "targetType": event.target_type,
        "targetId": event.target_id,
        "targetTitle": event.target_title,
        "targetSeriesTitle": series.map(|series| series.title.as_str()),
        "targetSeasonNumber": series.and_then(|series| series.season_number),
        "targetSeriesSeasonCount": series.and_then(|series| series.season_count),
        "metadata": metadata,
        "remoteIp": remote_ip,
        "remoteIpLocation": remote_ip_location.map(dashboard_ip_location_json),
        "createdAt": event.created_at,
    })
}
