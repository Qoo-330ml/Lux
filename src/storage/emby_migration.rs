use std::path::PathBuf;

use super::{Database, StorageError, StoredUserItemState};
use sqlx::Row;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredEmbyMigrationJob {
    pub id: String,
    pub plugin_id: String,
    pub created_by_user_id: String,
    pub source_label: String,
    pub source_base_url: String,
    pub secret_ref: String,
    pub status: String,
    pub phase: String,
    pub dry_run: bool,
    pub merge_policy: String,
    pub history_capability: String,
    pub cursor_json: String,
    pub processed_count: i64,
    pub total_count: i64,
    pub matched_count: i64,
    pub skipped_count: i64,
    pub failed_count: i64,
    pub cancel_requested: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredEmbyMigrationUserLink {
    pub job_id: String,
    pub emby_user_id: String,
    pub emby_username: String,
    pub lux_user_id: Option<String>,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredEmbyMigrationSource {
    pub source_base_url: String,
    pub secret_ref: String,
    pub source_label: String,
    pub history_capability: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredEmbyMigrationUserBinding {
    pub lux_user_id: String,
    pub source_base_url: String,
    pub secret_ref: Option<String>,
    pub emby_user_id: String,
    pub emby_username: String,
    pub password_pending: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredPlaybackHistoryEvent {
    pub id: String,
    pub user_id: String,
    pub item_id: String,
    pub event_type: String,
    pub position_ticks: i64,
    pub duration_ticks: Option<i64>,
    pub occurred_at: i64,
    pub source: String,
    pub source_event_key: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredEmbyMigrationItemMatch {
    pub job_id: String,
    pub emby_item_id: String,
    pub emby_item_type: String,
    pub lux_item_id: Option<String>,
    pub match_method: String,
    pub confidence: Option<i64>,
    pub status: String,
    pub detail_json: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredEmbyMigrationImportRecord {
    pub job_id: String,
    pub emby_user_id: String,
    pub emby_item_id: String,
    pub lux_user_id: String,
    pub lux_item_id: String,
    pub state_hash: String,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredMigrationMediaIdentity {
    pub id: String,
    pub item_type: String,
    pub title: String,
    pub production_year: Option<i64>,
    pub provider_ids_json: Option<String>,
    pub series_id: Option<String>,
    pub season_number: Option<i64>,
    pub episode_number: Option<i64>,
}

impl Database {
    pub(crate) async fn upsert_emby_migration_source(
        &self,
        source: &StoredEmbyMigrationSource,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO emby_migration_sources (
                 source_base_url, secret_ref, source_label, history_capability
             ) VALUES (?, ?, ?, ?)
             ON CONFLICT(source_base_url) DO UPDATE SET
                 secret_ref = excluded.secret_ref,
                 source_label = excluded.source_label,
                 history_capability = excluded.history_capability,
                 updated_at = unixepoch()",
        )
        .bind(&source.source_base_url)
        .bind(&source.secret_ref)
        .bind(&source.source_label)
        .bind(&source.history_capability)
        .execute(self.pool())
        .await
        .map(|_| ())
        .map_err(storage_error)
    }

    pub(crate) async fn find_emby_migration_source(
        &self,
        source_base_url: &str,
    ) -> Result<Option<StoredEmbyMigrationSource>, StorageError> {
        self.query(
            "SELECT source_base_url, secret_ref, source_label, history_capability
             FROM emby_migration_sources WHERE source_base_url = ?",
        )
        .bind(source_base_url)
        .fetch_optional(self.pool())
        .await
        .map(|row| {
            row.map(|row| StoredEmbyMigrationSource {
                source_base_url: row.get("source_base_url"),
                secret_ref: row.get("secret_ref"),
                source_label: row.get("source_label"),
                history_capability: row.get("history_capability"),
            })
        })
        .map_err(storage_error)
    }

    pub(crate) async fn upsert_emby_migration_user_binding(
        &self,
        binding: &StoredEmbyMigrationUserBinding,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO emby_migration_user_bindings (
                 lux_user_id, source_base_url, secret_ref, emby_user_id,
                 emby_username, password_pending
             ) VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(lux_user_id) DO UPDATE SET
                 source_base_url = excluded.source_base_url,
                 secret_ref = excluded.secret_ref,
                 emby_user_id = excluded.emby_user_id,
                 emby_username = excluded.emby_username,
                 password_pending = excluded.password_pending,
                 updated_at = unixepoch()",
        )
        .bind(&binding.lux_user_id)
        .bind(&binding.source_base_url)
        .bind(&binding.secret_ref)
        .bind(&binding.emby_user_id)
        .bind(&binding.emby_username)
        .bind(if binding.password_pending {
            1_i64
        } else {
            0_i64
        })
        .execute(self.pool())
        .await
        .map(|_| ())
        .map_err(storage_error)
    }

    pub(crate) async fn find_emby_migration_user_binding_by_username(
        &self,
        username: &str,
    ) -> Result<Option<StoredEmbyMigrationUserBinding>, StorageError> {
        self.query(
            "SELECT lux_user_id, source_base_url, secret_ref, emby_user_id,
                    emby_username, password_pending
             FROM emby_migration_user_bindings
             WHERE LOWER(emby_username) = LOWER(?) AND password_pending = 1
             LIMIT 1",
        )
        .bind(username)
        .fetch_optional(self.pool())
        .await
        .map(|row| {
            row.map(|row| StoredEmbyMigrationUserBinding {
                lux_user_id: row.get("lux_user_id"),
                source_base_url: row.get("source_base_url"),
                secret_ref: row.get("secret_ref"),
                emby_user_id: row.get("emby_user_id"),
                emby_username: row.get("emby_username"),
                password_pending: row.get::<i64, _>("password_pending") != 0,
            })
        })
        .map_err(storage_error)
    }

    pub(crate) async fn mark_emby_migration_password_ready(
        &self,
        lux_user_id: &str,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE emby_migration_user_bindings
             SET password_pending = 0, updated_at = unixepoch()
             WHERE lux_user_id = ? AND password_pending = 1",
        )
        .bind(lux_user_id)
        .execute(self.pool())
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(storage_error)
    }

    pub(crate) async fn insert_emby_migration_job(
        &self,
        id: &str,
        created_by_user_id: &str,
        source_label: &str,
        source_base_url: &str,
        secret_ref: &str,
        dry_run: bool,
        merge_policy: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO emby_migration_jobs (
                 id, plugin_id, created_by_user_id, source_label, source_base_url,
                 secret_ref, status, phase, dry_run, merge_policy
             ) VALUES (?, 'org.lux.emby-migration', ?, ?, ?, ?, 'PENDING', 'TESTING', ?, ?)",
        )
        .bind(id)
        .bind(created_by_user_id)
        .bind(source_label)
        .bind(source_base_url)
        .bind(secret_ref)
        .bind(if dry_run { 1_i64 } else { 0_i64 })
        .bind(merge_policy)
        .execute(self.pool())
        .await
        .map(|_| ())
        .map_err(storage_error)
    }

    pub(crate) async fn find_emby_migration_job(
        &self,
        id: &str,
    ) -> Result<Option<StoredEmbyMigrationJob>, StorageError> {
        self.query(
            "SELECT id, plugin_id, created_by_user_id, source_label, source_base_url,
                    secret_ref, status, phase, dry_run, merge_policy, cursor_json,
                    processed_count, total_count, matched_count, skipped_count, failed_count,
                    cancel_requested, error, history_capability
             FROM emby_migration_jobs WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await
        .map(|row| row.map(stored_migration_job))
        .map_err(storage_error)
    }

    pub(crate) async fn list_emby_migration_jobs(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredEmbyMigrationJob>, StorageError> {
        self.query(
            "SELECT id, plugin_id, created_by_user_id, source_label, source_base_url,
                    secret_ref, status, phase, dry_run, merge_policy, cursor_json,
                    processed_count, total_count, matched_count, skipped_count, failed_count,
                    cancel_requested, error, history_capability
             FROM emby_migration_jobs
             ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await
        .map(|rows| rows.into_iter().map(stored_migration_job).collect())
        .map_err(storage_error)
    }

    pub(crate) async fn update_emby_migration_job_status(
        &self,
        id: &str,
        status: &str,
        phase: &str,
        error: Option<&str>,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE emby_migration_jobs
             SET status = ?, phase = ?, error = ?, updated_at = unixepoch(),
                 started_at = CASE WHEN ? = 'RUNNING' AND started_at IS NULL THEN unixepoch() ELSE started_at END,
                 finished_at = CASE WHEN ? IN ('COMPLETED', 'CANCELLED', 'FAILED') THEN unixepoch() ELSE finished_at END
             WHERE id = ?",
        )
        .bind(status)
        .bind(phase)
        .bind(error)
        .bind(status)
        .bind(status)
        .bind(id)
        .execute(self.pool())
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(storage_error)
    }

    pub(crate) async fn update_emby_migration_job_history_capability(
        &self,
        id: &str,
        history_capability: &str,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE emby_migration_jobs
             SET history_capability = ?, updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(history_capability)
        .bind(id)
        .execute(self.pool())
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(storage_error)
    }

    pub(crate) async fn update_emby_migration_job_progress(
        &self,
        id: &str,
        cursor_json: &str,
        processed_count: i64,
        total_count: i64,
        matched_count: i64,
        skipped_count: i64,
        failed_count: i64,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE emby_migration_jobs
             SET cursor_json = ?, processed_count = ?, total_count = ?, matched_count = ?,
                 skipped_count = ?, failed_count = ?, updated_at = unixepoch()
             WHERE id = ? AND status IN ('PENDING', 'RUNNING')",
        )
        .bind(cursor_json)
        .bind(processed_count)
        .bind(total_count)
        .bind(matched_count)
        .bind(skipped_count)
        .bind(failed_count)
        .bind(id)
        .execute(self.pool())
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(storage_error)
    }

    pub(crate) async fn request_emby_migration_cancel(
        &self,
        id: &str,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE emby_migration_jobs
             SET cancel_requested = 1, updated_at = unixepoch()
             WHERE id = ? AND status IN ('PENDING', 'RUNNING')",
        )
        .bind(id)
        .execute(self.pool())
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(storage_error)
    }

    pub(crate) async fn upsert_emby_migration_user_link(
        &self,
        link: &StoredEmbyMigrationUserLink,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO emby_migration_user_links (
                 job_id, emby_user_id, emby_username, lux_user_id, status, error
             ) VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(job_id, emby_user_id) DO UPDATE SET
                 emby_username = excluded.emby_username,
                 lux_user_id = excluded.lux_user_id,
                 status = excluded.status,
                 error = excluded.error,
                 updated_at = unixepoch()",
        )
        .bind(&link.job_id)
        .bind(&link.emby_user_id)
        .bind(&link.emby_username)
        .bind(&link.lux_user_id)
        .bind(&link.status)
        .bind(&link.error)
        .execute(self.pool())
        .await
        .map(|_| ())
        .map_err(storage_error)
    }

    pub(crate) async fn list_emby_migration_user_links(
        &self,
        job_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredEmbyMigrationUserLink>, StorageError> {
        self.query(
            "SELECT job_id, emby_user_id, emby_username, lux_user_id, status, error
             FROM emby_migration_user_links
             WHERE job_id = ?
             ORDER BY emby_username, emby_user_id
             LIMIT ? OFFSET ?",
        )
        .bind(job_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredEmbyMigrationUserLink {
                    job_id: row.get("job_id"),
                    emby_user_id: row.get("emby_user_id"),
                    emby_username: row.get("emby_username"),
                    lux_user_id: row.get("lux_user_id"),
                    status: row.get("status"),
                    error: row.get("error"),
                })
                .collect()
        })
        .map_err(storage_error)
    }

    pub(crate) async fn list_migration_media_identities(
        &self,
        after_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<StoredMigrationMediaIdentity>, StorageError> {
        let mut query = self.query(
            "SELECT id, item_type, title, production_year, provider_ids_json,
                    series_id, season_number, episode_number
             FROM media_items
             WHERE removed_at IS NULL AND id > ?
             ORDER BY id LIMIT ?",
        );
        query = query.bind(after_id.unwrap_or_default()).bind(limit);
        query
            .fetch_all(self.pool())
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(|row| StoredMigrationMediaIdentity {
                        id: row.get("id"),
                        item_type: row.get("item_type"),
                        title: row.get("title"),
                        production_year: row.get("production_year"),
                        provider_ids_json: row.get("provider_ids_json"),
                        series_id: row.get("series_id"),
                        season_number: row.get("season_number"),
                        episode_number: row.get("episode_number"),
                    })
                    .collect()
            })
            .map_err(storage_error)
    }

    pub(crate) async fn upsert_emby_migration_item_match(
        &self,
        job_id: &str,
        emby_item_id: &str,
        emby_item_type: &str,
        lux_item_id: Option<&str>,
        match_method: &str,
        confidence: Option<i64>,
        status: &str,
        detail_json: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO emby_migration_item_matches (
                 job_id, emby_item_id, emby_item_type, lux_item_id, match_method,
                 confidence, status, detail_json
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(job_id, emby_item_id) DO UPDATE SET
                 emby_item_type = excluded.emby_item_type,
                 lux_item_id = excluded.lux_item_id,
                 match_method = excluded.match_method,
                 confidence = excluded.confidence,
                 status = excluded.status,
                 detail_json = excluded.detail_json,
                 updated_at = unixepoch()",
        )
        .bind(job_id)
        .bind(emby_item_id)
        .bind(emby_item_type)
        .bind(lux_item_id)
        .bind(match_method)
        .bind(confidence)
        .bind(status)
        .bind(detail_json)
        .execute(self.pool())
        .await
        .map(|_| ())
        .map_err(storage_error)
    }

    pub(crate) async fn list_emby_migration_item_matches(
        &self,
        job_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredEmbyMigrationItemMatch>, StorageError> {
        self.query(
            "SELECT job_id, emby_item_id, emby_item_type, lux_item_id,
                    match_method, confidence, status, detail_json
             FROM emby_migration_item_matches
             WHERE job_id = ?
             ORDER BY emby_item_id
             LIMIT ? OFFSET ?",
        )
        .bind(job_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredEmbyMigrationItemMatch {
                    job_id: row.get("job_id"),
                    emby_item_id: row.get("emby_item_id"),
                    emby_item_type: row.get("emby_item_type"),
                    lux_item_id: row.get("lux_item_id"),
                    match_method: row.get("match_method"),
                    confidence: row.get("confidence"),
                    status: row.get("status"),
                    detail_json: row.get("detail_json"),
                })
                .collect()
        })
        .map_err(storage_error)
    }

    pub(crate) async fn upsert_emby_migration_import_record(
        &self,
        job_id: &str,
        emby_user_id: &str,
        emby_item_id: &str,
        lux_user_id: &str,
        lux_item_id: &str,
        state_hash: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO emby_migration_import_records (
                 job_id, emby_user_id, emby_item_id, lux_user_id, lux_item_id,
                 state_hash, status, error
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(job_id, emby_user_id, emby_item_id) DO UPDATE SET
                 lux_user_id = excluded.lux_user_id,
                 lux_item_id = excluded.lux_item_id,
                 state_hash = excluded.state_hash,
                 status = excluded.status,
                 error = excluded.error,
                 imported_at = unixepoch()",
        )
        .bind(job_id)
        .bind(emby_user_id)
        .bind(emby_item_id)
        .bind(lux_user_id)
        .bind(lux_item_id)
        .bind(state_hash)
        .bind(status)
        .bind(error)
        .execute(self.pool())
        .await
        .map(|_| ())
        .map_err(storage_error)
    }

    pub(crate) async fn list_emby_migration_import_records(
        &self,
        job_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredEmbyMigrationImportRecord>, StorageError> {
        self.query(
            "SELECT job_id, emby_user_id, emby_item_id, lux_user_id, lux_item_id,
                    state_hash, status, error
             FROM emby_migration_import_records
             WHERE job_id = ?
             ORDER BY emby_user_id, emby_item_id
             LIMIT ? OFFSET ?",
        )
        .bind(job_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredEmbyMigrationImportRecord {
                    job_id: row.get("job_id"),
                    emby_user_id: row.get("emby_user_id"),
                    emby_item_id: row.get("emby_item_id"),
                    lux_user_id: row.get("lux_user_id"),
                    lux_item_id: row.get("lux_item_id"),
                    state_hash: row.get("state_hash"),
                    status: row.get("status"),
                    error: row.get("error"),
                })
                .collect()
        })
        .map_err(storage_error)
    }

    pub(crate) async fn find_user_item_state_for_migration(
        &self,
        user_id: &str,
        item_id: &str,
    ) -> Result<Option<StoredUserItemState>, StorageError> {
        self.query(
            "SELECT position_ticks, is_played, is_favorite, play_count,
                    last_played_at, version
             FROM user_item_state WHERE user_id = ? AND item_id = ?",
        )
        .bind(user_id)
        .bind(item_id)
        .fetch_optional(self.pool())
        .await
        .map(|row| {
            row.map(|row| StoredUserItemState {
                position_ticks: row.get("position_ticks"),
                is_played: row.get::<i64, _>("is_played") != 0,
                is_favorite: row.get::<i64, _>("is_favorite") != 0,
                play_count: row.get("play_count"),
                last_played_at: row.get("last_played_at"),
                version: row.get("version"),
            })
        })
        .map_err(storage_error)
    }

    pub(crate) async fn upsert_imported_user_item_state(
        &self,
        user_id: &str,
        item_id: &str,
        position_ticks: i64,
        is_played: bool,
        is_favorite: bool,
        play_count: i64,
        last_played_at: Option<i64>,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO user_item_state (
                 user_id, item_id, position_ticks, is_played, is_favorite,
                 play_count, last_played_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(user_id, item_id) DO UPDATE SET
                 position_ticks = excluded.position_ticks,
                 is_played = excluded.is_played,
                 is_favorite = excluded.is_favorite,
                 play_count = excluded.play_count,
                 last_played_at = excluded.last_played_at,
                 version = user_item_state.version + CASE
                     WHEN position_ticks != excluded.position_ticks
                       OR is_played != excluded.is_played
                       OR is_favorite != excluded.is_favorite
                       OR play_count != excluded.play_count
                       OR COALESCE(last_played_at, -1) != COALESCE(excluded.last_played_at, -1)
                     THEN 1 ELSE 0 END",
        )
        .bind(user_id)
        .bind(item_id)
        .bind(position_ticks)
        .bind(if is_played { 1_i64 } else { 0_i64 })
        .bind(if is_favorite { 1_i64 } else { 0_i64 })
        .bind(play_count)
        .bind(last_played_at)
        .execute(self.pool())
        .await
        .map(|_| ())
        .map_err(storage_error)
    }

    // Reserved for a future EVENT_HISTORY-capable source plugin. ITEM_STATE imports
    // intentionally never synthesize rows in this table.
    #[allow(dead_code)]
    pub(crate) async fn insert_playback_history_event(
        &self,
        event: &StoredPlaybackHistoryEvent,
    ) -> Result<bool, StorageError> {
        self.query(
            "INSERT INTO playback_history_events (
                 id, user_id, item_id, event_type, position_ticks, duration_ticks,
                 occurred_at, source, source_event_key
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(source, source_event_key) DO NOTHING",
        )
        .bind(&event.id)
        .bind(&event.user_id)
        .bind(&event.item_id)
        .bind(&event.event_type)
        .bind(event.position_ticks)
        .bind(event.duration_ticks)
        .bind(event.occurred_at)
        .bind(&event.source)
        .bind(&event.source_event_key)
        .execute(self.pool())
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(storage_error)
    }

    pub(crate) async fn list_playback_history_events(
        &self,
        user_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredPlaybackHistoryEvent>, StorageError> {
        self.query(
            "SELECT id, user_id, item_id, event_type, position_ticks, duration_ticks,
                    occurred_at, source, source_event_key
             FROM playback_history_events
             WHERE user_id = ?
             ORDER BY occurred_at DESC, id DESC
             LIMIT ? OFFSET ?",
        )
        .bind(user_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredPlaybackHistoryEvent {
                    id: row.get("id"),
                    user_id: row.get("user_id"),
                    item_id: row.get("item_id"),
                    event_type: row.get("event_type"),
                    position_ticks: row.get("position_ticks"),
                    duration_ticks: row.get("duration_ticks"),
                    occurred_at: row.get("occurred_at"),
                    source: row.get("source"),
                    source_event_key: row.get("source_event_key"),
                })
                .collect()
        })
        .map_err(storage_error)
    }

    pub(crate) async fn count_emby_migration_jobs(&self) -> Result<i64, StorageError> {
        self.query_scalar("SELECT COUNT(*) FROM emby_migration_jobs")
            .fetch_one(self.pool())
            .await
            .map_err(storage_error)
    }
}

fn stored_migration_job(row: sqlx::any::AnyRow) -> StoredEmbyMigrationJob {
    StoredEmbyMigrationJob {
        id: row.get("id"),
        plugin_id: row.get("plugin_id"),
        created_by_user_id: row.get("created_by_user_id"),
        source_label: row.get("source_label"),
        source_base_url: row.get("source_base_url"),
        secret_ref: row.get("secret_ref"),
        status: row.get("status"),
        phase: row.get("phase"),
        dry_run: row.get::<i64, _>("dry_run") != 0,
        merge_policy: row.get("merge_policy"),
        history_capability: row.get("history_capability"),
        cursor_json: row.get("cursor_json"),
        processed_count: row.get("processed_count"),
        total_count: row.get("total_count"),
        matched_count: row.get("matched_count"),
        skipped_count: row.get("skipped_count"),
        failed_count: row.get("failed_count"),
        cancel_requested: row.get::<i64, _>("cancel_requested") != 0,
        error: row.get("error"),
    }
}

fn storage_error(source: sqlx::Error) -> StorageError {
    StorageError::Sqlx {
        path: PathBuf::from("database"),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Config, storage::Database};
    use uuid::Uuid;

    async fn test_database() -> Result<(tempfile::TempDir, Database), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        })
        .await?;
        Ok((temp_dir, database))
    }

    async fn insert_test_user_and_item(
        database: &Database,
    ) -> Result<(String, String), Box<dyn std::error::Error>> {
        let user_id = Uuid::now_v7().to_string();
        let item_id = Uuid::now_v7().to_string();
        let library_id = Uuid::now_v7().to_string();
        database
            .insert_initial_user(&user_id, "migration-admin", "Migration Admin", "hash")
            .await?;
        sqlx::query("INSERT INTO libraries (id, name, kind) VALUES (?, ?, 'MOVIE')")
            .bind(&library_id)
            .bind("Migration Test")
            .execute(database.pool())
            .await?;
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(&item_id)
        .bind(&library_id)
        .bind("Migration Item")
        .bind("migration item")
        .execute(database.pool())
        .await?;
        Ok((user_id, item_id))
    }

    #[tokio::test]
    async fn imported_state_and_history_are_idempotent() -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let (user_id, item_id) = insert_test_user_and_item(&database).await?;

        database
            .upsert_imported_user_item_state(&user_id, &item_id, 120, true, true, 3, Some(200))
            .await?;
        let state = database
            .find_user_item_state_for_migration(&user_id, &item_id)
            .await?
            .expect("imported state should be stored");
        assert_eq!(state.position_ticks, 120);
        assert!(state.is_played);
        assert!(state.is_favorite);
        assert_eq!(state.play_count, 3);
        assert_eq!(state.last_played_at, Some(200));

        let event = StoredPlaybackHistoryEvent {
            id: Uuid::now_v7().to_string(),
            user_id: user_id.clone(),
            item_id: item_id.clone(),
            event_type: "PLAY_PROGRESS".to_owned(),
            position_ticks: 120,
            duration_ticks: Some(1_000),
            occurred_at: 200,
            source: "emby:test-server".to_owned(),
            source_event_key: "event-1".to_owned(),
        };
        assert!(database.insert_playback_history_event(&event).await?);
        assert!(!database.insert_playback_history_event(&event).await?);

        let event_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM playback_history_events")
            .fetch_one(database.pool())
            .await?;
        assert_eq!(event_count, 1);
        Ok(())
    }

    #[tokio::test]
    async fn migration_job_progress_and_cancellation_are_persisted()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let (user_id, _item_id) = insert_test_user_and_item(&database).await?;
        let job_id = Uuid::now_v7().to_string();

        database
            .insert_emby_migration_job(
                &job_id,
                &user_id,
                "Test Emby",
                "https://emby.example.test/",
                "emby-migration/test",
                true,
                "MERGE",
            )
            .await?;
        assert!(
            database
                .update_emby_migration_job_status(&job_id, "RUNNING", "ITEMS", None)
                .await?
        );
        assert!(
            database
                .update_emby_migration_job_history_capability(&job_id, "EVENT_HISTORY")
                .await?
        );
        assert!(
            database
                .update_emby_migration_job_progress(&job_id, r#"{"page":1}"#, 5, 10, 4, 1, 0)
                .await?
        );
        assert!(database.request_emby_migration_cancel(&job_id).await?);

        let job = database
            .find_emby_migration_job(&job_id)
            .await?
            .expect("migration job should be stored");
        assert_eq!(job.status, "RUNNING");
        assert_eq!(job.phase, "ITEMS");
        assert_eq!(job.history_capability, "EVENT_HISTORY");
        assert_eq!(job.processed_count, 5);
        assert_eq!(job.total_count, 10);
        assert_eq!(job.matched_count, 4);
        assert_eq!(job.skipped_count, 1);
        assert!(job.cancel_requested);

        database
            .upsert_emby_migration_source(&StoredEmbyMigrationSource {
                source_base_url: "https://emby.example.test/".to_owned(),
                secret_ref: "emby-migration/test.json".to_owned(),
                source_label: "emby.example.test".to_owned(),
                history_capability: "ITEM_STATE".to_owned(),
            })
            .await?;
        database
            .upsert_emby_migration_user_binding(&StoredEmbyMigrationUserBinding {
                lux_user_id: user_id.clone(),
                source_base_url: "https://emby.example.test/".to_owned(),
                secret_ref: Some("emby-migration/test.json".to_owned()),
                emby_user_id: "emby-user".to_owned(),
                emby_username: "Alice".to_owned(),
                password_pending: true,
            })
            .await?;
        let binding = database
            .find_emby_migration_user_binding_by_username("alice")
            .await?
            .expect("binding lookup should be case insensitive");
        assert_eq!(binding.emby_user_id, "emby-user");
        assert!(
            database
                .mark_emby_migration_password_ready(&user_id)
                .await?
        );
        assert!(
            database
                .find_emby_migration_user_binding_by_username("alice")
                .await?
                .is_none()
        );
        Ok(())
    }
}
