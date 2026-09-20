use std::{
    fmt,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use rand_core::{OsRng, RngCore};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::storage::{
    Database, NewWebPlaybackEvent, NewWebPlaybackSession, StorageError, StoredWebPlaybackSession,
    WebPlaybackEventClaim,
};

use super::decision::{
    PlaybackCapabilities, PlaybackDecisionInput, PlaybackPlan, PlaybackSourceKind, ServerTier,
    UnsupportedReason, choose_plan,
};
use super::hls::{HlsError, HlsManager, HlsSegmentContainer};

type HmacSha256 = Hmac<Sha256>;

pub const WEB_PLAYBACK_SESSION_TTL_SECONDS: i64 = 15 * 60;
pub const EMBY_DIRECT_STREAM_TTL_SECONDS: i64 = 12 * 60 * 60;
const WEB_PLAYBACK_CLEANUP_INTERVAL: Duration = Duration::from_secs(5);
const WEB_PLAYBACK_STALE_AFTER_SECONDS: i64 = 90;
const EMBY_DIRECT_STREAM_SESSION_ID: &str = "emby-direct-stream";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WebPlaybackPlan {
    Direct,
    ServerHls { tier: ServerTier },
    Unsupported { reason: UnsupportedReason },
}

impl From<PlaybackPlan> for WebPlaybackPlan {
    fn from(plan: PlaybackPlan) -> Self {
        match plan {
            PlaybackPlan::Direct => Self::Direct,
            PlaybackPlan::ServerHls { tier } => Self::ServerHls { tier },
            PlaybackPlan::Unsupported { reason } => Self::Unsupported { reason },
        }
    }
}

impl WebPlaybackPlan {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Direct => "DIRECT",
            Self::ServerHls { .. } => "SERVER_HLS",
            Self::Unsupported { .. } => "UNSUPPORTED",
        }
    }

    pub fn tier(&self) -> ServerTier {
        match self {
            Self::Direct | Self::Unsupported { .. } => ServerTier::Direct,
            Self::ServerHls { tier } => *tier,
        }
    }
}

pub struct CreateWebPlaybackSession<'a> {
    pub user_id: &'a str,
    pub is_admin: bool,
    pub item_id: &'a str,
    pub media_source_id: &'a str,
    pub play_session_prefix: &'a str,
    pub source_kind: PlaybackSourceKind,
    pub capabilities: PlaybackCapabilities,
}

pub(crate) struct WebPlaybackEvent<'a> {
    pub(crate) session_id: &'a str,
    pub(crate) user_id: &'a str,
    pub(crate) event_id: &'a str,
    pub(crate) sequence: i64,
    pub(crate) state: &'a str,
    pub(crate) position_ticks: i64,
    pub(crate) duration_ticks: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedWebPlaybackSession {
    pub id: String,
    pub user_id: String,
    pub item_id: String,
    pub media_source_id: String,
    pub play_session_id: String,
    pub plan: WebPlaybackPlan,
    pub expires_at: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignedResource {
    pub expires_at: i64,
    pub signature: String,
}

#[derive(Debug)]
pub(crate) enum WebPlaybackSessionError {
    Invalid(String),
    NotFound,
    Expired,
    NotActive,
    Hls(HlsError),
    Storage(StorageError),
}

impl fmt::Display for WebPlaybackSessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => formatter.write_str(message),
            Self::NotFound => formatter.write_str("web playback session not found"),
            Self::Expired => formatter.write_str("web playback session expired"),
            Self::NotActive => formatter.write_str("web playback session is not active"),
            Self::Hls(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for WebPlaybackSessionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Storage(error) => Some(error),
            Self::Invalid(_) | Self::NotFound | Self::Expired | Self::NotActive => None,
            Self::Hls(error) => Some(error),
        }
    }
}

impl From<StorageError> for WebPlaybackSessionError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<HlsError> for WebPlaybackSessionError {
    fn from(error: HlsError) -> Self {
        Self::Hls(error)
    }
}

#[derive(Clone)]
pub struct WebPlaybackSessionService {
    database: Database,
    signer: Arc<ResourceSigner>,
    hls: HlsManager,
    emby_start_lock: Arc<tokio::sync::Mutex<()>>,
}

impl WebPlaybackSessionService {
    pub(crate) fn new(database: Database, config_dir: std::path::PathBuf) -> Self {
        let hls = HlsManager::new(config_dir);
        let cleanup = hls.clone();
        let database_cleanup = database.clone();
        tokio::spawn(async move {
            if let Err(error) = cleanup.cleanup_orphans().await {
                tracing::warn!(%error, "failed to clean orphaned Web HLS directories");
            }
            let mut interval = tokio::time::interval(WEB_PLAYBACK_CLEANUP_INTERVAL);
            loop {
                interval.tick().await;
                let now = unix_timestamp();
                match database_cleanup
                    .take_inactive_web_playback_sessions(now, WEB_PLAYBACK_STALE_AFTER_SECONDS)
                    .await
                {
                    Ok(sessions) => stop_hls_sessions(&cleanup, sessions).await,
                    Err(error) => {
                        tracing::warn!(%error, "failed to reap inactive Web HLS sessions")
                    }
                }
                let sessions = match database_cleanup
                    .take_expired_web_playback_sessions(now)
                    .await
                {
                    Ok(sessions) => sessions,
                    Err(error) => {
                        tracing::warn!(%error, "failed to expire Web playback sessions");
                        continue;
                    }
                };
                stop_hls_sessions(&cleanup, sessions).await;
            }
        });
        Self {
            database,
            signer: Arc::new(ResourceSigner::random()),
            hls,
            emby_start_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub(crate) fn with_hls_executable(mut self, executable: std::path::PathBuf) -> Self {
        self.hls = self
            .hls
            .with_executable(executable.to_string_lossy().into_owned());
        self
    }

    pub(crate) async fn create(
        &self,
        input: CreateWebPlaybackSession<'_>,
    ) -> Result<CreatedWebPlaybackSession, WebPlaybackSessionError> {
        if input.user_id.is_empty() || input.item_id.is_empty() || input.media_source_id.is_empty()
        {
            return Err(WebPlaybackSessionError::Invalid(
                "playback session identifiers must not be empty".to_owned(),
            ));
        }
        let capabilities =
            apply_server_capabilities(input.capabilities, self.hls.hardware_transcode_available());
        let plan = WebPlaybackPlan::from(choose_plan(PlaybackDecisionInput {
            source_kind: input.source_kind,
            capabilities,
        }));
        let WebPlaybackPlan::Unsupported { .. } = plan else {
            let id = Uuid::now_v7().to_string();
            let play_session_id = format!("{}:{id}", input.play_session_prefix);
            let now = unix_timestamp();
            let expires_at = now.saturating_add(WEB_PLAYBACK_SESSION_TTL_SECONDS);
            self.database
                .insert_web_playback_session(NewWebPlaybackSession {
                    id: &id,
                    user_id: input.user_id,
                    item_id: input.item_id,
                    media_source_id: Some(input.media_source_id),
                    play_session_id: &play_session_id,
                    tier: i64::from(plan.tier().number()),
                    plan: plan.as_str(),
                    temp_dir: None,
                    is_admin: input.is_admin,
                    expires_at,
                    now,
                })
                .await?;
            return Ok(CreatedWebPlaybackSession {
                id,
                user_id: input.user_id.to_owned(),
                item_id: input.item_id.to_owned(),
                media_source_id: input.media_source_id.to_owned(),
                play_session_id,
                plan,
                expires_at,
            });
        };
        Ok(CreatedWebPlaybackSession {
            id: String::new(),
            user_id: input.user_id.to_owned(),
            item_id: input.item_id.to_owned(),
            media_source_id: input.media_source_id.to_owned(),
            play_session_id: String::new(),
            plan,
            expires_at: unix_timestamp(),
        })
    }

    pub(crate) async fn create_and_start_emby_hls(
        &self,
        input: CreateWebPlaybackSession<'_>,
        media_path: &std::path::Path,
        video_bitrate: Option<i64>,
        segment_container: HlsSegmentContainer,
        start_time_ticks: Option<i64>,
        runtime_ticks: Option<i64>,
    ) -> Result<CreatedWebPlaybackSession, WebPlaybackSessionError> {
        let _start_guard = self.emby_start_lock.lock().await;
        let user_id = input.user_id.to_owned();
        let created = self.create(input).await?;
        let WebPlaybackPlan::ServerHls { tier } = created.plan else {
            return Ok(created);
        };
        if let Err(error) = self
            .start_emby_hls_with_runtime(
                &created.id,
                tier,
                media_path,
                video_bitrate,
                segment_container,
                start_time_ticks,
                runtime_ticks,
            )
            .await
        {
            let _ = self.stop(&created.id, &user_id).await;
            return Err(error);
        }
        Ok(created)
    }

    pub(crate) fn sign_resource(
        &self,
        session_id: &str,
        resource: &str,
        expires_at: i64,
    ) -> Option<SignedResource> {
        Some(SignedResource {
            expires_at,
            signature: self.signer.sign(session_id, resource, expires_at)?,
        })
    }

    pub(crate) fn sign_emby_direct_stream(
        &self,
        user_id: &str,
        is_admin: bool,
        item_id: &str,
        media_source_id: &str,
        expires_at: i64,
    ) -> Option<String> {
        self.signer.sign(
            EMBY_DIRECT_STREAM_SESSION_ID,
            &emby_direct_stream_resource(user_id, is_admin, item_id, media_source_id),
            expires_at,
        )
    }

    pub(crate) fn verify_emby_direct_stream(
        &self,
        user_id: &str,
        is_admin: bool,
        item_id: &str,
        media_source_id: &str,
        expires_at: i64,
        signature: &str,
    ) -> bool {
        self.signer.verify(
            EMBY_DIRECT_STREAM_SESSION_ID,
            &emby_direct_stream_resource(user_id, is_admin, item_id, media_source_id),
            expires_at,
            signature,
            unix_timestamp(),
        )
    }

    pub(crate) async fn authorize_resource(
        &self,
        session_id: &str,
        resource: &str,
        expires_at: i64,
        signature: &str,
    ) -> Result<StoredWebPlaybackSession, WebPlaybackSessionError> {
        let now = unix_timestamp();
        if !self
            .signer
            .verify(session_id, resource, expires_at, signature, now)
        {
            return Err(WebPlaybackSessionError::NotFound);
        }
        let Some(session) = self.database.find_web_playback_session(session_id).await? else {
            return Err(WebPlaybackSessionError::NotFound);
        };
        if session.state != "ACTIVE" {
            return Err(WebPlaybackSessionError::NotActive);
        }
        if session.expires_at < now {
            let _ = self.hls.stop(session_id).await;
            return Err(WebPlaybackSessionError::Expired);
        }
        Ok(session)
    }

    pub(crate) async fn start_hls(
        &self,
        session_id: &str,
        tier: ServerTier,
        input: &std::path::Path,
        video_bitrate: Option<i64>,
        start_time_ticks: Option<i64>,
    ) -> Result<(), WebPlaybackSessionError> {
        self.start_hls_with_runtime(
            session_id,
            tier,
            input,
            video_bitrate,
            start_time_ticks,
            None,
        )
        .await
    }

    async fn start_hls_with_runtime(
        &self,
        session_id: &str,
        tier: ServerTier,
        input: &std::path::Path,
        video_bitrate: Option<i64>,
        start_time_ticks: Option<i64>,
        runtime_ticks: Option<i64>,
    ) -> Result<(), WebPlaybackSessionError> {
        self.hls
            .start(
                session_id,
                tier,
                input,
                video_bitrate,
                start_time_ticks,
                runtime_ticks,
            )
            .await?;
        let directory = self.hls.session_directory(session_id).await?;
        let now = unix_timestamp();
        if !self
            .database
            .set_web_playback_temp_dir(session_id, &directory.to_string_lossy(), now)
            .await?
        {
            let _ = self.hls.stop(session_id).await;
            return Err(WebPlaybackSessionError::NotFound);
        }
        Ok(())
    }

    async fn start_emby_hls_with_runtime(
        &self,
        session_id: &str,
        tier: ServerTier,
        input: &std::path::Path,
        video_bitrate: Option<i64>,
        segment_container: HlsSegmentContainer,
        start_time_ticks: Option<i64>,
        runtime_ticks: Option<i64>,
    ) -> Result<(), WebPlaybackSessionError> {
        self.hls
            .start_emby_vod(
                session_id,
                tier,
                input,
                video_bitrate,
                segment_container,
                start_time_ticks,
                runtime_ticks,
            )
            .await?;
        let directory = self.hls.session_directory(session_id).await?;
        let now = unix_timestamp();
        if !self
            .database
            .set_web_playback_temp_dir(session_id, &directory.to_string_lossy(), now)
            .await?
        {
            let _ = self.hls.stop(session_id).await;
            return Err(WebPlaybackSessionError::NotFound);
        }
        Ok(())
    }

    pub(crate) async fn wait_for_hls_manifest(
        &self,
        session_id: &str,
    ) -> Result<std::path::PathBuf, WebPlaybackSessionError> {
        if let Err(error) = self.activate_emby_hls_session(session_id, None).await {
            if matches!(&error, WebPlaybackSessionError::Hls(error) if error.is_terminal_process_failure())
            {
                self.stop_failed_hls_session(session_id).await;
            }
            return Err(error);
        }
        match self.hls.wait_for_manifest(session_id).await {
            Ok(path) => Ok(path),
            Err(error) => {
                self.stop_failed_hls_session(session_id).await;
                Err(error.into())
            }
        }
    }

    pub(crate) async fn hls_vod_manifest(
        &self,
        session_id: &str,
    ) -> Result<Option<String>, WebPlaybackSessionError> {
        Ok(self.hls.vod_manifest(session_id).await?)
    }

    pub(crate) async fn wait_for_hls_asset(
        &self,
        session_id: &str,
        asset: &str,
    ) -> Result<std::path::PathBuf, WebPlaybackSessionError> {
        if let Err(error) = self
            .activate_emby_hls_session(session_id, Some(asset))
            .await
        {
            if matches!(&error, WebPlaybackSessionError::Hls(error) if error.is_terminal_process_failure())
            {
                self.stop_failed_hls_session(session_id).await;
            }
            return Err(error);
        }
        match self.hls.wait_for_asset(session_id, asset).await {
            Ok(path) => Ok(path),
            Err(error) => {
                if error.is_terminal_process_failure() {
                    self.stop_failed_hls_session(session_id).await;
                }
                Err(error.into())
            }
        }
    }

    async fn activate_emby_hls_session(
        &self,
        session_id: &str,
        asset: Option<&str>,
    ) -> Result<(), WebPlaybackSessionError> {
        if self.hls.has_started(session_id).await? {
            return Ok(());
        }
        let _start_guard = self.emby_start_lock.lock().await;
        if self.hls.has_started(session_id).await? {
            return Ok(());
        }
        let session = self
            .database
            .find_web_playback_session(session_id)
            .await?
            .ok_or(WebPlaybackSessionError::NotFound)?;
        let source_id = session
            .media_source_id
            .as_deref()
            .ok_or(WebPlaybackSessionError::NotFound)?;
        let active_sessions = self
            .database
            .find_active_web_playback_sessions_for_source(
                &session.user_id,
                &session.item_id,
                source_id,
                "lux-emby",
            )
            .await?;
        for active_session in active_sessions {
            if active_session.id != session.id {
                self.stop(&active_session.id, &session.user_id).await?;
            }
        }
        self.hls.start_for_initial_asset(session_id, asset).await?;
        Ok(())
    }

    pub(crate) async fn hls_asset_path(
        &self,
        session_id: &str,
        asset: &str,
    ) -> Result<std::path::PathBuf, WebPlaybackSessionError> {
        Ok(self.hls.asset_path(session_id, asset).await?)
    }

    pub(crate) async fn hls_within_quota(
        &self,
        session_id: &str,
    ) -> Result<bool, WebPlaybackSessionError> {
        Ok(self.hls.within_quota(session_id).await?)
    }

    pub(crate) async fn heartbeat(
        &self,
        session_id: &str,
        user_id: &str,
    ) -> Result<i64, WebPlaybackSessionError> {
        let now = unix_timestamp();
        let expires_at = now.saturating_add(WEB_PLAYBACK_SESSION_TTL_SECONDS);
        if !self
            .database
            .touch_web_playback_session(session_id, user_id, expires_at, now)
            .await?
        {
            return Err(WebPlaybackSessionError::NotFound);
        }
        Ok(expires_at)
    }

    pub(crate) async fn stop(
        &self,
        session_id: &str,
        user_id: &str,
    ) -> Result<(), WebPlaybackSessionError> {
        let _ = self.hls.stop(session_id).await;
        let now = unix_timestamp();
        self.database
            .stop_web_playback_session(session_id, user_id, "STOPPED", now)
            .await?;
        Ok(())
    }

    pub(crate) async fn claim_event(
        &self,
        event: WebPlaybackEvent<'_>,
    ) -> Result<(WebPlaybackEventClaim, Option<StoredWebPlaybackSession>), WebPlaybackSessionError>
    {
        if event.event_id.is_empty()
            || event.sequence < 0
            || event.position_ticks < 0
            || event.duration_ticks.is_some_and(|value| value < 0)
        {
            return Err(WebPlaybackSessionError::Invalid(
                "invalid web playback event".to_owned(),
            ));
        }
        if !matches!(event.state, "PLAYING" | "PAUSED" | "STOPPED") {
            return Err(WebPlaybackSessionError::Invalid(
                "invalid web playback state".to_owned(),
            ));
        }
        let claim = self
            .database
            .accept_web_playback_event(NewWebPlaybackEvent {
                session_id: event.session_id,
                user_id: event.user_id,
                event_id: event.event_id,
                sequence: event.sequence,
                state: event.state,
                position_ticks: event.position_ticks,
                duration_ticks: event.duration_ticks,
                now: unix_timestamp(),
            })
            .await?;
        let session = self
            .database
            .find_web_playback_session(event.session_id)
            .await?;
        if matches!(
            claim,
            WebPlaybackEventClaim::Accepted | WebPlaybackEventClaim::Duplicate
        ) && event.state == "STOPPED"
            && let Err(error) = self.hls.stop(event.session_id).await
        {
            tracing::warn!(
                session_id = %event.session_id,
                %error,
                "failed to release stopped Web HLS session"
            );
        }
        Ok((claim, session))
    }

    async fn stop_failed_hls_session(&self, session_id: &str) {
        if let Err(error) = self.hls.stop(session_id).await {
            tracing::warn!(
                event = "web_hls_failed_session_cleanup",
                session_id = %session_id,
                %error,
                "failed to stop failed HLS process"
            );
        }
        let session = match self.database.find_web_playback_session(session_id).await {
            Ok(session) => session,
            Err(error) => {
                tracing::warn!(
                    event = "web_hls_failed_session_cleanup",
                    session_id = %session_id,
                    %error,
                    "failed to load failed HLS session"
                );
                return;
            }
        };
        let Some(session) = session else {
            return;
        };
        let now = unix_timestamp();
        if let Err(error) = self
            .database
            .stop_web_playback_session(session_id, &session.user_id, "FAILED", now)
            .await
        {
            tracing::warn!(
                event = "web_hls_failed_session_cleanup",
                session_id = %session_id,
                %error,
                "failed to mark HLS session"
            );
        }
    }
}

async fn stop_hls_sessions(hls: &HlsManager, sessions: Vec<StoredWebPlaybackSession>) {
    for session in sessions {
        if session.plan == "SERVER_HLS"
            && let Err(error) = hls.stop(&session.id).await
        {
            tracing::warn!(session_id = %session.id, %error, "failed to clean Web HLS session");
        }
    }
}

fn apply_server_capabilities(
    mut capabilities: PlaybackCapabilities,
    hardware_transcode_available: bool,
) -> PlaybackCapabilities {
    // A browser cannot assert that the NAS has a usable encoder. The server's
    // runtime probe/configuration is the authority for tier 3; the remaining
    // fields describe what the browser can consume.
    capabilities.hardware_transcode = hardware_transcode_available;
    capabilities
}

#[derive(Clone)]
pub struct ResourceSigner {
    key: [u8; 32],
}

impl ResourceSigner {
    pub fn random() -> Self {
        let mut key = [0_u8; 32];
        OsRng.fill_bytes(&mut key);
        Self { key }
    }

    pub fn sign(&self, session_id: &str, resource: &str, expires_at: i64) -> Option<String> {
        let mut mac = HmacSha256::new_from_slice(&self.key).ok()?;
        mac.update(signed_message(session_id, resource, expires_at).as_bytes());
        Some(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
    }

    pub fn verify(
        &self,
        session_id: &str,
        resource: &str,
        expires_at: i64,
        signature: &str,
        now: i64,
    ) -> bool {
        if expires_at < now {
            return false;
        }
        let Some(expected) = self.sign(session_id, resource, expires_at) else {
            return false;
        };
        expected.as_bytes().ct_eq(signature.as_bytes()).into()
    }
}

pub fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default()
}

fn signed_message(session_id: &str, resource: &str, expires_at: i64) -> String {
    format!("lux-web-playback\n{session_id}\n{resource}\n{expires_at}")
}

fn emby_direct_stream_resource(
    user_id: &str,
    is_admin: bool,
    item_id: &str,
    media_source_id: &str,
) -> String {
    format!("emby-direct-stream\n{user_id}\n{is_admin}\n{item_id}\n{media_source_id}")
}

#[cfg(test)]
mod tests {
    use std::{os::unix::fs::PermissionsExt, path::Path, sync::Arc};

    use super::{
        EMBY_DIRECT_STREAM_TTL_SECONDS, ResourceSigner, apply_server_capabilities, unix_timestamp,
    };
    use crate::{
        application::{
            libraries::LibraryService,
            playback::{
                decision::{PlaybackCapabilities, PlaybackSourceKind, ServerTier},
                hls::{HlsManager, HlsSegmentContainer},
                session::{
                    CreateWebPlaybackSession, WebPlaybackEvent, WebPlaybackPlan,
                    WebPlaybackSessionService,
                },
            },
            setup::SetupService,
        },
        config::Config,
        library::LibraryKind,
        storage::{NewWebPlaybackSession, WebPlaybackEventClaim},
    };
    use uuid::Uuid;

    #[test]
    fn signatures_are_bound_to_the_session_resource_and_expiry() {
        let signer = ResourceSigner::random();
        let expires_at = unix_timestamp() + 60;
        let signature = signer.sign("session-1", "direct", expires_at).unwrap();

        assert!(signer.verify(
            "session-1",
            "direct",
            expires_at,
            &signature,
            unix_timestamp()
        ));
        assert!(!signer.verify(
            "session-2",
            "direct",
            expires_at,
            &signature,
            unix_timestamp()
        ));
        assert!(!signer.verify("session-1", "hls", expires_at, &signature, unix_timestamp()));
        assert!(!signer.verify(
            "session-1",
            "direct",
            expires_at - 1,
            &signature,
            unix_timestamp()
        ));
    }

    #[test]
    fn expired_signatures_are_rejected_before_comparison() {
        let signer = ResourceSigner::random();
        let expires_at = unix_timestamp() - 1;
        let signature = signer.sign("session-1", "direct", expires_at).unwrap();

        assert!(!signer.verify(
            "session-1",
            "direct",
            expires_at,
            &signature,
            unix_timestamp()
        ));
    }

    #[test]
    fn emby_direct_stream_tickets_last_twelve_hours() {
        assert_eq!(EMBY_DIRECT_STREAM_TTL_SECONDS, 12 * 60 * 60);
    }

    #[test]
    fn server_hardware_capability_does_not_depend_on_the_browser_hint() {
        let normalized = apply_server_capabilities(
            PlaybackCapabilities {
                direct_play: false,
                hls: true,
                video_copy_to_fmp4: false,
                audio_copy_to_fmp4: false,
                hardware_transcode: false,
                software_transcode: true,
            },
            true,
        );

        assert!(normalized.hardware_transcode);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stopped_events_release_server_hls_resources() -> Result<(), Box<dyn std::error::Error>>
    {
        let temp_dir = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        };
        let database = crate::storage::Database::connect(&config).await?;
        let setup = SetupService::new(database.clone())?;
        let user = setup.complete("Admin", "Admin", "correct password").await?;
        let library = LibraryService::new(database.clone())
            .create_library("Playback", LibraryKind::Movie, false)
            .await?;
        let item_id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', 'Playback', 'playback', 'LOCAL_CONFIRMED')",
        )
        .bind(&item_id)
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;

        let script = temp_dir.path().join("fake-ffmpeg");
        tokio::fs::write(
            &script,
            "#!/bin/sh\nset -eu\nmanifest=\"\"\nsegment=\"\"\nwhile [ \"$#\" -gt 0 ]; do\n  case \"$1\" in\n    -hls_segment_filename) segment=\"$2\"; shift 2 ;;\n    *.m3u8) manifest=\"$1\"; shift ;;\n    *) shift ;;\n  esac\ndone\ndirectory=$(dirname \"$manifest\")\nmkdir -p \"$directory\"\nprintf '#EXTM3U\\n#EXT-X-MAP:URI=\\\"init.mp4\\\"\\n#EXTINF:1,\\nsegment_000000.m4s\\n' > \"$manifest\"\nprintf init > \"$directory/init.mp4\"\nprintf segment > \"$(printf '%s' \"$segment\" | sed 's/%06d/000000/')\"\n",
        )
        .await?;
        let mut permissions = tokio::fs::metadata(&script).await?.permissions();
        permissions.set_mode(0o700);
        tokio::fs::set_permissions(&script, permissions).await?;

        let service = WebPlaybackSessionService {
            database: database.clone(),
            signer: Arc::new(ResourceSigner::random()),
            hls: HlsManager::new_for_tests(
                config.config_dir.clone(),
                script.to_string_lossy().into_owned(),
            ),
            emby_start_lock: Arc::new(tokio::sync::Mutex::new(())),
        };
        let user_id = user.id.to_string();
        service
            .database
            .insert_web_playback_session(NewWebPlaybackSession {
                id: "session-1",
                user_id: &user_id,
                item_id: &item_id,
                media_source_id: None,
                play_session_id: "lux-web:session-1",
                tier: i64::from(ServerTier::Remux.number()),
                plan: "SERVER_HLS",
                temp_dir: None,
                is_admin: true,
                expires_at: unix_timestamp() + 900,
                now: unix_timestamp(),
            })
            .await?;
        service
            .start_hls(
                "session-1",
                ServerTier::Remux,
                Path::new("input.mkv"),
                None,
                None,
            )
            .await?;
        service.wait_for_hls_manifest("session-1").await?;

        let (claim, _) = service
            .claim_event(WebPlaybackEvent {
                session_id: "session-1",
                user_id: &user_id,
                event_id: "stop-1",
                sequence: 1,
                state: "STOPPED",
                position_ticks: 100,
                duration_ticks: Some(1_000),
            })
            .await?;

        assert_eq!(claim, WebPlaybackEventClaim::Accepted);
        assert!(!config.config_dir.join("web-playback/session-1").exists());
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_hls_manifest_stops_session_and_removes_resources()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        };
        let database = crate::storage::Database::connect(&config).await?;
        let setup = SetupService::new(database.clone())?;
        let user = setup.complete("Admin", "Admin", "correct password").await?;
        let library = LibraryService::new(database.clone())
            .create_library("Playback", LibraryKind::Movie, false)
            .await?;
        let item_id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', 'Playback', 'playback', 'LOCAL_CONFIRMED')",
        )
        .bind(&item_id)
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;

        let service = WebPlaybackSessionService {
            database: database.clone(),
            signer: Arc::new(ResourceSigner::random()),
            hls: HlsManager::new_for_tests(config.config_dir.clone(), "/usr/bin/false".to_owned()),
            emby_start_lock: Arc::new(tokio::sync::Mutex::new(())),
        };
        let user_id = user.id.to_string();
        let now = unix_timestamp();
        service
            .database
            .insert_web_playback_session(NewWebPlaybackSession {
                id: "failed-manifest",
                user_id: &user_id,
                item_id: &item_id,
                media_source_id: None,
                play_session_id: "lux-emby:failed-manifest",
                tier: i64::from(ServerTier::Remux.number()),
                plan: "SERVER_HLS",
                temp_dir: None,
                is_admin: true,
                expires_at: now + 900,
                now,
            })
            .await?;
        service
            .start_hls(
                "failed-manifest",
                ServerTier::Remux,
                Path::new("input.mkv"),
                None,
                None,
            )
            .await?;

        assert!(
            service
                .wait_for_hls_manifest("failed-manifest")
                .await
                .is_err()
        );
        let stored = database
            .find_web_playback_session("failed-manifest")
            .await?
            .ok_or("missing failed manifest session")?;
        assert_eq!(stored.state, "FAILED");
        assert!(
            !config
                .config_dir
                .join("web-playback/failed-manifest")
                .exists()
        );
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_emby_segment_restart_stops_session_and_removes_resources()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        };
        let database = crate::storage::Database::connect(&config).await?;
        let setup = SetupService::new(database.clone())?;
        let user = setup.complete("Admin", "Admin", "correct password").await?;
        let library = LibraryService::new(database.clone())
            .create_library("Playback", LibraryKind::Movie, false)
            .await?;
        let item_id = Uuid::now_v7().to_string();
        let source_id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', 'Playback', 'playback', 'LOCAL_CONFIRMED')",
        )
        .bind(&item_id)
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO media_sources (id, item_id, source_kind)
             VALUES (?, ?, 'LOCAL_FILE')",
        )
        .bind(&source_id)
        .bind(&item_id)
        .execute(database.pool())
        .await?;

        let script = temp_dir.path().join("fake-ffmpeg-failed-restart");
        tokio::fs::write(
            &script,
            r##"#!/bin/sh
set -eu
manifest=""
segment=""
start_number=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    -hls_segment_filename) segment="$2"; shift 2 ;;
    -start_number) start_number="$2"; shift 2 ;;
    *.m3u8) manifest="$1"; shift ;;
    *) shift ;;
  esac
done
if [ "$start_number" -ne 0 ]; then
  exit 1
fi
directory=$(dirname "$manifest")
mkdir -p "$directory"
printf '#EXTM3U\n#EXT-X-MAP:URI="init.mp4"\n#EXTINF:4,\nsegment_000000.m4s\n' > "$manifest"
printf init > "$directory/init.mp4"
printf segment > "$(printf '%s' "$segment" | sed 's/%06d/000000/')"
while :; do sleep 1; done
"##,
        )
        .await?;
        let mut permissions = tokio::fs::metadata(&script).await?.permissions();
        permissions.set_mode(0o700);
        tokio::fs::set_permissions(&script, permissions).await?;

        let service = WebPlaybackSessionService {
            database: database.clone(),
            signer: Arc::new(ResourceSigner::random()),
            hls: HlsManager::new_for_tests(
                config.config_dir.clone(),
                script.to_string_lossy().into_owned(),
            ),
            emby_start_lock: Arc::new(tokio::sync::Mutex::new(())),
        };
        let user_id = user.id.to_string();
        let created = service
            .create_and_start_emby_hls(
                CreateWebPlaybackSession {
                    user_id: &user_id,
                    is_admin: true,
                    item_id: &item_id,
                    media_source_id: &source_id,
                    play_session_prefix: "lux-emby",
                    source_kind: PlaybackSourceKind::LocalFile,
                    capabilities: PlaybackCapabilities {
                        direct_play: false,
                        hls: true,
                        video_copy_to_fmp4: false,
                        audio_copy_to_fmp4: false,
                        hardware_transcode: false,
                        software_transcode: true,
                    },
                },
                Path::new("input.mkv"),
                Some(1_000_000),
                HlsSegmentContainer::FragmentedMp4,
                None,
                Some(600 * 10_000_000),
            )
            .await?;
        service.wait_for_hls_manifest(&created.id).await?;

        assert!(
            service
                .wait_for_hls_asset(&created.id, "segment_000100.m4s")
                .await
                .is_err()
        );
        let stored = database
            .find_web_playback_session(&created.id)
            .await?
            .ok_or("missing failed segment session")?;
        assert_eq!(stored.state, "FAILED");
        assert!(
            !config
                .config_dir
                .join("web-playback")
                .join(&created.id)
                .exists()
        );

        let replacement = service
            .create_and_start_emby_hls(
                CreateWebPlaybackSession {
                    user_id: &user_id,
                    is_admin: true,
                    item_id: &item_id,
                    media_source_id: &source_id,
                    play_session_prefix: "lux-emby",
                    source_kind: PlaybackSourceKind::LocalFile,
                    capabilities: PlaybackCapabilities {
                        direct_play: false,
                        hls: true,
                        video_copy_to_fmp4: false,
                        audio_copy_to_fmp4: false,
                        hardware_transcode: false,
                        software_transcode: true,
                    },
                },
                Path::new("input.mkv"),
                Some(1_000_000),
                HlsSegmentContainer::FragmentedMp4,
                None,
                Some(600 * 10_000_000),
            )
            .await?;
        service.wait_for_hls_manifest(&replacement.id).await?;
        service.stop(&replacement.id, &user_id).await?;
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn emby_hls_offer_replaces_an_active_session_only_when_the_new_asset_starts()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8098".parse()?,
            config_dir: temp_dir.path().join("config"),
        };
        let database = crate::storage::Database::connect(&config).await?;
        let setup = SetupService::new(database.clone())?;
        let user = setup.complete("Admin", "Admin", "correct password").await?;
        let library = LibraryService::new(database.clone())
            .create_library("Playback", LibraryKind::Movie, false)
            .await?;
        let item_id = Uuid::now_v7().to_string();
        let source_id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', 'Playback', 'playback', 'LOCAL_CONFIRMED')",
        )
        .bind(&item_id)
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO media_sources (id, item_id, source_kind)
             VALUES (?, ?, 'LOCAL_FILE')",
        )
        .bind(&source_id)
        .bind(&item_id)
        .execute(database.pool())
        .await?;

        let script = temp_dir.path().join("fake-ffmpeg");
        tokio::fs::write(
            &script,
            r##"#!/bin/sh
set -eu
manifest=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    *.m3u8) manifest="$1" ;;
  esac
  shift
done
directory=$(dirname "$manifest")
mkdir -p "$directory"
printf '#EXTM3U\n' > "$manifest"
while :; do sleep 1; done
"##,
        )
        .await?;
        let mut permissions = tokio::fs::metadata(&script).await?.permissions();
        permissions.set_mode(0o700);
        tokio::fs::set_permissions(&script, permissions).await?;

        let service = WebPlaybackSessionService {
            database: database.clone(),
            signer: Arc::new(ResourceSigner::random()),
            hls: HlsManager::new_for_tests(
                config.config_dir.clone(),
                script.to_string_lossy().into_owned(),
            ),
            emby_start_lock: Arc::new(tokio::sync::Mutex::new(())),
        };
        let user_id = user.id.to_string();
        let capabilities = PlaybackCapabilities {
            direct_play: false,
            hls: true,
            video_copy_to_fmp4: false,
            audio_copy_to_fmp4: false,
            hardware_transcode: false,
            software_transcode: true,
        };
        let first = service
            .create_and_start_emby_hls(
                CreateWebPlaybackSession {
                    user_id: &user_id,
                    is_admin: true,
                    item_id: &item_id,
                    media_source_id: &source_id,
                    play_session_prefix: "lux-emby",
                    source_kind: PlaybackSourceKind::LocalFile,
                    capabilities,
                },
                Path::new("input.mkv"),
                Some(1_000_000),
                HlsSegmentContainer::FragmentedMp4,
                None,
                None,
            )
            .await?;
        assert!(matches!(
            first.plan,
            WebPlaybackPlan::ServerHls {
                tier: ServerTier::SoftwareTranscode
            }
        ));
        service.wait_for_hls_manifest(&first.id).await?;

        let second = service
            .create_and_start_emby_hls(
                CreateWebPlaybackSession {
                    user_id: &user_id,
                    is_admin: true,
                    item_id: &item_id,
                    media_source_id: &source_id,
                    play_session_prefix: "lux-emby",
                    source_kind: PlaybackSourceKind::LocalFile,
                    capabilities,
                },
                Path::new("input.mkv"),
                Some(2_000_000),
                HlsSegmentContainer::FragmentedMp4,
                None,
                None,
            )
            .await?;

        let first_before_second_asset = database
            .find_web_playback_session(&first.id)
            .await?
            .expect("first session before second asset");
        assert_eq!(first_before_second_asset.state, "ACTIVE");
        assert!(
            config
                .config_dir
                .join("web-playback")
                .join(&first.id)
                .exists()
        );
        assert!(!service.hls.has_started(&second.id).await?);

        service.wait_for_hls_manifest(&second.id).await?;

        let first_stored = database
            .find_web_playback_session(&first.id)
            .await?
            .expect("first session");
        let second_stored = database
            .find_web_playback_session(&second.id)
            .await?
            .expect("second session");
        assert_eq!(first_stored.state, "STOPPED");
        assert_eq!(second_stored.state, "ACTIVE");
        assert!(
            !config
                .config_dir
                .join("web-playback")
                .join(&first.id)
                .exists()
        );

        Ok(())
    }
}
