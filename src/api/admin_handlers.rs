use super::*;

pub(crate) async fn require_admin(
    headers: &HeaderMap,
    state: &AppState,
    require_csrf: bool,
) -> Result<(), Response> {
    if let Some(principal) = resolve_shared_admin_api_key(headers, state).await? {
        if !principal.can_manage_server()
            || (state.remote_access.is_remote(
                header_str(headers, "x-lux-peer-ip"),
                header_str(headers, "x-forwarded-for"),
            ) && !principal.can_remote_access())
        {
            return Err(api_error(
                headers,
                StatusCode::FORBIDDEN,
                lux::ApiErrorCode::PermissionDenied,
                "没有服务器管理权限",
            )
            .into_response());
        }
        return Ok(());
    }
    if users::lux_user_token_from_headers(headers).is_some() {
        return Err(api_error(
            headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::PermissionDenied,
            "客户端令牌不能调用管理员接口",
        )
        .into_response());
    }
    let user = require_web_user(headers, state).await?;
    if !user.can_manage_server {
        return Err(api_error(
            headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::PermissionDenied,
            "没有服务器管理权限",
        )
        .into_response());
    }
    if require_csrf && !users::has_client_token(headers) {
        let Some(auth) = state.auth.as_ref() else {
            return Err(api_error(
                headers,
                StatusCode::SERVICE_UNAVAILABLE,
                lux::ApiErrorCode::DatabaseUnavailable,
                "服务尚未就绪",
            )
            .into_response());
        };
        let Some(session_token) = request_cookie(headers, "lux_session") else {
            return Err(api_error(
                headers,
                StatusCode::UNAUTHORIZED,
                lux::ApiErrorCode::AuthenticationRequired,
                "需要登录",
            )
            .into_response());
        };
        let session = match auth.resolve(&session_token).await {
            Ok(Some(session)) => session,
            Ok(None) => {
                return Err(api_error(
                    headers,
                    StatusCode::UNAUTHORIZED,
                    lux::ApiErrorCode::AuthenticationRequired,
                    "需要登录",
                )
                .into_response());
            }
            Err(_) => {
                return Err(api_error(
                    headers,
                    StatusCode::SERVICE_UNAVAILABLE,
                    lux::ApiErrorCode::DatabaseUnavailable,
                    "认证暂时不可用",
                )
                .into_response());
            }
        };
        let Some(csrf_token) = headers
            .get("x-csrf-token")
            .and_then(|value| value.to_str().ok())
        else {
            return Err(api_error(
                headers,
                StatusCode::FORBIDDEN,
                lux::ApiErrorCode::CsrfFailed,
                "CSRF 校验失败",
            )
            .into_response());
        };
        if !auth.verify_csrf(&session, csrf_token) {
            return Err(api_error(
                headers,
                StatusCode::FORBIDDEN,
                lux::ApiErrorCode::CsrfFailed,
                "CSRF 校验失败",
            )
            .into_response());
        }
    }
    Ok(())
}

pub(crate) async fn require_admin_web_session(
    headers: &HeaderMap,
    state: &AppState,
    require_csrf: bool,
) -> Result<(), Response> {
    if users::has_client_token(headers) {
        return Err(api_error(
            headers,
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::PermissionDenied,
            "客户端令牌不能管理 API Key",
        )
        .into_response());
    }
    require_admin(headers, state, require_csrf).await
}

pub(crate) async fn resolve_shared_admin_api_key(
    headers: &HeaderMap,
    state: &AppState,
) -> Result<Option<crate::auth::users::AuthenticationPrincipal>, Response> {
    let Some(candidate) = lux_api_key_from_headers(headers) else {
        return Ok(None);
    };
    let Some(service) = state.admin_api_key.as_ref() else {
        return Err(api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "认证服务尚未就绪",
        )
        .into_response());
    };
    service.resolve_principal(&candidate).await.map_err(|_| {
        api_error(
            headers,
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "认证暂时不可用",
        )
        .into_response()
    })
}

pub(crate) fn lux_api_key_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get("X-Lux-Api-Key")
        .or_else(|| headers.get("X-Emby-Token"))
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            headers
                .get("Authorization")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().strip_prefix("Bearer "))
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        })
}

pub(crate) async fn normalize_lux_api_key_query(request: Request<Body>, next: Next) -> Response {
    let mut request = request;
    if request.uri().path().starts_with("/api/v1")
        && !request.headers().contains_key("X-Lux-Api-Key")
        && !request.headers().contains_key("X-Emby-Token")
        && !request.headers().contains_key("X-MediaBrowser-Token")
        && !request.headers().contains_key("Authorization")
        && let Some(query) = request.uri().query()
        && let Some(key) =
            url::form_urlencoded::parse(query.as_bytes()).find_map(|(name, value)| {
                name.eq_ignore_ascii_case("api_key")
                    .then_some(value.into_owned())
            })
        && let Ok(value) = HeaderValue::from_str(&key)
    {
        request.headers_mut().insert("X-Lux-Api-Key", value);
    }
    next.run(request).await
}

pub(crate) async fn probe_directory_writable(path: &FsPath) -> bool {
    let probe_path = path.join(format!(".lux-health-probe-{}", uuid::Uuid::now_v7()));
    let payload = [0_u8; 4096];
    let result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe_path)
            .await?;
        file.write_all(&payload).await?;
        file.sync_all().await
    }
    .await;
    let _ = fs::remove_file(probe_path).await;
    result.is_ok()
}

pub(crate) async fn admin_database_diagnostics_status(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(diagnostics) = state.database_diagnostics.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let mut response = Json(diagnostics.status().await).into_response();
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("no-store, max-age=0"),
    );
    response
}

pub(crate) async fn admin_start_database_diagnostics(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, true).await {
        return response;
    }
    let Some(diagnostics) = state.database_diagnostics.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match diagnostics.request_collection().await {
        Ok(status) => {
            let mut response = (StatusCode::ACCEPTED, Json(status)).into_response();
            response.headers_mut().insert(
                CACHE_CONTROL,
                HeaderValue::from_static("no-store, max-age=0"),
            );
            response
        }
        Err(
            crate::application::database_diagnostics::DatabaseDiagnosticsStartError::AlreadyRunning,
        ) => api_error(
            &headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "数据库体检正在运行",
        )
        .into_response(),
    }
}

pub(crate) async fn admin_export_database_diagnostics(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(diagnostics) = state.database_diagnostics.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(report) = diagnostics.report().await else {
        return api_error(
            &headers,
            StatusCode::CONFLICT,
            lux::ApiErrorCode::InvalidRequest,
            "数据库体检报告尚未就绪",
        )
        .into_response();
    };
    database_diagnostics_export_response(report)
}

fn database_diagnostics_export_response(report: Value) -> Response {
    let mut response = Json(report).into_response();
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("no-store, max-age=0"),
    );
    response.headers_mut().insert(
        CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"lux-database-diagnostics.json\""),
    );
    response
}

#[cfg(test)]
mod database_diagnostics_export_tests {
    use super::*;

    #[tokio::test]
    async fn report_export_is_a_no_store_json_attachment() {
        let response = database_diagnostics_export_response(json!({ "backend": "POSTGRESQL" }));

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store, max-age=0");
        assert_eq!(
            response.headers()[CONTENT_DISPOSITION],
            "attachment; filename=\"lux-database-diagnostics.json\""
        );
        let body = to_bytes(response.into_body(), 1024)
            .await
            .expect("JSON body");
        assert_eq!(body.as_ref(), br#"{"backend":"POSTGRESQL"}"#);
    }
}

pub(crate) const DEFAULT_SERVER_NAME: &str = "Lux Server";
const DASHBOARD_ACTIVITY_LIMIT: i64 = 24;
const DASHBOARD_PLAYBACK_LIMIT: usize = 24;

pub(crate) async fn admin_dashboard(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let health = match admin_health_payload(&state).await {
        Ok(health) => health,
        Err(status) => return status.into_response(),
    };
    let server_name = match database.server_name().await {
        Ok(Some(name)) if !name.trim().is_empty() => name,
        Ok(_) => DEFAULT_SERVER_NAME.to_owned(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let stats = match database.dashboard_stats().await {
        Ok(stats) => stats,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let sessions = match database.list_playback_sessions(None, None).await {
        Ok(sessions) => sessions,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let users = match UserStore::new(database.clone()) {
        Ok(users) => match users.list_users().await {
            Ok(users) => users,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        },
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let activity = match database
        .list_activity_events(DASHBOARD_ACTIVITY_LIMIT)
        .await
    {
        Ok(events) => {
            let series_by_event_id = match dashboard_activity_series_metadata(&state, &events).await
            {
                Ok(series) => series,
                Err(status) => return status.into_response(),
            };
            events
                .iter()
                .map(|event| {
                    dashboard_activity_json(
                        event,
                        state.ip_location.as_ref(),
                        series_by_event_id.get(&event.id),
                    )
                })
                .collect::<Vec<_>>()
        }
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let now_playing = match dashboard_playback_json(&state, &sessions, &users).await {
        Ok(sessions) => sessions,
        Err(status) => return status.into_response(),
    };
    Json(json!({
        "server": {
            "name": server_name,
            "version": VERSION,
            "commit": COMMIT,
            "schemaVersion": health["schemaVersion"],
        },
        "stats": dashboard_stats_json(&stats),
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

pub(crate) async fn dashboard_playback_json(
    state: &AppState,
    sessions: &[StoredPlaybackSession],
    users: &[UserRecord],
) -> Result<Vec<Value>, StatusCode> {
    let Some(catalog) = state.catalog.as_ref() else {
        return Ok(Vec::new());
    };
    let user_names = users
        .iter()
        .map(|user| (user.id.to_string(), user.display_name.clone()))
        .collect::<BTreeMap<_, _>>();
    let principal = AccessPrincipal::new(crate::domain::ids::UserId::new(), true);
    let sessions = sessions
        .iter()
        .take(DASHBOARD_PLAYBACK_LIMIT)
        .collect::<Vec<_>>();
    let web_play_session_ids = sessions
        .iter()
        .filter(|session| {
            session.play_session_id.starts_with("lux-emby:")
                || session.play_session_id.starts_with("lux-web:")
        })
        .map(|session| session.play_session_id.clone())
        .collect::<Vec<_>>();
    let Some(database) = state.database.as_ref() else {
        return Ok(Vec::new());
    };
    let web_sessions = database
        .find_web_playback_sessions_for_playbacks(&web_play_session_ids)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .into_iter()
        .map(|session| (session.play_session_id.clone(), session))
        .collect::<HashMap<_, _>>();
    let item_ids = sessions
        .iter()
        .map(|session| session.item_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let items_by_id = catalog
        .find_items(principal, &item_ids)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let series_ids = items_by_id
        .values()
        .filter(|item| item.item_type == "EPISODE")
        .filter_map(|item| item.series_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let series_by_id = catalog
        .find_items(principal, &series_ids)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let mut values = Vec::new();
    for session in sessions {
        let Some(item) = items_by_id.get(&session.item_id) else {
            continue;
        };
        let series = item
            .series_id
            .as_deref()
            .and_then(|series_id| series_by_id.get(series_id));
        let remote_ip_location = session.remote_ip.as_deref().and_then(|remote_ip| {
            state
                .ip_location
                .as_ref()
                .and_then(|service| service.cached_or_schedule(remote_ip))
        });
        values.push(dashboard_playback_item_json(
            session,
            item,
            series,
            user_names
                .get(&session.user_id)
                .map(String::as_str)
                .unwrap_or("未知账户"),
            remote_ip_location.as_ref(),
            web_sessions.get(&session.play_session_id),
        ));
    }
    Ok(values)
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
        "posterAvailable": item.poster_image_tag.is_some(),
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
    ip_location: Option<&IpLocationService>,
    series: Option<&DashboardActivitySeriesMetadata>,
) -> Value {
    let metadata =
        serde_json::from_str::<Value>(&event.metadata_json).unwrap_or_else(|_| json!({}));
    let remote_ip = metadata.get("remoteIp").and_then(Value::as_str);
    let remote_ip_location = remote_ip.and_then(|remote_ip| {
        ip_location.and_then(|service| service.cached_or_schedule(remote_ip))
    });
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
        "remoteIpLocation": remote_ip_location
            .as_ref()
            .map(dashboard_ip_location_json),
        "createdAt": event.created_at,
    })
}

pub(crate) struct DashboardActivitySeriesMetadata {
    title: String,
    season_number: Option<i64>,
    season_count: Option<i64>,
}

async fn dashboard_activity_series_metadata(
    state: &AppState,
    events: &[crate::storage::StoredActivityEvent],
) -> Result<HashMap<String, DashboardActivitySeriesMetadata>, StatusCode> {
    let Some(catalog) = state.catalog.as_ref() else {
        return Ok(HashMap::new());
    };
    let item_ids = events
        .iter()
        .filter(|event| event.target_type.as_deref() == Some("media_item"))
        .filter_map(|event| event.target_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if item_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let principal = AccessPrincipal::new(crate::domain::ids::UserId::new(), true);
    let items_by_id = catalog
        .find_items(principal, &item_ids)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let series_ids = items_by_id
        .values()
        .filter(|item| item.item_type == "EPISODE")
        .filter_map(|item| item.series_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let series_by_id = catalog
        .find_items(principal, &series_ids)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    Ok(events
        .iter()
        .filter_map(|event| {
            let item = event
                .target_id
                .as_deref()
                .and_then(|item_id| items_by_id.get(item_id))?;
            if item.item_type != "EPISODE" {
                return None;
            }
            let series = item
                .series_id
                .as_deref()
                .and_then(|series_id| series_by_id.get(series_id))?;
            Some((
                event.id.clone(),
                DashboardActivitySeriesMetadata {
                    title: series.title.clone(),
                    season_number: item.season_number,
                    season_count: series.season_count,
                },
            ))
        })
        .collect())
}

pub(crate) fn api_error(
    headers: &HeaderMap,
    status: StatusCode,
    code: lux::ApiErrorCode,
    message: &str,
) -> (StatusCode, Json<Value>) {
    let request_id = headers
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown");
    tracing::Span::current().record("errorCode", code.as_str());
    let body = lux::ApiError::new(code, message, request_id);
    (
        status,
        Json(json!({
            "error": {
                "code": body.code,
                "message": body.message,
                "requestId": body.request_id
            }
        })),
    )
}

pub(crate) fn user_avatar_error(headers: &HeaderMap, error: UserAvatarError) -> Response {
    match error {
        UserAvatarError::UnsupportedContentType | UserAvatarError::InvalidContent => api_error(
            headers,
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            "头像格式无效，仅支持 JPEG、PNG 或 WebP",
        )
        .into_response(),
        UserAvatarError::TooLarge { .. } => api_error(
            headers,
            StatusCode::PAYLOAD_TOO_LARGE,
            lux::ApiErrorCode::InvalidRequest,
            "头像不能超过 5 MiB",
        )
        .into_response(),
        UserAvatarError::InvalidPath(_) | UserAvatarError::Io { .. } => api_error(
            headers,
            StatusCode::INTERNAL_SERVER_ERROR,
            lux::ApiErrorCode::Internal,
            "头像暂时无法保存",
        )
        .into_response(),
    }
}

pub(crate) fn request_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(COOKIE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value.split(';').find_map(|part| {
                let (cookie_name, cookie_value) = part.trim().split_once('=')?;
                (cookie_name == name).then(|| cookie_value.to_owned())
            })
        })
}

pub(crate) fn secure_cookie_for_request(headers: &HeaderMap, policy: &RemoteAccessPolicy) -> bool {
    policy.is_secure_request(
        header_str(headers, "x-lux-peer-ip"),
        header_str(headers, "x-forwarded-proto"),
    )
}

pub(crate) fn build_cookie(
    name: &str,
    value: &str,
    http_only: bool,
    max_age: Option<i64>,
    secure: bool,
) -> Option<HeaderValue> {
    let mut cookie = format!("{name}={value}; Path=/;");
    if secure {
        cookie.push_str(" Secure;");
    }
    cookie.push_str(" SameSite=Lax");
    if http_only {
        cookie.push_str("; HttpOnly");
    }
    if let Some(max_age) = max_age {
        cookie.push_str(&format!("; Max-Age={max_age}"));
    }
    HeaderValue::from_str(&cookie).ok()
}
