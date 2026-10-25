use std::{
    collections::HashMap,
    fmt,
    net::IpAddr,
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
    Database, NewAuditEvent, NewEmbyDirectPlaybackTicket, NewUserActivityEvent,
    NewWebPlaybackEvent, NewWebPlaybackSession, PlaybackAdmissionResult, StorageError,
    StoredEmbyDirectPlaybackTicket, StoredWebPlaybackSession, UserAccessBlacklistIpRuleQuery,
    UserAccessBlacklistRuleMatchQuery, UserPlaybackPolicy, WebPlaybackEventClaim,
    WebPlaybackTranscodingDetails,
};

use crate::application::user_access_rules::{
    IpBlacklistEvaluationError, MAX_ACTIVE_IP_BLACKLIST_RULES_PER_REQUEST,
    UserAccessBlacklistRuleType, ip_blacklist_rules_match_address,
    normalize_user_access_blacklist_value,
};

use super::decision::{
    PlaybackCapabilities, PlaybackDecisionInput, PlaybackPlan, PlaybackSourceKind, ServerTier,
    UnsupportedReason, choose_plan,
};
use super::hls::{
    HlsError, HlsManager, HlsProgressSnapshot, HlsSegmentContainer, HlsStartOptions,
    HlsWebStartOptions,
};

type HmacSha256 = Hmac<Sha256>;

pub const WEB_PLAYBACK_SESSION_TTL_SECONDS: i64 = 15 * 60;
/// Signed asset URLs are embedded in a playlist the client fetches once, so they must outlive
/// several heartbeat-extended session windows. Access is still gated on the live session row.
pub const WEB_PLAYBACK_RESOURCE_TTL_SECONDS: i64 = 12 * 60 * 60;
pub const EMBY_DIRECT_STREAM_TTL_SECONDS: i64 = 12 * 60 * 60;
pub(crate) const EMBY_DIRECT_STREAM_TICKET_VERSION: &str = "2";
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

/// One session's stored plan plus whatever the running FFmpeg has reported so far.
#[derive(Debug)]
pub(crate) struct WebPlaybackStats {
    pub(crate) session: StoredWebPlaybackSession,
    pub(crate) progress: Option<HlsProgressSnapshot>,
    /// Measured from the newest closed segment, because the HLS muxer reports no bitrate.
    pub(crate) latest_segment_bitrate_kbps: Option<f64>,
    /// Total bytes written by this session, from the quota cache when it is fresh.
    pub(crate) directory_bytes: Option<u64>,
}

#[derive(Debug)]
pub(crate) enum WebPlaybackSessionError {
    Invalid(String),
    PlaybackPolicyDenied,
    NotFound,
    Expired,
    NotActive,
    AccessRulesUnavailable,
    Hls(HlsError),
    Storage(StorageError),
}

impl fmt::Display for WebPlaybackSessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => formatter.write_str(message),
            Self::PlaybackPolicyDenied => {
                formatter.write_str("server-side playback is not allowed by user policy")
            }
            Self::NotFound => formatter.write_str("web playback session not found"),
            Self::Expired => formatter.write_str("web playback session expired"),
            Self::NotActive => formatter.write_str("web playback session is not active"),
            Self::AccessRulesUnavailable => {
                formatter.write_str("user access rules are unavailable")
            }
            Self::Hls(error) => error.fmt(formatter),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for WebPlaybackSessionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Storage(error) => Some(error),
            Self::Invalid(_)
            | Self::PlaybackPolicyDenied
            | Self::NotFound
            | Self::Expired
            | Self::NotActive
            | Self::AccessRulesUnavailable => None,
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
    start_locks: SessionStartLocks,
}

/// Upper bound for the per-source start locks kept in memory.
const MAX_SESSION_START_LOCKS: usize = 4096;

/// Serializes HLS starts that can replace one another.
///
/// Starting a session stops any other active session of the same user, item, and media source,
/// so those starts must not interleave. Unrelated sources start concurrently: a single global
/// start lock serialized every HLS start on the server behind database work and an ffmpeg spawn.
#[derive(Clone, Default)]
struct SessionStartLocks {
    locks: Arc<tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

impl SessionStartLocks {
    async fn lock(&self, key: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.locks.lock().await;
            if locks.len() >= MAX_SESSION_START_LOCKS && !locks.contains_key(key) {
                locks.retain(|_, lock| Arc::strong_count(lock) > 1);
            }
            locks
                .entry(key.to_owned())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        lock.lock_owned().await
    }
}

fn emby_hls_start_key(user_id: &str, item_id: &str, media_source_id: &str) -> String {
    format!("{user_id}\u{1f}{item_id}\u{1f}{media_source_id}")
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
            start_locks: SessionStartLocks::default(),
        }
    }

    pub(crate) fn with_hls_executable(mut self, executable: std::path::PathBuf) -> Self {
        self.hls = self
            .hls
            .with_executable(executable.to_string_lossy().into_owned());
        self
    }

    #[cfg(test)]
    pub(crate) fn sign_emby_direct_stream_legacy(
        &self,
        user_id: &str,
        is_admin: bool,
        item_id: &str,
        media_source_id: &str,
        play_session_id: &str,
        expires_at: i64,
    ) -> Option<String> {
        self.signer.sign(
            EMBY_DIRECT_STREAM_SESSION_ID,
            &emby_direct_stream_resource(
                user_id,
                is_admin,
                item_id,
                media_source_id,
                play_session_id,
            ),
            expires_at,
        )
    }

    pub(crate) async fn check_playback_admission(
        &self,
        user_id: &str,
    ) -> Result<Option<PlaybackAdmissionResult>, WebPlaybackSessionError> {
        Ok(self
            .database
            .check_playback_admission(user_id, unix_timestamp())
            .await?)
    }

    pub(crate) async fn admit_playback_session(
        &self,
        user_id: &str,
        play_session_id: &str,
    ) -> Result<Option<PlaybackAdmissionResult>, WebPlaybackSessionError> {
        Ok(self
            .database
            .reserve_playback_admission(user_id, play_session_id, unix_timestamp())
            .await?)
    }

    pub(crate) async fn renew_playback_session_admission(
        &self,
        user_id: &str,
        play_session_id: &str,
    ) -> Result<bool, WebPlaybackSessionError> {
        Ok(self
            .database
            .renew_existing_playback_admission(user_id, play_session_id, unix_timestamp())
            .await?)
    }

    pub(crate) async fn release_playback_session_admission(
        &self,
        user_id: &str,
        play_session_id: &str,
    ) -> Result<bool, WebPlaybackSessionError> {
        Ok(self
            .database
            .release_playback_admission(user_id, play_session_id, unix_timestamp())
            .await?)
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
        let policy = self
            .database
            .get_user_playback_policy(input.user_id)
            .await?
            .ok_or(WebPlaybackSessionError::NotFound)?;
        let capabilities =
            apply_server_capabilities(input.capabilities, self.hls.hardware_transcode_available());
        let plan = WebPlaybackPlan::from(choose_plan_for_policy(
            PlaybackDecisionInput {
                source_kind: input.source_kind,
                capabilities,
            },
            policy,
        )?);
        // An administrator can turn server-side processing off entirely. The plan then degrades to
        // direct play (or an explicit unsupported reason) instead of starting FFmpeg.
        let plan = if !self.transcoding_settings().await.enabled {
            match plan {
                WebPlaybackPlan::ServerHls { .. } => WebPlaybackPlan::Unsupported {
                    reason: UnsupportedReason::ServerTranscodingDisabled,
                },
                plan => plan,
            }
        } else {
            plan
        };
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
        options: HlsStartOptions,
    ) -> Result<CreatedWebPlaybackSession, WebPlaybackSessionError> {
        let _start_guard = self
            .start_locks
            .lock(&emby_hls_start_key(
                input.user_id,
                input.item_id,
                input.media_source_id,
            ))
            .await;
        let user_id = input.user_id.to_owned();
        let created = self.create(input).await?;
        let WebPlaybackPlan::ServerHls { tier } = created.plan else {
            return Ok(created);
        };
        if let Err(error) = self
            .start_emby_hls_with_runtime(&created.id, tier, media_path, video_bitrate, options)
            .await
        {
            let _ = self.stop(&created.id, &user_id).await;
            return Err(error);
        }
        Ok(created)
    }

    pub(crate) async fn set_transcoding_details(
        &self,
        session_id: &str,
        user_id: &str,
        details: &WebPlaybackTranscodingDetails,
    ) -> Result<(), WebPlaybackSessionError> {
        if !self
            .database
            .set_web_playback_transcoding_details(session_id, user_id, details, unix_timestamp())
            .await?
        {
            return Err(WebPlaybackSessionError::NotFound);
        }
        Ok(())
    }

    pub(crate) async fn set_authorization_context(
        &self,
        session_id: &str,
        user_id: &str,
        device_id: Option<&str>,
        client_name: Option<&str>,
    ) -> Result<(), WebPlaybackSessionError> {
        if !self
            .database
            .set_web_playback_access_context(
                session_id,
                user_id,
                device_id,
                client_name,
                unix_timestamp(),
            )
            .await?
        {
            return Err(WebPlaybackSessionError::NotFound);
        }
        Ok(())
    }

    pub(crate) async fn register_emby_direct_playback_ticket(
        &self,
        user_id: &str,
        play_session_id: &str,
        authorization_device_id: Option<&str>,
        authorization_client_name: Option<&str>,
        expires_at: i64,
        created_at: i64,
    ) -> Result<(), StorageError> {
        self.database
            .insert_emby_direct_playback_ticket(NewEmbyDirectPlaybackTicket {
                user_id,
                play_session_id,
                authorization_device_id,
                authorization_client_name,
                expires_at,
                created_at,
            })
            .await
    }

    pub(crate) async fn authorize_emby_direct_playback_ticket_with_ip(
        &self,
        user_id: &str,
        play_session_id: &str,
        expires_at: i64,
        legacy_ticket: bool,
        client_ip: Option<IpAddr>,
    ) -> Result<(), WebPlaybackSessionError> {
        let now = unix_timestamp();
        if legacy_ticket {
            self.database
                .ensure_legacy_emby_direct_playback_ticket(
                    user_id,
                    play_session_id,
                    expires_at,
                    now,
                )
                .await?;
        }
        let Some(ticket) = self
            .database
            .find_emby_direct_playback_ticket(user_id, play_session_id)
            .await?
        else {
            return Err(WebPlaybackSessionError::NotFound);
        };
        if ticket.user_id != user_id || ticket.expires_at != expires_at {
            return Err(WebPlaybackSessionError::NotFound);
        }
        if ticket.revoked_at.is_some() {
            return Err(WebPlaybackSessionError::NotActive);
        }
        if ticket.expires_at < now {
            return Err(WebPlaybackSessionError::Expired);
        }
        if ticket.created_at > now {
            return Err(WebPlaybackSessionError::NotFound);
        }
        self.enforce_emby_direct_ticket_access_rules(&ticket, client_ip)
            .await
    }

    async fn enforce_emby_direct_ticket_access_rules(
        &self,
        ticket: &StoredEmbyDirectPlaybackTicket,
        client_ip: Option<IpAddr>,
    ) -> Result<(), WebPlaybackSessionError> {
        let device_id = ticket
            .authorization_device_id
            .as_deref()
            .map(|value| {
                normalize_user_access_blacklist_value(UserAccessBlacklistRuleType::Device, value)
                    .map_err(|_| WebPlaybackSessionError::AccessRulesUnavailable)
            })
            .transpose()?;
        let client_name = ticket
            .authorization_client_name
            .as_deref()
            .map(|value| {
                normalize_user_access_blacklist_value(UserAccessBlacklistRuleType::Client, value)
                    .map_err(|_| WebPlaybackSessionError::AccessRulesUnavailable)
            })
            .transpose()?;

        if device_id.is_none() || client_name.is_none() {
            let (device_rules_active, client_rules_active) = self
                .database
                .active_user_access_device_client_blacklist_rules(&ticket.user_id)
                .await?;
            if device_id.is_none() && device_rules_active {
                self.revoke_emby_direct_ticket_after_rule_hit(ticket, "DEVICE_CONTEXT_MISSING")
                    .await?;
                return Err(WebPlaybackSessionError::NotActive);
            }
            if client_name.is_none() && client_rules_active {
                self.revoke_emby_direct_ticket_after_rule_hit(ticket, "CLIENT_CONTEXT_MISSING")
                    .await?;
                return Err(WebPlaybackSessionError::NotActive);
            }
        }

        if let Some(device_id) = device_id.as_deref()
            && self
                .database
                .user_access_blacklist_matches(UserAccessBlacklistRuleMatchQuery {
                    user_id: &ticket.user_id,
                    device_id: Some(device_id),
                    client_name: None,
                })
                .await?
        {
            self.revoke_emby_direct_ticket_after_rule_hit(ticket, "DEVICE")
                .await?;
            return Err(WebPlaybackSessionError::NotActive);
        }
        if let Some(client_name) = client_name.as_deref()
            && self
                .database
                .user_access_blacklist_matches(UserAccessBlacklistRuleMatchQuery {
                    user_id: &ticket.user_id,
                    device_id: None,
                    client_name: Some(client_name),
                })
                .await?
        {
            self.revoke_emby_direct_ticket_after_rule_hit(ticket, "CLIENT")
                .await?;
            return Err(WebPlaybackSessionError::NotActive);
        }

        let ip_rules = self
            .database
            .list_user_access_ip_blacklist_rule_values(UserAccessBlacklistIpRuleQuery {
                user_id: &ticket.user_id,
                limit: MAX_ACTIVE_IP_BLACKLIST_RULES_PER_REQUEST as i64 + 1,
            })
            .await?;
        if ip_rules.is_empty() {
            return Ok(());
        }
        let Some(client_ip) = client_ip else {
            self.revoke_emby_direct_ticket_after_rule_hit(ticket, "IP_CONTEXT_MISSING")
                .await?;
            return Err(WebPlaybackSessionError::AccessRulesUnavailable);
        };
        match ip_blacklist_rules_match_address(client_ip, &ip_rules) {
            Ok(true) => {
                self.revoke_emby_direct_ticket_after_rule_hit(ticket, "IP")
                    .await?;
                Err(WebPlaybackSessionError::NotActive)
            }
            Ok(false) => Ok(()),
            Err(IpBlacklistEvaluationError::InvalidStoredRule) => {
                self.revoke_emby_direct_ticket_after_rule_hit(ticket, "IP_RULE_INVALID")
                    .await?;
                Err(WebPlaybackSessionError::AccessRulesUnavailable)
            }
            Err(IpBlacklistEvaluationError::TooManyRules) => {
                self.revoke_emby_direct_ticket_after_rule_hit(ticket, "IP_RULE_LIMIT")
                    .await?;
                Err(WebPlaybackSessionError::AccessRulesUnavailable)
            }
        }
    }

    async fn revoke_emby_direct_ticket_after_rule_hit(
        &self,
        ticket: &StoredEmbyDirectPlaybackTicket,
        reason: &str,
    ) -> Result<(), WebPlaybackSessionError> {
        let now = unix_timestamp();
        let revoked = self
            .database
            .revoke_emby_direct_playback_ticket(&ticket.user_id, &ticket.play_session_id, now)
            .await?;
        self.stop_emby_playback_session(&ticket.user_id, &ticket.play_session_id, now)
            .await;
        if revoked {
            self.record_emby_direct_ticket_rule_hit(ticket, reason, now)
                .await;
        }
        Ok(())
    }

    async fn stop_emby_playback_session(&self, user_id: &str, play_session_id: &str, now: i64) {
        match self
            .database
            .find_playback_session(user_id, play_session_id)
            .await
        {
            Ok(Some(session)) if session.state != "STOPPED" => {
                if let Err(error) = self.database.stop_playback_session(&session.id, now).await {
                    tracing::warn!(%error, "failed to stop playback after direct ticket rejection");
                }
            }
            Ok(Some(_)) | Ok(None) => {}
            Err(error) => {
                tracing::warn!(%error, "failed to find playback after direct ticket rejection");
            }
        }
        if let Err(error) = self
            .database
            .release_playback_admission(user_id, play_session_id, now)
            .await
        {
            tracing::warn!(%error, "failed to release admission after direct ticket rejection");
        }
    }

    async fn record_emby_direct_ticket_rule_hit(
        &self,
        ticket: &StoredEmbyDirectPlaybackTicket,
        reason: &str,
        occurred_at: i64,
    ) {
        let event_type = if matches!(reason, "DEVICE" | "CLIENT" | "IP") {
            "USER_ACCESS_BLACKLIST_HIT"
        } else {
            "USER_ACCESS_RULES_DENIED"
        };
        let metadata_json = serde_json::json!({
            "reason": reason,
            "source": "EMBY_DIRECT_TICKET",
        })
        .to_string();
        if let Err(error) = self
            .database
            .insert_user_activity_event(NewUserActivityEvent {
                user_id: &ticket.user_id,
                actor_user_id: None,
                event_type,
                target_type: Some("playback_session"),
                target_id: Some(&ticket.play_session_id),
                metadata_json: &metadata_json,
            })
            .await
        {
            tracing::warn!(event_type, %error, "failed to record playback blacklist activity");
        }
        if let Err(error) = self
            .database
            .insert_audit_event(NewAuditEvent {
                actor_user_id: None,
                event_type,
                target_type: Some("playback_session"),
                target_id: Some(&ticket.play_session_id),
                metadata_json: &metadata_json,
            })
            .await
        {
            tracing::warn!(event_type, %error, "failed to record playback blacklist audit");
        }
        tracing::warn!(
            event = event_type,
            reason,
            occurred_at,
            "Emby direct playback ticket revoked by access blacklist"
        );
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

    pub(crate) fn verify_resource_signature(
        &self,
        session_id: &str,
        resource: &str,
        expires_at: i64,
        signature: &str,
    ) -> Result<(), WebPlaybackSessionError> {
        if self.signer.verify(
            session_id,
            resource,
            expires_at,
            signature,
            unix_timestamp(),
        ) {
            Ok(())
        } else {
            Err(WebPlaybackSessionError::NotFound)
        }
    }

    pub(crate) fn verify_emby_direct_stream(
        &self,
        user_id: &str,
        is_admin: bool,
        item_id: &str,
        media_source_id: &str,
        play_session_id: &str,
        expires_at: i64,
        signature: &str,
    ) -> bool {
        self.signer.verify(
            EMBY_DIRECT_STREAM_SESSION_ID,
            &emby_direct_stream_resource(
                user_id,
                is_admin,
                item_id,
                media_source_id,
                play_session_id,
            ),
            expires_at,
            signature,
            unix_timestamp(),
        )
    }

    pub(crate) fn sign_emby_direct_stream_v2(
        &self,
        user_id: &str,
        is_admin: bool,
        item_id: &str,
        media_source_id: &str,
        play_session_id: &str,
        expires_at: i64,
    ) -> Option<String> {
        self.signer.sign(
            EMBY_DIRECT_STREAM_SESSION_ID,
            &emby_direct_stream_resource_v2(
                user_id,
                is_admin,
                item_id,
                media_source_id,
                play_session_id,
            ),
            expires_at,
        )
    }

    pub(crate) fn verify_emby_direct_stream_v2(
        &self,
        user_id: &str,
        is_admin: bool,
        item_id: &str,
        media_source_id: &str,
        play_session_id: &str,
        expires_at: i64,
        signature: &str,
    ) -> bool {
        self.signer.verify(
            EMBY_DIRECT_STREAM_SESSION_ID,
            &emby_direct_stream_resource_v2(
                user_id,
                is_admin,
                item_id,
                media_source_id,
                play_session_id,
            ),
            expires_at,
            signature,
            unix_timestamp(),
        )
    }

    pub(crate) async fn authorize_resource_with_ip(
        &self,
        session_id: &str,
        resource: &str,
        expires_at: i64,
        signature: &str,
        client_ip: Option<IpAddr>,
    ) -> Result<StoredWebPlaybackSession, WebPlaybackSessionError> {
        self.verify_resource_signature(session_id, resource, expires_at, signature)?;
        let now = unix_timestamp();
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
        if session.play_session_id.starts_with("lux-web:")
            || session.play_session_id.starts_with("lux-emby:")
        {
            self.enforce_playback_access_rules(&session, client_ip)
                .await?;
        }
        if session.plan == "SERVER_HLS" {
            let policy = self
                .database
                .get_user_playback_policy(&session.user_id)
                .await?
                .ok_or(WebPlaybackSessionError::NotFound)?;
            if !playback_policy_allows_hls_tier(policy, session.tier) {
                if let Err(error) = self.stop(session_id, &session.user_id).await {
                    tracing::warn!(
                        session_id = %session.id,
                        %error,
                        "failed to stop HLS session after playback policy was revoked"
                    );
                }
                return Err(WebPlaybackSessionError::PlaybackPolicyDenied);
            }
        }
        Ok(session)
    }

    async fn enforce_playback_access_rules(
        &self,
        session: &StoredWebPlaybackSession,
        client_ip: Option<IpAddr>,
    ) -> Result<(), WebPlaybackSessionError> {
        let context = self
            .database
            .find_web_playback_access_context(&session.id)
            .await?
            .ok_or(WebPlaybackSessionError::NotActive)?;
        if context.user_id != session.user_id {
            return Err(WebPlaybackSessionError::AccessRulesUnavailable);
        }

        let device_id = context.device_id.as_deref().map(|value| {
            normalize_user_access_blacklist_value(UserAccessBlacklistRuleType::Device, value)
                .map_err(|_| WebPlaybackSessionError::AccessRulesUnavailable)
        });
        let device_id = device_id.transpose()?;
        let client_name = context.client_name.as_deref().map(|value| {
            normalize_user_access_blacklist_value(UserAccessBlacklistRuleType::Client, value)
                .map_err(|_| WebPlaybackSessionError::AccessRulesUnavailable)
        });
        let client_name = client_name.transpose()?;

        if device_id.is_none() || client_name.is_none() {
            let (device_rules_active, client_rules_active) = self
                .database
                .active_user_access_device_client_blacklist_rules(&session.user_id)
                .await?;
            if (device_id.is_none() && device_rules_active)
                || (client_name.is_none() && client_rules_active)
            {
                self.stop_after_access_rule_rejection(session).await;
                return Err(WebPlaybackSessionError::NotActive);
            }
        }

        let device_id = device_id.as_deref();
        let client_name = client_name.as_deref();
        if self
            .database
            .user_access_blacklist_matches(UserAccessBlacklistRuleMatchQuery {
                user_id: &session.user_id,
                device_id,
                client_name,
            })
            .await?
        {
            self.stop_after_access_rule_rejection(session).await;
            return Err(WebPlaybackSessionError::NotActive);
        }

        let ip_rules = self
            .database
            .list_user_access_ip_blacklist_rule_values(UserAccessBlacklistIpRuleQuery {
                user_id: &session.user_id,
                limit: MAX_ACTIVE_IP_BLACKLIST_RULES_PER_REQUEST as i64 + 1,
            })
            .await
            .map_err(|_| WebPlaybackSessionError::AccessRulesUnavailable)?;
        if ip_rules.is_empty() {
            return Ok(());
        }
        let Some(client_ip) = client_ip else {
            self.stop_after_access_rule_rejection(session).await;
            return Err(WebPlaybackSessionError::AccessRulesUnavailable);
        };
        let matched = match ip_blacklist_rules_match_address(client_ip, &ip_rules) {
            Ok(matched) => matched,
            Err(
                IpBlacklistEvaluationError::InvalidStoredRule
                | IpBlacklistEvaluationError::TooManyRules,
            ) => {
                self.stop_after_access_rule_rejection(session).await;
                return Err(WebPlaybackSessionError::AccessRulesUnavailable);
            }
        };
        if matched {
            self.stop_after_access_rule_rejection(session).await;
            return Err(WebPlaybackSessionError::NotActive);
        }
        Ok(())
    }

    async fn stop_after_access_rule_rejection(&self, session: &StoredWebPlaybackSession) {
        if let Err(error) = self.stop(&session.id, &session.user_id).await {
            tracing::warn!(
                session_id = %session.id,
                %error,
                "failed to stop playback session after access rule rejection"
            );
        }
    }

    #[cfg(test)]
    pub(crate) async fn start_hls(
        &self,
        session_id: &str,
        tier: ServerTier,
        input: &std::path::Path,
        video_bitrate: Option<i64>,
        start_time_ticks: Option<i64>,
    ) -> Result<(), WebPlaybackSessionError> {
        self.start_hls_with_audio(
            session_id,
            tier,
            input,
            video_bitrate,
            start_time_ticks,
            None,
        )
        .await
    }

    pub(crate) async fn start_hls_with_audio(
        &self,
        session_id: &str,
        tier: ServerTier,
        input: &std::path::Path,
        video_bitrate: Option<i64>,
        start_time_ticks: Option<i64>,
        audio_stream_index: Option<i64>,
    ) -> Result<(), WebPlaybackSessionError> {
        self.start_hls_with_runtime(
            session_id,
            tier,
            input,
            HlsWebStartOptions {
                video_bitrate,
                start_time_ticks,
                runtime_ticks: None,
                audio_stream_index,
            },
        )
        .await
    }

    /// Current administrator transcoding policy; unreadable rows fall back to defaults.
    pub(crate) async fn transcoding_settings(
        &self,
    ) -> crate::application::transcoding::TranscodingSettings {
        crate::application::transcoding::read_transcoding_settings(&self.database).await
    }

    /// Applies a policy change immediately so capability reporting and the next session agree.
    pub(crate) fn apply_transcoding_settings(
        &self,
        settings: &crate::application::transcoding::TranscodingSettings,
    ) {
        self.hls.apply_settings(settings);
    }

    /// Re-applies the stored policy and runs the hardware encoder self-test.
    pub(crate) async fn refresh_hardware_status(
        &self,
    ) -> crate::application::playback::hls::HardwareEncoderStatus {
        let settings = self.transcoding_settings().await;
        self.hls.apply_settings(&settings);
        self.hls.probe_hardware_encoder().await
    }

    pub(crate) fn hardware_status(
        &self,
    ) -> crate::application::playback::hls::HardwareEncoderStatus {
        self.hls.hardware_status()
    }

    async fn mark_session_tier(
        &self,
        session_id: &str,
        tier: ServerTier,
    ) -> Result<(), WebPlaybackSessionError> {
        let Some(session) = self.database.find_web_playback_session(session_id).await? else {
            return Err(WebPlaybackSessionError::NotFound);
        };
        if self
            .database
            .update_web_playback_session_tier(
                session_id,
                &session.user_id,
                i64::from(tier.number()),
                &session.plan,
                unix_timestamp(),
            )
            .await?
        {
            Ok(())
        } else {
            Err(WebPlaybackSessionError::NotFound)
        }
    }

    /// Cheap liveness check right after a hardware start: a dead child means the GPU path failed.
    pub(crate) async fn hls_start_healthy(&self, session_id: &str) -> bool {
        if self.hls.process_alive(session_id).await.unwrap_or(false) {
            return true;
        }
        self.hls.manifest_ready(session_id).await
    }

    /// Restarts a failed Web HLS start as software transcoding, when the policy allows it.
    pub(crate) async fn restart_with_software(
        &self,
        session_id: &str,
        input: &std::path::Path,
        video_bitrate: Option<i64>,
        start_time_ticks: Option<i64>,
        audio_stream_index: Option<i64>,
    ) -> Result<ServerTier, WebPlaybackSessionError> {
        let settings = self.transcoding_settings().await;
        if settings.software_sessions <= 0 {
            return Err(WebPlaybackSessionError::Hls(HlsError::Limit));
        }
        let _ = self.hls.stop(session_id).await;
        self.start_hls_with_audio(
            session_id,
            ServerTier::SoftwareTranscode,
            input,
            video_bitrate,
            start_time_ticks,
            audio_stream_index,
        )
        .await?;
        self.mark_session_tier(session_id, ServerTier::SoftwareTranscode)
            .await?;
        Ok(ServerTier::SoftwareTranscode)
    }

    async fn start_hls_with_runtime(
        &self,
        session_id: &str,
        tier: ServerTier,
        input: &std::path::Path,
        options: HlsWebStartOptions,
    ) -> Result<(), WebPlaybackSessionError> {
        // Every HLS start re-reads the policy so a saved setting applies to the next session
        // without a restart, while running sessions keep their captured parameters.
        self.hls.apply_settings(&self.transcoding_settings().await);
        self.hls
            .start_with_audio(session_id, tier, input, options)
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
        options: HlsStartOptions,
    ) -> Result<(), WebPlaybackSessionError> {
        self.hls.apply_settings(&self.transcoding_settings().await);
        self.hls
            .start_emby_vod(session_id, tier, input, video_bitrate, options)
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
                // A broken hardware encoder must not take playback down: restart the same
                // session on the software tier and only then report the failure.
                if error.is_terminal_process_failure()
                    && let Ok(Some(tier)) = self.hls.downgrade_to_software(session_id).await
                {
                    let _ = self.mark_session_tier(session_id, tier).await;
                    match self.hls.wait_for_manifest(session_id).await {
                        Ok(path) => return Ok(path),
                        Err(retry) => {
                            if retry.is_terminal_process_failure() {
                                self.stop_failed_hls_session(session_id).await;
                            }
                            return Err(retry.into());
                        }
                    }
                }
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
        let session = self
            .database
            .find_web_playback_session(session_id)
            .await?
            .ok_or(WebPlaybackSessionError::NotFound)?;
        let source_id = session
            .media_source_id
            .clone()
            .ok_or(WebPlaybackSessionError::NotFound)?;
        let _start_guard = self
            .start_locks
            .lock(&emby_hls_start_key(
                &session.user_id,
                &session.item_id,
                &source_id,
            ))
            .await;
        if self.hls.has_started(session_id).await? {
            return Ok(());
        }
        let active_sessions = self
            .database
            .find_active_web_playback_sessions_for_source(
                &session.user_id,
                &session.item_id,
                &source_id,
                "lux-emby",
            )
            .await?;
        for active_session in active_sessions {
            if active_session.id != session.id {
                self.stop(&active_session.id, &session.user_id).await?;
            }
        }
        if let Err(error) = self.hls.start_for_initial_asset(session_id, asset).await {
            if error.is_terminal_process_failure()
                && let Ok(Some(tier)) = self.hls.downgrade_to_software(session_id).await
            {
                self.mark_session_tier(&session.id, tier).await?;
                self.hls.start_for_initial_asset(session_id, asset).await?;
                return Ok(());
            }
            return Err(error.into());
        }
        Ok(())
    }

    pub(crate) async fn hls_asset_path(
        &self,
        session_id: &str,
        asset: &str,
    ) -> Result<std::path::PathBuf, WebPlaybackSessionError> {
        Ok(self.hls.asset_path(session_id, asset).await?)
    }

    pub(crate) async fn hls_segment_container(
        &self,
        session_id: &str,
    ) -> Result<HlsSegmentContainer, WebPlaybackSessionError> {
        Ok(self.hls.segment_container(session_id).await?)
    }

    pub(crate) async fn hls_within_quota(
        &self,
        session_id: &str,
    ) -> Result<bool, WebPlaybackSessionError> {
        Ok(self.hls.within_quota(session_id).await?)
    }

    /// Read-only statistics for one live Web playback session.
    ///
    /// Only the owning user may read them; expired, stopped and unknown sessions are reported as
    /// missing so the endpoint cannot be used to probe another account's session identifiers.
    pub(crate) async fn stats(
        &self,
        session_id: &str,
        user_id: &str,
    ) -> Result<WebPlaybackStats, WebPlaybackSessionError> {
        let now = unix_timestamp();
        let session = self
            .database
            .find_web_playback_session(session_id)
            .await?
            .filter(|session| {
                session.user_id == user_id && session.state == "ACTIVE" && session.expires_at > now
            })
            .ok_or(WebPlaybackSessionError::NotFound)?;
        let (progress, latest_segment_bitrate_kbps, directory_bytes) =
            if session.plan == "SERVER_HLS" {
                (
                    self.hls.progress(session_id).await,
                    self.hls.latest_segment_bitrate_kbps(session_id).await,
                    self.hls.session_bytes(session_id).await.ok().flatten(),
                )
            } else {
                (None, None, None)
            };
        Ok(WebPlaybackStats {
            session,
            progress,
            latest_segment_bitrate_kbps,
            directory_bytes,
        })
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
        if let Some(session) = self.database.find_web_playback_session(session_id).await? {
            let _ = self
                .database
                .renew_existing_playback_admission(user_id, &session.play_session_id, now)
                .await?;
        }
        Ok(expires_at)
    }

    pub(crate) async fn stop_server_hls_sessions(&self, session_ids: &[String]) {
        for session_id in session_ids {
            if let Err(error) = self.hls.stop(session_id).await {
                tracing::warn!(
                    session_id = %session_id,
                    %error,
                    "failed to stop server HLS session after user access was revoked"
                );
            }
        }
    }

    pub(crate) async fn stop(
        &self,
        session_id: &str,
        user_id: &str,
    ) -> Result<(), WebPlaybackSessionError> {
        let session = self.database.find_web_playback_session(session_id).await?;
        // Stopping also removes the HLS working directory, so the session identifier must be
        // resolved to a stored session owned by the caller before it is used.
        let Some(session) = session else {
            return Ok(());
        };
        if session.user_id != user_id {
            return Err(WebPlaybackSessionError::NotFound);
        }
        let _ = self.hls.stop(session_id).await;
        let now = unix_timestamp();
        self.database
            .stop_web_playback_session(session_id, user_id, "STOPPED", now)
            .await?;
        if let Some(playback) = self
            .database
            .find_playback_session(user_id, &session.play_session_id)
            .await?
        {
            self.database
                .stop_playback_session(&playback.id, now)
                .await?;
        }
        self.database
            .release_playback_admission(user_id, &session.play_session_id, now)
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
            return;
        }
        if let Some(playback) = match self
            .database
            .find_playback_session(&session.user_id, &session.play_session_id)
            .await
        {
            Ok(playback) => playback,
            Err(error) => {
                tracing::warn!(
                    event = "web_hls_failed_session_cleanup",
                    session_id = %session_id,
                    %error,
                    "failed to load failed HLS playback record"
                );
                return;
            }
        } {
            if let Err(error) = self.database.stop_playback_session(&playback.id, now).await {
                tracing::warn!(
                    event = "web_hls_failed_session_cleanup",
                    session_id = %session_id,
                    %error,
                    "failed to stop failed HLS playback record"
                );
                return;
            }
        }
        if let Err(error) = self
            .database
            .release_playback_admission(&session.user_id, &session.play_session_id, now)
            .await
        {
            tracing::warn!(
                event = "web_hls_failed_session_cleanup",
                session_id = %session_id,
                %error,
                "failed to release failed HLS playback admission"
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

fn choose_plan_for_policy(
    input: PlaybackDecisionInput,
    policy: UserPlaybackPolicy,
) -> Result<PlaybackPlan, WebPlaybackSessionError> {
    let unrestricted_plan = choose_plan(input);
    if input.source_kind == PlaybackSourceKind::Strm
        || input.capabilities.direct_play
        || !matches!(unrestricted_plan, PlaybackPlan::ServerHls { .. })
    {
        return Ok(unrestricted_plan);
    }

    let capabilities = input.capabilities;
    let candidates = [
        (
            capabilities.video_copy_to_fmp4 && capabilities.audio_copy_to_fmp4,
            policy.allow_remux,
            ServerTier::Remux,
        ),
        (
            capabilities.video_copy_to_fmp4
                && (!capabilities.audio_copy_to_fmp4 || !policy.allow_remux),
            policy.allow_audio_transcode,
            ServerTier::AudioTranscode,
        ),
        (
            capabilities.hardware_transcode,
            policy.allow_video_transcode && policy.allow_audio_transcode,
            ServerTier::HardwareTranscode,
        ),
        (
            capabilities.software_transcode,
            policy.allow_video_transcode && policy.allow_audio_transcode,
            ServerTier::SoftwareTranscode,
        ),
    ];
    if let Some((_, _, tier)) = candidates
        .into_iter()
        .find(|(available, allowed, _)| *available && *allowed)
    {
        return Ok(PlaybackPlan::ServerHls { tier });
    }

    Err(WebPlaybackSessionError::PlaybackPolicyDenied)
}

fn playback_policy_allows_hls_tier(policy: UserPlaybackPolicy, tier: i64) -> bool {
    match tier {
        1 => policy.allow_remux,
        2 => policy.allow_audio_transcode,
        3 | 4 => policy.allow_audio_transcode && policy.allow_video_transcode,
        _ => false,
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
        if bool::from(expected.as_bytes().ct_eq(signature.as_bytes())) {
            return true;
        }
        // URLs signed before the length-prefixed format still work until they expire.
        let Some(legacy) = self.sign_legacy(session_id, resource, expires_at) else {
            return false;
        };
        legacy.as_bytes().ct_eq(signature.as_bytes()).into()
    }

    /// Legacy (unprefixed) format, kept only to let in-flight URLs finish.
    fn sign_legacy(&self, session_id: &str, resource: &str, expires_at: i64) -> Option<String> {
        let mut mac = HmacSha256::new_from_slice(&self.key).ok()?;
        mac.update(format!("lux-web-playback\n{session_id}\n{resource}\n{expires_at}").as_bytes());
        Some(URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes()))
    }
}

pub fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default()
}

/// Length-prefixes every field so a resource name containing a newline cannot shift the
/// boundaries between the signed components.
fn signed_message(session_id: &str, resource: &str, expires_at: i64) -> String {
    let mut message = String::with_capacity(session_id.len() + resource.len() + 64);
    message.push_str("lux-web-playback\n");
    for field in [session_id, resource] {
        message.push_str(&field.len().to_string());
        message.push(':');
        message.push_str(field);
        message.push('\n');
    }
    message.push_str(&expires_at.to_string());
    message
}

fn emby_direct_stream_resource(
    user_id: &str,
    is_admin: bool,
    item_id: &str,
    media_source_id: &str,
    play_session_id: &str,
) -> String {
    format!(
        "emby-direct-stream\n{user_id}\n{is_admin}\n{item_id}\n{media_source_id}\n{play_session_id}"
    )
}

fn emby_direct_stream_resource_v2(
    user_id: &str,
    is_admin: bool,
    item_id: &str,
    media_source_id: &str,
    play_session_id: &str,
) -> String {
    format!(
        "emby-direct-stream-v2\n{}\n{user_id}\n{is_admin}\n{item_id}\n{media_source_id}\n{play_session_id}",
        EMBY_DIRECT_STREAM_TICKET_VERSION
    )
}

#[cfg(test)]
mod tests {
    use std::{os::unix::fs::PermissionsExt, path::Path, sync::Arc};

    use super::{
        EMBY_DIRECT_STREAM_TTL_SECONDS, ResourceSigner, SessionStartLocks,
        apply_server_capabilities, choose_plan_for_policy, emby_hls_start_key, unix_timestamp,
    };
    use crate::{
        application::{
            libraries::LibraryService,
            playback::{
                decision::{
                    PlaybackCapabilities, PlaybackDecisionInput, PlaybackPlan, PlaybackSourceKind,
                    ServerTier, UnsupportedReason,
                },
                hls::{HlsManager, HlsSegmentContainer, HlsStartOptions},
                session::{
                    CreateWebPlaybackSession, WebPlaybackEvent, WebPlaybackPlan,
                    WebPlaybackSessionService,
                },
            },
            setup::SetupService,
        },
        config::Config,
        library::LibraryKind,
        storage::{NewWebPlaybackSession, UserPlaybackPolicy, WebPlaybackEventClaim},
    };
    use uuid::Uuid;

    #[tokio::test]
    async fn session_start_locks_serialize_one_source_without_blocking_others() {
        let locks = SessionStartLocks::default();
        let first = locks
            .lock(&emby_hls_start_key("user", "item", "source-a"))
            .await;
        let other = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            locks.lock(&emby_hls_start_key("user", "item", "source-b")),
        )
        .await
        .expect("an unrelated source must not wait for another start");
        drop(other);
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(50),
                locks.lock(&emby_hls_start_key("user", "item", "source-a")),
            )
            .await
            .is_err(),
            "the same source must serialize its starts"
        );
        drop(first);
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            locks.lock(&emby_hls_start_key("user", "item", "source-a")),
        )
        .await
        .expect("a released source start lock is available again");
    }

    #[test]
    fn session_start_keys_keep_their_fields_separate() {
        assert_ne!(
            emby_hls_start_key("user", "item", "source"),
            emby_hls_start_key("user", "item-s", "ource")
        );
        assert_ne!(
            emby_hls_start_key("user", "item", "source"),
            emby_hls_start_key("user-item", "", "source")
        );
    }

    #[test]
    fn legacy_signatures_still_verify_until_the_format_is_retired() {
        let signer = ResourceSigner::random();
        let expires_at = unix_timestamp() + 60;
        let legacy = signer
            .sign_legacy("session-1", "direct", expires_at)
            .expect("sign");
        assert!(signer.verify("session-1", "direct", expires_at, &legacy, unix_timestamp()));
        assert!(!signer.verify("session-2", "direct", expires_at, &legacy, unix_timestamp()));
    }

    #[test]
    fn signature_fields_cannot_be_shifted_by_embedded_newlines() {
        let signer = ResourceSigner::random();
        let expires_at = unix_timestamp() + 60;
        let signature = signer
            .sign("session-1", "direct\n1:x", expires_at)
            .expect("sign");
        assert!(!signer.verify(
            "session-1",
            "direct",
            expires_at,
            &signature,
            unix_timestamp()
        ));
        assert!(signer.verify(
            "session-1",
            "direct\n1:x",
            expires_at,
            &signature,
            unix_timestamp()
        ));
    }

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
    fn emby_direct_stream_v2_signature_is_separate_from_legacy_domain() {
        let signer = ResourceSigner::random();
        let expires_at = unix_timestamp() + 60;
        let legacy_resource =
            super::emby_direct_stream_resource("user-1", false, "item-1", "source-1", "session-1");
        let v2_resource = super::emby_direct_stream_resource_v2(
            "user-1",
            false,
            "item-1",
            "source-1",
            "session-1",
        );
        let legacy_signature = signer
            .sign("emby-direct-stream", &legacy_resource, expires_at)
            .unwrap();
        let v2_signature = signer
            .sign("emby-direct-stream", &v2_resource, expires_at)
            .unwrap();

        assert!(signer.verify(
            "emby-direct-stream",
            &legacy_resource,
            expires_at,
            &legacy_signature,
            unix_timestamp()
        ));
        assert!(!signer.verify(
            "emby-direct-stream",
            &v2_resource,
            expires_at,
            &legacy_signature,
            unix_timestamp()
        ));
        assert!(!signer.verify(
            "emby-direct-stream",
            &legacy_resource,
            expires_at,
            &v2_signature,
            unix_timestamp()
        ));
        assert!(v2_resource.starts_with("emby-direct-stream-v2\n2\n"));
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

    #[test]
    fn user_policy_skips_remux_and_selects_an_allowed_audio_transcode_tier() {
        let plan = choose_plan_for_policy(
            PlaybackDecisionInput {
                source_kind: PlaybackSourceKind::LocalFile,
                capabilities: PlaybackCapabilities {
                    direct_play: false,
                    hls: true,
                    video_copy_to_fmp4: true,
                    audio_copy_to_fmp4: true,
                    hardware_transcode: true,
                    software_transcode: true,
                },
            },
            UserPlaybackPolicy {
                can_play: true,
                max_simultaneous_playbacks: 2,
                allow_remux: false,
                allow_audio_transcode: true,
                allow_video_transcode: false,
            },
        );

        assert!(matches!(
            plan,
            Ok(PlaybackPlan::ServerHls {
                tier: ServerTier::AudioTranscode
            })
        ));
    }

    #[test]
    fn user_policy_selects_video_transcode_when_lower_tiers_are_forbidden() {
        let plan = choose_plan_for_policy(
            PlaybackDecisionInput {
                source_kind: PlaybackSourceKind::LocalFile,
                capabilities: PlaybackCapabilities {
                    direct_play: false,
                    hls: true,
                    video_copy_to_fmp4: false,
                    audio_copy_to_fmp4: true,
                    hardware_transcode: true,
                    software_transcode: true,
                },
            },
            UserPlaybackPolicy {
                can_play: true,
                max_simultaneous_playbacks: 2,
                allow_remux: false,
                allow_audio_transcode: true,
                allow_video_transcode: true,
            },
        );

        assert!(matches!(
            plan,
            Ok(PlaybackPlan::ServerHls {
                tier: ServerTier::HardwareTranscode
            })
        ));
    }

    #[test]
    fn user_policy_rejects_all_transcoding_without_affecting_direct_or_strm_rules() {
        let policy = UserPlaybackPolicy {
            can_play: true,
            max_simultaneous_playbacks: 2,
            allow_remux: false,
            allow_audio_transcode: false,
            allow_video_transcode: false,
        };
        let capabilities = PlaybackCapabilities {
            direct_play: false,
            hls: true,
            video_copy_to_fmp4: true,
            audio_copy_to_fmp4: true,
            hardware_transcode: true,
            software_transcode: true,
        };

        assert!(matches!(
            choose_plan_for_policy(
                PlaybackDecisionInput {
                    source_kind: PlaybackSourceKind::LocalFile,
                    capabilities,
                },
                policy,
            ),
            Err(super::WebPlaybackSessionError::PlaybackPolicyDenied)
        ));
        assert!(matches!(
            choose_plan_for_policy(
                PlaybackDecisionInput {
                    source_kind: PlaybackSourceKind::LocalFile,
                    capabilities: PlaybackCapabilities {
                        direct_play: true,
                        ..capabilities
                    },
                },
                policy,
            ),
            Ok(PlaybackPlan::Direct)
        ));
        assert!(matches!(
            choose_plan_for_policy(
                PlaybackDecisionInput {
                    source_kind: PlaybackSourceKind::Strm,
                    capabilities,
                },
                policy,
            ),
            Ok(PlaybackPlan::Unsupported {
                reason: UnsupportedReason::StrmRequiresDirectPlay,
            })
        ));
        assert!(matches!(
            choose_plan_for_policy(
                PlaybackDecisionInput {
                    source_kind: PlaybackSourceKind::LocalFile,
                    capabilities: PlaybackCapabilities {
                        video_copy_to_fmp4: false,
                        audio_copy_to_fmp4: false,
                        ..capabilities
                    },
                },
                UserPlaybackPolicy {
                    allow_remux: false,
                    allow_audio_transcode: false,
                    allow_video_transcode: true,
                    ..policy
                },
            ),
            Err(super::WebPlaybackSessionError::PlaybackPolicyDenied)
        ));
        assert!(
            matches!(
                choose_plan_for_policy(
                    PlaybackDecisionInput {
                        source_kind: PlaybackSourceKind::LocalFile,
                        capabilities,
                    },
                    UserPlaybackPolicy {
                        allow_remux: false,
                        allow_audio_transcode: false,
                        allow_video_transcode: true,
                        ..policy
                    },
                ),
                Err(super::WebPlaybackSessionError::PlaybackPolicyDenied)
            ),
            "video transcoding cannot be allowed when the current HLS output also transcodes audio"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stop_refuses_sessions_owned_by_another_user() -> Result<(), Box<dyn std::error::Error>>
    {
        let temp_dir = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        };
        let database = crate::storage::Database::connect(&config).await?;
        let setup = SetupService::new(database.clone())?;
        let owner = setup.complete("Owner", "Owner", "correct password").await?;
        let other = crate::auth::users::UserStore::new(database.clone())?
            .create_user("other", "Other", "correct password", false)
            .await?;
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
            start_locks: SessionStartLocks::default(),
        };
        let owner_id = owner.id.to_string();
        service
            .database
            .insert_web_playback_session(NewWebPlaybackSession {
                id: "owned-session",
                user_id: &owner_id,
                item_id: &item_id,
                media_source_id: None,
                play_session_id: "lux-web:owned-session",
                tier: i64::from(ServerTier::Remux.number()),
                plan: "SERVER_HLS",
                temp_dir: None,
                is_admin: false,
                expires_at: unix_timestamp() + 900,
                now: unix_timestamp(),
            })
            .await?;

        let error = service
            .stop("owned-session", &other.id.to_string())
            .await
            .expect_err("another user must not stop the session");
        assert!(matches!(error, super::WebPlaybackSessionError::NotFound));
        let stored = service
            .database
            .find_web_playback_session("owned-session")
            .await?
            .expect("session is still stored");
        assert_eq!(stored.state, "ACTIVE");
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stats_are_scoped_to_the_owning_user_and_live_sessions()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        };
        let database = crate::storage::Database::connect(&config).await?;
        let setup = SetupService::new(database.clone())?;
        let owner = setup.complete("Owner", "Owner", "correct password").await?;
        let other = crate::auth::users::UserStore::new(database.clone())?
            .create_user("other", "Other", "correct password", false)
            .await?;
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
            start_locks: SessionStartLocks::default(),
        };
        let owner_id = owner.id.to_string();
        let now = unix_timestamp();
        service
            .database
            .insert_web_playback_session(NewWebPlaybackSession {
                id: "stats-session",
                user_id: &owner_id,
                item_id: &item_id,
                media_source_id: None,
                play_session_id: "lux-web:stats-session",
                tier: i64::from(ServerTier::Remux.number()),
                plan: "SERVER_HLS",
                temp_dir: None,
                is_admin: false,
                expires_at: now + 900,
                now,
            })
            .await?;
        service
            .database
            .insert_web_playback_session(NewWebPlaybackSession {
                id: "expired-session",
                user_id: &owner_id,
                item_id: &item_id,
                media_source_id: None,
                play_session_id: "lux-web:expired-session",
                tier: i64::from(ServerTier::Remux.number()),
                plan: "SERVER_HLS",
                temp_dir: None,
                is_admin: false,
                expires_at: now - 1,
                now: now - 900,
            })
            .await?;

        let stats = service.stats("stats-session", &owner_id).await?;
        assert_eq!(stats.session.plan, "SERVER_HLS");
        assert!(
            stats.progress.is_none(),
            "a session without a running FFmpeg has no live progress"
        );

        for (session_id, user_id) in [
            ("stats-session", other.id.to_string()),
            ("expired-session", owner_id.clone()),
            ("unknown-session", owner_id.clone()),
        ] {
            let error = service
                .stats(session_id, &user_id)
                .await
                .expect_err("only the owner's live session exposes statistics");
            assert!(matches!(error, super::WebPlaybackSessionError::NotFound));
        }
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn disabling_server_transcoding_degrades_plans_to_unsupported()
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
        let source_id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO media_sources (id, item_id, source_kind, container, is_default)
             VALUES (?, ?, 'LOCAL_FILE', 'mkv', 1)",
        )
        .bind(&source_id)
        .bind(&item_id)
        .execute(database.pool())
        .await?;
        let service = WebPlaybackSessionService::new(database.clone(), config.config_dir.clone());
        crate::application::transcoding::write_transcoding_settings(
            &database,
            &crate::application::transcoding::TranscodingSettings {
                enabled: false,
                ..crate::application::transcoding::TranscodingSettings::default()
            },
        )
        .await?;
        let user_id = user.id.to_string();
        let transcode_capabilities = PlaybackCapabilities {
            direct_play: false,
            hls: true,
            video_copy_to_fmp4: true,
            audio_copy_to_fmp4: true,
            hardware_transcode: false,
            software_transcode: true,
        };
        let created = service
            .create(CreateWebPlaybackSession {
                user_id: &user_id,
                is_admin: true,
                item_id: &item_id,
                media_source_id: &source_id,
                play_session_prefix: "lux-web",
                source_kind: PlaybackSourceKind::LocalFile,
                capabilities: transcode_capabilities,
            })
            .await?;
        assert_eq!(
            created.plan,
            WebPlaybackPlan::Unsupported {
                reason: UnsupportedReason::ServerTranscodingDisabled
            }
        );
        assert!(created.id.is_empty(), "no session row is created");
        let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM web_playback_sessions")
            .fetch_one(database.pool())
            .await?;
        assert_eq!(stored, 0);

        let direct = service
            .create(CreateWebPlaybackSession {
                user_id: &user_id,
                is_admin: true,
                item_id: &item_id,
                media_source_id: &source_id,
                play_session_prefix: "lux-web",
                source_kind: PlaybackSourceKind::LocalFile,
                capabilities: PlaybackCapabilities {
                    direct_play: true,
                    ..transcode_capabilities
                },
            })
            .await?;
        assert_eq!(direct.plan, WebPlaybackPlan::Direct);
        Ok(())
    }

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
            start_locks: SessionStartLocks::default(),
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
            start_locks: SessionStartLocks::default(),
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
            start_locks: SessionStartLocks::default(),
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
                HlsStartOptions {
                    segment_container: HlsSegmentContainer::FragmentedMp4,
                    start_time_ticks: None,
                    runtime_ticks: Some(600 * 10_000_000),
                },
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
                HlsStartOptions {
                    segment_container: HlsSegmentContainer::FragmentedMp4,
                    start_time_ticks: None,
                    runtime_ticks: Some(600 * 10_000_000),
                },
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
            start_locks: SessionStartLocks::default(),
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
                HlsStartOptions {
                    segment_container: HlsSegmentContainer::FragmentedMp4,
                    start_time_ticks: None,
                    runtime_ticks: None,
                },
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
                HlsStartOptions {
                    segment_container: HlsSegmentContainer::FragmentedMp4,
                    start_time_ticks: None,
                    runtime_ticks: None,
                },
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
