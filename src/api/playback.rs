use super::*;

use crate::application::playback::{
    decision::ServerTier,
    session::{CreatedWebPlaybackSession, WebPlaybackPlan},
};
use crate::storage::MAX_PLAYBACK_SESSION_WINDOW_SECONDS;

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
    let mut sources = item.media_sources.iter().collect::<Vec<_>>();
    let query_media_source_requested = query.media_source_id.is_some();
    sources.sort_by(|left, right| {
        right
            .is_default
            .cmp(&left.is_default)
            .then_with(|| left.id.cmp(&right.id))
    });
    if let Some(source_id) = query.media_source_id {
        let Some(index) = sources.iter().position(|source| source.id == source_id) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let source = sources.remove(index);
        sources.insert(0, source);
    }
    if let Some(source_id) = request.media_source_id.as_deref() {
        let Some(index) = sources.iter().position(|source| source.id == source_id) else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let source = sources.remove(index);
        sources.insert(0, source);
    }
    let transcode_requested = method == Method::POST
        && sources.first().is_some_and(|source| {
            request.requests_server_transcoding_for_source(force_transcode, source)
        });
    if let Some(source) = sources.first() {
        tracing::info!(
            event = "emby_playback_negotiation",
            item_id_prefix = %playback_identifier_prefix(&item.id),
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
        .any(|source| emby_source_needs_strm_resolver(source))
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
    let transcode_session = if transcode_requested {
        let Some(source) = sources.first() else {
            return StatusCode::NOT_FOUND.into_response();
        };
        match create_emby_transcoding_session(&state, &user, &item.id, source, &request).await {
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
    Json(json!({
        "PlaySessionId": play_session_id,
        "MediaSources": sources
            .into_iter()
            .map(|source| {
                let mut value = emby_media_source_json_with_resolver(
                    &item.id,
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
                    if source_can_transcode {
                        // Emby advertises the device's available transcoding
                        // profiles even when direct play wins this request.
                        // Clients need this capability bit to offer a
                        // transcoding fallback on a later request.
                        object.insert("SupportsTranscoding".to_owned(), json!(true));
                    }
                    if direct_play_disabled {
                        // A transcoding offer must not leave the original
                        // direct-play capability set, otherwise many Emby
                        // clients ignore TranscodingUrl and open DirectStreamUrl.
                        object.insert("SupportsDirectPlay".to_owned(), json!(false));
                        if source_transcode_session.is_some() {
                            object.insert("SupportsDirectStream".to_owned(), json!(false));
                        }
                    }
                }
                let has_direct_stream_url = value
                    .get("DirectStreamUrl")
                    .is_some_and(Value::is_string);
                let transcoding_url = source_transcode_session
                    .as_ref()
                    .and_then(|session| {
                        state.web_playback.as_ref().and_then(|service| {
                            emby_transcoding_url(
                                service,
                                &item.id,
                                source,
                                session,
                                &request,
                                &device_id,
                            )
                        })
                    });
                if let Value::Object(object) = &mut value {
                    if source_transcode_session.is_none()
                        && has_direct_stream_url
                        && let Some(service) = state.web_playback.as_ref()
                        && let Some(url) = emby_signed_direct_stream_url(
                            service,
                            &item.id,
                            source,
                            &user,
                            if emby_source_needs_proxy_identity(source) {
                                standard_api_key.as_deref()
                            } else {
                                None
                            },
                        )
                    {
                        object.insert("DirectStreamUrl".to_owned(), json!(url));
                        // Third-party clients may send the media request through
                        // an independent stack. External proxies use the
                        // standard Emby token to identify the proxy user, while
                        // Lux still requires the signed ticket. For URL/path STRM
                        // sources the token is already embedded for clients that
                        // ignore AddApiKeyToDirectStreamUrl.
                        object.insert(
                            "AddApiKeyToDirectStreamUrl".to_owned(),
                            json!(emby_source_needs_proxy_identity(source)),
                        );
                    }
                    if let Some(url) = transcoding_url {
                        object.insert("SupportsTranscoding".to_owned(), json!(true));
                        object.insert("TranscodingUrl".to_owned(), json!(url));
                        object.insert("TranscodingSubProtocol".to_owned(), json!("hls"));
                        object.insert("TranscodingContainer".to_owned(), json!("mp4"));
                        object.insert("TranscodingMimeType".to_owned(), json!("video/mp4"));
                        // An Emby transcoding MediaSourceInfo does not expose
                        // a competing direct URL. Native players otherwise may
                        // select DirectStreamUrl before inspecting TranscodingUrl.
                        object.insert("DirectStreamUrl".to_owned(), Value::Null);
                        object.insert("AddApiKeyToDirectStreamUrl".to_owned(), json!(false));
                    }
                }
                if let Some(session) = transcode_session
                    .as_ref()
                    .filter(|session| session.media_source_id == source.id)
                {
                    tracing::info!(
                        event = "emby_transcoding_offer",
                        item_id_prefix = %playback_identifier_prefix(&item.id),
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
            .collect::<Vec<_>>(),
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
    #[serde(rename = "MaxAudioChannels", alias = "maxAudioChannels")]
    max_audio_channels: Option<i64>,
    #[serde(rename = "StartTimeTicks", alias = "startTimeTicks")]
    start_time_ticks: Option<i64>,
    #[serde(rename = "DeviceProfile", alias = "deviceProfile")]
    device_profile: Option<EmbyDeviceProfile>,
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
    #[serde(rename = "MaxAudioChannels", alias = "maxAudioChannels")]
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
            is_video_profile(profile.profile_type.as_deref()) && is_hls_profile(profile)
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
    fn direct_play_compatibility(
        &self,
        container: Option<&str>,
        video_codec: Option<&str>,
        audio_codec: Option<&str>,
    ) -> EmbyProfileCompatibility {
        let constraints = [
            profile_value_compatibility(self.container.as_deref(), container, values_match),
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

async fn create_emby_transcoding_session(
    state: &AppState,
    user: &UserRecord,
    item_id: &str,
    source: &crate::application::catalog::CatalogSource,
    request: &EmbyPlaybackInfoRequest,
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
    let created = service
        .create(CreateWebPlaybackSession {
            user_id: &user_id,
            is_admin: user.is_admin,
            item_id,
            media_source_id: &source.id,
            play_session_prefix: "lux-emby",
            source_kind: PlaybackSourceKind::LocalFile,
            capabilities: request.playback_capabilities_for_source(Some(source)),
        })
        .await
        .map_err(emby_playback_session_error_status)?;
    let WebPlaybackPlan::ServerHls { tier } = created.plan else {
        let _ = service.stop(&created.id, &user_id).await;
        return Err(StatusCode::BAD_GATEWAY);
    };
    let input =
        match canonical_local_media_path(&stored_source.root_path, &stored_source.relative_path)
            .await
        {
            Ok(path) => path,
            Err(LocalPathError::Missing) => {
                let _ = service.stop(&created.id, &user_id).await;
                return Err(StatusCode::NOT_FOUND);
            }
            Err(LocalPathError::Forbidden) => {
                let _ = service.stop(&created.id, &user_id).await;
                return Err(StatusCode::FORBIDDEN);
            }
        };
    if service.start_hls(&created.id, tier, &input).await.is_err() {
        let _ = service.stop(&created.id, &user_id).await;
        return Err(StatusCode::BAD_GATEWAY);
    }
    Ok(Some(created))
}

fn emby_playback_session_error_status(error: WebPlaybackSessionError) -> StatusCode {
    match error {
        WebPlaybackSessionError::Invalid(_) => StatusCode::BAD_REQUEST,
        WebPlaybackSessionError::NotFound => StatusCode::NOT_FOUND,
        WebPlaybackSessionError::Expired | WebPlaybackSessionError::NotActive => StatusCode::GONE,
        WebPlaybackSessionError::Hls(_) => StatusCode::BAD_GATEWAY,
        WebPlaybackSessionError::Storage(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

async fn standard_emby_playback_api_key(
    headers: &HeaderMap,
    query_api_key: Option<&str>,
    state: &AppState,
) -> Option<String> {
    // X-Lux-Api-Key is the shared Lux management credential, not a user
    // identity that an external Emby proxy can safely receive.
    let token = if headers.contains_key("X-Lux-Api-Key") {
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
        match service.resolve(&token).await {
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
    let internal_item_id = emby_internal_id(&request.item_id);
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
    match access
        .can_view_item(
            AccessPrincipal::new(user.id, user.is_admin),
            &internal_item_id,
        )
        .await
    {
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
    let media_source_id = request
        .media_source_id
        .as_deref()
        .filter(|value| !value.is_empty());
    if let Some(media_source_id) = media_source_id {
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
    let play_session_id = request
        .play_session_id
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("{}:{device_id}", internal_item_id));
    let emby_transcode_session_id = emby_transcode_session_id_from_play_session(&play_session_id);
    let user_id = user.id.to_string();
    let played_percent = match database.user_played_percent(&user_id).await {
        Ok(value) => value,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let previous_session = match database
        .find_playback_session(&user_id, &play_session_id)
        .await
    {
        Ok(session) => session,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let duration_ticks = if let Some(duration_ticks) = request.duration_ticks {
        Some(duration_ticks)
    } else if let Some(duration_ticks) = previous_session
        .as_ref()
        .and_then(|session| session.duration_ticks)
    {
        Some(duration_ticks)
    } else {
        emby_playback_event_duration_ticks(
            &state,
            AccessPrincipal::new(user.id, user.is_admin),
            &internal_item_id,
            media_source_id,
        )
        .await
    };
    let activity_event = playback_activity_event_type(previous_session.as_ref(), state_name);
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
    let remote_ip = request_client_ip(&headers, &state.remote_access);
    let activity_remote_ip = remote_ip.as_deref().or_else(|| {
        previous_session
            .as_ref()
            .and_then(|session| session.remote_ip.as_deref())
    });
    match database
        .record_playback_event(NewPlaybackEvent {
            user_id: &user_id,
            item_id: &internal_item_id,
            media_source_id,
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
            if let Some(session_id) = emby_transcode_session_id
                && let Some(service) = state.web_playback.as_ref()
            {
                let result = if state_name == "STOPPED" {
                    service.stop(session_id, &user_id).await
                } else {
                    service.heartbeat(session_id, &user_id).await.map(|_| ())
                };
                if let Err(error) = result {
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
                    media_source_id,
                    &play_session_id,
                    state_name,
                    request.position_ticks,
                    duration_ticks,
                    request.is_paused || state_name == "PAUSED",
                    client,
                    device_name,
                    device_type,
                    client_version,
                )
                .await;
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
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default()
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
) {
    let Some(webhooks) = state.webhooks.as_ref() else {
        return;
    };
    let dedupe_key = format!(
        "playback:{play_session_id}:{}:{occurred_at}",
        event_type.as_str()
    );
    let data = json!({
        "itemId": emby_public_id(item_id),
        "mediaSourceId": media_source_id,
        "playSessionId": play_session_id,
        "state": state_name,
        "positionTicks": position_ticks,
        "durationTicks": duration_ticks,
        "isPaused": is_paused,
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
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let user_id = user.id.to_string();
    let active_within_seconds = match query.active_within_seconds {
        Some(seconds) if (1..=MAX_PLAYBACK_SESSION_WINDOW_SECONDS).contains(&seconds) => {
            Some(seconds)
        }
        Some(_) => return StatusCode::BAD_REQUEST.into_response(),
        None => None,
    };
    let sessions = match database
        .list_playback_sessions(
            (!user.is_admin).then_some(user_id.as_str()),
            active_within_seconds,
        )
        .await
    {
        Ok(sessions) => sessions,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let catalog_items = if sessions
        .iter()
        .any(|session| session.duration_ticks.is_none_or(|ticks| ticks <= 0))
    {
        let item_ids = sessions
            .iter()
            .map(|session| session.item_id.clone())
            .collect::<Vec<_>>();
        let Some(catalog) = state.catalog.as_ref() else {
            return Json(
                sessions
                    .iter()
                    .map(|session| emby_session_json(session, None))
                    .collect::<Vec<_>>(),
            )
            .into_response();
        };
        match catalog
            .find_items(AccessPrincipal::new(user.id, user.is_admin), &item_ids)
            .await
        {
            Ok(items) => items,
            Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
        }
    } else {
        HashMap::new()
    };
    Json(
        sessions
            .iter()
            .map(|session| emby_session_json(session, catalog_items.get(&session.item_id)))
            .collect::<Vec<_>>(),
    )
    .into_response()
}

pub(super) fn emby_session_json(
    session: &crate::storage::StoredPlaybackSession,
    catalog_item: Option<&CatalogItem>,
) -> Value {
    let runtime_ticks = session_runtime_ticks(session, catalog_item);
    let mut now_playing_item = json!({
        "Id": emby_public_id(&session.item_id),
        "RunTimeTicks": runtime_ticks,
    });
    if let Some(item) = catalog_item
        && let Value::Object(object) = &mut now_playing_item
    {
        object.insert("Name".to_owned(), json!(item.title));
        object.insert("Type".to_owned(), json!(emby_item_type(&item.item_type)));
        if let Some(series_name) = item.series_name.as_deref() {
            object.insert("SeriesName".to_owned(), json!(series_name));
        }
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
            "PlayMethod": "DirectPlay",
            "VolumeLevel": 100,
        },
        "NowPlayingItem": now_playing_item,
        "RunTimeTicks": runtime_ticks,
        "LastActivityDate": session.last_event_at,
    })
}

fn session_runtime_ticks(
    session: &crate::storage::StoredPlaybackSession,
    catalog_item: Option<&CatalogItem>,
) -> i64 {
    session
        .duration_ticks
        .filter(|ticks| *ticks > 0)
        .or_else(|| {
            catalog_item.and_then(|item| {
                item.runtime_ticks
                    .filter(|ticks| *ticks > 0)
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
            })
        })
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

fn emby_transcoding_url(
    service: &WebPlaybackSessionService,
    item_id: &str,
    source: &crate::application::catalog::CatalogSource,
    session: &CreatedWebPlaybackSession,
    request: &EmbyPlaybackInfoRequest,
    device_id: &str,
) -> Option<String> {
    let signature = service.sign_resource(&session.id, "hls:index.m3u8", session.expires_at)?;
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
    if let Some(start_time_ticks) = request.start_time_ticks.filter(|ticks| *ticks > 0) {
        query.append_pair("StartTimeTicks", &start_time_ticks.to_string());
    }
    query.append_pair("SegmentContainer", "mp4");
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
        value if value.starts_with("segment_") && value.ends_with(".m4s") => "segment",
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
    if asset_kind != "manifest" && status.is_success() {
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

fn emby_transcoding_asset_url(
    service: &WebPlaybackSessionService,
    item_id: &str,
    source_id: &str,
    session_id: &str,
    play_session_id: &str,
    asset: &str,
    expires_at: i64,
) -> Option<String> {
    let resource = format!("hls:{asset}");
    let signature = service.sign_resource(session_id, &resource, expires_at)?;
    Some(format!(
        "/Videos/{}/transcoding/{session_id}/{asset}?MediaSourceId={source_id}&PlaySessionId={}&luxPlaybackExpires={}&luxPlaybackSignature={}",
        emby_public_id(item_id),
        percent_encode_filename(play_session_id),
        signature.expires_at,
        signature.signature,
    ))
}

#[derive(Default)]
struct EmbyTranscodingQuery {
    media_source_id: Option<String>,
    play_session_id: Option<String>,
    playback_session_id: Option<String>,
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
    _headers: &HeaderMap,
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
    let resource = format!("hls:{asset}");
    let session = match service
        .authorize_resource(session_id, &resource, expires_at, signature)
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
    let Some(source_id) = session.media_source_id.as_deref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if asset == "index.m3u8" {
        let path = match service.wait_for_hls_manifest(session_id).await {
            Ok(path) => path,
            Err(error) => return emby_playback_session_error_status(error).into_response(),
        };
        let Ok(bytes) = fs::read(path).await else {
            return StatusCode::NOT_FOUND.into_response();
        };
        let Ok(manifest) = String::from_utf8(bytes) else {
            return StatusCode::BAD_GATEWAY.into_response();
        };
        let Some(manifest) = rewrite_hls_manifest(&manifest, |asset| {
            emby_transcoding_asset_url(
                service,
                &session.item_id,
                source_id,
                &session.id,
                &session.play_session_id,
                asset,
                session.expires_at,
            )
        }) else {
            return StatusCode::BAD_GATEWAY.into_response();
        };
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
    let path = match service.hls_asset_path(session_id, asset).await {
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
    let content_type = if asset.ends_with(".m4s") || asset.ends_with(".mp4") {
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
    Response::builder()
        .status(StatusCode::OK)
        .header("Cache-Control", "private, no-store")
        .header("Content-Type", content_type)
        .header("Content-Length", metadata.len())
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

async fn create_web_playback_session_json(
    headers: &HeaderMap,
    state: &AppState,
    user: &UserRecord,
    item_id: &str,
    source: &crate::storage::StoredPlaybackSource,
    capabilities: PlaybackCapabilities,
) -> Result<Value, Response> {
    let source_kind = match source.source_kind.as_str() {
        "LOCAL_FILE" => PlaybackSourceKind::LocalFile,
        "STRM_URL" => PlaybackSourceKind::Strm,
        _ => return Err(StatusCode::NOT_IMPLEMENTED.into_response()),
    };
    let Some(service) = state.web_playback.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE.into_response());
    };
    let created = service
        .create(CreateWebPlaybackSession {
            user_id: &user.id.to_string(),
            is_admin: user.is_admin,
            item_id,
            media_source_id: &source.source_id,
            play_session_prefix: "lux-web",
            source_kind,
            capabilities,
        })
        .await
        .map_err(|error| web_playback_error(headers, error))?;
    if let WebPlaybackPlan::ServerHls { tier } = &created.plan {
        if source.source_kind != "LOCAL_FILE" {
            let _ = service.stop(&created.id, &user.id.to_string()).await;
            return Err(StatusCode::NOT_IMPLEMENTED.into_response());
        }
        let input = match canonical_local_media_path(&source.root_path, &source.relative_path).await
        {
            Ok(path) => path,
            Err(LocalPathError::Missing) => {
                let _ = service.stop(&created.id, &user.id.to_string()).await;
                return Err(StatusCode::NOT_FOUND.into_response());
            }
            Err(LocalPathError::Forbidden) => {
                let _ = service.stop(&created.id, &user.id.to_string()).await;
                return Err(StatusCode::FORBIDDEN.into_response());
            }
        };
        if let Err(error) = service.start_hls(&created.id, *tier, &input).await {
            let _ = service.stop(&created.id, &user.id.to_string()).await;
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
                "url": web_playback_resource_url(service, &created.id, "direct", created.expires_at),
                "proxyUrl": proxy_url,
                "rangeUrl": (source.source_kind == "STRM_URL"
                    && source.external_url.as_deref().is_some_and(|target| {
                        matches!(classify_strm_target(target).kind, StrmTargetKind::Url)
                    }))
                    .then(|| web_playback_range_url(service, &created.id, created.expires_at))
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
    match create_web_playback_session_json(
        &headers,
        &state,
        &user,
        &request.item_id,
        &source,
        request.capabilities.into(),
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
    let session = match create_web_playback_session_json(
        &headers,
        &state,
        &user,
        &request.item_id,
        &source,
        request.capabilities.into(),
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
    let session = match service
        .authorize_resource(&session_id, "direct", query.expires, &query.signature)
        .await
    {
        Ok(session) => session,
        Err(error) => return web_playback_error(&headers, error),
    };
    if session.plan != "DIRECT" {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Ok(user_id) = session.user_id.parse::<crate::domain::ids::UserId>() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    serve_media_file(
        &state,
        AccessPrincipal::new(user_id, session.is_admin),
        &headers,
        &method,
        &session.item_id,
        session.media_source_id.as_deref(),
        None,
    )
    .await
}

pub(super) async fn lux_web_playback_range(
    headers: HeaderMap,
    Path(session_id): Path<String>,
    Query(query): Query<WebPlaybackResourceQuery>,
    State(state): State<AppState>,
) -> Response {
    let Some(service) = state.web_playback.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let session = match service
        .authorize_resource(&session_id, "range", query.expires, &query.signature)
        .await
    {
        Ok(session) => session,
        Err(error) => return web_playback_error(&headers, error),
    };
    if session.plan != "DIRECT" {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(range) = headers.get("range").and_then(|value| value.to_str().ok()) else {
        return StatusCode::RANGE_NOT_SATISFIABLE.into_response();
    };
    let Ok(user_id) = session.user_id.parse::<crate::domain::ids::UserId>() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let principal = AccessPrincipal::new(user_id, session.is_admin);
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
    let user_agent = headers
        .get("user-agent")
        .and_then(|value| value.to_str().ok());
    let result = match resolver.fetch_range(external_url, range, user_agent).await {
        Ok(result) => result,
        Err(crate::application::strm_playback::StrmPlaybackError::InvalidRange) => {
            return StatusCode::RANGE_NOT_SATISFIABLE.into_response();
        }
        Err(crate::application::strm_playback::StrmPlaybackError::UnsupportedStatus(status))
            if status == 401 || status == 403 =>
        {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        Err(_) => return StatusCode::BAD_GATEWAY.into_response(),
    };
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
    let session = match service
        .authorize_resource(&session_id, &resource, query.expires, &query.signature)
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
            return StatusCode::NOT_FOUND.into_response();
        };
        let Ok(manifest) = String::from_utf8(bytes) else {
            return StatusCode::BAD_GATEWAY.into_response();
        };
        let Some(manifest) = rewrite_hls_manifest(&manifest, |asset| {
            web_playback_hls_url(service, &session_id, asset, session.expires_at)
        }) else {
            return StatusCode::BAD_GATEWAY.into_response();
        };
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
        Err(error) => return web_playback_error(&headers, error),
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
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let content_type = if asset.ends_with(".m4s") || asset.ends_with(".mp4") {
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
        let played_percent = match database.user_played_percent(&user.id.to_string()).await {
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
                remote_ip: request_client_ip(&headers, &state.remote_access).as_deref(),
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
    }
    Json(json!({
        "accepted": claim == WebPlaybackEventClaim::Accepted,
        "duplicate": claim == WebPlaybackEventClaim::Duplicate,
        "stale": claim == WebPlaybackEventClaim::Stale,
    }))
    .into_response()
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
    let remote_ip = request_client_ip(&headers, &state.remote_access);
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
    let user = match require_emby_user(&headers, &state, query.api_key.as_deref()).await {
        Ok(user) => user,
        Err(status) => return status.into_response(),
    };
    if let Err(status) = ensure_emby_user_scope(&user, &user_id) {
        return status.into_response();
    }
    let Some(access) = state.access.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let item_id = emby_internal_id(&item_id);
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
    let result = if played {
        database
            .set_user_item_played(&user_id, &item_id, value)
            .await
    } else {
        database
            .set_user_item_favorite(&user_id, &item_id, value)
            .await
    };
    match result {
        Ok(()) => {
            if played
                && database
                    .sync_played_container_states(&user_id, &item_id)
                    .await
                    .is_err()
            {
                return StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            let user_state = match database.find_user_item_state(&user_id, &item_id).await {
                Ok(user_state) => user_state,
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            };
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
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
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
            StatusCode::NO_CONTENT.into_response()
        }
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

#[cfg(test)]
mod emby_playback_tests {
    use super::{
        EmbyPlaybackInfoRequest, emby_force_transcode_from_raw, emby_hls_asset_kind,
        parse_emby_playback_info_request,
    };
    use crate::application::catalog::{CatalogSource, CatalogStream};
    use crate::application::playback::decision::{
        PlaybackDecisionInput, PlaybackPlan, PlaybackSourceKind, ServerTier, choose_plan,
    };
    use axum::{body::Bytes, extract::RawQuery};
    use std::collections::BTreeMap;

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
        assert_eq!(emby_hls_asset_kind("segment_000001.m4s"), "segment");
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
}
