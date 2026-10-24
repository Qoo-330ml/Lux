use super::*;

#[path = "emby_resume_visibility.rs"]
mod resume_visibility;
pub(super) use resume_visibility::emby_hide_from_resume;

use crate::application::playback::{
    decision::ServerTier,
    hls::{HlsSegmentContainer, HlsStartOptions},
    session::{CreatedWebPlaybackSession, WebPlaybackPlan},
};
use crate::storage::{
    MAX_PLAYBACK_SESSION_WINDOW_SECONDS, PlaybackAdmissionDecision, PlaybackAdmissionDenialReason,
    PlaybackAdmissionResult, StoredWebPlaybackSession, WebPlaybackTranscodingDetails,
};

/// Resolves the privilege snapshot for a signed playback resource.
///
/// Signed asset URLs stay valid for twelve hours, far longer than a login session, so the
/// `is_admin` column captured when the playback session was created must not outlive an
/// administrator demotion. Read the user's current role instead: a demoted user loses the
/// administrator bypass immediately and a disabled user loses the resource entirely.
async fn playback_resource_principal(
    database: Option<&crate::storage::Database>,
    session: &StoredWebPlaybackSession,
) -> Result<AccessPrincipal, StatusCode> {
    let Some(database) = database else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let users = UserStore::new(database.clone()).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    match users.find_by_id(&session.user_id).await {
        Ok(Some(user)) if !user.is_disabled => Ok(AccessPrincipal::new(user.id, user.is_admin)),
        Ok(Some(_)) | Ok(None) => Err(StatusCode::NOT_FOUND),
        Err(_) => Err(StatusCode::SERVICE_UNAVAILABLE),
    }
}

pub(super) async fn emby_playback_info(
    headers: HeaderMap,
    method: Method,
    Path(item_id): Path<String>,
    raw_query: RawQuery,
    State(state): State<AppState>,
    body: Bytes,
) -> Response {
    let force_transcode = method == Method::POST && emby_force_transcode_from_raw(&raw_query);
    let mut request = match parse_emby_playback_info_request(&body) {
        Ok(request) => request,
        Err(status) => return status.into_response(),
    };
    request.apply_query_parameters(&raw_query);
    let device_id = emby_playback_device_id(&headers, &raw_query);
    let query = emby_stream_query_from_raw(raw_query);
    let standard_api_key =
        standard_emby_playback_api_key(&headers, query.api_key.as_deref(), &state).await;
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let internal_item_id = emby_internal_id(&item_id);
    let item = match catalog.find_item(principal, &internal_item_id).await {
        Ok(Some(item)) => item,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(CatalogError::Storage(_)) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => {
            return StatusCode::NOT_FOUND.into_response();
        }
    };
    let (playback_items, preferred_source_item_id, preferred_media_source_id) = if item.library_id
        == crate::storage::GLOBAL_COLLECTION_LIBRARY_ID
        && item.item_type == "MOVIE"
    {
        let playback_sources = match catalog
            .global_collection_playback_sources(principal, &item.id)
            .await
        {
            Ok(Some(sources)) => sources,
            Ok(None) => return StatusCode::NOT_FOUND.into_response(),
            Err(CatalogError::Storage(_)) => {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => {
                return StatusCode::NOT_FOUND.into_response();
            }
        };
        (
            playback_sources.copies,
            playback_sources.preferred_source_item_id,
            playback_sources.preferred_media_source_id,
        )
    } else {
        (vec![item.clone()], None, None)
    };
    let mut sources = playback_items
        .iter()
        .flat_map(|source_item| {
            source_item
                .media_sources
                .iter()
                .map(move |source| (source_item, source))
        })
        .collect::<Vec<_>>();
    if sources.is_empty() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let query_media_source_requested = query.media_source_id.is_some();
    // Keep each version attached to its original item so the DTO, media URL,
    // subtitle routes and HLS session all authorize against the real owner.
    sources.sort_by_key(|(source_item, source)| {
        let preferred_copy =
            usize::from(preferred_source_item_id.as_deref() != Some(source_item.id.as_str()));
        let preferred_source =
            usize::from(preferred_media_source_id.as_deref() != Some(source.id.as_str()));
        let default_version =
            usize::from(!crate::application::catalog::belongs_to_default_version(
                &source_item.media_sources,
                source,
            ));
        (preferred_copy, preferred_source, default_version)
    });
    if let Some(source_id) = query.media_source_id {
        let Some(index) = sources
            .iter()
            .position(|(_, source)| source.id == source_id)
        else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let source = sources.remove(index);
        sources.insert(0, source);
    }
    if let Some(source_id) = request.media_source_id.as_deref() {
        let Some(index) = sources
            .iter()
            .position(|(_, source)| source.id == source_id)
        else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let source = sources.remove(index);
        sources.insert(0, source);
    }
    let Some(admission_service) = state.web_playback.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if let Err(response) =
        check_web_playback_admission(&headers, admission_service, &user.id.to_string()).await
    {
        return response;
    }
    let runtime_ticks = sources.first().and_then(|(source_item, source)| {
        super::emby_catalog::emby_source_runtime_ticks(source_item, source)
    });
    let transcode_requested = method == Method::POST
        && sources.first().is_some_and(|(_, source)| {
            request.requests_server_transcoding_for_source(force_transcode, source)
        });
    if let Some((source_item, source)) = sources.first() {
        tracing::info!(
            event = "emby_playback_negotiation",
            item_id_prefix = %playback_identifier_prefix(&source_item.id),
            source_id_prefix = %playback_identifier_prefix(&source.id),
            source_kind = %source.source_kind,
            query_media_source_requested,
            body_media_source_requested = request.media_source_id.is_some(),
            enable_direct_play = ?request.enable_direct_play,
            enable_direct_stream = ?request.enable_direct_stream,
            enable_transcoding = ?request.enable_transcoding,
            allow_video_stream_copy = ?request.allow_video_stream_copy,
            allow_audio_stream_copy = ?request.allow_audio_stream_copy,
            device_profile_present = request.device_profile.is_some(),
            device_profile_hls = request
                .device_profile
                .as_ref()
                .is_some_and(EmbyDeviceProfile::supports_lux_hls),
            direct_play_compatibility = ?request
                .device_profile
                .as_ref()
                .map(|profile| profile.direct_play_compatibility(source)),
            source_bitrate = ?source.bitrate,
            max_streaming_bitrate = ?request.effective_max_streaming_bitrate(),
            source_bitrate_exceeds_limit = request.source_exceeds_streaming_bitrate(source),
            force_transcode,
            transcode_requested,
            "negotiated Emby playback source"
        );
    }
    let strm_resolver_available = if sources
        .iter()
        .any(|(_, source)| emby_source_needs_strm_resolver(source))
    {
        match state.plugins.as_ref() {
            Some(plugins) => match plugins.has_available_strm_resolver().await {
                Ok(available) => available,
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            },
            None => false,
        }
    } else {
        false
    };
    let transcode_user_id = user.id.to_string();
    let transcode_start_time_ticks = if transcode_requested {
        let Some((source_item, _)) = sources.first() else {
            return StatusCode::NOT_FOUND.into_response();
        };
        match emby_transcoding_start_time_ticks(
            &state,
            &transcode_user_id,
            &source_item.id,
            request.start_time_ticks,
            runtime_ticks,
        )
        .await
        {
            Ok(value) => value,
            Err(status) => return status.into_response(),
        }
    } else {
        None
    };
    let transcode_container = if transcode_requested {
        let Some((_, source)) = sources.first() else {
            return StatusCode::NOT_FOUND.into_response();
        };
        match request.selected_hls_container(source) {
            Ok(container) => Some(container),
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        }
    } else {
        None
    };
    let authorization_context = if transcode_requested {
        match emby_playback_authorization_context(
            &headers,
            query.api_key.as_deref(),
            query.client.as_deref(),
            &state,
            &device_id,
        )
        .await
        {
            Ok(context) => context,
            Err(status) => return status.into_response(),
        }
    } else {
        (None, None)
    };
    let transcode_session = if transcode_requested {
        let Some((source_item, source)) = sources.first() else {
            return StatusCode::NOT_FOUND.into_response();
        };
        match create_emby_transcoding_session(
            &state,
            &user,
            &source_item.id,
            source,
            EmbyTranscodingSessionOptions {
                request: &request,
                authorization_device_id: authorization_context.0.as_deref(),
                authorization_client_name: authorization_context.1.as_deref(),
                hls: HlsStartOptions {
                    segment_container: transcode_container.unwrap_or(HlsSegmentContainer::MpegTs),
                    start_time_ticks: transcode_start_time_ticks,
                    runtime_ticks,
                },
            },
        )
        .await
        {
            Ok(session) => session,
            Err(status) => return status.into_response(),
        }
    } else {
        None
    };
    let play_session_id = transcode_session
        .as_ref()
        .map(|session| session.play_session_id.clone())
        .unwrap_or_else(|| Uuid::now_v7().to_string());
    let direct_ticket_expires_at =
        current_unix_timestamp().saturating_add(EMBY_DIRECT_STREAM_TTL_SECONDS);
    let mut direct_ticket_issued = false;
    let media_sources = sources
        .into_iter()
        .map(|(source_item, source)| {
            let mut value = emby_media_source_json_with_resolver(
                &source_item.id,
                source,
                true,
                strm_resolver_available,
            );
            let source_transcode_session = transcode_session
                .as_ref()
                .filter(|session| session.media_source_id == source.id);
            let source_can_transcode = source.source_kind == "LOCAL_FILE"
                && (source_transcode_session.is_some()
                    || request.enable_transcoding != Some(false)
                        && (request.enable_transcoding == Some(true)
                            || request
                                .device_profile
                                .as_ref()
                                .is_some_and(EmbyDeviceProfile::supports_lux_hls)));
            let direct_play_disabled = request.enable_direct_play == Some(false)
                || force_transcode
                || source_transcode_session.is_some();
            if let Value::Object(object) = &mut value {
                object.insert(
                    "RunTimeTicks".to_owned(),
                    super::emby_catalog::emby_source_runtime_ticks(source_item, source)
                        .map(Value::from)
                        .unwrap_or(Value::Null),
                );
                if source_can_transcode {
                    // Emby advertises the device's available transcoding
                    // profiles even when direct play wins this request.
                    // Clients need this capability bit to offer a
                    // transcoding fallback on a later request.
                    object.insert("SupportsTranscoding".to_owned(), json!(true));
                }
                if direct_play_disabled {
                    // A transcoding offer must not leave direct play enabled,
                    // otherwise clients can ignore TranscodingUrl.
                    object.insert("SupportsDirectPlay".to_owned(), json!(false));
                    if source_transcode_session.is_some() {
                        object.insert("SupportsDirectStream".to_owned(), json!(false));
                    }
                }
            }
            let has_direct_stream_url = value.get("DirectStreamUrl").is_some_and(Value::is_string);
            let transcoding_url = source_transcode_session.as_ref().and_then(|session| {
                state.web_playback.as_ref().and_then(|service| {
                    emby_transcoding_url(
                        service,
                        &source_item.id,
                        source,
                        session,
                        EmbyTranscodingUrlOptions {
                            request: &request,
                            hls: HlsStartOptions {
                                segment_container: transcode_container
                                    .unwrap_or(HlsSegmentContainer::MpegTs),
                                start_time_ticks: transcode_start_time_ticks,
                                runtime_ticks,
                            },
                            device_id: &device_id,
                        },
                    )
                })
            });
            if let Value::Object(object) = &mut value {
                if source_transcode_session.is_none()
                    && has_direct_stream_url
                    && let Some(service) = state.web_playback.as_ref()
                    && let Some(url) = emby_signed_direct_stream_url(
                        service,
                        &source_item.id,
                        source,
                        &user,
                        &play_session_id,
                        direct_ticket_expires_at,
                        if emby_source_needs_proxy_identity(source) {
                            standard_api_key.as_deref()
                        } else {
                            None
                        },
                        &device_id,
                    )
                {
                    direct_ticket_issued = true;
                    object.insert("DirectStreamUrl".to_owned(), json!(url));
                    // External proxies use the standard Emby token to identify
                    // the proxy user, while Lux requires the signed ticket.
                    // URL/path STRM sources already carry the token for clients
                    // that ignore AddApiKeyToDirectStreamUrl.
                    object.insert(
                        "AddApiKeyToDirectStreamUrl".to_owned(),
                        json!(emby_source_needs_proxy_identity(source)),
                    );
                }
                if let Some(url) = transcoding_url {
                    let segment_container =
                        transcode_container.unwrap_or(HlsSegmentContainer::MpegTs);
                    object.insert("SupportsTranscoding".to_owned(), json!(true));
                    object.insert("TranscodingUrl".to_owned(), json!(url));
                    object.insert("TranscodingSubProtocol".to_owned(), json!("hls"));
                    object.insert(
                        "TranscodingContainer".to_owned(),
                        json!(segment_container.emby_container()),
                    );
                    object.insert(
                        "TranscodingMimeType".to_owned(),
                        json!(segment_container.mime_type()),
                    );
                    // Emby keeps DirectStreamUrl and TranscodingUrl pointed at
                    // the same HLS manifest during transcoding.
                    object.insert("DirectStreamUrl".to_owned(), json!(url.clone()));
                    object.insert("AddApiKeyToDirectStreamUrl".to_owned(), json!(false));
                }
            }
            if let Some(session) = transcode_session
                .as_ref()
                .filter(|session| session.media_source_id == source.id)
            {
                tracing::info!(
                    event = "emby_transcoding_offer",
                    item_id_prefix = %playback_identifier_prefix(&source_item.id),
                    source_id_prefix = %playback_identifier_prefix(&source.id),
                    session_id_prefix = %playback_identifier_prefix(&session.id),
                    transcoding_url_present = value
                        .get("TranscodingUrl")
                        .is_some_and(|value| value.is_string()),
                    supports_direct_play = value
                        .get("SupportsDirectPlay")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false),
                    supports_direct_stream = value
                        .get("SupportsDirectStream")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false),
                    "prepared Emby transcoding offer"
                );
            }
            value
        })
        .collect::<Vec<_>>();
    if direct_ticket_issued {
        let authorization_context = match emby_playback_authorization_context(
            &headers,
            query.api_key.as_deref(),
            query.client.as_deref(),
            &state,
            &device_id,
        )
        .await
        {
            Ok(context) => context,
            Err(status) => return status.into_response(),
        };
        let Some(service) = state.web_playback.as_ref() else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        let user_id = user.id.to_string();
        if service
            .register_emby_direct_playback_ticket(
                &user_id,
                &play_session_id,
                authorization_context.0.as_deref(),
                authorization_context.1.as_deref(),
                direct_ticket_expires_at,
                current_unix_timestamp(),
            )
            .await
            .is_err()
        {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    }
    Json(json!({
        "PlaySessionId": play_session_id,
        // Emby clients use this item-level duration as the canonical media
        // duration alongside the VOD HLS manifest and playback callbacks.
        "RunTimeTicks": runtime_ticks,
        "MediaSources": media_sources,
    }))
    .into_response()
}

#[derive(Debug, Default, Deserialize)]
struct EmbyPlaybackInfoRequest {
    #[serde(
        rename = "MediaSourceId",
        alias = "mediaSourceId",
        alias = "media_source_id"
    )]
    media_source_id: Option<String>,
    #[serde(
        rename = "MaxStreamingBitrate",
        alias = "maxStreamingBitrate",
        alias = "max_streaming_bitrate"
    )]
    max_streaming_bitrate: Option<i64>,
    #[serde(rename = "EnableDirectPlay", alias = "enableDirectPlay")]
    enable_direct_play: Option<bool>,
    #[serde(rename = "EnableDirectStream", alias = "enableDirectStream")]
    enable_direct_stream: Option<bool>,
    #[serde(rename = "EnableTranscoding", alias = "enableTranscoding")]
    enable_transcoding: Option<bool>,
    #[serde(rename = "AllowVideoStreamCopy", alias = "allowVideoStreamCopy")]
    allow_video_stream_copy: Option<bool>,
    #[serde(rename = "AllowAudioStreamCopy", alias = "allowAudioStreamCopy")]
    allow_audio_stream_copy: Option<bool>,
    #[serde(rename = "AudioStreamIndex", alias = "audioStreamIndex")]
    audio_stream_index: Option<i64>,
    #[serde(rename = "SubtitleStreamIndex", alias = "subtitleStreamIndex")]
    subtitle_stream_index: Option<i64>,
    #[serde(
        default,
        deserialize_with = "deserialize_emby_optional_i64",
        rename = "MaxAudioChannels",
        alias = "maxAudioChannels"
    )]
    max_audio_channels: Option<i64>,
    #[serde(rename = "StartTimeTicks", alias = "startTimeTicks")]
    start_time_ticks: Option<i64>,
    #[serde(rename = "SegmentContainer", alias = "segmentContainer")]
    segment_container: Option<String>,
    #[serde(rename = "TranscodingContainer", alias = "transcodingContainer")]
    transcoding_container: Option<String>,
    #[serde(rename = "DeviceProfile", alias = "deviceProfile")]
    device_profile: Option<EmbyDeviceProfile>,
    #[serde(skip)]
    request_container_hints: Vec<String>,
}

impl EmbyPlaybackInfoRequest {
    fn apply_query_parameters(&mut self, raw_query: &RawQuery) {
        let Some(raw_query) = raw_query.0.as_deref() else {
            return;
        };
        for (name, value) in url::form_urlencoded::parse(raw_query.as_bytes()) {
            if name.eq_ignore_ascii_case("MaxStreamingBitrate")
                || name.eq_ignore_ascii_case("max_streaming_bitrate")
            {
                if let Ok(value) = value.parse::<i64>() {
                    self.max_streaming_bitrate = Some(value);
                }
                continue;
            }
            if name.eq_ignore_ascii_case("StartTimeTicks")
                || name.eq_ignore_ascii_case("start_time_ticks")
            {
                if let Ok(value) = value.parse::<i64>() {
                    self.start_time_ticks = Some(value);
                }
                continue;
            }
            if name.eq_ignore_ascii_case("SegmentContainer")
                || name.eq_ignore_ascii_case("segment_container")
                || name.eq_ignore_ascii_case("TranscodingContainer")
                || name.eq_ignore_ascii_case("transcoding_container")
            {
                self.request_container_hints.push(value.into_owned());
                continue;
            }
            let value = match parse_emby_bool(value.as_ref()) {
                Some(value) => value,
                None => continue,
            };
            if name.eq_ignore_ascii_case("EnableDirectPlay")
                || name.eq_ignore_ascii_case("enable_direct_play")
            {
                self.enable_direct_play = Some(value);
            } else if name.eq_ignore_ascii_case("EnableDirectStream")
                || name.eq_ignore_ascii_case("enable_direct_stream")
            {
                self.enable_direct_stream = Some(value);
            } else if name.eq_ignore_ascii_case("EnableTranscoding")
                || name.eq_ignore_ascii_case("enable_transcoding")
            {
                self.enable_transcoding = Some(value);
            } else if name.eq_ignore_ascii_case("AllowVideoStreamCopy")
                || name.eq_ignore_ascii_case("allow_video_stream_copy")
            {
                self.allow_video_stream_copy = Some(value);
            } else if name.eq_ignore_ascii_case("AllowAudioStreamCopy")
                || name.eq_ignore_ascii_case("allow_audio_stream_copy")
            {
                self.allow_audio_stream_copy = Some(value);
            }
        }
    }

    fn requests_server_transcoding(&self, force_transcode: bool) -> bool {
        force_transcode
            || (self.enable_transcoding == Some(true) && self.enable_direct_play != Some(true))
    }

    fn requests_server_transcoding_for_source(
        &self,
        force_transcode: bool,
        source: &crate::application::catalog::CatalogSource,
    ) -> bool {
        // URL STRM sources are handed to the external playback proxy. Lux's
        // HLS pipeline is intentionally local-file-only, so a client profile
        // mismatch must not turn this source into an impossible transcode
        // request.
        if source.source_kind.eq_ignore_ascii_case("STRM_URL") {
            return false;
        }
        if self.device_profile.is_none() {
            return self.requests_server_transcoding(force_transcode);
        }
        if force_transcode {
            return true;
        }
        if self.enable_transcoding == Some(true) {
            if self.enable_direct_play != Some(true) {
                return true;
            }
            return self.device_profile.as_ref().is_some_and(|profile| {
                profile.supports_lux_hls()
                    && self.source_requires_server_transcoding(profile, source)
            });
        }
        if self.force_explicitly_selects_a_plan() {
            return false;
        }
        self.device_profile.as_ref().is_some_and(|profile| {
            profile.supports_lux_hls() && self.source_requires_server_transcoding(profile, source)
        })
    }

    fn source_requires_server_transcoding(
        &self,
        profile: &EmbyDeviceProfile,
        source: &crate::application::catalog::CatalogSource,
    ) -> bool {
        profile.direct_play_compatibility(source) == EmbyProfileCompatibility::Incompatible
            || self.source_exceeds_streaming_bitrate(source)
    }

    fn effective_max_streaming_bitrate(&self) -> Option<i64> {
        self.max_streaming_bitrate.or_else(|| {
            self.device_profile
                .as_ref()
                .and_then(|profile| profile.max_streaming_bitrate)
        })
    }

    fn source_exceeds_streaming_bitrate(
        &self,
        source: &crate::application::catalog::CatalogSource,
    ) -> bool {
        let Some(limit) = self
            .effective_max_streaming_bitrate()
            .filter(|limit| *limit > 0)
        else {
            return false;
        };
        source
            .bitrate
            .is_some_and(|bitrate| bitrate > 0 && bitrate > limit)
    }

    fn force_explicitly_selects_a_plan(&self) -> bool {
        self.enable_direct_play.is_some() || self.enable_transcoding.is_some()
    }

    fn playback_capabilities_for_source(
        &self,
        source: Option<&crate::application::catalog::CatalogSource>,
    ) -> PlaybackCapabilities {
        // Emby treats omitted playback switches as enabled for POST
        // PlaybackInfo requests. This matters for clients that send only a
        // DeviceProfile in the body.
        let direct_stream = self.enable_direct_stream.unwrap_or(true);
        let allow_video_stream_copy = self.allow_video_stream_copy.unwrap_or(true);
        let allow_audio_stream_copy = self.allow_audio_stream_copy.unwrap_or(true);
        let (video_copy_allowed, audio_copy_allowed) = self
            .device_profile
            .as_ref()
            .zip(source)
            .map(|(profile, source)| {
                (
                    profile.supports_hls_video_copy(source),
                    profile.supports_hls_audio_copy(source),
                )
            })
            .unwrap_or((true, true));
        let video_bitrate_exceeds_limit =
            source.is_some_and(|source| self.source_exceeds_streaming_bitrate(source));
        PlaybackCapabilities {
            direct_play: false,
            hls: true,
            video_copy_to_fmp4: direct_stream
                && allow_video_stream_copy
                && video_copy_allowed
                && !video_bitrate_exceeds_limit,
            audio_copy_to_fmp4: direct_stream && allow_audio_stream_copy && audio_copy_allowed,
            hardware_transcode: true,
            software_transcode: true,
        }
    }

    fn selected_hls_container(
        &self,
        _source: &crate::application::catalog::CatalogSource,
    ) -> Result<HlsSegmentContainer, EmbyHlsContainerError> {
        let requested = self.requested_hls_container()?;
        let Some(device_profile) = self.device_profile.as_ref() else {
            return Ok(requested.unwrap_or(HlsSegmentContainer::MpegTs));
        };
        let profiles = device_profile
            .transcoding_profiles
            .iter()
            .filter(|profile| {
                is_video_profile(profile.profile_type.as_deref())
                    && is_hls_profile(profile)
                    && profile.supports_hls_segment_container()
            })
            .collect::<Vec<_>>();
        if let Some(requested) = requested {
            if profiles
                .iter()
                .any(|profile| profile.supports_hls_segment_container_value(requested))
            {
                return Ok(requested);
            }
            if profiles.is_empty() {
                return Ok(requested);
            }
            return Err(EmbyHlsContainerError::Unsupported);
        }
        Ok(profiles
            .first()
            .and_then(|profile| profile.first_hls_segment_container())
            .unwrap_or(HlsSegmentContainer::MpegTs))
    }

    fn requested_hls_container(
        &self,
    ) -> Result<Option<HlsSegmentContainer>, EmbyHlsContainerError> {
        let mut values = self
            .segment_container
            .iter()
            .chain(self.transcoding_container.iter())
            .chain(self.request_container_hints.iter());
        let mut selected = None;
        for value in values.by_ref() {
            let container =
                normalize_hls_segment_container(value).ok_or(EmbyHlsContainerError::Invalid)?;
            if selected.is_some_and(|existing| existing != container) {
                return Err(EmbyHlsContainerError::Conflict);
            }
            selected = Some(container);
        }
        Ok(selected)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EmbyHlsContainerError {
    Invalid,
    Conflict,
    Unsupported,
}

#[derive(Debug, Default, Deserialize)]
struct EmbyDeviceProfile {
    #[serde(
        rename = "MaxStreamingBitrate",
        alias = "maxStreamingBitrate",
        alias = "max_streaming_bitrate"
    )]
    max_streaming_bitrate: Option<i64>,
    #[serde(default, rename = "DirectPlayProfiles", alias = "directPlayProfiles")]
    direct_play_profiles: Vec<EmbyPlaybackProfile>,
    #[serde(default, rename = "TranscodingProfiles", alias = "transcodingProfiles")]
    transcoding_profiles: Vec<EmbyPlaybackProfile>,
}

#[derive(Debug, Default, Deserialize)]
struct EmbyPlaybackProfile {
    #[serde(rename = "Container", alias = "container")]
    container: Option<String>,
    #[serde(rename = "VideoCodec", alias = "videoCodec")]
    video_codec: Option<String>,
    #[serde(rename = "AudioCodec", alias = "audioCodec")]
    audio_codec: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_emby_optional_i64",
        rename = "MaxAudioChannels",
        alias = "maxAudioChannels"
    )]
    max_audio_channels: Option<i64>,
    #[serde(rename = "Protocol", alias = "protocol")]
    protocol: Option<String>,
    #[serde(rename = "Type", alias = "type")]
    profile_type: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EmbyProfileCompatibility {
    Compatible,
    Incompatible,
    Unknown,
}

impl EmbyDeviceProfile {
    fn supports_lux_hls(&self) -> bool {
        self.transcoding_profiles.iter().any(|profile| {
            is_video_profile(profile.profile_type.as_deref())
                && is_hls_profile(profile)
                && profile.supports_hls_segment_container()
        })
    }

    fn direct_play_compatibility(
        &self,
        source: &crate::application::catalog::CatalogSource,
    ) -> EmbyProfileCompatibility {
        let video_codec = source
            .streams
            .iter()
            .find(|stream| stream.stream_type.eq_ignore_ascii_case("VIDEO"))
            .and_then(|stream| stream.codec.as_deref());
        let audio_codec = source
            .streams
            .iter()
            .find(|stream| stream.stream_type.eq_ignore_ascii_case("AUDIO"))
            .and_then(|stream| stream.codec.as_deref());
        let mut has_unknown_profile = false;
        for profile in &self.direct_play_profiles {
            if !is_video_profile(profile.profile_type.as_deref()) {
                continue;
            }
            match profile.direct_play_compatibility(
                source.container.as_deref(),
                video_codec,
                audio_codec,
            ) {
                EmbyProfileCompatibility::Compatible => {
                    return EmbyProfileCompatibility::Compatible;
                }
                EmbyProfileCompatibility::Unknown => has_unknown_profile = true,
                EmbyProfileCompatibility::Incompatible => {}
            }
        }
        if has_unknown_profile {
            EmbyProfileCompatibility::Unknown
        } else {
            EmbyProfileCompatibility::Incompatible
        }
    }

    fn supports_hls_video_copy(&self, source: &crate::application::catalog::CatalogSource) -> bool {
        let video_codec = source
            .streams
            .iter()
            .find(|stream| stream.stream_type.eq_ignore_ascii_case("VIDEO"))
            .and_then(|stream| stream.codec.as_deref());
        self.transcoding_profiles.iter().any(|profile| {
            is_video_profile(profile.profile_type.as_deref())
                && is_hls_profile(profile)
                && codec_value_compatibility(profile.video_codec.as_deref(), video_codec)
                    == EmbyProfileCompatibility::Compatible
        })
    }

    fn supports_hls_audio_copy(&self, source: &crate::application::catalog::CatalogSource) -> bool {
        let audio_codec = source
            .streams
            .iter()
            .find(|stream| stream.stream_type.eq_ignore_ascii_case("AUDIO"))
            .and_then(|stream| stream.codec.as_deref());
        self.transcoding_profiles.iter().any(|profile| {
            is_video_profile(profile.profile_type.as_deref())
                && is_hls_profile(profile)
                && codec_value_compatibility(profile.audio_codec.as_deref(), audio_codec)
                    == EmbyProfileCompatibility::Compatible
        })
    }
}

impl EmbyPlaybackProfile {
    fn supports_hls_segment_container(&self) -> bool {
        self.container.as_deref().is_none_or(|value| {
            value
                .split(',')
                .any(|value| normalize_hls_segment_container(value).is_some())
        })
    }

    fn supports_hls_segment_container_value(&self, container: HlsSegmentContainer) -> bool {
        self.container.as_deref().is_none_or(|value| {
            value
                .split(',')
                .any(|value| normalize_hls_segment_container(value) == Some(container))
        })
    }

    fn first_hls_segment_container(&self) -> Option<HlsSegmentContainer> {
        self.container
            .as_deref()
            .and_then(|value| value.split(',').find_map(normalize_hls_segment_container))
    }

    fn direct_play_compatibility(
        &self,
        container: Option<&str>,
        video_codec: Option<&str>,
        audio_codec: Option<&str>,
    ) -> EmbyProfileCompatibility {
        let constraints = [
            profile_value_compatibility(
                self.container.as_deref(),
                container,
                emby_container_values_match,
            ),
            codec_value_compatibility(self.video_codec.as_deref(), video_codec),
            codec_value_compatibility(self.audio_codec.as_deref(), audio_codec),
        ];
        if constraints.contains(&EmbyProfileCompatibility::Incompatible) {
            EmbyProfileCompatibility::Incompatible
        } else if constraints.contains(&EmbyProfileCompatibility::Unknown) {
            EmbyProfileCompatibility::Unknown
        } else {
            EmbyProfileCompatibility::Compatible
        }
    }
}

fn normalize_hls_segment_container(value: &str) -> Option<HlsSegmentContainer> {
    match value.trim().to_ascii_lowercase().as_str() {
        "ts" | "mpegts" | "m2ts" => Some(HlsSegmentContainer::MpegTs),
        "mp4" | "fmp4" => Some(HlsSegmentContainer::FragmentedMp4),
        _ => None,
    }
}

fn is_hls_profile(profile: &EmbyPlaybackProfile) -> bool {
    profile
        .protocol
        .as_deref()
        .is_some_and(|protocol| protocol.eq_ignore_ascii_case("hls"))
}

fn is_video_profile(profile_type: Option<&str>) -> bool {
    profile_type.is_none_or(|value| {
        let value = value.trim();
        value.is_empty() || value.eq_ignore_ascii_case("video")
    })
}

fn profile_value_compatibility(
    profile_value: Option<&str>,
    actual_value: Option<&str>,
    values_match: impl Fn(&str, &str) -> bool,
) -> EmbyProfileCompatibility {
    let Some(profile_value) = profile_value
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return EmbyProfileCompatibility::Compatible;
    };
    let Some(actual_value) = actual_value
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return EmbyProfileCompatibility::Unknown;
    };
    if profile_value
        .split(',')
        .any(|candidate| values_match(candidate.trim(), actual_value))
    {
        EmbyProfileCompatibility::Compatible
    } else {
        EmbyProfileCompatibility::Incompatible
    }
}

fn codec_value_compatibility(
    profile_value: Option<&str>,
    actual_value: Option<&str>,
) -> EmbyProfileCompatibility {
    profile_value_compatibility(profile_value, actual_value, |candidate, actual| {
        normalize_emby_codec(candidate) == normalize_emby_codec(actual)
    })
}

fn values_match(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

fn emby_container_values_match(left: &str, right: &str) -> bool {
    let left_is_matroska = ["mkv", "matroska", "matroska,webm"]
        .iter()
        .any(|value| left.trim().eq_ignore_ascii_case(value));
    let right_is_matroska = ["mkv", "matroska", "matroska,webm"]
        .iter()
        .any(|value| right.trim().eq_ignore_ascii_case(value));
    (left_is_matroska && right_is_matroska) || values_match(left.trim(), right.trim())
}

fn normalize_emby_codec(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "avc" => "h264".to_owned(),
        "h265" => "hevc".to_owned(),
        "mp4a" => "aac".to_owned(),
        value => value.to_owned(),
    }
}

fn parse_emby_playback_info_request(body: &Bytes) -> Result<EmbyPlaybackInfoRequest, StatusCode> {
    if body.is_empty() || body.iter().all(u8::is_ascii_whitespace) {
        return Ok(EmbyPlaybackInfoRequest::default());
    }
    serde_json::from_slice(body).map_err(|_| StatusCode::BAD_REQUEST)
}

fn deserialize_emby_optional_i64<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    match value {
        None => Ok(None),
        Some(serde_json::Value::Number(value)) => value
            .as_i64()
            .map(Some)
            .ok_or_else(|| serde::de::Error::custom("expected a signed integer")),
        Some(serde_json::Value::String(value)) => value
            .trim()
            .parse::<i64>()
            .map(Some)
            .map_err(|_| serde::de::Error::custom("expected an integer or integer string")),
        Some(_) => Err(serde::de::Error::custom(
            "expected an integer or integer string",
        )),
    }
}

fn parse_emby_bool(value: &str) -> Option<bool> {
    match value.trim() {
        "1" => Some(true),
        "0" => Some(false),
        value if value.eq_ignore_ascii_case("true") => Some(true),
        value if value.eq_ignore_ascii_case("false") => Some(false),
        _ => None,
    }
}

fn emby_force_transcode_from_raw(raw_query: &RawQuery) -> bool {
    raw_query.0.as_deref().is_some_and(|raw_query| {
        url::form_urlencoded::parse(raw_query.as_bytes()).any(|(name, value)| {
            name.eq_ignore_ascii_case("forceTranscode")
                && parse_emby_bool(value.as_ref()) == Some(true)
        })
    })
}

struct EmbyTranscodingSessionOptions<'a> {
    request: &'a EmbyPlaybackInfoRequest,
    authorization_device_id: Option<&'a str>,
    authorization_client_name: Option<&'a str>,
    hls: HlsStartOptions,
}

async fn create_emby_transcoding_session(
    state: &AppState,
    user: &UserRecord,
    item_id: &str,
    source: &crate::application::catalog::CatalogSource,
    options: EmbyTranscodingSessionOptions<'_>,
) -> Result<Option<CreatedWebPlaybackSession>, StatusCode> {
    if source.source_kind != "LOCAL_FILE" {
        return Ok(None);
    }
    let Some(service) = state.web_playback.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let Some(access) = state.access.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let stored_source = match access
        .authorized_playback_source(principal, item_id, Some(&source.id))
        .await
    {
        Ok(Some(source)) => source,
        Ok(None) => return Err(StatusCode::NOT_FOUND),
        Err(_) => return Err(StatusCode::SERVICE_UNAVAILABLE),
    };
    let user_id = user.id.to_string();
    let input =
        match canonical_local_media_path(&stored_source.root_path, &stored_source.relative_path)
            .await
        {
            Ok(path) => path,
            Err(LocalPathError::Missing) => return Err(StatusCode::NOT_FOUND),
            Err(LocalPathError::Forbidden) => return Err(StatusCode::FORBIDDEN),
        };
    let video_bitrate = emby_transcoding_video_bitrate(source, options.request);
    let created = service
        .create_and_start_emby_hls(
            CreateWebPlaybackSession {
                user_id: &user_id,
                is_admin: user.is_admin,
                item_id,
                media_source_id: &source.id,
                play_session_prefix: "lux-emby",
                source_kind: PlaybackSourceKind::LocalFile,
                capabilities: options
                    .request
                    .playback_capabilities_for_source(Some(source)),
            },
            &input,
            video_bitrate,
            options.hls,
        )
        .await
        .map_err(emby_playback_session_error_status)?;
    if !matches!(created.plan, WebPlaybackPlan::ServerHls { .. }) {
        return Err(StatusCode::BAD_GATEWAY);
    }
    if let Err(error) = service
        .set_authorization_context(
            &created.id,
            &user_id,
            options.authorization_device_id,
            options.authorization_client_name,
        )
        .await
    {
        let _ = service.stop(&created.id, &user_id).await;
        return Err(emby_playback_session_error_status(error));
    }
    if let WebPlaybackPlan::ServerHls { tier } = &created.plan {
        let output = emby_transcoding_output(
            source,
            options.request,
            *tier,
            options.hls.segment_container,
        );
        if service
            .set_transcoding_details(&created.id, &user_id, &output)
            .await
            .is_err()
        {
            let _ = service.stop(&created.id, &user_id).await;
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
    }
    Ok(Some(created))
}

async fn emby_playback_authorization_context(
    headers: &HeaderMap,
    query_api_key: Option<&str>,
    query_client: Option<&str>,
    state: &AppState,
    request_device_id: &str,
) -> Result<(Option<String>, Option<String>), StatusCode> {
    let registered_device = if let Some(token) =
        emby_token_from_headers(headers).or_else(|| query_api_key.map(str::to_owned))
    {
        let Some(auth) = state.emby_auth.as_ref() else {
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        };
        auth.device_info(&token)
            .await
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
    } else {
        None
    };
    let declared_device = emby_device_info_from_headers(headers);
    let device_id = registered_device
        .as_ref()
        .map(|device| device.device_id.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| non_empty_device_id(request_device_id))
        .or_else(|| non_empty_device_id(&declared_device.device_id));
    let client_name = registered_device
        .as_ref()
        .map(|device| device.client.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| non_empty_profile_value(Some(&declared_device.client)).map(str::to_owned))
        .or_else(|| {
            query_client
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
        });
    Ok((device_id, client_name))
}

async fn emby_transcoding_start_time_ticks(
    state: &AppState,
    user_id: &str,
    item_id: &str,
    requested_start_time_ticks: Option<i64>,
    runtime_ticks: Option<i64>,
) -> Result<Option<i64>, StatusCode> {
    // An explicit valid client value, including zero, is authoritative. Some
    // clients omit StartTimeTicks even when they are resuming an item, so
    // recover the saved Emby/Lux position only when the field is absent.
    if let Some(start_time_ticks) = requested_start_time_ticks {
        return Ok(emby_start_time_hint(start_time_ticks, runtime_ticks));
    }
    let Some(database) = state.database.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let user_state = database
        .find_user_item_state(user_id, item_id)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let Some(user_state) = user_state else {
        return Ok(None);
    };
    if user_state.is_played {
        return Ok(None);
    }
    Ok(emby_start_time_hint(
        user_state.position_ticks,
        runtime_ticks,
    ))
}

fn emby_start_time_hint(start_time_ticks: i64, runtime_ticks: Option<i64>) -> Option<i64> {
    (start_time_ticks > 0
        && !runtime_ticks.is_some_and(|runtime| runtime > 0 && start_time_ticks >= runtime))
    .then_some(start_time_ticks)
}

fn emby_playback_session_error_status(error: WebPlaybackSessionError) -> StatusCode {
    match error {
        WebPlaybackSessionError::Invalid(_) => StatusCode::BAD_REQUEST,
        WebPlaybackSessionError::PlaybackPolicyDenied => StatusCode::FORBIDDEN,
        WebPlaybackSessionError::NotFound => StatusCode::NOT_FOUND,
        WebPlaybackSessionError::Expired | WebPlaybackSessionError::NotActive => StatusCode::GONE,
        WebPlaybackSessionError::AccessRulesUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        WebPlaybackSessionError::Hls(_) => StatusCode::BAD_GATEWAY,
        WebPlaybackSessionError::Storage(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

async fn standard_emby_playback_api_key(
    headers: &HeaderMap,
    query_api_key: Option<&str>,
    state: &AppState,
) -> Option<String> {
    // A normalized Emby credential is authoritative even when an unused Lux
    // key header is present. Without normalization, preserve the legacy guard.
    let token = if headers.contains_key("X-Lux-Api-Key")
        && !headers.contains_key(super::emby::NORMALIZED_CREDENTIAL_HEADER)
    {
        None
    } else {
        emby_token_from_headers(headers).or_else(|| {
            query_api_key
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        })
    }?;

    // The shared management key is also accepted on Emby-compatible routes
    // for administration. Never copy it into a playback URL, even when it
    // was supplied as the query api_key.
    if let Some(service) = state.admin_api_key.as_ref() {
        match service.resolve_principal(&token).await {
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) => {}
        }
    }
    Some(token)
}

#[derive(Deserialize, Default)]
pub(super) struct PlaybackEventRequest {
    #[serde(rename = "ItemId", alias = "itemId", alias = "mediaServerItemId")]
    item_id: String,
    #[serde(
        rename = "MediaSourceId",
        alias = "mediaSourceId",
        alias = "mediaServerMediaSourceId"
    )]
    media_source_id: Option<String>,
    #[serde(
        rename = "PlaySessionId",
        alias = "playSessionId",
        alias = "mediaServerPlaySessionId"
    )]
    play_session_id: Option<String>,
    #[serde(
        rename = "PositionTicks",
        alias = "positionTicks",
        alias = "PlaybackPositionTicks",
        alias = "playbackPositionTicks",
        default
    )]
    position_ticks: i64,
    #[serde(rename = "RunTimeTicks", alias = "runTimeTicks")]
    duration_ticks: Option<i64>,
    #[serde(rename = "IsPaused", alias = "isPaused", default)]
    is_paused: bool,
    #[serde(rename = "DeviceId", alias = "deviceId")]
    device_id: Option<String>,
    #[serde(rename = "Client", alias = "client")]
    client: Option<String>,
    #[serde(rename = "DeviceName", alias = "deviceName", alias = "Device")]
    device_name: Option<String>,
    #[serde(
        rename = "ApplicationVersion",
        alias = "applicationVersion",
        alias = "ClientVersion",
        alias = "clientVersion",
        alias = "Version"
    )]
    client_version: Option<String>,
    #[serde(rename = "DeviceType", alias = "deviceType")]
    device_type: Option<String>,
    #[serde(rename = "PlayMethod", alias = "playMethod")]
    play_method: Option<String>,
}

fn parse_emby_playback_event(body: &Bytes) -> Result<PlaybackEventRequest, StatusCode> {
    serde_json::from_slice(body).map_err(|_| StatusCode::BAD_REQUEST)
}

pub(super) async fn emby_playing(
    headers: HeaderMap,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
    body: Bytes,
) -> Response {
    let request = match parse_emby_playback_event(&body) {
        Ok(request) => request,
        Err(status) => return status.into_response(),
    };
    handle_emby_playback_event(headers, query, state, request, "PLAYING").await
}

pub(super) async fn emby_playing_progress(
    headers: HeaderMap,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
    body: Bytes,
) -> Response {
    let request = match parse_emby_playback_event(&body) {
        Ok(request) => request,
        Err(status) => return status.into_response(),
    };
    let state_name = if request.is_paused {
        "PAUSED"
    } else {
        "PLAYING"
    };
    handle_emby_playback_event(headers, query, state, request, state_name).await
}

pub(super) async fn emby_playing_stopped(
    headers: HeaderMap,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
    body: Bytes,
) -> Response {
    let request = match parse_emby_playback_event(&body) {
        Ok(request) => request,
        Err(status) => return status.into_response(),
    };
    handle_emby_playback_event(headers, query, state, request, "STOPPED").await
}

pub(super) async fn handle_emby_playback_event(
    headers: HeaderMap,
    query: EmbyTokenQuery,
    state: AppState,
    request: PlaybackEventRequest,
    state_name: &'static str,
) -> Response {
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => {
            tracing::warn!(
                event = "emby_playback_callback_rejected",
                stage = "authentication",
                status_code = status.as_u16(),
                playback_state = state_name,
                "rejected emby playback callback"
            );
            return status.into_response();
        }
    };
    if request.position_ticks < 0
        || request.duration_ticks.is_some_and(|duration| duration < 0)
        || request.item_id.is_empty()
    {
        tracing::warn!(
            event = "emby_playback_callback_rejected",
            stage = "validation",
            status_code = StatusCode::BAD_REQUEST.as_u16(),
            playback_state = state_name,
            item_id_present = !request.item_id.is_empty(),
            position_ticks = request.position_ticks,
            duration_ticks_present = request.duration_ticks.is_some(),
            "rejected invalid emby playback callback"
        );
        return StatusCode::BAD_REQUEST.into_response();
    }
    let item_id_prefix = playback_identifier_prefix(&request.item_id);
    let Some(database) = state.database.as_ref() else {
        tracing::error!(
            event = "emby_playback_callback_rejected",
            stage = "database",
            status_code = StatusCode::SERVICE_UNAVAILABLE.as_u16(),
            playback_state = state_name,
            item_id_prefix = %item_id_prefix,
            "playback database is unavailable"
        );
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let mut internal_item_id = emby_internal_id(&request.item_id);
    let requested_media_source_id = request
        .media_source_id
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let mut media_source_id = requested_media_source_id.clone();
    let requested_play_session_id = request
        .play_session_id
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let requested_previous_session =
        if let Some(play_session_id) = requested_play_session_id.as_deref() {
            match database
                .find_playback_session(&user.id.to_string(), play_session_id)
                .await
            {
                Ok(session) => session,
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            }
        } else {
            None
        };
    let is_global_collection_work = match database.find_item_library_id(&internal_item_id).await {
        Ok(Some(library_id)) if library_id == crate::storage::GLOBAL_COLLECTION_LIBRARY_ID => {
            let Some(catalog) = state.catalog.as_ref() else {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            };
            let logical_item = match catalog.find_item(principal, &internal_item_id).await {
                Ok(Some(item)) if item.item_type == "MOVIE" => item,
                Ok(Some(_)) => return StatusCode::NOT_FOUND.into_response(),
                Ok(None) => return StatusCode::NOT_FOUND.into_response(),
                Err(CatalogError::Storage(_)) => {
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                }
                Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => {
                    return StatusCode::NOT_FOUND.into_response();
                }
            };
            let playback_sources = match catalog
                .global_collection_playback_sources(principal, &logical_item.id)
                .await
            {
                Ok(Some(sources)) => sources,
                Ok(None) => return StatusCode::NOT_FOUND.into_response(),
                Err(CatalogError::Storage(_)) => {
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                }
                Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => {
                    return StatusCode::NOT_FOUND.into_response();
                }
            };
            let matching_copy = if let Some(source_id) = requested_media_source_id.as_deref() {
                let mut matches = playback_sources.copies.iter().filter(|copy| {
                    copy.media_sources
                        .iter()
                        .any(|source| source.id == source_id)
                });
                let Some(copy) = matches.next() else {
                    return StatusCode::NOT_FOUND.into_response();
                };
                if matches.next().is_some() {
                    return StatusCode::NOT_FOUND.into_response();
                }
                copy
            } else if let Some(previous_session) = requested_previous_session.as_ref() {
                let Some(copy) = playback_sources
                    .copies
                    .iter()
                    .find(|copy| copy.id == previous_session.item_id)
                else {
                    return StatusCode::NOT_FOUND.into_response();
                };
                if previous_session
                    .media_source_id
                    .as_ref()
                    .is_some_and(|source_id| {
                        !copy
                            .media_sources
                            .iter()
                            .any(|source| source.id == *source_id)
                    })
                {
                    return StatusCode::NOT_FOUND.into_response();
                }
                media_source_id = previous_session.media_source_id.clone();
                copy
            } else if playback_sources.copies.len() == 1 {
                &playback_sources.copies[0]
            } else {
                return StatusCode::BAD_REQUEST.into_response();
            };
            internal_item_id = matching_copy.id.clone();
            true
        }
        Ok(_) => false,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let Some(access) = state.access.as_ref() else {
        tracing::error!(
            event = "emby_playback_callback_rejected",
            stage = "access_service",
            status_code = StatusCode::SERVICE_UNAVAILABLE.as_u16(),
            playback_state = state_name,
            item_id_prefix = %item_id_prefix,
            "playback access service is unavailable"
        );
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match access.can_view_item(principal, &internal_item_id).await {
        Ok(true) => {}
        Ok(false) => {
            tracing::warn!(
                event = "emby_playback_callback_rejected",
                stage = "item_access",
                status_code = StatusCode::NOT_FOUND.as_u16(),
                playback_state = state_name,
                item_id_prefix = %item_id_prefix,
                "playback item is not accessible"
            );
            return StatusCode::NOT_FOUND.into_response();
        }
        Err(error) => {
            tracing::error!(
                event = "emby_playback_callback_rejected",
                stage = "item_access",
                status_code = StatusCode::SERVICE_UNAVAILABLE.as_u16(),
                playback_state = state_name,
                item_id_prefix = %item_id_prefix,
                error = %error,
                "failed to check playback item access"
            );
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
    }
    let mut header_device = emby_device_info_from_headers(&headers);
    if header_device.client.is_empty()
        || header_device.device.is_empty()
        || header_device.device_id.is_empty()
        || header_device.version.is_empty()
    {
        let token = emby_token_from_headers(&headers).or_else(|| query.api_key.clone());
        if let (Some(auth), Some(token)) = (state.emby_auth.as_ref(), token) {
            match auth.device_info(&token).await {
                Ok(Some(device)) => merge_emby_device_info(&mut header_device, device),
                Ok(None) => {}
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            }
        }
    }
    let device_id = request
        .device_id
        .filter(|value| !value.is_empty())
        .or_else(|| (!header_device.device_id.is_empty()).then_some(header_device.device_id))
        .unwrap_or_else(|| "unknown".to_owned());
    let client = request
        .client
        .as_deref()
        .or_else(|| (!header_device.client.is_empty()).then_some(header_device.client.as_str()));
    let device_name = request
        .device_name
        .as_deref()
        .or_else(|| (!header_device.device.is_empty()).then_some(header_device.device.as_str()));
    let client_version = request
        .client_version
        .as_deref()
        .or_else(|| (!header_device.version.is_empty()).then_some(header_device.version.as_str()));
    let device_type = request
        .device_type
        .as_deref()
        .or_else(|| (!header_device.device.is_empty()).then_some(header_device.device.as_str()));
    let mut play_session_id = requested_play_session_id
        .clone()
        .unwrap_or_else(|| format!("{}:{device_id}", internal_item_id));
    if state_name == "STOPPED" && requested_play_session_id.is_none() {
        let stop_device_id = {
            let trimmed = device_id.trim();
            (!trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("unknown")).then_some(trimmed)
        };
        match database
            .find_active_playback_session_for_stop(
                &user.id.to_string(),
                &internal_item_id,
                media_source_id.as_deref(),
                stop_device_id,
            )
            .await
        {
            Ok(Some(session)) => play_session_id = session.play_session_id,
            Ok(None) => {}
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        }
    }
    let previous_session = if requested_previous_session
        .as_ref()
        .is_some_and(|session| session.play_session_id == play_session_id)
    {
        requested_previous_session
    } else {
        match database
            .find_playback_session(&user.id.to_string(), &play_session_id)
            .await
        {
            Ok(session) => session,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        }
    };
    if is_global_collection_work && let Some(session) = previous_session.as_ref() {
        let source_mismatch = media_source_id
            .as_deref()
            .zip(session.media_source_id.as_deref())
            .is_some_and(|(requested, stored)| requested != stored);
        let device_mismatch = !device_id.trim().is_empty()
            && !device_id.eq_ignore_ascii_case("unknown")
            && !session.device_id.trim().is_empty()
            && !session.device_id.eq_ignore_ascii_case("unknown")
            && session.device_id != device_id;
        if session.item_id != internal_item_id || source_mismatch || device_mismatch {
            return StatusCode::NOT_FOUND.into_response();
        }
        if media_source_id.is_none() {
            media_source_id = session.media_source_id.clone();
        }
    }
    if is_global_collection_work {
        match access
            .authorized_playback_source(principal, &internal_item_id, media_source_id.as_deref())
            .await
        {
            Ok(Some(_)) => {}
            Ok(None) => return StatusCode::NOT_FOUND.into_response(),
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        }
    } else if let Some(media_source_id) = media_source_id.as_deref() {
        match database
            .media_source_belongs_to_item(media_source_id, &internal_item_id)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                tracing::warn!(
                    event = "emby_playback_callback_rejected",
                    stage = "media_source",
                    status_code = StatusCode::NOT_FOUND.as_u16(),
                    playback_state = state_name,
                    item_id_prefix = %item_id_prefix,
                    "playback media source does not belong to item"
                );
                return StatusCode::NOT_FOUND.into_response();
            }
            Err(error) => {
                tracing::error!(
                    event = "emby_playback_callback_rejected",
                    stage = "media_source",
                    status_code = StatusCode::SERVICE_UNAVAILABLE.as_u16(),
                    playback_state = state_name,
                    item_id_prefix = %item_id_prefix,
                    error = %error,
                    "failed to check playback media source"
                );
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
        }
    }
    let playback_probe_source_id = if state_name == "PLAYING"
        && !request.is_paused
        && state.strm_probe.is_some()
    {
        match access
            .authorized_playback_source(principal, &internal_item_id, media_source_id.as_deref())
            .await
        {
            Ok(Some(source)) if source.source_kind == "STRM_URL" => Some(source.source_id),
            _ => None,
        }
    } else {
        None
    };
    let emby_transcode_session_id = emby_transcode_session_id_from_play_session(&play_session_id);
    let user_id = user.id.to_string();
    // Stop only the HLS session selected by the callback's play session ID or
    // by the active session matching its item, source, and device.
    if state_name == "STOPPED"
        && let Some(session_id) = emby_transcode_session_id
        && let Some(service) = state.web_playback.as_ref()
    {
        if let Err(error) = service.stop(session_id, &user_id).await {
            tracing::warn!(
                event = "emby_transcoding_session_stop_failed",
                item_id_prefix = %item_id_prefix,
                playback_state = state_name,
                error = %error,
                "failed to stop Emby transcoding session"
            );
        }
    }
    let played_percent = match database.user_played_percent(&user_id).await {
        Ok(value) => value,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    // A client playing a growing HLS playlist may report the currently
    // available playlist length here. Prefer Lux's known source/item runtime
    // so that callbacks cannot replace the canonical duration with that
    // partial value. Fall back to the client only when the catalog has no
    // usable runtime at all.
    let duration_ticks = emby_playback_event_duration_ticks(
        &state,
        principal,
        &internal_item_id,
        media_source_id.as_deref(),
    )
    .await
    .or_else(|| request.duration_ticks.filter(|duration| *duration > 0))
    .or_else(|| {
        previous_session
            .as_ref()
            .and_then(|session| session.duration_ticks)
            .filter(|duration| *duration > 0)
    });
    let activity_event = playback_activity_event_type(previous_session.as_ref(), state_name);
    let resumed = playback_resumed(previous_session.as_ref(), state_name);
    let occurred_at = current_unix_timestamp();
    let webhook_event = webhook_event_type_for_playback(
        activity_event,
        should_publish_playback_progress(
            previous_session.as_ref(),
            state_name,
            request.position_ticks,
            occurred_at,
        ),
    );
    let remote_ip = request_client_ip(&headers, &state).await;
    let activity_remote_ip = remote_ip.as_deref().or_else(|| {
        previous_session
            .as_ref()
            .and_then(|session| session.remote_ip.as_deref())
    });
    match database
        .record_playback_event(NewPlaybackEvent {
            user_id: &user_id,
            item_id: &internal_item_id,
            media_source_id: media_source_id.as_deref(),
            play_session_id: &play_session_id,
            device_id: &device_id,
            client,
            device_name,
            client_version,
            device_type,
            remote_ip: remote_ip.as_deref(),
            state: state_name,
            position_ticks: request.position_ticks,
            duration_ticks,
            played_percent,
            is_paused: request.is_paused || state_name == "PAUSED",
        })
        .await
    {
        Ok(()) => {
            if database
                .sync_played_container_states(&user_id, &internal_item_id)
                .await
                .is_err()
            {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            if let Some(service) = state.web_playback.as_ref() {
                let admission_update = if state_name == "STOPPED" {
                    service
                        .release_playback_session_admission(&user_id, &play_session_id)
                        .await
                        .map(|_| ())
                } else {
                    service
                        .renew_playback_session_admission(&user_id, &play_session_id)
                        .await
                        .map(|_| ())
                };
                if let Err(error) = admission_update {
                    return web_playback_error(&headers, error);
                }
            }
            state.user_events.publish_home_coalesced().await;
            if state_name != "STOPPED"
                && let Some(session_id) = emby_transcode_session_id
                && let Some(service) = state.web_playback.as_ref()
            {
                if let Err(error) = service.heartbeat(session_id, &user_id).await {
                    tracing::warn!(
                        event = "emby_transcoding_session_refresh_failed",
                        session_id = %session_id,
                        playback_state = state_name,
                        error = %error,
                        "failed to refresh Emby transcoding session"
                    );
                }
            }
            if let Some(event_type) = activity_event {
                record_activity_event(
                    Some(database),
                    &state.admin_events,
                    &user_id,
                    event_type,
                    Some(&internal_item_id),
                    json!({
                        "client": client,
                        "clientVersion": client_version,
                        "deviceName": device_name,
                        "deviceType": device_type,
                        "state": state_name,
                        "remoteIp": activity_remote_ip,
                    }),
                )
                .await;
            }
            if let Some(event_type) = webhook_event {
                publish_playback_webhook(
                    &state,
                    event_type,
                    occurred_at,
                    &internal_item_id,
                    media_source_id.as_deref(),
                    &play_session_id,
                    state_name,
                    request.position_ticks,
                    duration_ticks,
                    request.is_paused || state_name == "PAUSED",
                    client,
                    device_name,
                    device_type,
                    client_version,
                    &user.display_name,
                    AccessPrincipal::new(user.id, user.is_admin),
                    request.play_method.as_deref(),
                    if event_type_is_stopped(event_type) {
                        activity_remote_ip
                    } else {
                        None
                    },
                    resumed,
                )
                .await;
            }
            if let Some(source_id) = playback_probe_source_id
                && let Some(strm_probe) = state.strm_probe.as_ref()
            {
                strm_probe.schedule_playback_probe(&internal_item_id, &source_id);
            }
            tracing::info!(
                event = "emby_playback_callback_recorded",
                playback_state = state_name,
                item_id_prefix = %item_id_prefix,
                position_ticks = request.position_ticks,
                duration_ticks_present = duration_ticks.is_some(),
                is_paused = request.is_paused || state_name == "PAUSED",
                client = playback_client_label(client),
                "recorded emby playback callback"
            );
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => {
            tracing::error!(
                event = "emby_playback_callback_rejected",
                stage = "storage",
                status_code = StatusCode::SERVICE_UNAVAILABLE.as_u16(),
                playback_state = state_name,
                item_id_prefix = %item_id_prefix,
                error = %error,
                "failed to record emby playback callback"
            );
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

async fn emby_playback_event_duration_ticks(
    state: &AppState,
    principal: AccessPrincipal,
    item_id: &str,
    media_source_id: Option<&str>,
) -> Option<i64> {
    let catalog = state.catalog.as_ref()?;
    let item = catalog.find_item(principal, item_id).await.ok().flatten()?;
    item.runtime_ticks
        .filter(|ticks| *ticks > 0)
        .or_else(|| {
            media_source_id.and_then(|source_id| {
                item.media_sources
                    .iter()
                    .find(|source| source.id == source_id)
                    .and_then(|source| source.duration_ticks)
                    .filter(|ticks| *ticks > 0)
            })
        })
        .or_else(|| {
            item.media_sources
                .iter()
                .find(|source| source.is_default)
                .and_then(|source| source.duration_ticks)
                .filter(|ticks| *ticks > 0)
        })
        .or_else(|| {
            item.media_sources
                .iter()
                .find_map(|source| source.duration_ticks)
                .filter(|ticks| *ticks > 0)
        })
}

pub(super) fn playback_identifier_prefix(value: &str) -> String {
    value.chars().take(8).collect()
}

pub(super) fn playback_client_label(value: Option<&str>) -> &'static str {
    match value.map(str::trim) {
        Some(value) if value.eq_ignore_ascii_case("vidhub") => "vidhub",
        Some(value) if value.eq_ignore_ascii_case("senplayer") => "senplayer",
        Some(value) if value.eq_ignore_ascii_case("infuse") => "infuse",
        Some(_) => "other",
        None => "unknown",
    }
}

const PLAYBACK_WEBHOOK_PROGRESS_INTERVAL_SECONDS: i64 = 30;

pub(super) fn current_unix_timestamp() -> i64 {
    crate::clock::unix_timestamp()
}

pub(super) fn should_publish_playback_progress(
    previous: Option<&StoredPlaybackSession>,
    state_name: &str,
    position_ticks: i64,
    occurred_at: i64,
) -> bool {
    matches!(state_name, "PLAYING" | "PAUSED")
        && previous.is_some_and(|session| {
            occurred_at.saturating_sub(session.last_event_at)
                >= PLAYBACK_WEBHOOK_PROGRESS_INTERVAL_SECONDS
                && position_ticks > session.position_ticks
        })
}

pub(super) fn webhook_event_type_for_playback(
    activity_event: Option<&str>,
    progress_due: bool,
) -> Option<WebhookEventType> {
    match activity_event {
        Some("PLAYBACK_STARTED") => Some(WebhookEventType::PlaybackStarted),
        Some("PLAYBACK_PAUSED") => Some(WebhookEventType::PlaybackPaused),
        Some("PLAYBACK_STOPPED") => Some(WebhookEventType::PlaybackStopped),
        _ if progress_due => Some(WebhookEventType::PlaybackProgress),
        _ => None,
    }
}

pub(super) fn playback_resumed(previous: Option<&StoredPlaybackSession>, state_name: &str) -> bool {
    state_name == "PLAYING" && previous.is_some_and(|session| session.state == "PAUSED")
}

fn event_type_is_stopped(event_type: WebhookEventType) -> bool {
    event_type == WebhookEventType::PlaybackStopped
}

#[derive(Clone, Debug, Default)]
struct PlaybackNotificationContext {
    item_title: Option<String>,
    series_title: Option<String>,
    season_number: Option<i64>,
    series_season_count: Option<i64>,
    container: Option<String>,
    size: Option<i64>,
    bitrate: Option<i64>,
    overview: Option<String>,
    play_method: Option<String>,
}

async fn playback_notification_context(
    state: &AppState,
    principal: AccessPrincipal,
    item_id: &str,
    media_source_id: Option<&str>,
    requested_play_method: Option<&str>,
) -> PlaybackNotificationContext {
    let Some(catalog) = state.catalog.as_ref() else {
        return PlaybackNotificationContext {
            play_method: requested_play_method.map(normalize_play_method),
            ..PlaybackNotificationContext::default()
        };
    };
    let item = match catalog.find_item(principal, item_id).await {
        Ok(Some(item)) => item,
        Ok(None) | Err(_) => {
            return PlaybackNotificationContext {
                play_method: requested_play_method.map(normalize_play_method),
                ..PlaybackNotificationContext::default()
            };
        }
    };
    let source = media_source_id
        .and_then(|source_id| {
            item.media_sources
                .iter()
                .find(|source| source.id == source_id)
        })
        .or_else(|| item.media_sources.iter().find(|source| source.is_default))
        .or_else(|| item.media_sources.first());
    let play_method = requested_play_method
        .map(normalize_play_method)
        .or_else(|| source.map(|source| default_play_method(&source.source_kind).to_owned()));
    let series = if item.item_type == "EPISODE" {
        match item.series_id.as_deref() {
            Some(series_id) => catalog.find_item(principal, series_id).await.ok().flatten(),
            None => None,
        }
    } else {
        None
    };
    let series_title = (item.item_type == "EPISODE")
        .then(|| {
            series
                .as_ref()
                .map(|series| series.title.as_str())
                .or(item.series_name.as_deref())
        })
        .flatten()
        .and_then(|title| bounded_playback_text(Some(title)));
    PlaybackNotificationContext {
        item_title: bounded_playback_text(Some(&item.title)),
        series_title,
        season_number: item.season_number.filter(|number| *number >= 0),
        series_season_count: series
            .and_then(|series| series.season_count)
            .filter(|count| *count > 0),
        container: source.and_then(|source| bounded_playback_text(source.container.as_deref())),
        size: source.and_then(|source| source.size.filter(|value| *value >= 0)),
        bitrate: source.and_then(|source| source.bitrate.filter(|value| *value >= 0)),
        overview: bounded_playback_overview(item.overview.as_deref()),
        play_method,
    }
}

fn normalize_play_method(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "directplay" | "direct_play" => "DirectPlay".to_owned(),
        "directstream" | "direct_stream" => "DirectStream".to_owned(),
        "remux" => "Remux".to_owned(),
        "transcode" | "transcoding" => "Transcode".to_owned(),
        _ => bounded_playback_text(Some(value)).unwrap_or_else(|| "DirectPlay".to_owned()),
    }
}

fn default_play_method(source_kind: &str) -> &'static str {
    if source_kind.eq_ignore_ascii_case("STRM_URL") {
        "DirectStream"
    } else {
        "DirectPlay"
    }
}

fn bounded_playback_overview(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    (!value.is_empty() && value.chars().count() <= 512).then(|| value.to_owned())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn publish_playback_webhook(
    state: &AppState,
    event_type: WebhookEventType,
    occurred_at: i64,
    item_id: &str,
    media_source_id: Option<&str>,
    play_session_id: &str,
    state_name: &str,
    position_ticks: i64,
    duration_ticks: Option<i64>,
    is_paused: bool,
    client: Option<&str>,
    device_name: Option<&str>,
    device_type: Option<&str>,
    client_version: Option<&str>,
    user_name: &str,
    principal: AccessPrincipal,
    requested_play_method: Option<&str>,
    remote_ip: Option<&str>,
    resumed: bool,
) {
    let Some(webhooks) = state.webhooks.as_ref() else {
        return;
    };
    let dedupe_key = format!(
        "playback:{play_session_id}:{}:{occurred_at}:{position_ticks}",
        event_type.as_str(),
    );
    let context = playback_notification_context(
        state,
        principal,
        item_id,
        media_source_id,
        requested_play_method,
    )
    .await;
    let data = json!({
        "itemId": emby_public_id(item_id),
        "itemTitle": context.item_title,
        "seriesTitle": context.series_title,
        "seasonNumber": context.season_number,
        "seriesSeasonCount": context.series_season_count,
        "userName": bounded_playback_text(Some(user_name)),
        "mediaSourceId": media_source_id,
        "playSessionId": play_session_id,
        "state": state_name,
        "positionTicks": position_ticks,
        "durationTicks": duration_ticks,
        "isPaused": is_paused,
        "container": context.container,
        "size": context.size,
        "bitrate": context.bitrate,
        "overview": context.overview,
        "playMethod": context.play_method,
        "remoteIp": remote_ip.map(str::to_owned),
        "resumed": resumed,
        "client": bounded_playback_text(client),
        "deviceName": bounded_playback_text(device_name),
        "deviceType": bounded_playback_text(device_type),
        "clientVersion": bounded_playback_text(client_version),
    });
    if let Err(error) = webhooks
        .publish(event_type, &dedupe_key, occurred_at, data)
        .await
    {
        tracing::warn!(
            event = "playback_webhook_enqueue_failed",
            webhook_event_type = event_type.as_str(),
            error = %error,
            "failed to enqueue playback webhook"
        );
    }
}

pub(super) fn bounded_playback_text(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    (value.chars().count() <= 128).then(|| value.to_owned())
}

pub(super) async fn emby_sessions(
    headers: HeaderMap,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let auth_principal =
        match require_emby_principal(&headers, &state, query.api_key.as_deref()).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let user_id = auth_principal
        .user_id()
        .map(|user_id| user_id.to_string())
        .unwrap_or_default();
    let active_within_seconds = match query.active_within_seconds {
        Some(seconds) if (1..=MAX_PLAYBACK_SESSION_WINDOW_SECONDS).contains(&seconds) => {
            Some(seconds)
        }
        Some(_) => return StatusCode::BAD_REQUEST.into_response(),
        None => None,
    };
    let sessions = match database
        .list_playback_sessions(
            (!auth_principal.is_admin()).then_some(user_id.as_str()),
            active_within_seconds,
        )
        .await
    {
        Ok(sessions) => sessions,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let emby_play_session_ids = sessions
        .iter()
        .filter(|session| session.play_session_id.starts_with("lux-emby:"))
        .map(|session| session.play_session_id.clone())
        .collect::<Vec<_>>();
    let web_sessions = match database
        .find_web_playback_sessions_for_playbacks(&emby_play_session_ids)
        .await
    {
        Ok(sessions) => sessions
            .into_iter()
            .map(|session| (session.play_session_id.clone(), session))
            .collect::<std::collections::HashMap<_, _>>(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let user_names = if auth_principal.is_admin() {
        let session_user_ids = sessions
            .iter()
            .map(|session| session.user_id.clone())
            .collect::<Vec<_>>();
        match database.find_user_display_names(&session_user_ids).await {
            Ok(user_names) => user_names,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        }
    } else {
        auth_principal
            .user()
            .map(|user| {
                std::collections::HashMap::from([(user_id.clone(), user.display_name.clone())])
            })
            .unwrap_or_default()
    };
    let catalog_items = if sessions.is_empty() {
        HashMap::new()
    } else {
        let item_ids = sessions
            .iter()
            .map(|session| session.item_id.clone())
            .collect::<Vec<_>>();
        let Some(catalog) = state.catalog.as_ref() else {
            return Json(
                sessions
                    .iter()
                    .map(|session| {
                        emby_session_json(
                            session,
                            None,
                            web_sessions.get(&session.play_session_id),
                            user_names.get(&session.user_id).map(String::as_str),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
            .into_response();
        };
        let access_principal = match emby_access_principal(&auth_principal, None) {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
        match catalog.find_items(access_principal, &item_ids).await {
            Ok(items) => items,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        }
    };
    Json(
        sessions
            .iter()
            .map(|session| {
                emby_session_json(
                    session,
                    catalog_items.get(&session.item_id),
                    web_sessions.get(&session.play_session_id),
                    user_names.get(&session.user_id).map(String::as_str),
                )
            })
            .collect::<Vec<_>>(),
    )
    .into_response()
}

pub(super) async fn emby_stop_session(
    headers: HeaderMap,
    Path(session_id): Path<String>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    let auth_principal =
        match require_emby_principal(&headers, &state, query.api_key.as_deref()).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let session = match database.find_playback_session_by_id(&session_id).await {
        Ok(Some(session)) => session,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    if let Some(user) = auth_principal.user()
        && !user.is_admin
        && session.user_id != user.id.to_string()
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    if let Some(transcode_session_id) =
        emby_transcode_session_id_from_play_session(&session.play_session_id)
        && let Some(service) = state.web_playback.as_ref()
        && let Err(error) = service.stop(transcode_session_id, &session.user_id).await
    {
        tracing::warn!(
            session_id = %session.id,
            error = %error,
            "failed to stop Emby web playback session"
        );
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    match database
        .stop_playback_session(&session.id, current_unix_timestamp())
        .await
    {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) fn emby_session_json(
    session: &crate::storage::StoredPlaybackSession,
    catalog_item: Option<&CatalogItem>,
    web_session: Option<&crate::storage::StoredWebPlaybackSession>,
    user_name: Option<&str>,
) -> Value {
    let runtime_ticks = session_runtime_ticks(session, catalog_item);
    let (play_method, transcoding_info) = emby_session_playback_details(session, web_session);
    let mut now_playing_item = json!({
        "Id": emby_public_id(&session.item_id),
        "RunTimeTicks": runtime_ticks,
        "Overview": catalog_item
            .and_then(|item| item.overview.as_deref())
            .unwrap_or_default(),
        "OriginalTitle": catalog_item
            .and_then(|item| item.original_title.as_deref())
            .unwrap_or_default(),
        "ProductionYear": catalog_item
            .and_then(|item| item.production_year)
            .unwrap_or_default(),
        "ImageTags": {},
        "BackdropImageTags": [],
        "ParentBackdropImageTags": [],
        "ProviderIds": {},
        "ParentId": "",
        "SeriesId": "",
        "SeasonName": "",
    });
    if let Some(item) = catalog_item
        && let Value::Object(object) = &mut now_playing_item
    {
        let mut image_tags = serde_json::Map::new();
        if let Some(tag) = item.poster_image_tag.as_ref() {
            image_tags.insert("Primary".to_owned(), json!(tag));
        } else if item.item_type == "EPISODE"
            && let Some(tag) = item.thumb_image_tag.as_ref()
        {
            image_tags.insert("Primary".to_owned(), json!(tag));
        }
        for (name, tag) in [
            ("Logo", item.logo_image_tag.as_ref()),
            ("Thumb", item.thumb_image_tag.as_ref()),
            ("Banner", item.banner_image_tag.as_ref()),
            ("Disc", item.disc_image_tag.as_ref()),
            ("Art", item.art_image_tag.as_ref()),
            ("Wallpaper", item.wallpaper_image_tag.as_ref()),
        ] {
            if let Some(tag) = tag {
                image_tags.insert(name.to_owned(), json!(tag));
            }
        }
        let backdrop_image_tags = if item.fanart_image_tags.is_empty() {
            item.fanart_image_tag
                .as_ref()
                .map(|tag| vec![tag.clone()])
                .unwrap_or_default()
        } else {
            item.fanart_image_tags.clone()
        };
        let series_id = item.series_id.clone().or_else(|| {
            (item.item_type == "SEASON")
                .then(|| item.parent_id.clone())
                .flatten()
        });
        let season_name = match item.item_type.as_str() {
            "SEASON" => Some(item.title.clone()),
            "EPISODE" => item
                .season_number
                .map(|number| format!("Season {number:02}")),
            _ => None,
        };
        let provider_ids = item
            .provider_ids
            .iter()
            .map(|(name, value)| {
                let name = match name.to_ascii_lowercase().as_str() {
                    "tmdb" => "Tmdb",
                    "tvdb" => "Tvdb",
                    "imdb" => "Imdb",
                    _ => name,
                };
                (name.to_owned(), value.clone())
            })
            .collect::<BTreeMap<_, _>>();
        object.insert("Name".to_owned(), json!(item.title));
        object.insert("Type".to_owned(), json!(emby_item_type(&item.item_type)));
        object.insert(
            "SeriesName".to_owned(),
            json!(item.series_name.as_deref().unwrap_or_default()),
        );
        object.insert(
            "ParentId".to_owned(),
            json!(emby_public_id(
                item.parent_id.as_deref().unwrap_or(&item.library_id)
            )),
        );
        object.insert(
            "SeriesId".to_owned(),
            json!(
                series_id
                    .map(|value| emby_public_id(&value))
                    .unwrap_or_default()
            ),
        );
        object.insert(
            "SeasonName".to_owned(),
            json!(season_name.unwrap_or_default()),
        );
        object.insert("ImageTags".to_owned(), Value::Object(image_tags));
        object.insert("BackdropImageTags".to_owned(), json!(backdrop_image_tags));
        object.insert(
            "ParentBackdropImageTags".to_owned(),
            json!(item.series_fanart_image_tags),
        );
        object.insert("ProviderIds".to_owned(), json!(provider_ids));
        if let Some(season_number) = item.season_number {
            object.insert("ParentIndexNumber".to_owned(), json!(season_number));
        }
        if let Some(episode_number) = item.episode_number {
            object.insert("IndexNumber".to_owned(), json!(episode_number));
            // Older Emby clients, including some session-card consumers, use
            // the legacy Index alias instead of IndexNumber.
            object.insert("Index".to_owned(), json!(episode_number));
        }
    }
    // Session consumers perform arithmetic and comparisons on these fields.
    // Never serialize an Option here: null values from a partial playback
    // callback make otherwise valid sessions unusable to those clients.
    json!({
        "Id": session.id,
        "UserId": session.user_id,
        "UserName": user_name.unwrap_or("Unknown"),
        "UserPrimaryImageTag": Value::Null,
        "ItemId": emby_public_id(&session.item_id),
        "MediaSourceId": session.media_source_id.as_deref().unwrap_or(""),
        "PlaySessionId": session.play_session_id,
        "Client": session.client.as_deref().unwrap_or("Unknown"),
        "DeviceId": session.device_id,
        "DeviceName": session.device_name.as_deref().unwrap_or("Unknown"),
        "DeviceType": session.device_type.as_deref().unwrap_or("Unknown"),
        "ApplicationVersion": session.client_version.as_deref().unwrap_or("Unknown"),
        "RemoteEndPoint": session.remote_ip.as_deref().unwrap_or(""),
        "PlayState": {
            "PositionTicks": session.position_ticks.max(0),
            "IsPaused": session.is_paused,
            "CanSeek": true,
            "PlayMethod": play_method,
            "VolumeLevel": 100,
        },
        "NowPlayingItem": now_playing_item,
        "RunTimeTicks": runtime_ticks,
        "LastActivityDate": emby_session_timestamp(session.last_event_at),
        "TranscodingInfo": transcoding_info,
    })
}

fn emby_session_timestamp(unix_seconds: i64) -> Value {
    time::OffsetDateTime::from_unix_timestamp(unix_seconds)
        .ok()
        .and_then(|timestamp| {
            timestamp
                .format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .map(Value::String)
        .unwrap_or(Value::Null)
}

fn emby_session_playback_details(
    session: &crate::storage::StoredPlaybackSession,
    web_session: Option<&crate::storage::StoredWebPlaybackSession>,
) -> (&'static str, Option<Value>) {
    let Some(web_session) = web_session.filter(|session| session.plan == "SERVER_HLS") else {
        let play_method = if session.play_session_id.starts_with("lux-emby:") {
            "Transcode"
        } else {
            "DirectPlay"
        };
        return (
            play_method,
            Some(json!({
                // Session-card consumers perform arithmetic on these fields,
                // including for direct play and sessions without probe data.
                "Container": "",
                "VideoCodec": "",
                "AudioCodec": "",
                "VideoBitrate": 0,
                "AudioBitrate": 0,
                "Bitrate": 0,
                "IsVideoDirect": play_method == "DirectPlay",
                "IsAudioDirect": play_method == "DirectPlay",
            })),
        );
    };
    let play_method = if web_session.tier <= i64::from(ServerTier::Remux.number()) {
        "DirectStream"
    } else {
        "Transcode"
    };
    let total_bitrate = web_session
        .video_bitrate
        .unwrap_or_default()
        .saturating_add(web_session.audio_bitrate.unwrap_or_default());
    let container = web_session.transcoding_container.as_deref().unwrap_or("");
    let video_codec = web_session.video_codec.as_deref().unwrap_or("");
    let audio_codec = web_session.audio_codec.as_deref().unwrap_or("");
    let video_bitrate = web_session.video_bitrate.unwrap_or_default();
    let audio_bitrate = web_session.audio_bitrate.unwrap_or_default();
    (
        play_method,
        Some(json!({
            "Container": container,
            "VideoCodec": video_codec,
            "AudioCodec": audio_codec,
            "VideoBitrate": video_bitrate,
            "AudioBitrate": audio_bitrate,
            "Bitrate": total_bitrate,
            "IsVideoDirect": web_session.tier <= i64::from(ServerTier::AudioTranscode.number()),
            "IsAudioDirect": web_session.tier == i64::from(ServerTier::Remux.number()),
        })),
    )
}

fn session_runtime_ticks(
    session: &crate::storage::StoredPlaybackSession,
    catalog_item: Option<&CatalogItem>,
) -> i64 {
    let catalog_runtime = catalog_item.and_then(|item| {
        session
            .media_source_id
            .as_deref()
            .and_then(|source_id| {
                item.media_sources
                    .iter()
                    .find(|source| source.id == source_id)
            })
            .and_then(|source| super::emby_catalog::emby_source_runtime_ticks(item, source))
            .or_else(|| super::emby_catalog::emby_item_runtime_ticks(item))
    });
    catalog_runtime
        .or_else(|| session.duration_ticks.filter(|ticks| *ticks > 0))
        .unwrap_or_default()
}

pub(super) async fn lux_get_playback(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match access
        .can_view_item(AccessPrincipal::new(user.id, user.is_admin), &item_id)
        .await
    {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let user_id = user.id.to_string();
    let user_state = match database.find_user_item_state(&user_id, &item_id).await {
        Ok(state) => state,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let active_session = match database
        .find_active_playback_session(&user_id, &item_id)
        .await
    {
        Ok(session) => session,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    Json(json!({
        "itemId": item_id,
        "positionTicks": user_state.as_ref().map(|value| value.position_ticks).unwrap_or_default(),
        "isPlayed": user_state.as_ref().map(|value| value.is_played).unwrap_or(false),
        "isFavorite": user_state.as_ref().map(|value| value.is_favorite).unwrap_or(false),
        "playCount": user_state.as_ref().map(|value| value.play_count).unwrap_or_default(),
        "state": active_session.as_ref().map(|value| value.state.as_str()),
        "isPaused": active_session.as_ref().map(|value| value.is_paused).unwrap_or(false),
        "lastEventAt": active_session.as_ref().map(|value| value.last_event_at),
    }))
    .into_response()
}

pub(super) async fn lux_list_playback_history(
    headers: HeaderMap,
    Query(query): Query<AdminJobsQuery>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let (offset, limit) = match page_params(query.page, query.page_size) {
        Ok(params) => params,
        Err(message) => {
            return api_error(
                &headers,
                StatusCode::BAD_REQUEST,
                lux::ApiErrorCode::InvalidRequest,
                message,
            )
            .into_response();
        }
    };
    let Some(service) = state.emby_migration.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service
        .list_playback_history(&user.id.to_string(), offset, limit)
        .await
    {
        Ok(events) => Json(json!({
            "events": events,
            "page": offset / limit + 1,
            "pageSize": limit,
        }))
        .into_response(),
        Err(error) => emby_migration_error(&headers, error),
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(super) struct WebPlaybackCapabilitiesRequest {
    #[serde(default)]
    direct_play: bool,
    #[serde(default)]
    hls: bool,
    #[serde(default)]
    video_copy_to_fmp4: bool,
    #[serde(default)]
    audio_copy_to_fmp4: bool,
    #[serde(default)]
    hardware_transcode: bool,
    #[serde(default)]
    software_transcode: bool,
}

impl From<WebPlaybackCapabilitiesRequest> for PlaybackCapabilities {
    fn from(value: WebPlaybackCapabilitiesRequest) -> Self {
        Self {
            direct_play: value.direct_play,
            hls: value.hls,
            video_copy_to_fmp4: value.video_copy_to_fmp4,
            audio_copy_to_fmp4: value.audio_copy_to_fmp4,
            hardware_transcode: value.hardware_transcode,
            software_transcode: value.software_transcode,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WebPlaybackSessionRequest {
    item_id: String,
    source_id: String,
    #[serde(default)]
    capabilities: WebPlaybackCapabilitiesRequest,
    #[serde(default)]
    audio_stream_index: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WebPlaybackResourceQuery {
    expires: i64,
    signature: String,
}

pub(super) fn web_playback_error(headers: &HeaderMap, error: WebPlaybackSessionError) -> Response {
    let (status, code, message) = match error {
        WebPlaybackSessionError::Invalid(message) => (
            StatusCode::BAD_REQUEST,
            lux::ApiErrorCode::InvalidRequest,
            message,
        ),
        WebPlaybackSessionError::PlaybackPolicyDenied => (
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::PermissionDenied,
            "当前用户策略未允许所需的服务端转码档位".to_owned(),
        ),
        WebPlaybackSessionError::NotFound => (
            StatusCode::NOT_FOUND,
            lux::ApiErrorCode::NotFound,
            "播放会话不存在".to_owned(),
        ),
        WebPlaybackSessionError::Expired => (
            StatusCode::GONE,
            lux::ApiErrorCode::NotFound,
            "播放会话已过期".to_owned(),
        ),
        WebPlaybackSessionError::NotActive => (
            StatusCode::GONE,
            lux::ApiErrorCode::NotFound,
            "播放会话已结束".to_owned(),
        ),
        WebPlaybackSessionError::AccessRulesUnavailable => (
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "播放授权暂时不可用".to_owned(),
        ),
        WebPlaybackSessionError::Hls(_) => (
            StatusCode::BAD_GATEWAY,
            lux::ApiErrorCode::Internal,
            "服务端 HLS 暂时不可用".to_owned(),
        ),
        WebPlaybackSessionError::Storage(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            lux::ApiErrorCode::DatabaseUnavailable,
            "播放会话暂时不可用".to_owned(),
        ),
    };
    api_error(headers, status, code, &message).into_response()
}

async fn authorize_web_playback_resource(
    headers: &HeaderMap,
    state: &AppState,
    service: &WebPlaybackSessionService,
    session_id: &str,
    resource: &str,
    expires_at: i64,
    signature: &str,
) -> Result<StoredWebPlaybackSession, WebPlaybackSessionError> {
    service.verify_resource_signature(session_id, resource, expires_at, signature)?;
    let client_ip = super::request_access_ip(headers, state)
        .await
        .map_err(|_| WebPlaybackSessionError::AccessRulesUnavailable)?;
    service
        .authorize_resource_with_ip(session_id, resource, expires_at, signature, client_ip)
        .await
}

fn web_playback_device_id(headers: &HeaderMap) -> Option<String> {
    let device_id = headers
        .get("x-lux-device-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| uuid::Uuid::parse_str(value).ok())?;
    (device_id.get_version_num() == 4).then(|| device_id.to_string())
}

async fn web_playback_authorization_context(
    headers: &HeaderMap,
    state: &AppState,
    expected_user_id: &str,
) -> Result<(Option<String>, Option<String>), WebPlaybackSessionError> {
    if let Some(session_token) = super::request_cookie(headers, "lux_session") {
        let auth = state
            .auth
            .as_ref()
            .ok_or(WebPlaybackSessionError::AccessRulesUnavailable)?;
        let client_ip = super::request_access_ip(headers, state)
            .await
            .map_err(|_| WebPlaybackSessionError::AccessRulesUnavailable)?;
        let session = auth
            .resolve_with_ip(&session_token, client_ip)
            .await
            .map_err(|_| WebPlaybackSessionError::AccessRulesUnavailable)?
            .ok_or(WebPlaybackSessionError::NotFound)?;
        if session.user.id.to_string() != expected_user_id {
            return Err(WebPlaybackSessionError::NotFound);
        }
        let device_id = session
            .device_id
            .or_else(|| web_playback_device_id(headers));
        return Ok((device_id, Some("Lux Web".to_owned())));
    }

    let token = emby_token_from_headers(headers).or_else(|| {
        headers
            .get("X-Lux-Token")
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .map(|value| value.strip_prefix("Bearer ").unwrap_or(value).trim())
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    });
    let Some(token) = token else {
        return Err(WebPlaybackSessionError::NotFound);
    };
    let auth = state
        .emby_auth
        .as_ref()
        .ok_or(WebPlaybackSessionError::AccessRulesUnavailable)?;
    let client_ip = super::request_access_ip(headers, state)
        .await
        .map_err(|_| WebPlaybackSessionError::AccessRulesUnavailable)?;
    let user = auth
        .resolve_token_with_ip(&token, client_ip)
        .await
        .map_err(|_| WebPlaybackSessionError::AccessRulesUnavailable)?
        .ok_or(WebPlaybackSessionError::NotFound)?;
    if user.id.to_string() != expected_user_id {
        return Err(WebPlaybackSessionError::NotFound);
    }
    let device = auth
        .device_info(&token)
        .await
        .map_err(|_| WebPlaybackSessionError::AccessRulesUnavailable)?
        .ok_or(WebPlaybackSessionError::NotFound)?;
    let device_id = (!device.device_id.trim().is_empty()).then_some(device.device_id);
    let client_name = (!device.client.trim().is_empty()).then_some(device.client);
    Ok((device_id, client_name))
}

/// Expiry stamped into signed asset URLs.
///
/// A playlist is fetched once and embeds these URLs, so the signature has to outlive a single
/// heartbeat window; the live session row stays the actual authorization gate.
fn web_playback_resource_expiry() -> i64 {
    crate::application::playback::session::unix_timestamp()
        .saturating_add(crate::application::playback::session::WEB_PLAYBACK_RESOURCE_TTL_SECONDS)
}

pub(super) fn web_playback_resource_url(
    service: &WebPlaybackSessionService,
    session_id: &str,
    resource: &str,
    expires_at: i64,
) -> Option<String> {
    let signature = service.sign_resource(session_id, resource, expires_at)?;
    Some(format!(
        "/api/v1/playback/sessions/{session_id}/{resource}?expires={}&signature={}",
        signature.expires_at, signature.signature
    ))
}

pub(super) fn web_playback_range_url(
    service: &WebPlaybackSessionService,
    session_id: &str,
    expires_at: i64,
) -> Option<String> {
    let signature = service.sign_resource(session_id, "range", expires_at)?;
    Some(format!(
        "/api/v1/playback/sessions/{session_id}/range?expires={}&signature={}",
        signature.expires_at, signature.signature
    ))
}

pub(super) fn web_playback_hls_url(
    service: &WebPlaybackSessionService,
    session_id: &str,
    asset: &str,
    expires_at: i64,
) -> Option<String> {
    let resource = format!("hls:{asset}");
    let signature = service.sign_resource(session_id, &resource, expires_at)?;
    Some(format!(
        "/api/v1/playback/sessions/{session_id}/hls/{asset}?expires={}&signature={}",
        signature.expires_at, signature.signature
    ))
}

struct EmbyTranscodingUrlOptions<'a> {
    request: &'a EmbyPlaybackInfoRequest,
    hls: HlsStartOptions,
    device_id: &'a str,
}

fn emby_transcoding_url(
    service: &WebPlaybackSessionService,
    item_id: &str,
    source: &crate::application::catalog::CatalogSource,
    session: &CreatedWebPlaybackSession,
    options: EmbyTranscodingUrlOptions<'_>,
) -> Option<String> {
    let request = options.request;
    let segment_container = options.hls.segment_container;
    let start_time_ticks = options.hls.start_time_ticks;
    let device_id = options.device_id;
    let signature = service.sign_resource(
        &session.id,
        &emby_hls_resource(segment_container, "index.m3u8"),
        web_playback_resource_expiry(),
    )?;
    let public_item_id = emby_public_id(item_id);
    let tier = match &session.plan {
        WebPlaybackPlan::ServerHls { tier } => *tier,
        _ => ServerTier::SoftwareTranscode,
    };
    let hls_profile = request.device_profile.as_ref().and_then(|profile| {
        profile.transcoding_profiles.iter().find(|profile| {
            is_video_profile(profile.profile_type.as_deref()) && is_hls_profile(profile)
        })
    });
    let video_codec = hls_profile
        .and_then(|profile| first_profile_value(profile.video_codec.as_deref()))
        .or_else(|| {
            (tier != ServerTier::HardwareTranscode && tier != ServerTier::SoftwareTranscode)
                .then(|| source_stream_codec(source, "VIDEO").map(str::to_owned))
                .flatten()
        })
        .unwrap_or_else(|| "h264".to_owned());
    let audio_codec = hls_profile
        .and_then(|profile| first_profile_value(profile.audio_codec.as_deref()))
        .or_else(|| {
            (tier != ServerTier::AudioTranscode
                && tier != ServerTier::HardwareTranscode
                && tier != ServerTier::SoftwareTranscode)
                .then(|| source_stream_codec(source, "AUDIO").map(str::to_owned))
                .flatten()
        })
        .unwrap_or_else(|| "aac".to_owned());
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query.append_pair("DeviceId", device_id);
    query.append_pair("MediaSourceId", &source.id);
    query.append_pair("PlaySessionId", &session.play_session_id);
    query.append_pair("VideoCodec", &video_codec);
    query.append_pair("AudioCodec", &audio_codec);
    if let Some(video_bitrate) = emby_transcoding_video_bitrate(source, request) {
        query.append_pair("VideoBitrate", &video_bitrate.to_string());
    }
    if let Some(audio_bitrate) = emby_transcoding_audio_bitrate(source, tier) {
        query.append_pair("AudioBitrate", &audio_bitrate.to_string());
    }
    let audio_stream_index = request
        .audio_stream_index
        .or_else(|| source_stream_index(source, "AUDIO"))
        .unwrap_or(-1);
    query.append_pair("AudioStreamIndex", &audio_stream_index.to_string());
    if let Some(subtitle_stream_index) = request.subtitle_stream_index {
        query.append_pair("SubtitleStreamIndex", &subtitle_stream_index.to_string());
    }
    if let Some(max_audio_channels) = request
        .max_audio_channels
        .or_else(|| hls_profile.and_then(|profile| profile.max_audio_channels))
    {
        query.append_pair(
            "TranscodingMaxAudioChannels",
            &max_audio_channels.to_string(),
        );
    }
    if let Some(start_time_ticks) = start_time_ticks.filter(|ticks| *ticks > 0) {
        query.append_pair("StartTimeTicks", &start_time_ticks.to_string());
    }
    query.append_pair("SegmentContainer", segment_container.emby_container());
    query.append_pair("MinSegments", "1");
    query.append_pair("BreakOnNonKeyFrames", "True");
    query.append_pair("TranscodeReasons", emby_transcode_reason(request, source));
    query.append_pair("luxPlaybackSessionId", &session.id);
    query.append_pair("luxPlaybackExpires", &signature.expires_at.to_string());
    query.append_pair("luxPlaybackSignature", &signature.signature);
    Some(format!(
        "/Videos/{public_item_id}/master.m3u8?{}",
        query.finish()
    ))
}

fn emby_playback_device_id(headers: &HeaderMap, raw_query: &RawQuery) -> String {
    let query_device_id = raw_query.0.as_deref().and_then(|raw_query| {
        url::form_urlencoded::parse(raw_query.as_bytes()).find_map(|(name, value)| {
            name.eq_ignore_ascii_case("DeviceId")
                .then(|| non_empty_device_id(value.as_ref()))
                .flatten()
        })
    });
    query_device_id
        .or_else(|| {
            ["X-Emby-Device-Id", "X-MediaBrowser-Device-Id"]
                .into_iter()
                .find_map(|name| {
                    headers
                        .get(name)
                        .and_then(|value| value.to_str().ok())
                        .and_then(non_empty_device_id)
                })
        })
        .or_else(|| non_empty_device_id(&emby_device_info_from_headers(headers).device_id))
        .unwrap_or_else(|| "unknown".to_owned())
}

fn non_empty_device_id(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value.chars().count() <= 128).then(|| value.to_owned())
}

fn non_empty_profile_value(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn first_profile_value(value: Option<&str>) -> Option<String> {
    non_empty_profile_value(value)?
        .split(',')
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(str::to_owned)
}

fn source_stream_codec<'a>(
    source: &'a crate::application::catalog::CatalogSource,
    stream_type: &str,
) -> Option<&'a str> {
    source
        .streams
        .iter()
        .find(|stream| stream.stream_type.eq_ignore_ascii_case(stream_type))
        .and_then(|stream| non_empty_profile_value(stream.codec.as_deref()))
}

fn source_stream_index(
    source: &crate::application::catalog::CatalogSource,
    stream_type: &str,
) -> Option<i64> {
    source
        .streams
        .iter()
        .find(|stream| stream.stream_type.eq_ignore_ascii_case(stream_type) && stream.is_default)
        .or_else(|| {
            source
                .streams
                .iter()
                .find(|stream| stream.stream_type.eq_ignore_ascii_case(stream_type))
        })
        .map(|stream| stream.index)
}

fn stream_detail_i64(
    stream: &crate::application::catalog::CatalogStream,
    name: &str,
) -> Option<i64> {
    stream
        .details
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .and_then(|(_, value)| match value {
            Value::Number(value) => value.as_i64(),
            Value::String(value) => value.trim().parse::<i64>().ok(),
            _ => None,
        })
        .filter(|value| *value > 0)
}

fn emby_transcoding_video_bitrate(
    source: &crate::application::catalog::CatalogSource,
    request: &EmbyPlaybackInfoRequest,
) -> Option<i64> {
    let bitrate = source.bitrate.filter(|value| *value > 0)?;
    Some(
        request
            .effective_max_streaming_bitrate()
            .filter(|value| *value > 0)
            .map_or(bitrate, |limit| bitrate.min(limit)),
    )
}

fn emby_transcoding_audio_bitrate(
    source: &crate::application::catalog::CatalogSource,
    tier: ServerTier,
) -> Option<i64> {
    if matches!(
        tier,
        ServerTier::AudioTranscode | ServerTier::HardwareTranscode | ServerTier::SoftwareTranscode
    ) {
        return Some(192_000);
    }
    source
        .streams
        .iter()
        .filter(|stream| stream.stream_type.eq_ignore_ascii_case("AUDIO"))
        .find_map(|stream| stream_detail_i64(stream, "BitRate"))
}

/// The output description for a Lux Web HLS session.
///
/// Copied tracks (remux and audio-transcode) intentionally leave their codec empty: the Web
/// session row does not duplicate the source streams, and the player reads those from the media
/// source. Only codecs the server actually re-encodes are asserted here.
fn web_transcoding_output(
    tier: ServerTier,
    segment_container: HlsSegmentContainer,
) -> WebPlaybackTranscodingDetails {
    let video_transcoded = matches!(
        tier,
        ServerTier::HardwareTranscode | ServerTier::SoftwareTranscode
    );
    let audio_transcoded = matches!(
        tier,
        ServerTier::AudioTranscode | ServerTier::HardwareTranscode | ServerTier::SoftwareTranscode
    );
    WebPlaybackTranscodingDetails {
        video_codec: video_transcoded.then(|| "h264".to_owned()),
        audio_codec: audio_transcoded.then(|| "aac".to_owned()),
        video_bitrate: None,
        audio_bitrate: audio_transcoded.then_some(192_000),
        transcoding_container: Some(segment_container.emby_container().to_owned()),
    }
}

fn emby_transcoding_output(
    source: &crate::application::catalog::CatalogSource,
    request: &EmbyPlaybackInfoRequest,
    tier: ServerTier,
    segment_container: HlsSegmentContainer,
) -> WebPlaybackTranscodingDetails {
    let video_transcoded = matches!(
        tier,
        ServerTier::HardwareTranscode | ServerTier::SoftwareTranscode
    );
    let audio_transcoded = matches!(
        tier,
        ServerTier::AudioTranscode | ServerTier::HardwareTranscode | ServerTier::SoftwareTranscode
    );
    WebPlaybackTranscodingDetails {
        video_codec: if video_transcoded {
            Some("h264".to_owned())
        } else {
            source_stream_codec(source, "VIDEO").map(str::to_owned)
        },
        audio_codec: if audio_transcoded {
            Some("aac".to_owned())
        } else {
            source_stream_codec(source, "AUDIO").map(str::to_owned)
        },
        video_bitrate: if video_transcoded {
            emby_transcoding_video_bitrate(source, request)
        } else {
            None
        },
        audio_bitrate: emby_transcoding_audio_bitrate(source, tier),
        transcoding_container: Some(segment_container.emby_container().to_owned()),
    }
}

fn emby_transcode_reason(
    request: &EmbyPlaybackInfoRequest,
    source: &crate::application::catalog::CatalogSource,
) -> &'static str {
    if request.source_exceeds_streaming_bitrate(source) {
        "ContainerBitrateExceedsLimit"
    } else if request.device_profile.as_ref().is_some_and(|profile| {
        profile.direct_play_compatibility(source) == EmbyProfileCompatibility::Incompatible
    }) {
        "ContainerNotSupported"
    } else {
        "DirectPlayError"
    }
}

fn emby_hls_asset_kind(asset: &str) -> &'static str {
    match asset {
        "index.m3u8" => "manifest",
        "init.mp4" => "initialization",
        value if is_emby_logical_init_asset(value) => "initialization",
        value if is_emby_generation_init_asset(value) => "initialization",
        value if is_emby_segment_asset(value) => "segment",
        value if is_emby_generation_segment_asset(value) => "segment",
        _ => "other",
    }
}

fn record_emby_hls_asset_response(
    method: &Method,
    public_item_id: &str,
    session_id: Option<&str>,
    asset: &str,
    status: StatusCode,
    duration_ms: u128,
) {
    let asset_kind = emby_hls_asset_kind(asset);
    if !matches!(asset_kind, "manifest" | "initialization") && status.is_success() {
        return;
    }
    let session_id_prefix = session_id
        .map(playback_identifier_prefix)
        .unwrap_or_else(|| "missing".to_owned());
    let duration_ms = u64::try_from(duration_ms).unwrap_or(u64::MAX);
    if status.is_success() {
        tracing::info!(
            event = "emby_hls_asset_request",
            method = %method,
            item_id_prefix = %playback_identifier_prefix(public_item_id),
            session_id_prefix = %session_id_prefix,
            asset_kind,
            status_code = status.as_u16(),
            duration_ms,
            "served Emby HLS asset"
        );
    } else {
        tracing::warn!(
            event = "emby_hls_asset_request",
            method = %method,
            item_id_prefix = %playback_identifier_prefix(public_item_id),
            session_id_prefix = %session_id_prefix,
            asset_kind,
            status_code = status.as_u16(),
            duration_ms,
            "Emby HLS asset request failed"
        );
    }
}

struct EmbyTranscodingAssetUrlOptions<'a> {
    item_id: &'a str,
    source_id: &'a str,
    session_id: &'a str,
    play_session_id: &'a str,
    asset: &'a str,
    segment_container: HlsSegmentContainer,
    expires_at: i64,
}

fn emby_transcoding_asset_url(
    service: &WebPlaybackSessionService,
    options: EmbyTranscodingAssetUrlOptions<'_>,
) -> Option<String> {
    let resource = emby_hls_resource(options.segment_container, options.asset);
    let signature = service.sign_resource(options.session_id, &resource, options.expires_at)?;
    Some(format!(
        "/Videos/{}/transcoding/{}/{}?MediaSourceId={}&PlaySessionId={}&SegmentContainer={}&luxPlaybackExpires={}&luxPlaybackSignature={}",
        emby_public_id(options.item_id),
        options.session_id,
        options.asset,
        options.source_id,
        percent_encode_filename(options.play_session_id),
        options.segment_container.emby_container(),
        signature.expires_at,
        signature.signature,
    ))
}

fn emby_hls_resource(segment_container: HlsSegmentContainer, asset: &str) -> String {
    format!("hls:{}:{asset}", segment_container.emby_container())
}

#[derive(Default)]
struct EmbyTranscodingQuery {
    media_source_id: Option<String>,
    play_session_id: Option<String>,
    playback_session_id: Option<String>,
    segment_container: Option<String>,
    expires: Option<i64>,
    signature: Option<String>,
}

fn emby_transcoding_query_from_raw(raw_query: RawQuery) -> EmbyTranscodingQuery {
    let mut query = EmbyTranscodingQuery::default();
    let Some(raw_query) = raw_query.0 else {
        return query;
    };
    for (name, value) in url::form_urlencoded::parse(raw_query.as_bytes()) {
        if query.media_source_id.is_none()
            && (name.eq_ignore_ascii_case("MediaSourceId")
                || name.eq_ignore_ascii_case("mediaSourceId"))
        {
            query.media_source_id = Some(value.into_owned());
        } else if query.play_session_id.is_none()
            && (name.eq_ignore_ascii_case("PlaySessionId")
                || name.eq_ignore_ascii_case("playSessionId"))
        {
            query.play_session_id = Some(value.into_owned());
        } else if query.playback_session_id.is_none()
            && name.eq_ignore_ascii_case("luxPlaybackSessionId")
        {
            query.playback_session_id = Some(value.into_owned());
        } else if query.segment_container.is_none() && name.eq_ignore_ascii_case("SegmentContainer")
        {
            query.segment_container = Some(value.into_owned());
        } else if query.expires.is_none() && name.eq_ignore_ascii_case("luxPlaybackExpires") {
            query.expires = value.parse().ok();
        } else if query.signature.is_none() && name.eq_ignore_ascii_case("luxPlaybackSignature") {
            query.signature = Some(value.into_owned());
        }
    }
    query
}

fn emby_transcoding_session_id(query: &EmbyTranscodingQuery) -> Option<&str> {
    query
        .playback_session_id
        .as_deref()
        .or_else(|| query.play_session_id.as_deref()?.strip_prefix("lux-emby:"))
}

fn emby_transcode_session_id_from_play_session(value: &str) -> Option<&str> {
    value.strip_prefix("lux-emby:")
}

pub(super) async fn emby_transcoding_master(
    headers: HeaderMap,
    method: Method,
    Path(item_id): Path<String>,
    raw_query: RawQuery,
    State(state): State<AppState>,
) -> Response {
    let started = std::time::Instant::now();
    let query = emby_transcoding_query_from_raw(raw_query);
    let session_id = emby_transcoding_session_id(&query);
    let method_for_log = method.clone();
    let response = if let Some(session_id) = session_id {
        serve_emby_transcoding_asset(
            &headers,
            method,
            &item_id,
            session_id,
            "index.m3u8",
            &query,
            &state,
        )
        .await
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    };
    record_emby_hls_asset_response(
        &method_for_log,
        &item_id,
        session_id,
        "index.m3u8",
        response.status(),
        started.elapsed().as_millis(),
    );
    response
}

pub(super) async fn emby_transcoding_asset(
    headers: HeaderMap,
    method: Method,
    Path((item_id, session_id, asset)): Path<(String, String, String)>,
    raw_query: RawQuery,
    State(state): State<AppState>,
) -> Response {
    let started = std::time::Instant::now();
    let query = emby_transcoding_query_from_raw(raw_query);
    let method_for_log = method.clone();
    let response = serve_emby_transcoding_asset(
        &headers,
        method,
        &item_id,
        &session_id,
        &asset,
        &query,
        &state,
    )
    .await;
    record_emby_hls_asset_response(
        &method_for_log,
        &item_id,
        Some(&session_id),
        &asset,
        response.status(),
        started.elapsed().as_millis(),
    );
    response
}

async fn serve_emby_transcoding_asset(
    headers: &HeaderMap,
    method: Method,
    public_item_id: &str,
    session_id: &str,
    asset: &str,
    query: &EmbyTranscodingQuery,
    state: &AppState,
) -> Response {
    let Some(expires_at) = query.expires else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(signature) = query.signature.as_deref() else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(service) = state.web_playback.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(segment_container_value) = query.segment_container.as_deref() else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(segment_container) = normalize_hls_segment_container(segment_container_value) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let resource = emby_hls_resource(segment_container, asset);
    if service
        .verify_resource_signature(session_id, &resource, expires_at, signature)
        .is_err()
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let client_ip = match super::request_access_ip(headers, state).await {
        Ok(client_ip) => client_ip,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let session = match service
        .authorize_resource_with_ip(session_id, &resource, expires_at, signature, client_ip)
        .await
    {
        Ok(session) => session,
        Err(error) => return emby_playback_session_error_status(error).into_response(),
    };
    if session.plan != "SERVER_HLS"
        || emby_internal_id(public_item_id) != session.item_id
        || query
            .media_source_id
            .as_deref()
            .is_some_and(|source_id| session.media_source_id.as_deref() != Some(source_id))
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    match service.hls_segment_container(session_id).await {
        Ok(actual) if actual == segment_container => {}
        Ok(_) => return StatusCode::NOT_FOUND.into_response(),
        Err(error) => return emby_playback_session_error_status(error).into_response(),
    }
    let Some(source_id) = session.media_source_id.as_deref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if asset == "index.m3u8" {
        let manifest = match service.hls_vod_manifest(session_id).await {
            Ok(Some(manifest)) => manifest,
            Ok(None) => {
                let path = match service.wait_for_hls_manifest(session_id).await {
                    Ok(path) => path,
                    Err(error) => {
                        return emby_playback_session_error_status(error).into_response();
                    }
                };
                let Ok(bytes) = fs::read(path).await else {
                    return StatusCode::NOT_FOUND.into_response();
                };
                let Ok(manifest) = String::from_utf8(bytes) else {
                    return StatusCode::BAD_GATEWAY.into_response();
                };
                manifest
            }
            Err(error) => return emby_playback_session_error_status(error).into_response(),
        };
        let Some(manifest) = rewrite_hls_manifest(&manifest, |asset| {
            emby_transcoding_asset_url(
                service,
                EmbyTranscodingAssetUrlOptions {
                    item_id: &session.item_id,
                    source_id,
                    session_id: &session.id,
                    play_session_id: &session.play_session_id,
                    asset,
                    segment_container,
                    expires_at: web_playback_resource_expiry(),
                },
            )
        }) else {
            return StatusCode::BAD_GATEWAY.into_response();
        };
        let response = Response::builder()
            .status(StatusCode::OK)
            .header("Cache-Control", "private, no-store")
            .header("Content-Type", "application/vnd.apple.mpegurl")
            .header("Content-Length", manifest.len())
            .body(if method == Method::HEAD {
                Body::empty()
            } else {
                Body::from(manifest)
            })
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
        if method == Method::GET
            && response.status().is_success()
            && let Err(response) = admit_web_playback_request(headers, service, &session).await
        {
            return response;
        }
        return response;
    }
    let path = match service.wait_for_hls_asset(session_id, asset).await {
        Ok(path) => path,
        Err(error) => return emby_playback_session_error_status(error).into_response(),
    };
    match service.hls_within_quota(session_id).await {
        Ok(true) => {}
        Ok(false) => {
            let _ = service.stop(session_id, &session.user_id).await;
            return StatusCode::INSUFFICIENT_STORAGE.into_response();
        }
        Err(error) => return emby_playback_session_error_status(error).into_response(),
    }
    let metadata = match fs::metadata(&path).await {
        Ok(metadata) if metadata.is_file() => metadata,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let content_type = if asset.ends_with(".ts") {
        "video/mp2t"
    } else if asset.ends_with(".m4s") || asset.ends_with(".mp4") {
        "video/mp4"
    } else {
        "application/octet-stream"
    };
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        let Ok(file) = fs::File::open(path).await else {
            return StatusCode::NOT_FOUND.into_response();
        };
        Body::from_stream(tokio_util::io::ReaderStream::new(file))
    };
    let response = Response::builder()
        .status(StatusCode::OK)
        .header("Cache-Control", "private, no-store")
        .header("Content-Type", content_type)
        .header("Content-Length", metadata.len())
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    if method == Method::GET
        && response.status().is_success()
        && let Err(response) = admit_web_playback_request(headers, service, &session).await
    {
        return response;
    }
    response
}

fn is_emby_generation_init_asset(asset: &str) -> bool {
    asset
        .strip_prefix("generation_")
        .and_then(|value| value.strip_suffix("_init.mp4"))
        .is_some_and(|generation| {
            generation.len() == 6 && generation.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn is_emby_generation_segment_asset(asset: &str) -> bool {
    let Some(value) = asset.strip_prefix("generation_") else {
        return false;
    };
    let Some((generation, segment)) = value.split_once("_segment_") else {
        return false;
    };
    generation.len() == 6
        && generation.bytes().all(|byte| byte.is_ascii_digit())
        && (segment
            .strip_suffix(".m4s")
            .or_else(|| segment.strip_suffix(".ts")))
        .is_some_and(|value| value.len() == 6 && value.bytes().all(|byte| byte.is_ascii_digit()))
}

fn is_emby_segment_asset(asset: &str) -> bool {
    asset.starts_with("segment_") && (asset.ends_with(".m4s") || asset.ends_with(".ts"))
}

fn is_emby_logical_init_asset(asset: &str) -> bool {
    let Some(value) = asset
        .strip_prefix("init_")
        .and_then(|value| value.strip_suffix(".mp4"))
    else {
        return false;
    };
    value.len() == 6 && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn playback_admission_error(
    headers: &HeaderMap,
    result: &PlaybackAdmissionResult,
) -> Option<Response> {
    let limit = if result.max_simultaneous_playbacks == 0 {
        "不限".to_owned()
    } else {
        format!("{} 路", result.max_simultaneous_playbacks)
    };
    let (status, code, message) = match result.decision {
        PlaybackAdmissionDecision::Admitted => return None,
        PlaybackAdmissionDecision::Denied(PlaybackAdmissionDenialReason::PlaybackDisabled) => (
            StatusCode::FORBIDDEN,
            lux::ApiErrorCode::PermissionDenied,
            format!(
                "当前账户不允许播放（当前 {} 路，策略上限 {}）",
                result.active_playback_count, limit
            ),
        ),
        PlaybackAdmissionDecision::Denied(PlaybackAdmissionDenialReason::ConcurrentLimit) => (
            StatusCode::TOO_MANY_REQUESTS,
            lux::ApiErrorCode::RateLimited,
            format!(
                "同时播放数已达上限（当前 {} 路，上限 {}）",
                result.active_playback_count, limit
            ),
        ),
    };
    Some(api_error(headers, status, code, &message).into_response())
}

async fn check_web_playback_admission(
    headers: &HeaderMap,
    service: &crate::application::playback::session::WebPlaybackSessionService,
    user_id: &str,
) -> Result<(), Response> {
    let result = service
        .check_playback_admission(user_id)
        .await
        .map_err(|error| web_playback_error(headers, error))?
        .ok_or_else(|| StatusCode::NOT_FOUND.into_response())?;
    if let Some(response) = playback_admission_error(headers, &result) {
        return Err(response);
    }
    Ok(())
}

pub(super) async fn admit_emby_direct_playback(
    headers: &HeaderMap,
    state: &AppState,
    user_id: &str,
    play_session_id: &str,
) -> Result<(), Response> {
    let Some(service) = state.web_playback.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE.into_response());
    };
    let result = service
        .admit_playback_session(user_id, play_session_id)
        .await
        .map_err(|error| web_playback_error(headers, error))?
        .ok_or_else(|| StatusCode::NOT_FOUND.into_response())?;
    if let Some(response) = playback_admission_error(headers, &result) {
        return Err(response);
    }
    Ok(())
}

async fn admit_web_playback_request(
    headers: &HeaderMap,
    service: &crate::application::playback::session::WebPlaybackSessionService,
    session: &StoredWebPlaybackSession,
) -> Result<(), Response> {
    let result = service
        .admit_playback_session(&session.user_id, &session.play_session_id)
        .await
        .map_err(|error| web_playback_error(headers, error))?
        .ok_or_else(|| StatusCode::NOT_FOUND.into_response())?;
    if let Some(response) = playback_admission_error(headers, &result) {
        let _ = service.stop(&session.id, &session.user_id).await;
        return Err(response);
    }
    Ok(())
}

async fn create_web_playback_session_json(
    headers: &HeaderMap,
    state: &AppState,
    user: &UserRecord,
    item_id: &str,
    source: &crate::storage::StoredPlaybackSource,
    mut capabilities: PlaybackCapabilities,
    audio_stream_index: Option<i64>,
) -> Result<Value, Response> {
    let source_kind = match source.source_kind.as_str() {
        "LOCAL_FILE" => PlaybackSourceKind::LocalFile,
        "STRM_URL" => PlaybackSourceKind::Strm,
        _ => return Err(StatusCode::NOT_IMPLEMENTED.into_response()),
    };
    let Some(service) = state.web_playback.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE.into_response());
    };
    check_web_playback_admission(headers, service, &user.id.to_string()).await?;
    if source_kind == PlaybackSourceKind::LocalFile && audio_stream_index.is_some() {
        // Direct Play cannot select a non-first source audio stream. Reuse
        // the existing HLS plan so the selected track is fixed in the
        // server-side playback session.
        capabilities.direct_play = false;
    }
    let user_id = user.id.to_string();
    let created = service
        .create(CreateWebPlaybackSession {
            user_id: &user_id,
            is_admin: user.is_admin,
            item_id,
            media_source_id: &source.source_id,
            play_session_prefix: "lux-web",
            source_kind,
            capabilities,
        })
        .await
        .map_err(|error| web_playback_error(headers, error))?;
    if !created.id.is_empty() {
        let (device_id, client_name) =
            match web_playback_authorization_context(headers, state, &user_id).await {
                Ok(context) => context,
                Err(error) => {
                    let _ = service.stop(&created.id, &user_id).await;
                    return Err(web_playback_error(headers, error));
                }
            };
        if let Err(error) = service
            .set_authorization_context(
                &created.id,
                &user_id,
                device_id.as_deref(),
                client_name.as_deref(),
            )
            .await
        {
            let _ = service.stop(&created.id, &user_id).await;
            return Err(web_playback_error(headers, error));
        }
    }
    if let WebPlaybackPlan::ServerHls { tier } = &created.plan {
        if source.source_kind != "LOCAL_FILE" {
            let _ = service.stop(&created.id, &user_id).await;
            return Err(StatusCode::NOT_IMPLEMENTED.into_response());
        }
        let input = match canonical_local_media_path(&source.root_path, &source.relative_path).await
        {
            Ok(path) => path,
            Err(LocalPathError::Missing) => {
                let _ = service.stop(&created.id, &user_id).await;
                return Err(StatusCode::NOT_FOUND.into_response());
            }
            Err(LocalPathError::Forbidden) => {
                let _ = service.stop(&created.id, &user_id).await;
                return Err(StatusCode::FORBIDDEN.into_response());
            }
        };
        if let Err(error) = service
            .start_hls_with_audio(&created.id, *tier, &input, None, None, audio_stream_index)
            .await
        {
            let _ = service.stop(&created.id, &user_id).await;
            return Err(web_playback_error(headers, error));
        }
        // Persist what the server actually produces so the admin dashboard and the player's
        // information panel describe the same session instead of guessing.
        let output = web_transcoding_output(*tier, HlsSegmentContainer::FragmentedMp4);
        if let Err(error) = service
            .set_transcoding_details(&created.id, &user_id, &output)
            .await
        {
            let _ = service.stop(&created.id, &user_id).await;
            return Err(web_playback_error(headers, error));
        }
    }
    let plan = match &created.plan {
        WebPlaybackPlan::Direct => {
            let proxy_url = if source.source_kind == "STRM_URL"
                && source.external_url.as_deref().is_some_and(|target| {
                    matches!(
                        classify_strm_target(target).kind,
                        StrmTargetKind::Url | StrmTargetKind::Path
                    )
                }) {
                Some(super::emby_catalog::emby_media_source_stream_url_parts(
                    item_id,
                    &source.source_id,
                    &source.source_kind,
                    source.container.as_deref(),
                ))
            } else {
                None
            };
            json!({
                "type": "DIRECT",
                "url": web_playback_resource_url(service, &created.id, "direct", web_playback_resource_expiry()),
                "proxyUrl": proxy_url,
                "rangeUrl": (source.source_kind == "STRM_URL"
                    && source.external_url.as_deref().is_some_and(|target| {
                        matches!(classify_strm_target(target).kind, StrmTargetKind::Url)
                    }))
                    .then(|| web_playback_range_url(service, &created.id, web_playback_resource_expiry()))
                    .flatten(),
            })
        }
        WebPlaybackPlan::ServerHls { tier } => json!({
            "type": "SERVER_HLS",
            "manifestUrl": web_playback_hls_url(
                service,
                &created.id,
                "index.m3u8",
                created.expires_at,
            ),
            "tier": tier.number(),
        }),
        WebPlaybackPlan::Unsupported { reason } => json!({
            "type": "UNSUPPORTED",
            "reason": reason.to_string(),
        }),
    };
    Ok(json!({
        "sessionId": (!created.id.is_empty()).then_some(created.id),
        "playSessionId": (!created.play_session_id.is_empty()).then_some(created.play_session_id),
        "tier": created.plan.tier().number(),
        "expiresAt": created.expires_at,
        "plan": plan,
        "sourceId": created.media_source_id,
    }))
}

async fn validate_web_audio_stream(
    state: &AppState,
    principal: AccessPrincipal,
    item_id: &str,
    source_id: &str,
    requested: Option<i64>,
) -> Result<Option<i64>, Response> {
    let Some(index) = requested else {
        return Ok(None);
    };
    if index < 0 {
        return Err(StatusCode::BAD_REQUEST.into_response());
    }
    let Some(catalog) = state.catalog.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE.into_response());
    };
    let item = match catalog.find_item(principal, item_id).await {
        Ok(Some(item)) => item,
        Ok(None) => return Err(StatusCode::NOT_FOUND.into_response()),
        Err(CatalogError::Storage(_)) => {
            return Err(StatusCode::SERVICE_UNAVAILABLE.into_response());
        }
        Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => {
            return Err(StatusCode::NOT_FOUND.into_response());
        }
    };
    let Some(source) = item
        .media_sources
        .iter()
        .find(|source| source.id == source_id)
    else {
        return Err(StatusCode::NOT_FOUND.into_response());
    };
    if source
        .streams
        .iter()
        .any(|stream| stream.index == index && stream.stream_type.eq_ignore_ascii_case("AUDIO"))
    {
        Ok(Some(index))
    } else {
        Err(StatusCode::BAD_REQUEST.into_response())
    }
}

pub(super) async fn lux_create_web_playback_session(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<WebPlaybackSessionRequest>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_web_csrf(&headers, &state).await {
        return response;
    }
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let source = match access
        .authorized_playback_source(principal, &request.item_id, Some(&request.source_id))
        .await
    {
        Ok(Some(source)) => source,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let audio_stream_index = match validate_web_audio_stream(
        &state,
        principal,
        &request.item_id,
        &request.source_id,
        request.audio_stream_index,
    )
    .await
    {
        Ok(index) => index,
        Err(response) => return response,
    };
    match create_web_playback_session_json(
        &headers,
        &state,
        &user,
        &request.item_id,
        &source,
        request.capabilities.into(),
        audio_stream_index,
    )
    .await
    {
        Ok(session) => Json(session).into_response(),
        Err(response) => response,
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WebPlaybackBootstrapRequest {
    item_id: String,
    source_id: Option<String>,
    #[serde(default)]
    capabilities: WebPlaybackCapabilitiesRequest,
    #[serde(default)]
    audio_stream_index: Option<i64>,
}

pub(super) async fn lux_create_web_playback_bootstrap(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(request): Json<WebPlaybackBootstrapRequest>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_web_csrf(&headers, &state).await {
        return response;
    }
    let Some(catalog) = state.catalog.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let principal = AccessPrincipal::new(user.id, user.is_admin);
    let source_id = request
        .source_id
        .as_deref()
        .filter(|value| !value.is_empty());
    let (item_result, source_result) = tokio::join!(
        catalog.find_item(principal, &request.item_id),
        access.authorized_playback_source(principal, &request.item_id, source_id),
    );
    let item = match item_result {
        Ok(Some(item)) => item,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(CatalogError::Storage(_)) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(CatalogError::LibraryNotFound | CatalogError::AccessDenied) => {
            return StatusCode::NOT_FOUND.into_response();
        }
    };
    let source = match source_result {
        Ok(Some(source)) => source,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let user_id = user.id.to_string();
    let (detail_result, active_session_result) = tokio::join!(
        load_lux_item_detail(&state, database, &item, &user_id),
        database.find_active_playback_session(&user_id, &request.item_id),
    );
    let detail = match detail_result {
        Ok(detail) => detail,
        Err(()) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let active_session = match active_session_result {
        Ok(session) => session,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let audio_stream_index = match validate_web_audio_stream(
        &state,
        principal,
        &request.item_id,
        &source.source_id,
        request.audio_stream_index,
    )
    .await
    {
        Ok(index) => index,
        Err(response) => return response,
    };
    let session = match create_web_playback_session_json(
        &headers,
        &state,
        &user,
        &request.item_id,
        &source,
        request.capabilities.into(),
        audio_stream_index,
    )
    .await
    {
        Ok(session) => session,
        Err(response) => return response,
    };
    let playback = json!({
        "itemId": request.item_id,
        "positionTicks": detail.user_state.as_ref().map(|value| value.position_ticks).unwrap_or_default(),
        "isPlayed": detail.user_state.as_ref().map(|value| value.is_played).unwrap_or(false),
        "isFavorite": detail.user_state.as_ref().map(|value| value.is_favorite).unwrap_or(false),
        "playCount": detail.user_state.as_ref().map(|value| value.play_count).unwrap_or_default(),
        "state": active_session.as_ref().map(|value| value.state.as_str()),
        "isPaused": active_session.as_ref().map(|value| value.is_paused).unwrap_or(false),
        "lastEventAt": active_session.as_ref().map(|value| value.last_event_at),
    });
    Json(json!({
        "item": detail.body,
        "playback": playback,
        "session": session,
    }))
    .into_response()
}

pub(super) async fn lux_web_playback_direct(
    headers: HeaderMap,
    method: Method,
    Path(session_id): Path<String>,
    Query(query): Query<WebPlaybackResourceQuery>,
    State(state): State<AppState>,
) -> Response {
    let Some(service) = state.web_playback.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let session = match authorize_web_playback_resource(
        &headers,
        &state,
        service,
        &session_id,
        "direct",
        query.expires,
        &query.signature,
    )
    .await
    {
        Ok(session) => session,
        Err(error) => return web_playback_error(&headers, error),
    };
    if session.plan != "DIRECT" {
        return StatusCode::NOT_FOUND.into_response();
    }
    if method == Method::GET
        && let Err(response) = admit_web_playback_request(&headers, service, &session).await
    {
        return response;
    }
    let principal = match playback_resource_principal(state.database.as_ref(), &session).await {
        Ok(principal) => principal,
        Err(status) => return status.into_response(),
    };
    let response = serve_media_file(
        &state,
        principal,
        &headers,
        &method,
        &session.item_id,
        session.media_source_id.as_deref(),
        None,
    )
    .await;
    if method == Method::GET && !response.status().is_success() {
        let _ = service.stop(&session_id, &session.user_id).await;
    }
    response
}

pub(super) async fn lux_web_playback_range(
    headers: HeaderMap,
    method: Method,
    Path(session_id): Path<String>,
    Query(query): Query<WebPlaybackResourceQuery>,
    State(state): State<AppState>,
) -> Response {
    let Some(service) = state.web_playback.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let session = match authorize_web_playback_resource(
        &headers,
        &state,
        service,
        &session_id,
        "range",
        query.expires,
        &query.signature,
    )
    .await
    {
        Ok(session) => session,
        Err(error) => return web_playback_error(&headers, error),
    };
    if session.plan != "DIRECT" {
        return StatusCode::NOT_FOUND.into_response();
    }
    if method != Method::GET {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let Some(range) = headers.get("range").and_then(|value| value.to_str().ok()) else {
        return StatusCode::RANGE_NOT_SATISFIABLE.into_response();
    };
    let principal = match playback_resource_principal(state.database.as_ref(), &session).await {
        Ok(principal) => principal,
        Err(status) => return status.into_response(),
    };
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let source = match access
        .authorized_playback_source(
            principal,
            &session.item_id,
            session.media_source_id.as_deref(),
        )
        .await
    {
        Ok(Some(source)) => source,
        Ok(None) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    if source.source_kind != "STRM_URL" {
        return StatusCode::NOT_IMPLEMENTED.into_response();
    }
    let Some(external_url) = source.external_url.as_deref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !matches!(classify_strm_target(external_url).kind, StrmTargetKind::Url) {
        return StatusCode::NOT_IMPLEMENTED.into_response();
    }
    let Some(resolver) = state.strm_playback.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if let Err(response) = admit_web_playback_request(&headers, service, &session).await {
        return response;
    }
    let user_agent = headers
        .get("user-agent")
        .and_then(|value| value.to_str().ok());
    let result = match resolver.fetch_range(external_url, range, user_agent).await {
        Ok(result) => result,
        Err(crate::application::strm_playback::StrmPlaybackError::InvalidRange) => {
            let _ = service.stop(&session_id, &session.user_id).await;
            return StatusCode::RANGE_NOT_SATISFIABLE.into_response();
        }
        Err(crate::application::strm_playback::StrmPlaybackError::UnsupportedStatus(status))
            if status == 401 || status == 403 =>
        {
            let _ = service.stop(&session_id, &session.user_id).await;
            return StatusCode::UNAUTHORIZED.into_response();
        }
        Err(_) => {
            let _ = service.stop(&session_id, &session.user_id).await;
            return StatusCode::BAD_GATEWAY.into_response();
        }
    };
    if let Some(strm_probe) = state.strm_probe.as_ref() {
        strm_probe.schedule_playback_probe(&session.item_id, &source.source_id);
    }
    let mut response = Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header("Accept-Ranges", "bytes")
        .header("Content-Range", result.content_range)
        .header("Content-Length", result.content_length)
        .header("Cache-Control", "private, no-store");
    if let Some(content_type) = result.content_type {
        response = response.header("Content-Type", content_type);
    }
    if let Some(etag) = result.etag {
        response = response.header("ETag", etag);
    }
    response
        .body(Body::from(result.body))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

pub(super) async fn lux_web_playback_hls(
    headers: HeaderMap,
    method: Method,
    Path((session_id, asset)): Path<(String, String)>,
    Query(query): Query<WebPlaybackResourceQuery>,
    State(state): State<AppState>,
) -> Response {
    let Some(service) = state.web_playback.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let resource = format!("hls:{asset}");
    let session = match authorize_web_playback_resource(
        &headers,
        &state,
        service,
        &session_id,
        &resource,
        query.expires,
        &query.signature,
    )
    .await
    {
        Ok(session) => session,
        Err(error) => return web_playback_error(&headers, error),
    };
    if session.plan != "SERVER_HLS" {
        return StatusCode::NOT_FOUND.into_response();
    }
    if matches!(asset.as_str(), "index.m3u8") {
        let path = match service.wait_for_hls_manifest(&session_id).await {
            Ok(path) => path,
            Err(error) => return web_playback_error(&headers, error),
        };
        let Ok(bytes) = fs::read(path).await else {
            if method == Method::GET {
                let _ = service.stop(&session_id, &session.user_id).await;
            }
            return StatusCode::NOT_FOUND.into_response();
        };
        let Ok(manifest) = String::from_utf8(bytes) else {
            if method == Method::GET {
                let _ = service.stop(&session_id, &session.user_id).await;
            }
            return StatusCode::BAD_GATEWAY.into_response();
        };
        let Some(manifest) = rewrite_hls_manifest(&manifest, |asset| {
            web_playback_hls_url(service, &session_id, asset, web_playback_resource_expiry())
        }) else {
            if method == Method::GET {
                let _ = service.stop(&session_id, &session.user_id).await;
            }
            return StatusCode::BAD_GATEWAY.into_response();
        };
        if method == Method::GET
            && let Err(response) = admit_web_playback_request(&headers, service, &session).await
        {
            return response;
        }
        return Response::builder()
            .status(StatusCode::OK)
            .header("Cache-Control", "private, no-store")
            .header("Content-Type", "application/vnd.apple.mpegurl")
            .header("Content-Length", manifest.len())
            .body(if method == Method::HEAD {
                Body::empty()
            } else {
                Body::from(manifest)
            })
            .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response());
    }
    let path = match service.hls_asset_path(&session_id, &asset).await {
        Ok(path) => path,
        Err(error) => {
            if method == Method::GET {
                let _ = service.stop(&session_id, &session.user_id).await;
            }
            return web_playback_error(&headers, error);
        }
    };
    match service.hls_within_quota(&session_id).await {
        Ok(true) => {}
        Ok(false) => {
            let _ = service.stop(&session_id, &session.user_id).await;
            return StatusCode::INSUFFICIENT_STORAGE.into_response();
        }
        Err(error) => return web_playback_error(&headers, error),
    }
    let metadata = match fs::metadata(&path).await {
        Ok(metadata) if metadata.is_file() => metadata,
        _ => {
            if method == Method::GET {
                let _ = service.stop(&session_id, &session.user_id).await;
            }
            return StatusCode::NOT_FOUND.into_response();
        }
    };
    if method == Method::GET
        && let Err(response) = admit_web_playback_request(&headers, service, &session).await
    {
        return response;
    }
    let content_type = if asset.ends_with(".m4s") || asset.ends_with(".mp4") {
        "video/mp4"
    } else {
        "application/octet-stream"
    };
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        let Ok(file) = fs::File::open(path).await else {
            let _ = service.stop(&session_id, &session.user_id).await;
            return StatusCode::NOT_FOUND.into_response();
        };
        Body::from_stream(tokio_util::io::ReaderStream::new(file))
    };
    Response::builder()
        .status(StatusCode::OK)
        .header("Cache-Control", "private, no-store")
        .header("Content-Type", content_type)
        .header("Content-Length", metadata.len())
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

pub(super) fn rewrite_hls_manifest(
    manifest: &str,
    mut url_for_asset: impl FnMut(&str) -> Option<String>,
) -> Option<String> {
    let mut output = String::with_capacity(manifest.len() + 256);
    for line in manifest.lines() {
        let mut rewritten = line.to_owned();
        if let Some(uri_start) = line.find("URI=\"") {
            let value_start = uri_start + 5;
            let value_end = line[value_start..].find('\"')? + value_start;
            let asset = &line[value_start..value_end];
            let url = url_for_asset(asset)?;
            rewritten.replace_range(value_start..value_end, &url);
        } else if !line.trim_start().starts_with('#') && !line.trim().is_empty() {
            let asset = line.trim();
            let url = url_for_asset(asset)?;
            rewritten = line.replace(asset, &url);
        }
        output.push_str(&rewritten);
        output.push('\n');
    }
    Some(output)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct WebPlaybackEventRequest {
    event_id: String,
    sequence: i64,
    state: LuxPlaybackState,
    position_ticks: i64,
    duration_ticks: Option<i64>,
}

pub(super) async fn lux_web_playback_event(
    headers: HeaderMap,
    Path(session_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<WebPlaybackEventRequest>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_web_csrf(&headers, &state).await {
        return response;
    }
    let Some(service) = state.web_playback.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let (claim, session) = match service
        .claim_event(WebPlaybackEvent {
            session_id: &session_id,
            user_id: &user.id.to_string(),
            event_id: &request.event_id,
            sequence: request.sequence,
            state: request.state.as_str(),
            position_ticks: request.position_ticks,
            duration_ticks: request.duration_ticks,
        })
        .await
    {
        Ok(result) => result,
        Err(error) => return web_playback_error(&headers, error),
    };
    let Some(session) = session else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if claim == WebPlaybackEventClaim::Accepted {
        let Some(database) = state.database.as_ref() else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        let user_id = user.id.to_string();
        let previous_session = match database
            .find_playback_session(&user_id, &session.play_session_id)
            .await
        {
            Ok(session) => session,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        let activity_event =
            playback_activity_event_type(previous_session.as_ref(), request.state.as_str());
        let resumed = playback_resumed(previous_session.as_ref(), request.state.as_str());
        let occurred_at = current_unix_timestamp();
        let webhook_event = webhook_event_type_for_playback(
            activity_event,
            should_publish_playback_progress(
                previous_session.as_ref(),
                request.state.as_str(),
                request.position_ticks,
                occurred_at,
            ),
        );
        let remote_ip = request_client_ip(&headers, &state).await;
        let activity_remote_ip = remote_ip.as_deref().or_else(|| {
            previous_session
                .as_ref()
                .and_then(|session| session.remote_ip.as_deref())
        });
        let played_percent = match database.user_played_percent(&user_id).await {
            Ok(value) => value,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
        if database
            .record_playback_event(NewPlaybackEvent {
                user_id: &session.user_id,
                item_id: &session.item_id,
                media_source_id: session.media_source_id.as_deref(),
                play_session_id: &session.play_session_id,
                device_id: "lux-web",
                client: Some("Lux"),
                device_name: Some("Web"),
                client_version: None,
                device_type: Some("Web"),
                remote_ip: remote_ip.as_deref(),
                state: request.state.as_str(),
                position_ticks: request.position_ticks,
                duration_ticks: request.duration_ticks,
                played_percent,
                is_paused: matches!(request.state, LuxPlaybackState::Paused),
            })
            .await
            .is_err()
        {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        if database
            .sync_played_container_states(&session.user_id, &session.item_id)
            .await
            .is_err()
        {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        state.user_events.publish_home_coalesced().await;
        if let Some(event_type) = activity_event {
            record_activity_event(
                Some(database),
                &state.admin_events,
                &user_id,
                event_type,
                Some(&session.item_id),
                json!({
                    "client": "Lux",
                    "deviceType": "Web",
                    "deviceName": "Web",
                    "state": request.state.as_str(),
                    "remoteIp": activity_remote_ip,
                }),
            )
            .await;
        }
        if let Some(event_type) = webhook_event {
            publish_playback_webhook(
                &state,
                event_type,
                occurred_at,
                &session.item_id,
                session.media_source_id.as_deref(),
                &session.play_session_id,
                request.state.as_str(),
                request.position_ticks,
                request.duration_ticks,
                matches!(request.state, LuxPlaybackState::Paused),
                Some("Lux"),
                Some("Web"),
                Some("Web"),
                None,
                &user.display_name,
                AccessPrincipal::new(user.id, user.is_admin),
                web_playback_notification_method(&session),
                if event_type_is_stopped(event_type) {
                    activity_remote_ip
                } else {
                    None
                },
                resumed,
            )
            .await;
        }
    }
    if matches!(
        claim,
        WebPlaybackEventClaim::Accepted | WebPlaybackEventClaim::Duplicate
    ) {
        let admission_result = if matches!(request.state, LuxPlaybackState::Stopped) {
            service.stop(&session.id, &session.user_id).await
        } else {
            service
                .renew_playback_session_admission(&session.user_id, &session.play_session_id)
                .await
                .map(|_| ())
        };
        if let Err(error) = admission_result {
            return web_playback_error(&headers, error);
        }
    }
    Json(json!({
        "accepted": claim == WebPlaybackEventClaim::Accepted,
        "duplicate": claim == WebPlaybackEventClaim::Duplicate,
        "stale": claim == WebPlaybackEventClaim::Stale,
    }))
    .into_response()
}

fn web_playback_notification_method(
    session: &crate::storage::StoredWebPlaybackSession,
) -> Option<&'static str> {
    if session.plan != "SERVER_HLS" {
        return None;
    }
    if session.tier <= i64::from(ServerTier::Remux.number()) {
        Some("DirectStream")
    } else {
        Some("Transcode")
    }
}

pub(super) async fn lux_web_playback_heartbeat(
    headers: HeaderMap,
    Path(session_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_web_csrf(&headers, &state).await {
        return response;
    }
    let Some(service) = state.web_playback.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.heartbeat(&session_id, &user.id.to_string()).await {
        Ok(expires_at) => {
            Json(json!({ "sessionId": session_id, "expiresAt": expires_at })).into_response()
        }
        Err(error) => web_playback_error(&headers, error),
    }
}

/// Read-only live statistics for the player's information panel.
///
/// This is a `GET`, so it carries no CSRF token and performs no writes: it never extends the
/// session, never starts FFmpeg and never admits a playback request.
pub(super) async fn lux_web_playback_stats(
    headers: HeaderMap,
    Path(session_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    let Some(service) = state.web_playback.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.stats(&session_id, &user.id.to_string()).await {
        Ok(stats) => Json(web_playback_stats_json(&stats)).into_response(),
        Err(error) => web_playback_error(&headers, error),
    }
}

pub(super) fn web_playback_stats_json(
    stats: &crate::application::playback::session::WebPlaybackStats,
) -> Value {
    let session = &stats.session;
    json!({
        "sessionId": session.id,
        "playSessionId": session.play_session_id,
        "sourceId": session.media_source_id,
        "plan": session.plan,
        "tier": session.tier,
        "state": session.state,
        "expiresAt": session.expires_at,
        "output": {
            "container": session.transcoding_container,
            "videoCodec": session.video_codec,
            "audioCodec": session.audio_codec,
            "videoBitrate": session.video_bitrate,
            "audioBitrate": session.audio_bitrate,
        },
        "progress": stats.progress.as_ref().map(|progress| json!({
            "generation": progress.generation,
            "frame": progress.frame,
            "fps": progress.fps,
            "bitrateKbps": progress.bitrate_kbps,
            "totalSize": progress.total_size,
            "outTimeSeconds": progress
                .out_time_us
                .map(|micros| micros as f64 / 1_000_000.0),
            "speed": progress.speed,
            "duplicatedFrames": progress.duplicated_frames,
            "droppedFrames": progress.dropped_frames,
            "finished": progress.finished,
            "updatedAt": progress.updated_at_ms,
        })),
    })
}

pub(super) async fn lux_delete_web_playback_session(
    headers: HeaderMap,
    Path(session_id): Path<String>,
    State(state): State<AppState>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_web_csrf(&headers, &state).await {
        return response;
    }
    let Some(service) = state.web_playback.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.stop(&session_id, &user.id.to_string()).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => web_playback_error(&headers, error),
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(super) enum LuxPlaybackState {
    #[default]
    Playing,
    Paused,
    Stopped,
}

impl LuxPlaybackState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Playing => "PLAYING",
            Self::Paused => "PAUSED",
            Self::Stopped => "STOPPED",
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LuxProgressRequest {
    position_ticks: i64,
    duration_ticks: Option<i64>,
    #[serde(default)]
    state: LuxPlaybackState,
    #[serde(default)]
    play_method: Option<String>,
}

pub(super) async fn lux_post_progress(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<LuxProgressRequest>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_web_csrf(&headers, &state).await {
        return response;
    }
    if request.position_ticks < 0 || request.duration_ticks.is_some_and(|duration| duration < 0) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match access
        .can_view_item(AccessPrincipal::new(user.id, user.is_admin), &item_id)
        .await
    {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let user_id = user.id.to_string();
    let play_session_id = format!("lux-web:{user_id}:{item_id}");
    let playback_state = request.state;
    let previous_session = match database
        .find_playback_session(&user_id, &play_session_id)
        .await
    {
        Ok(session) => session,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let activity_event =
        playback_activity_event_type(previous_session.as_ref(), playback_state.as_str());
    let resumed = playback_resumed(previous_session.as_ref(), playback_state.as_str());
    let occurred_at = current_unix_timestamp();
    let webhook_event = webhook_event_type_for_playback(
        activity_event,
        should_publish_playback_progress(
            previous_session.as_ref(),
            playback_state.as_str(),
            request.position_ticks,
            occurred_at,
        ),
    );
    let remote_ip = request_client_ip(&headers, &state).await;
    let activity_remote_ip = remote_ip.as_deref().or_else(|| {
        previous_session
            .as_ref()
            .and_then(|session| session.remote_ip.as_deref())
    });
    match database
        .record_playback_event(NewPlaybackEvent {
            user_id: &user_id,
            item_id: &item_id,
            media_source_id: None,
            play_session_id: &play_session_id,
            device_id: "lux-web",
            client: Some("Lux"),
            device_name: Some("Web"),
            client_version: None,
            device_type: Some("Web"),
            remote_ip: remote_ip.as_deref(),
            state: playback_state.as_str(),
            position_ticks: request.position_ticks,
            duration_ticks: request.duration_ticks,
            played_percent: match database.user_played_percent(&user_id).await {
                Ok(value) => value,
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            },
            is_paused: matches!(playback_state, LuxPlaybackState::Paused),
        })
        .await
    {
        Ok(()) => {
            if database
                .sync_played_container_states(&user_id, &item_id)
                .await
                .is_err()
            {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            state.user_events.publish_home_coalesced().await;
            if let Some(event_type) = activity_event {
                record_activity_event(
                    Some(database),
                    &state.admin_events,
                    &user_id,
                    event_type,
                    Some(&item_id),
                    json!({
                        "client": "Lux",
                        "deviceType": "Web",
                        "deviceName": "Web",
                        "state": playback_state.as_str(),
                        "remoteIp": activity_remote_ip,
                    }),
                )
                .await;
            }
            if let Some(event_type) = webhook_event {
                publish_playback_webhook(
                    &state,
                    event_type,
                    occurred_at,
                    &item_id,
                    None,
                    &play_session_id,
                    playback_state.as_str(),
                    request.position_ticks,
                    request.duration_ticks,
                    matches!(playback_state, LuxPlaybackState::Paused),
                    Some("Lux"),
                    Some("Web"),
                    Some("Web"),
                    None,
                    &user.display_name,
                    AccessPrincipal::new(user.id, user.is_admin),
                    request.play_method.as_deref(),
                    if event_type_is_stopped(event_type) {
                        activity_remote_ip
                    } else {
                        None
                    },
                    resumed,
                )
                .await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn emby_mark_played(
    headers: HeaderMap,
    Path((user_id, item_id)): Path<(String, String)>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    handle_emby_user_flag(headers, user_id, item_id, query, state, true, true).await
}

pub(super) async fn emby_unmark_played(
    headers: HeaderMap,
    Path((user_id, item_id)): Path<(String, String)>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    handle_emby_user_flag(headers, user_id, item_id, query, state, true, false).await
}

pub(super) async fn emby_mark_favorite(
    headers: HeaderMap,
    Path((user_id, item_id)): Path<(String, String)>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    handle_emby_user_flag(headers, user_id, item_id, query, state, false, true).await
}

pub(super) async fn emby_unmark_favorite(
    headers: HeaderMap,
    Path((user_id, item_id)): Path<(String, String)>,
    Query(query): Query<EmbyTokenQuery>,
    State(state): State<AppState>,
) -> Response {
    handle_emby_user_flag(headers, user_id, item_id, query, state, false, false).await
}

pub(super) async fn handle_emby_user_flag(
    headers: HeaderMap,
    user_id: String,
    item_id: String,
    query: EmbyTokenQuery,
    state: AppState,
    played: bool,
    value: bool,
) -> Response {
    let auth_principal =
        match require_emby_principal(&headers, &state, query.api_key.as_deref()).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    let principal =
        match emby_access_principal_for_target(&state, &auth_principal, Some(&user_id)).await {
            Ok(principal) => principal,
            Err(status) => return status.into_response(),
        };
    let target_user_id = principal.user_id_string();
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let item_id = emby_internal_id(&item_id);
    match access.can_view_item(principal, &item_id).await {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let result = if played {
        database
            .set_user_item_played(&target_user_id, &item_id, value)
            .await
    } else {
        database
            .set_user_item_favorite(&target_user_id, &item_id, value)
            .await
    };
    match result {
        Ok(()) => {
            if played
                && database
                    .sync_played_container_states(&target_user_id, &item_id)
                    .await
                    .is_err()
            {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            let user_state = match database
                .find_user_item_state(&target_user_id, &item_id)
                .await
            {
                Ok(user_state) => user_state,
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            };
            state.user_events.publish(if played {
                UserEventScope::Item
            } else {
                UserEventScope::Favorites
            });
            Json(emby_user_item_data_json(&item_id, user_state.as_ref())).into_response()
        }
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

/// Emby's PlayedItems and FavoriteItems mutations return UserItemDataDto, not
/// an empty 204 response. Some clients deserialize this response immediately
/// after toggling the state, so keep the response shape aligned with the
/// public Emby contract.
fn emby_user_item_data_json(
    item_id: &str,
    user_state: Option<&crate::storage::StoredUserItemState>,
) -> Value {
    let mut object = serde_json::Map::from_iter([
        (
            "PlaybackPositionTicks".to_owned(),
            json!(
                user_state
                    .map(|state| state.position_ticks)
                    .unwrap_or_default()
            ),
        ),
        (
            "PlayCount".to_owned(),
            json!(user_state.map(|state| state.play_count).unwrap_or_default()),
        ),
        (
            "IsFavorite".to_owned(),
            json!(user_state.map(|state| state.is_favorite).unwrap_or(false)),
        ),
        (
            "Played".to_owned(),
            json!(user_state.map(|state| state.is_played).unwrap_or(false)),
        ),
        (
            "HideFromResume".to_owned(),
            json!(
                user_state
                    .map(|state| state.hide_from_resume)
                    .unwrap_or(false)
            ),
        ),
        ("ItemId".to_owned(), json!(emby_public_id(item_id))),
        ("Key".to_owned(), json!(emby_public_id(item_id))),
    ]);
    if let Some(last_played_at) = user_state.and_then(|state| state.last_played_at)
        && let Some(last_played_date) = emby_timestamp(last_played_at)
    {
        object.insert("LastPlayedDate".to_owned(), json!(last_played_date));
    }
    Value::Object(object)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LuxFavoriteRequest {
    pub(super) favorite: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LuxPlayedRequest {
    pub(super) played: bool,
}

pub(super) async fn lux_set_favorite(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<LuxFavoriteRequest>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_web_csrf(&headers, &state).await {
        return response;
    }
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match access
        .can_view_item(AccessPrincipal::new(user.id, user.is_admin), &item_id)
        .await
    {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database
        .set_user_item_favorite(&user.id.to_string(), &item_id, request.favorite)
        .await
    {
        Ok(()) => {
            state.user_events.publish(UserEventScope::Favorites);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub(super) async fn lux_set_played(
    headers: HeaderMap,
    Path(item_id): Path<String>,
    State(state): State<AppState>,
    Json(request): Json<LuxPlayedRequest>,
) -> Response {
    let user = match require_web_user(&headers, &state).await {
        Ok(user) => user,
        Err(response) => return response,
    };
    if let Err(response) = require_web_csrf(&headers, &state).await {
        return response;
    }
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match access
        .can_view_item(AccessPrincipal::new(user.id, user.is_admin), &item_id)
        .await
    {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match database
        .set_user_item_played(&user.id.to_string(), &item_id, request.played)
        .await
    {
        Ok(()) => {
            if database
                .sync_played_container_states(&user.id.to_string(), &item_id)
                .await
                .is_err()
            {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            state.user_events.publish(UserEventScope::Item);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

#[cfg(test)]
mod emby_playback_tests {
    use super::{
        EmbyPlaybackInfoRequest, emby_force_transcode_from_raw, emby_hls_asset_kind,
        emby_start_time_hint, is_emby_generation_init_asset, parse_emby_playback_info_request,
        playback_resumed,
    };
    use crate::application::catalog::{CatalogSource, CatalogStream};
    use crate::application::playback::decision::{
        PlaybackDecisionInput, PlaybackPlan, PlaybackSourceKind, ServerTier, choose_plan,
    };
    use crate::application::playback::hls::HlsSegmentContainer;
    use axum::{body::Bytes, extract::RawQuery};
    use std::collections::BTreeMap;

    #[tokio::test]
    async fn signed_playback_resources_follow_the_current_administrator_role()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config = crate::config::Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        };
        let database = crate::storage::Database::connect(&config).await?;
        let users = crate::auth::users::UserStore::new(database.clone())?;
        let user = users
            .create_user(
                "signed-resource",
                "Signed Resource",
                "password-for-tests",
                true,
            )
            .await?;
        let user_id = user.id.to_string();
        let library = crate::application::libraries::LibraryService::new(database.clone())
            .create_library("Signed playback", crate::library::LibraryKind::Movie, false)
            .await?;
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('signed-resource-item', ?, 'MOVIE', 'Signed', 'signed', 'LOCAL_CONFIRMED')",
        )
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        database
            .insert_web_playback_session(crate::storage::NewWebPlaybackSession {
                id: "signed-resource-session",
                user_id: &user_id,
                item_id: "signed-resource-item",
                media_source_id: None,
                play_session_id: "lux-web:signed-resource-session",
                tier: i64::from(ServerTier::Remux.number()),
                plan: "DIRECT",
                temp_dir: None,
                is_admin: true,
                expires_at: now + 900,
                now,
            })
            .await?;
        let session = database
            .find_web_playback_session("signed-resource-session")
            .await?
            .ok_or("playback session")?;
        assert!(session.is_admin);

        let principal = super::playback_resource_principal(Some(&database), &session)
            .await
            .expect("active administrator keeps the resource");
        assert!(principal.is_admin);

        users
            .create_user("other-admin", "Other Admin", "password-for-tests", true)
            .await?;
        users
            .update_user(
                &user_id,
                crate::auth::users::UserUpdate {
                    is_admin: Some(false),
                    ..Default::default()
                },
            )
            .await?;
        let principal = super::playback_resource_principal(Some(&database), &session)
            .await
            .expect("demoted administrator keeps playback access");
        assert!(
            !principal.is_admin,
            "the stored administrator snapshot must not outlive a demotion"
        );

        users
            .update_user(
                &user_id,
                crate::auth::users::UserUpdate {
                    is_disabled: Some(true),
                    ..Default::default()
                },
            )
            .await?;
        assert!(
            super::playback_resource_principal(Some(&database), &session)
                .await
                .is_err(),
            "a disabled user loses signed playback resources"
        );
        Ok(())
    }

    fn profile_source(container: &str, video_codec: &str, audio_codec: &str) -> CatalogSource {
        CatalogSource {
            id: "source-1".to_owned(),
            source_kind: "LOCAL_FILE".to_owned(),
            container: Some(container.to_owned()),
            size: None,
            external_url: None,
            edition_name: None,
            quality_label: None,
            bitrate: None,
            duration_ticks: None,
            is_default: true,
            file_name: None,
            probe_status: "READY".to_owned(),
            streams: vec![
                CatalogStream {
                    index: 0,
                    stream_type: "VIDEO".to_owned(),
                    codec: Some(video_codec.to_owned()),
                    language: None,
                    title: None,
                    is_external: false,
                    is_default: true,
                    is_forced: false,
                    details: BTreeMap::new(),
                },
                CatalogStream {
                    index: 1,
                    stream_type: "AUDIO".to_owned(),
                    codec: Some(audio_codec.to_owned()),
                    language: None,
                    title: None,
                    is_external: false,
                    is_default: true,
                    is_forced: false,
                    details: BTreeMap::new(),
                },
            ],
            chapters: Vec::new(),
        }
    }

    #[test]
    fn emby_hls_asset_kind_uses_bounded_categories() {
        assert_eq!(emby_hls_asset_kind("index.m3u8"), "manifest");
        assert_eq!(emby_hls_asset_kind("init.mp4"), "initialization");
        assert_eq!(emby_hls_asset_kind("init_000001.mp4"), "initialization");
        assert_eq!(
            emby_hls_asset_kind("generation_000001_init.mp4"),
            "initialization"
        );
        assert_eq!(emby_hls_asset_kind("segment_000001.m4s"), "segment");
        assert_eq!(emby_hls_asset_kind("segment_000001.ts"), "segment");
        assert_eq!(
            emby_hls_asset_kind("generation_000001_segment_000001.m4s"),
            "segment"
        );
        assert_eq!(
            emby_hls_asset_kind("generation_000001_segment_000001.ts"),
            "segment"
        );
        assert_eq!(emby_hls_asset_kind("unexpected.bin"), "other");
    }

    #[test]
    fn playback_info_request_selects_copy_and_transcode_tiers() {
        let remux = parse_emby_playback_info_request(&Bytes::from_static(
            br#"{
                "EnableDirectPlay": false,
                "EnableDirectStream": true,
                "EnableTranscoding": true,
                "AllowVideoStreamCopy": true,
                "AllowAudioStreamCopy": true
            }"#,
        ))
        .expect("valid PlaybackInfo request");
        assert_eq!(
            choose_plan(PlaybackDecisionInput {
                source_kind: PlaybackSourceKind::LocalFile,
                capabilities: remux.playback_capabilities_for_source(None),
            }),
            PlaybackPlan::ServerHls {
                tier: ServerTier::Remux,
            }
        );

        let audio_transcode = EmbyPlaybackInfoRequest {
            enable_direct_play: Some(false),
            enable_direct_stream: Some(true),
            enable_transcoding: Some(true),
            allow_video_stream_copy: Some(true),
            allow_audio_stream_copy: Some(false),
            ..EmbyPlaybackInfoRequest::default()
        };
        assert_eq!(
            choose_plan(PlaybackDecisionInput {
                source_kind: PlaybackSourceKind::LocalFile,
                capabilities: audio_transcode.playback_capabilities_for_source(None),
            }),
            PlaybackPlan::ServerHls {
                tier: ServerTier::AudioTranscode,
            }
        );
    }

    #[test]
    fn empty_playback_info_body_keeps_direct_play_compatibility() {
        let request = parse_emby_playback_info_request(&Bytes::new())
            .expect("empty PlaybackInfo body is valid");
        assert!(!request.requests_server_transcoding(false));
    }

    #[test]
    fn playing_after_pause_is_reported_as_resume() {
        let previous = crate::storage::StoredPlaybackSession {
            id: "session".to_owned(),
            user_id: "user".to_owned(),
            item_id: "item".to_owned(),
            media_source_id: None,
            play_session_id: "play".to_owned(),
            device_id: "device".to_owned(),
            client: None,
            device_name: None,
            client_version: None,
            device_type: None,
            remote_ip: None,
            state: "PAUSED".to_owned(),
            position_ticks: 100,
            duration_ticks: Some(1_000),
            is_paused: true,
            started_at: 1,
            last_event_at: 1,
        };

        assert!(playback_resumed(Some(&previous), "PLAYING"));
        assert!(!playback_resumed(Some(&previous), "PAUSED"));
        assert!(!playback_resumed(None, "PLAYING"));
    }

    #[test]
    fn transcode_flag_without_direct_play_flag_requests_server_transcoding() {
        let request = parse_emby_playback_info_request(&Bytes::from_static(
            br#"{
                "EnableTranscoding": true,
                "EnableDirectStream": false,
                "AllowVideoStreamCopy": false,
                "AllowAudioStreamCopy": false
            }"#,
        ))
        .expect("valid PlaybackInfo request");
        assert!(request.requests_server_transcoding(false));
    }

    #[test]
    fn device_profile_uses_hls_when_direct_play_profile_does_not_match() {
        let request = parse_emby_playback_info_request(&Bytes::from_static(
            br#"{
                "MediaSourceId": "source-1",
                "DeviceProfile": {
                    "DirectPlayProfiles": [{
                        "Container": "mp4",
                        "VideoCodec": "h264",
                        "AudioCodec": "aac",
                        "Type": "Video"
                    }],
                    "TranscodingProfiles": [{
                        "Container": "mp4",
                        "VideoCodec": "h264",
                        "AudioCodec": "aac",
                        "Protocol": "hls",
                        "Type": "Video"
                    }]
                }
            }"#,
        ))
        .expect("valid DeviceProfile request");
        let source = profile_source("mkv", "hevc", "aac");
        assert!(request.requests_server_transcoding_for_source(false, &source));
    }

    #[test]
    fn url_strm_does_not_request_server_transcoding_for_filebar_profile() {
        let request = parse_emby_playback_info_request(&Bytes::from_static(
            br#"{
                "DeviceProfile": {
                    "DirectPlayProfiles": [{
                        "Container": "mkv",
                        "VideoCodec": "h264",
                        "AudioCodec": "aac",
                        "Type": "Video"
                    }],
                    "TranscodingProfiles": [{
                        "Container": "mp4",
                        "VideoCodec": "h264",
                        "AudioCodec": "aac",
                        "Protocol": "hls",
                        "Type": "Video"
                    }]
                }
            }"#,
        ))
        .expect("valid FileBar-shaped DeviceProfile request");
        let mut source = profile_source("strm", "h264", "eac3");
        source.source_kind = "STRM_URL".to_owned();

        assert!(!request.requests_server_transcoding_for_source(false, &source));
    }

    #[test]
    fn direct_play_profile_matches_matroska_container_aliases() {
        let request = parse_emby_playback_info_request(&Bytes::from_static(
            br#"{
                "DeviceProfile": {
                    "DirectPlayProfiles": [{
                        "Container": "mkv",
                        "VideoCodec": "h264",
                        "AudioCodec": "aac",
                        "Type": "Video"
                    }],
                    "TranscodingProfiles": [{
                        "Container": "mp4",
                        "Protocol": "hls",
                        "Type": "Video"
                    }]
                }
            }"#,
        ))
        .expect("valid DeviceProfile request");
        let source = profile_source("matroska,webm", "h264", "aac");

        assert!(!request.requests_server_transcoding_for_source(false, &source));
    }

    #[test]
    fn device_profile_accepts_string_max_audio_channels() {
        let request = parse_emby_playback_info_request(&Bytes::from_static(
            br#"{
                "DeviceProfile": {
                    "TranscodingProfiles": [{
                        "MaxAudioChannels": "6"
                    }]
                }
            }"#,
        ))
        .expect("valid DeviceProfile request");

        assert_eq!(
            request
                .device_profile
                .expect("DeviceProfile")
                .transcoding_profiles[0]
                .max_audio_channels,
            Some(6)
        );
    }

    #[test]
    fn device_profile_can_override_direct_play_flag_when_transcoding_is_enabled() {
        let request = parse_emby_playback_info_request(&Bytes::from_static(
            br#"{
                "EnableDirectPlay": true,
                "EnableDirectStream": true,
                "EnableTranscoding": true,
                "DeviceProfile": {
                    "DirectPlayProfiles": [{
                        "Container": "mp4",
                        "VideoCodec": "h264",
                        "AudioCodec": "aac",
                        "Type": "Video"
                    }],
                    "TranscodingProfiles": [{
                        "Protocol": "hls",
                        "VideoCodec": "h264",
                        "AudioCodec": "aac",
                        "Type": "Video"
                    }]
                }
            }"#,
        ))
        .expect("valid PlaybackInfo request");
        let source = profile_source("mkv", "hevc", "aac");
        assert!(request.requests_server_transcoding_for_source(false, &source));
    }

    #[test]
    fn unknown_source_codecs_do_not_count_as_a_direct_play_mismatch() {
        let request = parse_emby_playback_info_request(&Bytes::from_static(
            br#"{
                "DeviceProfile": {
                    "DirectPlayProfiles": [{
                        "Container": "mkv",
                        "VideoCodec": "h264",
                        "AudioCodec": "aac",
                        "Type": "Video"
                    }],
                    "TranscodingProfiles": [{
                        "Protocol": "hls",
                        "VideoCodec": "h264",
                        "AudioCodec": "aac",
                        "Type": "Video"
                    }]
                }
            }"#,
        ))
        .expect("valid PlaybackInfo request");
        let mut source = profile_source("mkv", "h264", "aac");
        source.probe_status = "PENDING".to_owned();
        source.streams.clear();

        assert!(!request.requests_server_transcoding_for_source(false, &source));
    }

    #[test]
    fn device_profile_disables_video_copy_for_an_incompatible_transcoding_codec() {
        let request = parse_emby_playback_info_request(&Bytes::from_static(
            br#"{
                "EnableDirectPlay": true,
                "EnableDirectStream": true,
                "EnableTranscoding": true,
                "AllowVideoStreamCopy": true,
                "AllowAudioStreamCopy": true,
                "DeviceProfile": {
                    "DirectPlayProfiles": [{
                        "Container": "mp4",
                        "VideoCodec": "h264",
                        "AudioCodec": "aac",
                        "Type": "Video"
                    }],
                    "TranscodingProfiles": [{
                        "Protocol": "hls",
                        "VideoCodec": "h264",
                        "AudioCodec": "aac",
                        "Type": "Video"
                    }]
                }
            }"#,
        ))
        .expect("valid PlaybackInfo request");
        let source = profile_source("mkv", "hevc", "aac");
        assert_eq!(
            choose_plan(PlaybackDecisionInput {
                source_kind: PlaybackSourceKind::LocalFile,
                capabilities: request.playback_capabilities_for_source(Some(&source)),
            }),
            PlaybackPlan::ServerHls {
                tier: ServerTier::HardwareTranscode,
            }
        );
    }

    #[test]
    fn device_profile_keeps_direct_play_when_profile_matches() {
        let request = parse_emby_playback_info_request(&Bytes::from_static(
            br#"{
                "DeviceProfile": {
                    "DirectPlayProfiles": [{
                        "Container": "mkv,mp4",
                        "VideoCodec": "h264,hevc",
                        "AudioCodec": "aac",
                        "Type": "Video"
                    }],
                    "TranscodingProfiles": [{
                        "Protocol": "hls",
                        "Type": "Video"
                    }]
                }
            }"#,
        ))
        .expect("valid DeviceProfile request");
        let source = profile_source("mkv", "hevc", "aac");
        assert!(!request.requests_server_transcoding_for_source(false, &source));
    }

    #[test]
    fn device_profile_only_request_transcodes_unsupported_audio_with_default_flags() {
        let request = parse_emby_playback_info_request(&Bytes::from_static(
            br#"{
                "DeviceProfile": {
                    "DirectPlayProfiles": [{
                        "Container": "mkv",
                        "VideoCodec": "h264",
                        "AudioCodec": "aac",
                        "Type": "Video"
                    }],
                    "TranscodingProfiles": [{
                        "Container": "mp4",
                        "VideoCodec": "h264",
                        "AudioCodec": "aac",
                        "Protocol": "hls",
                        "Type": "Video"
                    }]
                }
            }"#,
        ))
        .expect("valid DeviceProfile request");
        let source = profile_source("mkv", "h264", "dts");

        assert!(request.requests_server_transcoding_for_source(false, &source));
        assert_eq!(
            choose_plan(PlaybackDecisionInput {
                source_kind: PlaybackSourceKind::LocalFile,
                capabilities: request.playback_capabilities_for_source(Some(&source)),
            }),
            PlaybackPlan::ServerHls {
                tier: ServerTier::AudioTranscode,
            }
        );
    }

    #[test]
    fn device_profile_only_request_respects_streaming_bitrate_limit() {
        let request = parse_emby_playback_info_request(&Bytes::from_static(
            br#"{
                "MaxStreamingBitrate": 8000000,
                "DeviceProfile": {
                    "DirectPlayProfiles": [{
                        "Container": "mkv",
                        "VideoCodec": "h264",
                        "AudioCodec": "aac",
                        "Type": "Video"
                    }],
                    "TranscodingProfiles": [{
                        "Container": "mp4",
                        "VideoCodec": "h264",
                        "AudioCodec": "aac",
                        "Protocol": "hls",
                        "Type": "Video"
                    }]
                }
            }"#,
        ))
        .expect("valid DeviceProfile request");
        let mut source = profile_source("mkv", "h264", "aac");
        source.bitrate = Some(13_912_978);

        assert!(request.requests_server_transcoding_for_source(false, &source));
        assert_eq!(
            choose_plan(PlaybackDecisionInput {
                source_kind: PlaybackSourceKind::LocalFile,
                capabilities: request.playback_capabilities_for_source(Some(&source)),
            }),
            PlaybackPlan::ServerHls {
                tier: ServerTier::HardwareTranscode,
            }
        );
    }

    #[test]
    fn unqualified_hls_profile_defaults_to_mpeg_ts() {
        let request = parse_emby_playback_info_request(&Bytes::from_static(
            br#"{
                "DeviceProfile": {
                    "TranscodingProfiles": [{
                        "Protocol": "hls",
                        "Type": "Video"
                    }]
                }
            }"#,
        ))
        .expect("valid DeviceProfile request");

        assert_eq!(
            request.selected_hls_container(&profile_source("mkv", "hevc", "aac")),
            Ok(HlsSegmentContainer::MpegTs)
        );
    }

    #[test]
    fn explicit_hls_profile_container_selects_fragmented_mp4() {
        let request = parse_emby_playback_info_request(&Bytes::from_static(
            br#"{
                "DeviceProfile": {
                    "TranscodingProfiles": [{
                        "Container": "mp4",
                        "Protocol": "hls",
                        "Type": "Video"
                    }]
                }
            }"#,
        ))
        .expect("valid DeviceProfile request");

        assert_eq!(
            request.selected_hls_container(&profile_source("mkv", "hevc", "aac")),
            Ok(HlsSegmentContainer::FragmentedMp4)
        );
    }

    #[test]
    fn conflicting_segment_container_hints_are_rejected() {
        let mut request = EmbyPlaybackInfoRequest::default();
        request.apply_query_parameters(&RawQuery(Some(
            "SegmentContainer=ts&TranscodingContainer=mp4".to_owned(),
        )));

        assert!(
            request
                .selected_hls_container(&profile_source("mkv", "hevc", "aac"))
                .is_err()
        );
    }

    #[test]
    fn device_profile_streaming_bitrate_limit_is_used_as_fallback() {
        let request = parse_emby_playback_info_request(&Bytes::from_static(
            br#"{
                "DeviceProfile": {
                    "MaxStreamingBitrate": 8000000,
                    "DirectPlayProfiles": [{
                        "Container": "mkv",
                        "VideoCodec": "h264",
                        "AudioCodec": "aac",
                        "Type": "Video"
                    }],
                    "TranscodingProfiles": [{
                        "Container": "mp4",
                        "VideoCodec": "h264",
                        "AudioCodec": "aac",
                        "Protocol": "hls",
                        "Type": "Video"
                    }]
                }
            }"#,
        ))
        .expect("valid DeviceProfile request");
        let mut source = profile_source("mkv", "h264", "aac");
        source.bitrate = Some(13_912_978);

        assert!(request.requests_server_transcoding_for_source(false, &source));
    }

    #[test]
    fn max_streaming_bitrate_query_parameter_is_read() {
        let mut request = parse_emby_playback_info_request(&Bytes::from_static(
            br#"{
                "DeviceProfile": {
                    "DirectPlayProfiles": [{
                        "Container": "mkv",
                        "VideoCodec": "h264",
                        "AudioCodec": "aac",
                        "Type": "Video"
                    }],
                    "TranscodingProfiles": [{
                        "Protocol": "hls",
                        "Type": "Video"
                    }]
                }
            }"#,
        ))
        .expect("valid DeviceProfile request");
        request.apply_query_parameters(&RawQuery(Some("MaxStreamingBitrate=8000000".to_owned())));
        let mut source = profile_source("mkv", "h264", "aac");
        source.bitrate = Some(13_912_978);

        assert!(request.requests_server_transcoding_for_source(false, &source));
    }

    #[test]
    fn start_time_ticks_query_parameter_is_read() {
        let mut request = EmbyPlaybackInfoRequest::default();
        request.apply_query_parameters(&RawQuery(Some("StartTimeTicks=12345678".to_owned())));

        assert_eq!(request.start_time_ticks, Some(12_345_678));
    }

    #[test]
    fn start_time_hint_rejects_non_positive_and_out_of_range_positions() {
        assert_eq!(emby_start_time_hint(0, Some(100)), None);
        assert_eq!(emby_start_time_hint(-1, Some(100)), None);
        assert_eq!(emby_start_time_hint(100, Some(100)), None);
        assert_eq!(emby_start_time_hint(101, Some(100)), None);
        assert_eq!(emby_start_time_hint(i64::MAX, Some(100)), None);
        assert_eq!(emby_start_time_hint(99, Some(100)), Some(99));
        assert_eq!(emby_start_time_hint(101, None), Some(101));
    }

    #[test]
    fn recognizes_only_valid_internal_emby_generation_init_names() {
        assert!(is_emby_generation_init_asset("generation_000123_init.mp4"));
        assert!(!is_emby_generation_init_asset("generation_123_init.mp4"));
        assert!(!is_emby_generation_init_asset(
            "../generation_000123_init.mp4"
        ));
        assert!(!is_emby_generation_init_asset("init.mp4"));
    }

    #[test]
    fn force_transcode_query_overrides_direct_play_request() {
        let request = parse_emby_playback_info_request(&Bytes::from_static(
            br#"{
                "EnableDirectPlay": true,
                "EnableDirectStream": true
            }"#,
        ))
        .expect("valid PlaybackInfo request");
        assert!(request.requests_server_transcoding(true));
    }

    #[test]
    fn force_transcode_query_only_accepts_true_values() {
        assert!(emby_force_transcode_from_raw(&RawQuery(Some(
            "forceTranscode=true".to_owned(),
        ))));
        assert!(emby_force_transcode_from_raw(&RawQuery(Some(
            "ForceTranscode=1".to_owned(),
        ))));
        assert!(!emby_force_transcode_from_raw(&RawQuery(Some(
            "forceTranscode=false".to_owned(),
        ))));
    }

    #[test]
    fn playback_flags_are_read_from_standard_query_parameters() {
        let mut request = parse_emby_playback_info_request(&Bytes::from_static(
            br#"{
                "DeviceProfile": {
                    "DirectPlayProfiles": [{
                        "Container": "mp4",
                        "VideoCodec": "h264",
                        "AudioCodec": "aac",
                        "Type": "Video"
                    }],
                    "TranscodingProfiles": [{
                        "Container": "mp4",
                        "VideoCodec": "h264",
                        "AudioCodec": "aac",
                        "Protocol": "hls",
                        "Type": "Video"
                    }]
                }
            }"#,
        ))
        .expect("valid DeviceProfile request");
        request.apply_query_parameters(&RawQuery(Some(
            "EnableDirectPlay=false&EnableDirectStream=false&EnableTranscoding=true&AllowVideoStreamCopy=false&AllowAudioStreamCopy=false"
                .to_owned(),
        )));

        let source = profile_source("mkv", "hevc", "aac");
        assert!(request.requests_server_transcoding(false));
        assert_eq!(
            choose_plan(PlaybackDecisionInput {
                source_kind: PlaybackSourceKind::LocalFile,
                capabilities: request.playback_capabilities_for_source(Some(&source)),
            }),
            PlaybackPlan::ServerHls {
                tier: ServerTier::HardwareTranscode,
            }
        );
    }

    #[test]
    fn signed_asset_urls_outlive_one_heartbeat_window() {
        use super::web_playback_resource_expiry;
        use crate::application::playback::session::{
            ResourceSigner, WEB_PLAYBACK_SESSION_TTL_SECONDS,
        };

        let signer = ResourceSigner::random();
        let expires_at = web_playback_resource_expiry();
        let signature = signer
            .sign("session-1", "hls:segment_000000.m4s", expires_at)
            .expect("resource signature");
        // A playlist fetched at playback start is still usable after the first session window.
        assert!(signer.verify(
            "session-1",
            "hls:segment_000000.m4s",
            expires_at,
            &signature,
            crate::application::playback::session::unix_timestamp()
                + WEB_PLAYBACK_SESSION_TTL_SECONDS
                + 60,
        ));
    }
}
