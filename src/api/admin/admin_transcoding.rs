use super::*;

pub(super) fn api_routes() -> Router<AppState> {
    Router::new().route(
        "/api/v1/admin/transcoding/sessions",
        get(admin_transcoding_sessions),
    )
}

/// Live server-side transcoding progress for the admin transcoding page.
///
/// Read-only: it lists active HLS sessions and reuses the session statistics service for the
/// per-session FFmpeg snapshot. Expired or already stopped rows simply have no progress.
pub(crate) async fn admin_transcoding_sessions(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    if let Err(response) = require_admin(&headers, &state, false).await {
        return response;
    }
    let Some(database) = state.database.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(service) = state.web_playback.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let rows = match database.active_server_transcoding_sessions(24).await {
        Ok(rows) => rows,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let mut sessions = Vec::with_capacity(rows.len());
    let mut summary = TranscodingSummary::default();
    for row in rows {
        let stats = service.stats(&row.session_id, &row.user_id).await.ok();
        summary.record(row.tier, stats.as_ref());
        sessions.push(transcoding_session_json(&row, stats.as_ref()));
    }
    Json(json!({
        "sessions": sessions,
        "summary": summary.to_json(),
    }))
    .into_response()
}

#[derive(Default)]
struct TranscodingSummary {
    hardware: i64,
    software: i64,
    stream_copy: i64,
    output_bytes: u64,
}

impl TranscodingSummary {
    fn record(
        &mut self,
        tier: i64,
        stats: Option<&crate::application::playback::session::WebPlaybackStats>,
    ) {
        match tier {
            tier if tier == i64::from(ServerTier::HardwareTranscode.number()) => self.hardware += 1,
            tier if tier == i64::from(ServerTier::SoftwareTranscode.number()) => self.software += 1,
            _ => self.stream_copy += 1,
        }
        if let Some(bytes) = stats.and_then(|stats| stats.directory_bytes) {
            self.output_bytes = self.output_bytes.saturating_add(bytes);
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "activeCount": self.hardware + self.software + self.stream_copy,
            "hardwareCount": self.hardware,
            "softwareCount": self.software,
            "streamCopyCount": self.stream_copy,
            "outputBytes": self.output_bytes,
        })
    }
}

fn transcoding_session_json(
    row: &crate::storage::StoredTranscodingSessionRow,
    stats: Option<&crate::application::playback::session::WebPlaybackStats>,
) -> Value {
    let progress = stats.and_then(|stats| stats.progress.as_ref());
    let latest_segment_bitrate = stats.and_then(|stats| stats.latest_segment_bitrate_kbps);
    let directory_bytes = stats.and_then(|stats| stats.directory_bytes);
    json!({
        "sessionId": row.session_id,
        "playSessionId": stats.map(|stats| stats.session.play_session_id.as_str()),
        "userId": row.user_id,
        "userName": row.user_name,
        "itemId": row.item_id,
        "title": row.item_title,
        "itemType": row.item_type,
        "tier": row.tier,
        "state": row.state,
        "container": row.transcoding_container,
        "videoCodec": row.video_codec,
        "audioCodec": row.audio_codec,
        "audioBitrate": row.audio_bitrate,
        "createdAt": row.created_at,
        "updatedAt": row.updated_at,
        "expiresAt": row.expires_at,
        "progress": progress.map(|progress| json!({
            "fps": progress.fps,
            "speed": progress.speed,
            "bitrateKbps": progress.bitrate_kbps,
            "latestSegmentBitrateKbps": latest_segment_bitrate,
            "averageBitrateKbps": directory_bytes
                .zip(progress.out_time_us)
                .filter(|(_, micros)| *micros > 0)
                .map(|(bytes, micros)| bytes as f64 * 8.0 * 1_000.0 / micros as f64),
            "directoryBytes": directory_bytes,
            "outTimeSeconds": progress.out_time_us.map(|micros| micros as f64 / 1_000_000.0),
            "droppedFrames": progress.dropped_frames,
            "finished": progress.finished,
            "updatedAt": progress.updated_at_ms,
        })),
    })
}
