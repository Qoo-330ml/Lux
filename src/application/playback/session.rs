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
use super::hls::{HlsError, HlsManager};

type HmacSha256 = Hmac<Sha256>;

pub const WEB_PLAYBACK_SESSION_TTL_SECONDS: i64 = 15 * 60;
const WEB_PLAYBACK_CLEANUP_INTERVAL: Duration = Duration::from_secs(5);

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
                for session in sessions {
                    if session.plan == "SERVER_HLS"
                        && let Err(error) = cleanup.stop(&session.id).await
                    {
                        tracing::warn!(session_id = %session.id, %error, "failed to clean expired Web HLS session");
                    }
                }
            }
        });
        Self {
            database,
            signer: Arc::new(ResourceSigner::random()),
            hls,
        }
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
        let mut capabilities = input.capabilities;
        capabilities.hardware_transcode &= self.hls.hardware_transcode_available();
        let plan = WebPlaybackPlan::from(choose_plan(PlaybackDecisionInput {
            source_kind: input.source_kind,
            capabilities,
        }));
        let WebPlaybackPlan::Unsupported { .. } = plan else {
            let id = Uuid::now_v7().to_string();
            let play_session_id = format!("lux-web:{id}");
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
    ) -> Result<(), WebPlaybackSessionError> {
        self.hls.start(session_id, tier, input).await?;
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
        Ok(self.hls.wait_for_manifest(session_id).await?)
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
        Ok((claim, session))
    }
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

#[cfg(test)]
mod tests {
    use super::{ResourceSigner, unix_timestamp};

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
}
