use super::*;
use crate::storage::{
    PlaybackAdmissionDecision, PlaybackAdmissionDenialReason, PlaybackAdmissionResult,
};

/// Upper bound for the session listing; live sessions on one server stay far below it.
const MAX_LISTED_PLAYBACK_SESSIONS: i64 = 500;

/// A single item source should never accumulate more active playback sessions than this.
const MAX_SESSIONS_PER_SOURCE: i64 = 32;

/// Row of the per-user playback policy read by
/// [`Database::reserve_playback_admission`]; `can_play` stays the raw `0`/`1` integer so the
/// conversion to [`bool`] happens in one place.
#[derive(Debug, sqlx::FromRow)]
struct PlaybackPolicyRow {
    can_play: i64,
    max_simultaneous_playbacks: i64,
}

/// Row of the active-playback aggregation read by
/// [`Database::reserve_playback_admission`]: `session_is_active` is `1` when the requested
/// session already holds an admission.
#[derive(Debug, sqlx::FromRow)]
struct PlaybackAdmissionCountsRow {
    active_count: i64,
    session_is_active: i64,
}

/// Row of the `RETURNING` clause of [`Database::record_playback_event`]'s user-item-state upsert.
#[derive(Debug, sqlx::FromRow)]
struct PlaySessionPointerRow {
    last_play_session_id: String,
    /// `is_played` as stored before this event flips it, so the flip is reported exactly once.
    is_played: i64,
}

/// Row of the container-wide played aggregation read by
/// [`Database::sync_played_container_states`].
#[derive(Debug, sqlx::FromRow)]
struct PlayedParentStateRow {
    parent_id: String,
    is_played: i64,
}

/// Row of [`Database::find_web_playback_access_context`].
#[derive(Debug, sqlx::FromRow)]
struct WebPlaybackAccessContextRow {
    user_id: String,
    authorization_device_id: Option<String>,
    authorization_client_name: Option<String>,
}

impl From<WebPlaybackAccessContextRow> for StoredWebPlaybackAccessContext {
    fn from(row: WebPlaybackAccessContextRow) -> Self {
        Self {
            user_id: row.user_id,
            device_id: row.authorization_device_id,
            client_name: row.authorization_client_name,
        }
    }
}

/// Row of [`Database::find_emby_direct_playback_ticket`].
#[derive(Debug, sqlx::FromRow)]
struct EmbyDirectPlaybackTicketRow {
    user_id: String,
    play_session_id: String,
    authorization_device_id: Option<String>,
    authorization_client_name: Option<String>,
    expires_at: i64,
    revoked_at: Option<i64>,
    created_at: i64,
}

impl From<EmbyDirectPlaybackTicketRow> for StoredEmbyDirectPlaybackTicket {
    fn from(row: EmbyDirectPlaybackTicketRow) -> Self {
        Self {
            user_id: row.user_id,
            play_session_id: row.play_session_id,
            authorization_device_id: row.authorization_device_id,
            authorization_client_name: row.authorization_client_name,
            expires_at: row.expires_at,
            revoked_at: row.revoked_at,
            created_at: row.created_at,
        }
    }
}

/// Row of [`Database::transcoding_history_page`]; the two name columns come from `LEFT JOIN`s and
/// stay optional.
#[derive(Debug, sqlx::FromRow)]
struct TranscodingHistoryRow {
    id: String,
    session_id: String,
    user_id: String,
    user_name: Option<String>,
    item_id: String,
    item_title: Option<String>,
    item_type: Option<String>,
    media_source_id: Option<String>,
    tier: i64,
    outcome: String,
    started_at: i64,
    finished_at: i64,
    wall_seconds: i64,
    encoded_seconds: Option<i64>,
    average_speed: Option<f64>,
    last_fps: Option<f64>,
    output_bytes: Option<i64>,
    failure_reason: Option<String>,
}

impl From<TranscodingHistoryRow> for StoredTranscodingHistory {
    fn from(row: TranscodingHistoryRow) -> Self {
        Self {
            id: row.id,
            session_id: row.session_id,
            user_id: row.user_id,
            user_name: row.user_name,
            item_id: row.item_id,
            item_title: row.item_title,
            item_type: row.item_type,
            media_source_id: row.media_source_id,
            tier: row.tier,
            outcome: row.outcome,
            started_at: row.started_at,
            finished_at: row.finished_at,
            wall_seconds: row.wall_seconds,
            encoded_seconds: row.encoded_seconds,
            average_speed: row.average_speed,
            last_fps: row.last_fps,
            output_bytes: row.output_bytes,
            failure_reason: row.failure_reason,
        }
    }
}

/// Row of [`Database::active_server_transcoding_sessions`]; the two name columns come from
/// `LEFT JOIN`s and stay optional.
#[derive(Debug, sqlx::FromRow)]
struct TranscodingSessionRow {
    session_id: String,
    user_id: String,
    user_name: Option<String>,
    item_id: String,
    item_title: Option<String>,
    item_type: Option<String>,
    tier: i64,
    state: String,
    expires_at: i64,
    created_at: i64,
    updated_at: i64,
    video_codec: Option<String>,
    audio_codec: Option<String>,
    audio_bitrate: Option<i64>,
    transcoding_container: Option<String>,
}

impl From<TranscodingSessionRow> for StoredTranscodingSessionRow {
    fn from(row: TranscodingSessionRow) -> Self {
        Self {
            session_id: row.session_id,
            user_id: row.user_id,
            user_name: row.user_name,
            item_id: row.item_id,
            item_title: row.item_title,
            item_type: row.item_type,
            tier: row.tier,
            state: row.state,
            expires_at: row.expires_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
            video_codec: row.video_codec,
            audio_codec: row.audio_codec,
            audio_bitrate: row.audio_bitrate,
            transcoding_container: row.transcoding_container,
        }
    }
}

impl Database {
    pub(crate) async fn list_current_user_playback_sessions(
        &self,
        user_id: &str,
        page_size: i64,
        offset: i64,
    ) -> Result<(Vec<StoredPlaybackSession>, i64), StorageError> {
        let page_size = page_size.clamp(1, 100);
        let offset = offset.max(0);
        let total_count: i64 = self
            .query_scalar(
                "SELECT COUNT(*) FROM playback_sessions
                 WHERE user_id = ?
                   AND state != 'STOPPED'
                   AND last_event_at > unixepoch() - ?",
            )
            .bind(user_id)
            .bind(PLAYBACK_SESSION_STALE_AFTER_SECONDS)
            .fetch_one(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        let sessions = self
            .query(
                "SELECT id, user_id, item_id, media_source_id, play_session_id,
                        device_id, client, device_name, client_version, device_type,
                        remote_ip, state,
                        position_ticks, duration_ticks, is_paused, started_at,
                        last_event_at
                 FROM playback_sessions
                 WHERE user_id = ?
                   AND state != 'STOPPED'
                   AND last_event_at > unixepoch() - ?
                 ORDER BY last_event_at DESC, id ASC
                 LIMIT ? OFFSET ?",
            )
            .bind(user_id)
            .bind(PLAYBACK_SESSION_STALE_AFTER_SECONDS)
            .bind(page_size)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
            .map_err(self.sqlx_err())?
            .into_iter()
            .map(stored_playback_session)
            .collect::<Result<Vec<_>, _>>()
            .map_err(self.sqlx_err())?;
        Ok((sessions, total_count))
    }

    pub(crate) async fn check_playback_admission(
        &self,
        user_id: &str,
        now: i64,
    ) -> Result<Option<PlaybackAdmissionResult>, StorageError> {
        let Some(policy) = self.get_user_playback_policy(user_id).await? else {
            return Ok(None);
        };
        let stale_before = now
            .max(0)
            .saturating_sub(PLAYBACK_SESSION_STALE_AFTER_SECONDS);
        let active_playback_count: i64 = self
            .query_scalar(
                "SELECT COUNT(*) FROM (
                     SELECT play_session_id FROM playback_admissions
                     WHERE user_id = ? AND state = 'ACTIVE' AND last_seen_at > ?
                     UNION
                     SELECT play_session_id FROM playback_sessions
                     WHERE user_id = ? AND state IN ('PLAYING', 'PAUSED') AND last_event_at > ?
                 ) active_playbacks",
            )
            .bind(user_id)
            .bind(stale_before)
            .bind(user_id)
            .bind(stale_before)
            .fetch_one(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        let decision = if !policy.can_play {
            PlaybackAdmissionDecision::Denied(PlaybackAdmissionDenialReason::PlaybackDisabled)
        } else if policy.max_simultaneous_playbacks > 0
            && active_playback_count >= policy.max_simultaneous_playbacks
        {
            PlaybackAdmissionDecision::Denied(PlaybackAdmissionDenialReason::ConcurrentLimit)
        } else {
            PlaybackAdmissionDecision::Admitted
        };
        Ok(Some(PlaybackAdmissionResult {
            decision,
            active_playback_count,
            max_simultaneous_playbacks: policy.max_simultaneous_playbacks,
        }))
    }

    pub(crate) async fn reserve_playback_admission(
        &self,
        user_id: &str,
        play_session_id: &str,
        now: i64,
    ) -> Result<Option<PlaybackAdmissionResult>, StorageError> {
        let mut transaction = self.begin_playback_admission_transaction().await?;
        let row_lock = if self.backend() == crate::config::DatabaseBackend::Postgres {
            " FOR UPDATE"
        } else {
            ""
        };
        let policy_query = format!(
            "SELECT can_play, max_simultaneous_playbacks FROM users WHERE id = ?{row_lock}"
        );
        let policy = self
            .query_as::<PlaybackPolicyRow>(sqlx::AssertSqlSafe(policy_query))
            .bind(user_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        let Some(policy) = policy else {
            transaction.commit().await.map_err(self.sqlx_err())?;
            return Ok(None);
        };
        let can_play = policy.can_play != 0;
        let limit = policy.max_simultaneous_playbacks;
        let now = now.max(0);
        let stale_before = now.saturating_sub(PLAYBACK_SESSION_STALE_AFTER_SECONDS);
        self.query(
            "UPDATE playback_admissions SET state = 'RELEASED'
             WHERE user_id = ? AND state = 'ACTIVE' AND last_seen_at <= ?",
        )
        .bind(user_id)
        .bind(stale_before)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;
        let active_sessions = self
            .query_as::<PlaybackAdmissionCountsRow>(
                "SELECT COUNT(*) AS active_count,
                        COALESCE(MAX(CASE WHEN play_session_id = ? THEN 1 ELSE 0 END), 0)
                            AS session_is_active
                 FROM (
                     SELECT play_session_id FROM playback_admissions
                     WHERE user_id = ? AND state = 'ACTIVE' AND last_seen_at > ?
                     UNION
                     SELECT play_session_id FROM playback_sessions
                     WHERE user_id = ? AND state IN ('PLAYING', 'PAUSED') AND last_event_at > ?
                 ) active_playbacks",
            )
            .bind(play_session_id)
            .bind(user_id)
            .bind(stale_before)
            .bind(user_id)
            .bind(stale_before)
            .fetch_one(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        let active_count = active_sessions.active_count;
        let session_is_active = active_sessions.session_is_active != 0;
        let decision = if !can_play {
            Some(PlaybackAdmissionDecision::Denied(
                PlaybackAdmissionDenialReason::PlaybackDisabled,
            ))
        } else if limit > 0 && active_count >= limit && !session_is_active {
            Some(PlaybackAdmissionDecision::Denied(
                PlaybackAdmissionDenialReason::ConcurrentLimit,
            ))
        } else {
            None
        };
        if let Some(decision) = decision {
            transaction.commit().await.map_err(self.sqlx_err())?;
            return Ok(Some(PlaybackAdmissionResult {
                decision,
                active_playback_count: active_count,
                max_simultaneous_playbacks: limit,
            }));
        }

        self.query(
            "INSERT INTO playback_admissions (
                 user_id, play_session_id, state, admitted_at, last_seen_at
             ) VALUES (?, ?, 'ACTIVE', ?, ?)
             ON CONFLICT(user_id, play_session_id) DO UPDATE SET
                 state = 'ACTIVE',
                 admitted_at = CASE
                     WHEN playback_admissions.state = 'ACTIVE'
                      AND playback_admissions.last_seen_at > ?
                     THEN playback_admissions.admitted_at
                     ELSE excluded.admitted_at
                 END,
                 last_seen_at = excluded.last_seen_at",
        )
        .bind(user_id)
        .bind(play_session_id)
        .bind(now)
        .bind(now)
        .bind(stale_before)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;
        let active_playback_count: i64 = self
            .query_scalar(
                "SELECT COUNT(*) FROM (
                     SELECT play_session_id FROM playback_admissions
                     WHERE user_id = ? AND state = 'ACTIVE' AND last_seen_at > ?
                     UNION
                     SELECT play_session_id FROM playback_sessions
                     WHERE user_id = ? AND state IN ('PLAYING', 'PAUSED') AND last_event_at > ?
                 ) active_playbacks",
            )
            .bind(user_id)
            .bind(stale_before)
            .bind(user_id)
            .bind(stale_before)
            .fetch_one(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        transaction.commit().await.map_err(self.sqlx_err())?;
        Ok(Some(PlaybackAdmissionResult {
            decision: PlaybackAdmissionDecision::Admitted,
            active_playback_count,
            max_simultaneous_playbacks: limit,
        }))
    }

    pub(crate) async fn renew_existing_playback_admission(
        &self,
        user_id: &str,
        play_session_id: &str,
        now: i64,
    ) -> Result<bool, StorageError> {
        let max_function = self.scalar_max_function();
        let stale_before = now
            .max(0)
            .saturating_sub(PLAYBACK_SESSION_STALE_AFTER_SECONDS);
        let query = format!(
            "UPDATE playback_admissions
             SET last_seen_at = {max_function}(last_seen_at, ?)
             WHERE user_id = ? AND play_session_id = ? AND state = 'ACTIVE'
               AND last_seen_at > ?"
        );
        self.query(sqlx::AssertSqlSafe(query))
            .bind(now.max(0))
            .bind(user_id)
            .bind(play_session_id)
            .bind(stale_before)
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected() == 1)
            .map_err(self.sqlx_err())
    }

    // Called after playback stop, failure, or expiry.
    pub(crate) async fn release_playback_admission(
        &self,
        user_id: &str,
        play_session_id: &str,
        now: i64,
    ) -> Result<bool, StorageError> {
        let max_function = self.scalar_max_function();
        let query = format!(
            "UPDATE playback_admissions
             SET state = 'RELEASED', last_seen_at = {max_function}(last_seen_at, ?)
             WHERE user_id = ? AND play_session_id = ? AND state = 'ACTIVE'"
        );
        self.query(sqlx::AssertSqlSafe(query))
            .bind(now.max(0))
            .bind(user_id)
            .bind(play_session_id)
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected() == 1)
            .map_err(self.sqlx_err())
    }

    pub(crate) async fn set_user_item_played(
        &self,
        user_id: &str,
        item_id: &str,
        played: bool,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO user_item_state (user_id, item_id, is_played, play_count, last_played_at)
             VALUES (?, ?, ?, CASE WHEN ? = 1 THEN 1 ELSE 0 END,
                     CASE WHEN ? = 1 THEN unixepoch() ELSE NULL END)
             ON CONFLICT(user_id, item_id) DO UPDATE SET
                 is_played = excluded.is_played,
                 play_count = CASE
                     WHEN excluded.is_played = 1 AND user_item_state.is_played = 0
                     THEN user_item_state.play_count + 1 ELSE user_item_state.play_count END,
                 last_played_at = CASE
                     WHEN excluded.is_played = 1 THEN unixepoch()
                     ELSE user_item_state.last_played_at END,
                 version = user_item_state.version + CASE
                     WHEN excluded.is_played != user_item_state.is_played THEN 1 ELSE 0 END",
        )
        .bind(user_id)
        .bind(item_id)
        .bind(database_flag(played))
        .bind(database_flag(played))
        .bind(database_flag(played))
        .execute(&self.pool)
        .await
        .map_err(self.sqlx_err())?;
        self.refresh_user_series_next_for_item(user_id, item_id)
            .await
    }

    pub(crate) async fn set_user_item_resume_hidden(
        &self,
        user_id: &str,
        item_id: &str,
        hidden: bool,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO user_item_state (user_id, item_id, hide_from_resume)
             VALUES (?, ?, ?)
             ON CONFLICT(user_id, item_id) DO UPDATE SET
                 hide_from_resume = excluded.hide_from_resume,
                 version = user_item_state.version + CASE
                     WHEN excluded.hide_from_resume != user_item_state.hide_from_resume THEN 1 ELSE 0 END",
        )
        .bind(user_id)
        .bind(item_id)
        .bind(database_flag(hidden))
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn set_user_item_favorite(
        &self,
        user_id: &str,
        item_id: &str,
        favorite: bool,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO user_item_state (user_id, item_id, is_favorite)
             VALUES (?, ?, ?)
             ON CONFLICT(user_id, item_id) DO UPDATE SET
                 is_favorite = excluded.is_favorite,
                 version = user_item_state.version + CASE
                     WHEN excluded.is_favorite != user_item_state.is_favorite THEN 1 ELSE 0 END",
        )
        .bind(user_id)
        .bind(item_id)
        .bind(database_flag(favorite))
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(self.sqlx_err())
    }

    /// Records one playback event and reports the state it changed.
    ///
    /// The session row, the user item state, the optional admission renewal, and the resume
    /// pointer (only when the event is a state change or a played flip) commit together, so a
    /// single event costs one write transaction.
    pub(crate) async fn record_playback_event(
        &self,
        event: NewPlaybackEvent<'_>,
    ) -> Result<PlaybackEventEffects, StorageError> {
        let mut transaction = self.pool.begin().await.map_err(self.sqlx_err())?;
        let max_function = self.scalar_max_function();
        let auto_played = event.duration_ticks.is_some_and(|duration_ticks| {
            playback_reached_played_threshold(
                event.position_ticks,
                duration_ticks,
                event.played_percent,
            )
        });
        let playback_session_query = format!(
            "INSERT INTO playback_sessions (
                id, user_id, item_id, media_source_id, play_session_id,
                device_id, client, device_name, client_version, device_type,
                remote_ip, state,
                position_ticks, duration_ticks, is_paused
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(user_id, play_session_id) DO UPDATE SET
                item_id = excluded.item_id,
                media_source_id = excluded.media_source_id,
                device_id = CASE
                    WHEN excluded.device_id = 'unknown' THEN playback_sessions.device_id
                    ELSE excluded.device_id END,
                client = COALESCE(excluded.client, playback_sessions.client),
                device_name = COALESCE(excluded.device_name, playback_sessions.device_name),
                client_version = COALESCE(excluded.client_version, playback_sessions.client_version),
                device_type = COALESCE(excluded.device_type, playback_sessions.device_type),
                remote_ip = COALESCE(excluded.remote_ip, playback_sessions.remote_ip),
                state = excluded.state,
                position_ticks = {max_function}(playback_sessions.position_ticks, excluded.position_ticks),
                duration_ticks = COALESCE(excluded.duration_ticks, playback_sessions.duration_ticks),
                is_paused = excluded.is_paused,
                last_event_at = unixepoch()"
        );
        self.query(sqlx::AssertSqlSafe(playback_session_query))
            .bind(Uuid::now_v7().to_string())
            .bind(event.user_id)
            .bind(event.item_id)
            .bind(event.media_source_id)
            .bind(event.play_session_id)
            .bind(event.device_id)
            .bind(event.client)
            .bind(event.device_name)
            .bind(event.client_version)
            .bind(event.device_type)
            .bind(event.remote_ip)
            .bind(event.state)
            .bind(event.position_ticks)
            .bind(event.duration_ticks)
            .bind(database_flag(event.is_paused))
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        if event.renew_admission {
            let renew_query = format!(
                "UPDATE playback_admissions
                 SET last_seen_at = {max_function}(last_seen_at, unixepoch())
                 WHERE user_id = ? AND play_session_id = ? AND state = 'ACTIVE'
                   AND last_seen_at > unixepoch() - ?"
            );
            self.query(sqlx::AssertSqlSafe(renew_query))
                .bind(event.user_id)
                .bind(event.play_session_id)
                .bind(PLAYBACK_SESSION_STALE_AFTER_SECONDS)
                .execute(&mut *transaction)
                .await
                .map_err(self.sqlx_err())?;
        }
        let same_session = "user_item_state.last_play_session_id = excluded.last_play_session_id";
        let newer_session = "(user_item_state.last_play_session_id IS NULL OR EXISTS (
            SELECT 1
            FROM playback_sessions incoming
            LEFT JOIN playback_sessions previous
              ON previous.user_id = user_item_state.user_id
             AND previous.play_session_id = user_item_state.last_play_session_id
            WHERE incoming.user_id = excluded.user_id
              AND incoming.item_id = excluded.item_id
              AND incoming.play_session_id = excluded.last_play_session_id
              AND (
                  previous.id IS NULL
                  OR incoming.started_at > previous.started_at
                  OR (incoming.started_at = previous.started_at AND incoming.id > previous.id)
              )
        ))";
        let user_item_state_query = format!(
            "INSERT INTO user_item_state (
                user_id, item_id, position_ticks, last_played_at, last_play_session_id
             ) VALUES (?, ?, ?, unixepoch(), ?)
             ON CONFLICT(user_id, item_id) DO UPDATE SET
                position_ticks = CASE
                    WHEN {same_session}
                    THEN {max_function}(user_item_state.position_ticks, excluded.position_ticks)
                    ELSE excluded.position_ticks END,
                last_played_at = CASE
                    WHEN {same_session} AND excluded.position_ticks <= user_item_state.position_ticks
                    THEN user_item_state.last_played_at
                    ELSE excluded.last_played_at END,
                last_play_session_id = excluded.last_play_session_id,
                version = user_item_state.version + CASE
                    WHEN {same_session} AND excluded.position_ticks <= user_item_state.position_ticks
                    THEN 0 ELSE 1 END
             WHERE {same_session} OR {newer_session}
             RETURNING last_play_session_id, is_played"
        );
        let persisted_session = self
            .query_as::<PlaySessionPointerRow>(sqlx::AssertSqlSafe(user_item_state_query))
            .bind(event.user_id)
            .bind(event.item_id)
            .bind(event.position_ticks)
            .bind(event.play_session_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        let event_session_is_current = persisted_session
            .as_ref()
            .is_some_and(|row| row.last_play_session_id == event.play_session_id);
        // The upsert above left `is_played` alone, so this is the value the event starts from.
        let already_played = persisted_session
            .as_ref()
            .is_some_and(|row| row.is_played != 0);
        let is_played_changed = auto_played && event_session_is_current && !already_played;
        if auto_played && event_session_is_current {
            self.query(
                "UPDATE user_item_state
                 SET is_played = 1,
                     play_count = CASE WHEN is_played = 0 THEN play_count + 1 ELSE play_count END,
                     last_played_at = unixepoch(),
                     version = version + CASE WHEN is_played = 0 THEN 1 ELSE 0 END
                 WHERE user_id = ? AND item_id = ?",
            )
            .bind(event.user_id)
            .bind(event.item_id)
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        }
        // A plain progress tick leaves the derived pointer and the played container states alone;
        // only a start, pause, stop, or played flip can change what they hold.
        let state_changed = event.previous_state != Some(event.state);
        let mut container_scope = None;
        if state_changed || is_played_changed {
            container_scope = self
                .refresh_user_series_next_for_item_in(
                    &mut transaction,
                    event.user_id,
                    event.item_id,
                )
                .await?;
        }
        transaction.commit().await.map_err(self.sqlx_err())?;
        Ok(PlaybackEventEffects {
            is_played_changed,
            container_scope,
        })
    }

    /// Recomputes the played state of the episode's season and series.
    ///
    /// Callers pass the scope when they already read it (the playback event path reads it while
    /// refreshing the resume pointer); otherwise this reads the episode row once.
    pub(crate) async fn sync_played_container_states(
        &self,
        user_id: &str,
        item_id: &str,
    ) -> Result<(), StorageError> {
        let episode_scope = self
            .query_as::<(Option<String>, Option<String>)>(
                "SELECT parent_id, series_id FROM media_items
                 WHERE id = ? AND item_type = 'EPISODE'",
            )
            .bind(item_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        let Some((parent_id, series_id)) = episode_scope else {
            return Ok(());
        };
        let scope = PlaybackContainerScope {
            parent_id,
            series_id,
        };
        self.sync_played_container_states_for_scope(user_id, &scope)
            .await
    }

    /// Played-state sync for an episode whose container scope is already known.
    pub(crate) async fn sync_played_container_states_for_scope(
        &self,
        user_id: &str,
        scope: &PlaybackContainerScope,
    ) -> Result<(), StorageError> {
        let mut parent_ids = Vec::new();
        for candidate in [scope.parent_id.clone(), scope.series_id.clone()]
            .into_iter()
            .flatten()
        {
            if !parent_ids.contains(&candidate) {
                parent_ids.push(candidate);
            }
        }
        if parent_ids.is_empty() {
            return Ok(());
        }

        let parent_placeholders = std::iter::repeat_n("?", parent_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let mut transaction = self.pool.begin().await.map_err(self.sqlx_err())?;
        let parent_state_sql = format!(
            "WITH parent_scope AS (
                 SELECT id, item_type FROM media_items WHERE id IN ({parent_placeholders})
             ), eligible AS (
                 SELECT parent_scope.id AS parent_id, episode.id AS episode_id
                 FROM parent_scope
                 JOIN media_items episode
                   ON episode.item_type = 'EPISODE'
                  AND episode.removed_at IS NULL
                  AND episode.has_available_source = 1
                  AND ((parent_scope.item_type = 'SEASON' AND episode.parent_id = parent_scope.id)
                    OR (parent_scope.item_type = 'SERIES' AND episode.series_id = parent_scope.id))
             ), parent_states AS (
                 SELECT eligible.parent_id,
                        CASE WHEN COUNT(*) > 0
                                  AND SUM(CASE WHEN COALESCE(state.is_played, 0) = 0 THEN 1 ELSE 0 END) = 0
                             THEN 1 ELSE 0 END AS is_played
                 FROM eligible
                 LEFT JOIN user_item_state state
                   ON state.user_id = ? AND state.item_id = eligible.episode_id
                 GROUP BY eligible.parent_id
             )
             SELECT parent_scope.id AS parent_id,
                    COALESCE(parent_states.is_played, 0) AS is_played
             FROM parent_scope
             LEFT JOIN parent_states ON parent_states.parent_id = parent_scope.id"
        );
        let mut parent_state_query =
            self.query_as::<PlayedParentStateRow>(sqlx::AssertSqlSafe(parent_state_sql));
        for parent_id in &parent_ids {
            parent_state_query = parent_state_query.bind(parent_id);
        }
        parent_state_query = parent_state_query.bind(user_id);
        let parent_states = parent_state_query
            .fetch_all(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?
            .into_iter()
            .map(|row| (row.parent_id, row.is_played))
            .collect::<Vec<_>>();

        let values = std::iter::repeat_n("(?, ?, ?, CASE WHEN ? = 1 THEN 1 ELSE 0 END, CASE WHEN ? = 1 THEN unixepoch() ELSE NULL END)", parent_states.len())
            .collect::<Vec<_>>()
            .join(", ");
        let mut state_write = self.query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO user_item_state (user_id, item_id, is_played, play_count, last_played_at)
             VALUES {values}
             ON CONFLICT(user_id, item_id) DO UPDATE SET
                 is_played = excluded.is_played,
                 play_count = CASE
                     WHEN excluded.is_played = 1 AND user_item_state.is_played = 0
                     THEN user_item_state.play_count + 1 ELSE user_item_state.play_count END,
                 last_played_at = CASE
                     WHEN excluded.is_played = 1 THEN unixepoch()
                     ELSE user_item_state.last_played_at END,
                 version = user_item_state.version + CASE
                     WHEN excluded.is_played != user_item_state.is_played THEN 1 ELSE 0 END"
        )));
        for (parent_id, is_played) in parent_states {
            state_write = state_write
                .bind(user_id)
                .bind(parent_id)
                .bind(is_played)
                .bind(is_played)
                .bind(is_played);
        }
        state_write
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        transaction.commit().await.map_err(self.sqlx_err())
        // The resume pointer only reads episode-level progress, so this sync does not touch it.
        // Whoever changed the episode state refreshes the pointer in its own transaction.
    }

    pub(crate) async fn list_playback_sessions(
        &self,
        user_id: Option<&str>,
        active_within_seconds: Option<i64>,
    ) -> Result<Vec<StoredPlaybackSession>, StorageError> {
        let active_within_seconds = active_within_seconds
            .unwrap_or(PLAYBACK_SESSION_STALE_AFTER_SECONDS)
            .min(PLAYBACK_SESSION_STALE_AFTER_SECONDS)
            .clamp(1, MAX_PLAYBACK_SESSION_WINDOW_SECONDS);
        let (query, bind) = if user_id.is_some() {
            (
                "SELECT id, user_id, item_id, media_source_id, play_session_id,
                        device_id, client, device_name, client_version, device_type,
                        remote_ip, state,
                        position_ticks, duration_ticks, is_paused, started_at,
                        last_event_at
                 FROM playback_sessions
                 WHERE user_id = ?
                   AND state != 'STOPPED'
                   AND last_event_at > unixepoch() - ?
                 ORDER BY last_event_at DESC, id
                 LIMIT ?",
                user_id,
            )
        } else {
            (
                "SELECT id, user_id, item_id, media_source_id, play_session_id,
                        device_id, client, device_name, client_version, device_type,
                        remote_ip, state,
                        position_ticks, duration_ticks, is_paused, started_at,
                        last_event_at
                 FROM playback_sessions
                 WHERE state != 'STOPPED'
                   AND last_event_at > unixepoch() - ?
                 ORDER BY last_event_at DESC, id
                 LIMIT ?",
                None,
            )
        };
        let mut statement = self.query(query);
        if let Some(user_id) = bind {
            statement = statement.bind(user_id);
        }
        statement = statement
            .bind(active_within_seconds)
            .bind(MAX_LISTED_PLAYBACK_SESSIONS);
        statement
            .fetch_all(&self.pool)
            .await
            .map_err(self.sqlx_err())?
            .into_iter()
            .map(stored_playback_session)
            .collect::<Result<Vec<_>, _>>()
            .map_err(self.sqlx_err())
    }

    pub(crate) async fn find_playback_session(
        &self,
        user_id: &str,
        play_session_id: &str,
    ) -> Result<Option<StoredPlaybackSession>, StorageError> {
        self.query(
            "SELECT id, user_id, item_id, media_source_id, play_session_id,
                    device_id, client, device_name, client_version, device_type,
                    remote_ip, state,
                    position_ticks, duration_ticks, is_paused, started_at,
                    last_event_at
             FROM playback_sessions
             WHERE user_id = ? AND play_session_id = ?",
        )
        .bind(user_id)
        .bind(play_session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(self.sqlx_err())?
        .map(stored_playback_session)
        .transpose()
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn find_playback_session_by_id(
        &self,
        session_id: &str,
    ) -> Result<Option<StoredPlaybackSession>, StorageError> {
        self.query(
            "SELECT id, user_id, item_id, media_source_id, play_session_id,
                    device_id, client, device_name, client_version, device_type,
                    remote_ip, state,
                    position_ticks, duration_ticks, is_paused, started_at,
                    last_event_at
             FROM playback_sessions
             WHERE id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(self.sqlx_err())?
        .map(stored_playback_session)
        .transpose()
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn stop_playback_session(
        &self,
        session_id: &str,
        now: i64,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE playback_sessions
             SET state = 'STOPPED', is_paused = 0, last_event_at = ?
             WHERE id = ?",
        )
        .bind(now)
        .bind(session_id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn find_active_playback_session_for_stop(
        &self,
        user_id: &str,
        item_id: &str,
        media_source_id: Option<&str>,
        device_id: Option<&str>,
    ) -> Result<Option<StoredPlaybackSession>, StorageError> {
        let media_source_filter = if media_source_id.is_some() {
            " AND media_source_id = ?"
        } else {
            ""
        };
        let device_filter = if device_id.is_some() {
            " AND device_id = ?"
        } else {
            ""
        };
        let query = format!(
            "SELECT id, user_id, item_id, media_source_id, play_session_id,
                    device_id, client, device_name, client_version, device_type,
                    remote_ip, state,
                    position_ticks, duration_ticks, is_paused, started_at,
                    last_event_at
             FROM playback_sessions
             WHERE user_id = ?
               AND item_id = ?
               {device_filter}
               AND state != 'STOPPED'
               AND last_event_at > unixepoch() - ?
               {media_source_filter}
             ORDER BY last_event_at DESC, id
             LIMIT 2"
        );
        let mut statement = self.query(sqlx::AssertSqlSafe(query));
        statement = statement.bind(user_id).bind(item_id);
        if let Some(device_id) = device_id {
            statement = statement.bind(device_id);
        }
        statement = statement.bind(PLAYBACK_SESSION_STALE_AFTER_SECONDS);
        if let Some(media_source_id) = media_source_id {
            statement = statement.bind(media_source_id);
        }
        let sessions = statement
            .fetch_all(&self.pool)
            .await
            .map_err(self.sqlx_err())?
            .into_iter()
            .map(stored_playback_session)
            .collect::<Result<Vec<_>, _>>()
            .map_err(self.sqlx_err())?;
        if sessions.len() == 1 {
            Ok(sessions.into_iter().next())
        } else {
            Ok(None)
        }
    }

    pub(crate) async fn find_active_playback_session(
        &self,
        user_id: &str,
        item_id: &str,
    ) -> Result<Option<StoredPlaybackSession>, StorageError> {
        self.query(
            "SELECT id, user_id, item_id, media_source_id, play_session_id,
                    device_id, client, device_name, client_version, device_type,
                    remote_ip, state,
                    position_ticks, duration_ticks, is_paused, started_at,
                    last_event_at
             FROM playback_sessions
             WHERE user_id = ?
               AND item_id = ?
               AND state != 'STOPPED'
               AND last_event_at > unixepoch() - ?
             ORDER BY last_event_at DESC, id
             LIMIT 1",
        )
        .bind(user_id)
        .bind(item_id)
        .bind(PLAYBACK_SESSION_STALE_AFTER_SECONDS)
        .fetch_optional(&self.pool)
        .await
        .map_err(self.sqlx_err())?
        .map(stored_playback_session)
        .transpose()
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn insert_web_playback_session(
        &self,
        session: NewWebPlaybackSession<'_>,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO web_playback_sessions (
                id, user_id, item_id, media_source_id, play_session_id,
                tier, plan, state, temp_dir, is_admin, expires_at, last_heartbeat_at,
                last_sequence, created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, 'ACTIVE', ?, ?, ?, ?, -1, ?, ?)",
        )
        .bind(session.id)
        .bind(session.user_id)
        .bind(session.item_id)
        .bind(session.media_source_id)
        .bind(session.play_session_id)
        .bind(session.tier)
        .bind(session.plan)
        .bind(session.temp_dir)
        .bind(database_flag(session.is_admin))
        .bind(session.expires_at)
        .bind(session.now)
        .bind(session.now)
        .bind(session.now)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn set_web_playback_access_context(
        &self,
        session_id: &str,
        user_id: &str,
        device_id: Option<&str>,
        client_name: Option<&str>,
        updated_at: i64,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE web_playback_sessions
             SET authorization_device_id = ?, authorization_client_name = ?, updated_at = ?
             WHERE id = ? AND user_id = ? AND state = 'ACTIVE'",
        )
        .bind(device_id)
        .bind(client_name)
        .bind(updated_at)
        .bind(session_id)
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn find_web_playback_access_context(
        &self,
        session_id: &str,
    ) -> Result<Option<StoredWebPlaybackAccessContext>, StorageError> {
        self.query_as::<WebPlaybackAccessContextRow>(
            "SELECT user_id, authorization_device_id, authorization_client_name
             FROM web_playback_sessions
             WHERE id = ? AND state = 'ACTIVE'",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(StoredWebPlaybackAccessContext::from))
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn insert_emby_direct_playback_ticket(
        &self,
        ticket: NewEmbyDirectPlaybackTicket<'_>,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO emby_direct_playback_tickets (
                 user_id, play_session_id, authorization_device_id,
                 authorization_client_name, expires_at, created_at
             ) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(ticket.user_id)
        .bind(ticket.play_session_id)
        .bind(ticket.authorization_device_id)
        .bind(ticket.authorization_client_name)
        .bind(ticket.expires_at)
        .bind(ticket.created_at)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn ensure_legacy_emby_direct_playback_ticket(
        &self,
        user_id: &str,
        play_session_id: &str,
        expires_at: i64,
        created_at: i64,
    ) -> Result<bool, StorageError> {
        self.query(
            "INSERT INTO emby_direct_playback_tickets (
                 user_id, play_session_id, authorization_device_id,
                 authorization_client_name, expires_at, created_at
             ) VALUES (?, ?, NULL, NULL, ?, ?)
             ON CONFLICT(user_id, play_session_id) DO NOTHING",
        )
        .bind(user_id)
        .bind(play_session_id)
        .bind(expires_at)
        .bind(created_at)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn find_emby_direct_playback_ticket(
        &self,
        user_id: &str,
        play_session_id: &str,
    ) -> Result<Option<StoredEmbyDirectPlaybackTicket>, StorageError> {
        self.query_as::<EmbyDirectPlaybackTicketRow>(
            "SELECT user_id, play_session_id, authorization_device_id,
                    authorization_client_name, expires_at, revoked_at, created_at
             FROM emby_direct_playback_tickets
             WHERE user_id = ? AND play_session_id = ?",
        )
        .bind(user_id)
        .bind(play_session_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(StoredEmbyDirectPlaybackTicket::from))
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn revoke_emby_direct_playback_ticket(
        &self,
        user_id: &str,
        play_session_id: &str,
        revoked_at: i64,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE emby_direct_playback_tickets
             SET revoked_at = ?
             WHERE user_id = ? AND play_session_id = ? AND revoked_at IS NULL
               AND created_at <= ? AND expires_at >= ?",
        )
        .bind(revoked_at)
        .bind(user_id)
        .bind(play_session_id)
        .bind(revoked_at)
        .bind(revoked_at)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn delete_expired_emby_direct_playback_tickets(
        &self,
        now: i64,
        limit: i64,
    ) -> Result<u64, StorageError> {
        let limit = limit.clamp(1, 1_000);
        self.query(
            "DELETE FROM emby_direct_playback_tickets
             WHERE (user_id, play_session_id) IN (
                 SELECT user_id, play_session_id
                 FROM emby_direct_playback_tickets
                 WHERE expires_at < ?
                 ORDER BY expires_at, user_id, play_session_id
                 LIMIT ?
             )",
        )
        .bind(now.max(0))
        .bind(limit)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected())
        .map_err(self.sqlx_err())
    }

    /// Inserts one finished-transcode row; a session id is recorded at most once.
    ///
    /// Rows older than [`TRANSCODING_HISTORY_RETENTION_SECONDS`] are dropped on the same call so
    /// the table stays bounded without a background worker.
    pub(crate) async fn insert_transcoding_history(
        &self,
        entry: NewTranscodingHistory<'_>,
    ) -> Result<(), StorageError> {
        let now = entry.now;
        self.query(
            "INSERT INTO transcoding_history (
                 id, session_id, user_id, item_id, media_source_id, tier, outcome,
                 started_at, finished_at, wall_seconds, encoded_seconds, average_speed,
                 last_fps, output_bytes, failure_reason, created_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(session_id) DO NOTHING",
        )
        .bind(entry.id)
        .bind(entry.session_id)
        .bind(entry.user_id)
        .bind(entry.item_id)
        .bind(entry.media_source_id)
        .bind(entry.tier)
        .bind(entry.outcome)
        .bind(entry.started_at)
        .bind(entry.finished_at)
        .bind(entry.wall_seconds)
        .bind(entry.encoded_seconds)
        .bind(entry.average_speed)
        .bind(entry.last_fps)
        .bind(entry.output_bytes)
        .bind(entry.failure_reason)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(self.sqlx_err())?;
        let _ = self
            .query(
                "DELETE FROM transcoding_history
                 WHERE finished_at < ?",
            )
            .bind(now.saturating_sub(TRANSCODING_HISTORY_RETENTION_SECONDS))
            .execute(&self.pool)
            .await;
        Ok(())
    }

    /// One page of finished transcodes, newest first, with item title and user name.
    pub(crate) async fn transcoding_history_page(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<StoredTranscodingHistory>, i64), StorageError> {
        let total: i64 = self
            .query_scalar("SELECT COUNT(*) FROM transcoding_history")
            .fetch_one(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        let rows = self
            .query_as::<TranscodingHistoryRow>(
                "SELECT h.id, h.session_id, h.user_id, u.display_name AS user_name,
                        h.item_id, mi.title AS item_title, mi.item_type, h.media_source_id,
                        h.tier, h.outcome, h.started_at, h.finished_at, h.wall_seconds,
                        h.encoded_seconds, h.average_speed, h.last_fps, h.output_bytes,
                        h.failure_reason
                 FROM transcoding_history h
                 LEFT JOIN users u ON u.id = h.user_id
                 LEFT JOIN media_items mi ON mi.id = h.item_id
                 ORDER BY h.finished_at DESC, h.id DESC
                 LIMIT ? OFFSET ?",
            )
            .bind(limit.clamp(1, 50))
            .bind(offset.max(0))
            .fetch_all(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        Ok((
            rows.into_iter()
                .map(StoredTranscodingHistory::from)
                .collect(),
            total,
        ))
    }

    /// Active server-side transcoding sessions for the admin progress list, newest first.
    pub(crate) async fn active_server_transcoding_sessions(
        &self,
        limit: i64,
    ) -> Result<Vec<StoredTranscodingSessionRow>, StorageError> {
        self.query_as::<TranscodingSessionRow>(
            "SELECT w.id AS session_id, w.user_id, u.display_name AS user_name,
                    w.item_id, mi.title AS item_title, mi.item_type, w.tier, w.state,
                    w.expires_at, w.created_at, w.updated_at, w.video_codec, w.audio_codec,
                    w.audio_bitrate, w.transcoding_container
             FROM web_playback_sessions w
             LEFT JOIN users u ON u.id = w.user_id
             LEFT JOIN media_items mi ON mi.id = w.item_id
             WHERE w.state = 'ACTIVE' AND w.plan = 'SERVER_HLS'
             ORDER BY w.updated_at DESC, w.id DESC
             LIMIT ?",
        )
        .bind(limit.clamp(1, 100))
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(StoredTranscodingSessionRow::from)
                .collect()
        })
        .map_err(self.sqlx_err())
    }

    /// Records the tier actually running, after a hardware start fell back to software.
    pub(crate) async fn update_web_playback_session_tier(
        &self,
        session_id: &str,
        user_id: &str,
        tier: i64,
        plan: &str,
        now: i64,
    ) -> Result<bool, StorageError> {
        Ok(self
            .query(
                "UPDATE web_playback_sessions
                 SET tier = ?, plan = ?, updated_at = ?
                 WHERE id = ? AND user_id = ? AND state = 'ACTIVE'",
            )
            .bind(tier)
            .bind(plan)
            .bind(now)
            .bind(session_id)
            .bind(user_id)
            .execute(&self.pool)
            .await
            .map_err(self.sqlx_err())?
            .rows_affected()
            == 1)
    }

    pub(crate) async fn set_web_playback_transcoding_details(
        &self,
        session_id: &str,
        user_id: &str,
        details: &WebPlaybackTranscodingDetails,
        now: i64,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE web_playback_sessions
             SET video_codec = ?, audio_codec = ?, video_bitrate = ?, audio_bitrate = ?,
                 transcoding_container = ?, updated_at = ?
             WHERE id = ? AND user_id = ? AND state = 'ACTIVE'",
        )
        .bind(details.video_codec.as_deref())
        .bind(details.audio_codec.as_deref())
        .bind(details.video_bitrate)
        .bind(details.audio_bitrate)
        .bind(details.transcoding_container.as_deref())
        .bind(now)
        .bind(session_id)
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn find_web_playback_session(
        &self,
        session_id: &str,
    ) -> Result<Option<StoredWebPlaybackSession>, StorageError> {
        self.query(
            "SELECT id, user_id, item_id, media_source_id, play_session_id,
                    tier, plan, state, temp_dir, is_admin, expires_at, last_heartbeat_at,
                    last_sequence, video_codec, audio_codec, video_bitrate, audio_bitrate,
                    transcoding_container, created_at, updated_at
             FROM web_playback_sessions
             WHERE id = ?",
        )
        .bind(session_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(self.sqlx_err())?
        .map(stored_web_playback_session)
        .transpose()
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn find_web_playback_sessions_for_playbacks(
        &self,
        play_session_ids: &[String],
    ) -> Result<Vec<StoredWebPlaybackSession>, StorageError> {
        if play_session_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat_n("?", play_session_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "SELECT id, user_id, item_id, media_source_id, play_session_id,
                    tier, plan, state, temp_dir, is_admin, expires_at, last_heartbeat_at,
                    last_sequence, video_codec, audio_codec, video_bitrate, audio_bitrate,
                    transcoding_container, created_at, updated_at
             FROM web_playback_sessions
             WHERE state = 'ACTIVE' AND play_session_id IN ({placeholders})"
        );
        let mut statement = self.query(sqlx::AssertSqlSafe(query));
        for play_session_id in play_session_ids {
            statement = statement.bind(play_session_id);
        }
        statement
            .fetch_all(&self.pool)
            .await
            .map_err(self.sqlx_err())?
            .into_iter()
            .map(stored_web_playback_session)
            .collect::<Result<Vec<_>, _>>()
            .map_err(self.sqlx_err())
    }

    pub(crate) async fn find_active_web_playback_sessions_for_source(
        &self,
        user_id: &str,
        item_id: &str,
        media_source_id: &str,
        play_session_prefix: &str,
    ) -> Result<Vec<StoredWebPlaybackSession>, StorageError> {
        let play_session_pattern = format!("{play_session_prefix}:%");
        self.query(
            "SELECT id, user_id, item_id, media_source_id, play_session_id,
                    tier, plan, state, temp_dir, is_admin, expires_at, last_heartbeat_at,
                    last_sequence, video_codec, audio_codec, video_bitrate, audio_bitrate,
                    transcoding_container, created_at, updated_at
             FROM web_playback_sessions
             WHERE user_id = ? AND item_id = ? AND media_source_id = ?
               AND state = 'ACTIVE' AND play_session_id LIKE ?
             ORDER BY created_at ASC, id ASC
             LIMIT ?",
        )
        .bind(user_id)
        .bind(item_id)
        .bind(media_source_id)
        .bind(play_session_pattern)
        .bind(MAX_SESSIONS_PER_SOURCE)
        .fetch_all(&self.pool)
        .await
        .map_err(self.sqlx_err())?
        .into_iter()
        .map(stored_web_playback_session)
        .collect::<Result<Vec<_>, _>>()
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn take_expired_web_playback_sessions(
        &self,
        now: i64,
    ) -> Result<Vec<StoredWebPlaybackSession>, StorageError> {
        let rows = self
            .query(
                "SELECT id, user_id, item_id, media_source_id, play_session_id,
                        tier, plan, state, temp_dir, is_admin, expires_at, last_heartbeat_at,
                        last_sequence, video_codec, audio_codec, video_bitrate, audio_bitrate,
                        transcoding_container, created_at, updated_at
                 FROM web_playback_sessions
                 WHERE state = 'ACTIVE' AND expires_at < ?
                 ORDER BY expires_at ASC, id ASC
                 LIMIT 128",
            )
            .bind(now)
            .fetch_all(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        let sessions = rows
            .into_iter()
            .map(stored_web_playback_session)
            .collect::<Result<Vec<_>, _>>()
            .map_err(self.sqlx_err())?;
        self.stop_selected_web_playback_sessions(sessions, now, "expires_at < ?", now)
            .await
    }

    pub(crate) async fn take_inactive_web_playback_sessions(
        &self,
        now: i64,
        inactive_after_seconds: i64,
    ) -> Result<Vec<StoredWebPlaybackSession>, StorageError> {
        let cutoff = now.saturating_sub(inactive_after_seconds.max(0));
        let rows = self
            .query(
                "SELECT id, user_id, item_id, media_source_id, play_session_id,
                        tier, plan, state, temp_dir, is_admin, expires_at, last_heartbeat_at,
                        last_sequence, video_codec, audio_codec, video_bitrate, audio_bitrate,
                        transcoding_container, created_at, updated_at
                 FROM web_playback_sessions
                 WHERE state = 'ACTIVE' AND plan = 'SERVER_HLS' AND last_heartbeat_at < ?
                 ORDER BY last_heartbeat_at ASC, id ASC
                 LIMIT 128",
            )
            .bind(cutoff)
            .fetch_all(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        let sessions = rows
            .into_iter()
            .map(stored_web_playback_session)
            .collect::<Result<Vec<_>, _>>()
            .map_err(self.sqlx_err())?;
        self.stop_selected_web_playback_sessions(
            sessions,
            now,
            "plan = 'SERVER_HLS' AND last_heartbeat_at < ?",
            cutoff,
        )
        .await
    }

    async fn stop_selected_web_playback_sessions(
        &self,
        sessions: Vec<StoredWebPlaybackSession>,
        now: i64,
        eligibility: &str,
        eligibility_value: i64,
    ) -> Result<Vec<StoredWebPlaybackSession>, StorageError> {
        if sessions.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat_n("?", sessions.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "UPDATE web_playback_sessions
             SET state = 'STOPPED', updated_at = ?
             WHERE id IN ({placeholders}) AND state = 'ACTIVE' AND {eligibility}
             RETURNING id"
        );
        let mut query = self.query_scalar::<String>(sqlx::AssertSqlSafe(sql));
        query = query.bind(now);
        for session in &sessions {
            query = query.bind(&session.id);
        }
        let stopped_ids = query
            .bind(eligibility_value)
            .fetch_all(&self.pool)
            .await
            .map_err(self.sqlx_err())?
            .into_iter()
            .collect::<HashSet<_>>();
        Ok(sessions
            .into_iter()
            .filter(|session| stopped_ids.contains(&session.id))
            .collect())
    }

    pub(crate) async fn set_web_playback_temp_dir(
        &self,
        session_id: &str,
        temp_dir: &str,
        now: i64,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE web_playback_sessions
             SET temp_dir = ?, updated_at = ?
             WHERE id = ? AND state = 'ACTIVE'",
        )
        .bind(temp_dir)
        .bind(now)
        .bind(session_id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn touch_web_playback_session(
        &self,
        session_id: &str,
        user_id: &str,
        expires_at: i64,
        now: i64,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE web_playback_sessions
             SET expires_at = ?, last_heartbeat_at = ?, updated_at = ?
             WHERE id = ? AND user_id = ? AND state = 'ACTIVE' AND expires_at >= ?",
        )
        .bind(expires_at)
        .bind(now)
        .bind(now)
        .bind(session_id)
        .bind(user_id)
        .bind(now)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn stop_web_playback_session(
        &self,
        session_id: &str,
        user_id: &str,
        state: &str,
        now: i64,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE web_playback_sessions
             SET state = ?, updated_at = ?
             WHERE id = ? AND user_id = ? AND state = 'ACTIVE'",
        )
        .bind(state)
        .bind(now)
        .bind(session_id)
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn accept_web_playback_event(
        &self,
        event: NewWebPlaybackEvent<'_>,
    ) -> Result<WebPlaybackEventClaim, StorageError> {
        let mut transaction = self.pool.begin().await.map_err(self.sqlx_err())?;
        let inserted = self
            .query(
                "INSERT INTO web_playback_events (
                    session_id, event_id, sequence, state,
                    position_ticks, duration_ticks, created_at
                 ) VALUES (?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT DO NOTHING",
            )
            .bind(event.session_id)
            .bind(event.event_id)
            .bind(event.sequence)
            .bind(event.state)
            .bind(event.position_ticks)
            .bind(event.duration_ticks)
            .bind(event.now)
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        if inserted.rows_affected() == 0 {
            transaction.commit().await.map_err(self.sqlx_err())?;
            return Ok(WebPlaybackEventClaim::Duplicate);
        }
        let updated = self
            .query(
                "UPDATE web_playback_sessions
                 SET state = CASE WHEN ? = 'STOPPED' THEN 'STOPPED' ELSE state END,
                     last_sequence = ?,
                     last_heartbeat_at = CASE WHEN ? = 'STOPPED' THEN last_heartbeat_at ELSE ? END,
                     updated_at = ?
                 WHERE id = ? AND user_id = ? AND state = 'ACTIVE'
                   AND expires_at >= ? AND last_sequence < ?",
            )
            .bind(event.state)
            .bind(event.sequence)
            .bind(event.state)
            .bind(event.now)
            .bind(event.now)
            .bind(event.session_id)
            .bind(event.user_id)
            .bind(event.now)
            .bind(event.sequence)
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        transaction.commit().await.map_err(self.sqlx_err())?;
        Ok(if updated.rows_affected() == 1 {
            WebPlaybackEventClaim::Accepted
        } else {
            WebPlaybackEventClaim::Stale
        })
    }
}

pub(crate) struct StoredTranscodingSessionRow {
    pub(crate) session_id: String,
    pub(crate) user_id: String,
    pub(crate) user_name: Option<String>,
    pub(crate) item_id: String,
    pub(crate) item_title: Option<String>,
    pub(crate) item_type: Option<String>,
    pub(crate) tier: i64,
    pub(crate) state: String,
    pub(crate) expires_at: i64,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) video_codec: Option<String>,
    pub(crate) audio_codec: Option<String>,
    pub(crate) audio_bitrate: Option<i64>,
    pub(crate) transcoding_container: Option<String>,
}

/// Transcode history is diagnostic data, not business data: keep a month.
const TRANSCODING_HISTORY_RETENTION_SECONDS: i64 = 30 * 24 * 60 * 60;

pub(crate) struct NewTranscodingHistory<'a> {
    pub(crate) id: &'a str,
    pub(crate) session_id: &'a str,
    pub(crate) user_id: &'a str,
    pub(crate) item_id: &'a str,
    pub(crate) media_source_id: Option<&'a str>,
    pub(crate) tier: i64,
    pub(crate) outcome: &'a str,
    pub(crate) started_at: i64,
    pub(crate) finished_at: i64,
    pub(crate) wall_seconds: i64,
    pub(crate) encoded_seconds: Option<i64>,
    pub(crate) average_speed: Option<f64>,
    pub(crate) last_fps: Option<f64>,
    pub(crate) output_bytes: Option<i64>,
    pub(crate) failure_reason: Option<&'a str>,
    pub(crate) now: i64,
}

pub(crate) struct StoredTranscodingHistory {
    pub(crate) id: String,
    pub(crate) session_id: String,
    pub(crate) user_id: String,
    pub(crate) user_name: Option<String>,
    pub(crate) item_id: String,
    pub(crate) item_title: Option<String>,
    pub(crate) item_type: Option<String>,
    pub(crate) media_source_id: Option<String>,
    pub(crate) tier: i64,
    pub(crate) outcome: String,
    pub(crate) started_at: i64,
    pub(crate) finished_at: i64,
    pub(crate) wall_seconds: i64,
    pub(crate) encoded_seconds: Option<i64>,
    pub(crate) average_speed: Option<f64>,
    pub(crate) last_fps: Option<f64>,
    pub(crate) output_bytes: Option<i64>,
    pub(crate) failure_reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::test_database;
    use sqlx::FromRow;

    #[test]
    fn session_listing_bound_is_small_enough_to_stay_cheap() {
        assert!((100..=1000).contains(&MAX_LISTED_PLAYBACK_SESSIONS));
    }

    /// Creates the user, library and movie that the transcoding readers join against.
    async fn insert_transcoding_fixtures(
        database: &Database,
    ) -> Result<(), Box<dyn std::error::Error>> {
        sqlx::query(
            "INSERT INTO libraries (id, name, kind) VALUES ('library-a', 'Library A', 'MIXED')",
        )
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO users (id, username_normalized, display_name, password_hash)
             VALUES ('user-a', 'user-a', 'User A', 'hash')",
        )
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('item-a', 'library-a', 'MOVIE', 'Movie A', 'movie a', 'LOCAL_CONFIRMED')",
        )
        .execute(database.pool())
        .await?;
        Ok(())
    }

    #[tokio::test]
    async fn transcoding_history_page_decodes_named_row_fields()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        insert_transcoding_fixtures(&database).await?;
        database
            .insert_transcoding_history(NewTranscodingHistory {
                id: "history-a",
                session_id: "play-session-a",
                user_id: "user-a",
                item_id: "item-a",
                media_source_id: Some("source-a"),
                tier: 2,
                outcome: "COMPLETED",
                started_at: 100,
                finished_at: 300,
                wall_seconds: 200,
                encoded_seconds: Some(180),
                average_speed: Some(1.5),
                last_fps: Some(24.0),
                output_bytes: Some(4096),
                failure_reason: None,
                now: 300,
            })
            .await?;

        let (rows, total) = database.transcoding_history_page(10, 0).await?;
        assert_eq!(total, 1);
        let row = rows
            .first()
            .ok_or("transcoding history row was not returned")?;
        assert_eq!(row.id, "history-a");
        assert_eq!(row.session_id, "play-session-a");
        assert_eq!(row.user_id, "user-a");
        assert_eq!(row.user_name.as_deref(), Some("User A"));
        assert_eq!(row.item_id, "item-a");
        assert_eq!(row.item_title.as_deref(), Some("Movie A"));
        assert_eq!(row.item_type.as_deref(), Some("MOVIE"));
        assert_eq!(row.media_source_id.as_deref(), Some("source-a"));
        assert_eq!(row.tier, 2);
        assert_eq!(row.outcome, "COMPLETED");
        assert_eq!(row.started_at, 100);
        assert_eq!(row.finished_at, 300);
        assert_eq!(row.wall_seconds, 200);
        assert_eq!(row.encoded_seconds, Some(180));
        assert_eq!(row.average_speed, Some(1.5));
        assert_eq!(row.last_fps, Some(24.0));
        assert_eq!(row.output_bytes, Some(4096));
        assert_eq!(row.failure_reason, None);
        Ok(())
    }

    #[tokio::test]
    async fn active_server_transcoding_sessions_decodes_named_row_fields()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        insert_transcoding_fixtures(&database).await?;
        sqlx::query(
            "INSERT INTO web_playback_sessions (
                 id, user_id, item_id, media_source_id, play_session_id, tier, plan, state,
                 temp_dir, is_admin, expires_at, last_heartbeat_at, last_sequence,
                 video_codec, audio_codec, video_bitrate, audio_bitrate, transcoding_container,
                 created_at, updated_at
             ) VALUES (
                 'session-a', 'user-a', 'item-a', NULL, 'play-a', 3, 'SERVER_HLS', 'ACTIVE',
                 '/tmp/transcode', 0, 500, 150, 4,
                 'h264', 'aac', 2000, 128, 'mkv',
                 10, 20
             )",
        )
        .execute(database.pool())
        .await?;

        let rows = database.active_server_transcoding_sessions(10).await?;
        let row = rows
            .first()
            .ok_or("transcoding session row was not returned")?;
        assert_eq!(row.session_id, "session-a");
        assert_eq!(row.user_id, "user-a");
        assert_eq!(row.user_name.as_deref(), Some("User A"));
        assert_eq!(row.item_id, "item-a");
        assert_eq!(row.item_title.as_deref(), Some("Movie A"));
        assert_eq!(row.item_type.as_deref(), Some("MOVIE"));
        assert_eq!(row.tier, 3);
        assert_eq!(row.state, "ACTIVE");
        assert_eq!(row.expires_at, 500);
        assert_eq!(row.created_at, 10);
        assert_eq!(row.updated_at, 20);
        assert_eq!(row.video_codec.as_deref(), Some("h264"));
        assert_eq!(row.audio_codec.as_deref(), Some("aac"));
        assert_eq!(row.audio_bitrate, Some(128));
        assert_eq!(row.transcoding_container.as_deref(), Some("mkv"));
        Ok(())
    }

    #[tokio::test]
    async fn transcoding_rows_report_a_missing_column_instead_of_panicking()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let row = sqlx::query("SELECT 1 AS other")
            .fetch_one(database.pool())
            .await?;
        let error =
            TranscodingHistoryRow::from_row(&row).expect_err("missing columns must not panic");
        assert!(
            matches!(error, sqlx::Error::ColumnNotFound(_)),
            "unexpected decode error: {error}"
        );
        let error =
            TranscodingSessionRow::from_row(&row).expect_err("missing columns must not panic");
        assert!(
            matches!(error, sqlx::Error::ColumnNotFound(_)),
            "unexpected decode error: {error}"
        );
        Ok(())
    }
}
