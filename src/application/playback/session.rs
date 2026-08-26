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
        let capabilities =
            apply_server_capabilities(input.capabilities, self.hls.hardware_transcode_available());
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
        if claim == WebPlaybackEventClaim::Accepted
            && event.state == "STOPPED"
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

#[cfg(test)]
mod tests {
    use std::{os::unix::fs::PermissionsExt, path::Path, sync::Arc};

    use super::{ResourceSigner, apply_server_capabilities, unix_timestamp};
    use crate::{
        application::{
            libraries::LibraryService,
            playback::{
                decision::{PlaybackCapabilities, ServerTier},
                hls::HlsManager,
                session::{WebPlaybackEvent, WebPlaybackSessionService},
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
            .start_hls("session-1", ServerTier::Remux, Path::new("input.mkv"))
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
}
