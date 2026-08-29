use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

#[cfg(test)]
use super::StoredUserItemState;
use super::{Database, StorageError};
use sqlx::Row;

const EMBY_MIGRATION_WRITE_BATCH_SIZE: usize = 100;

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
    pub scope_json: String,
    pub emby_user_ids_json: String,
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

pub(crate) struct NewEmbyMigrationJob<'a> {
    pub id: &'a str,
    pub created_by_user_id: &'a str,
    pub source_label: &'a str,
    pub source_base_url: &'a str,
    pub secret_ref: &'a str,
    pub dry_run: bool,
    pub merge_policy: &'a str,
    pub scope_json: &'a str,
    pub emby_user_ids_json: &'a str,
}

pub(crate) struct EmbyMigrationJobProgress<'a> {
    pub id: &'a str,
    pub cursor_json: &'a str,
    pub processed_count: i64,
    pub total_count: i64,
    pub matched_count: i64,
    pub skipped_count: i64,
    pub failed_count: i64,
}

/// The complete, atomically committed result of one bounded Emby item-state page.
///
/// Keeping every page output in this one value makes it impossible for callers to advance the
/// durable cursor without also supplying the reports, state writes, import records and
/// deduplication markers that describe that source page.
pub(crate) struct EmbyMigrationItemPageBatch<'a> {
    pub job_id: &'a str,
    pub merge_policy: &'a str,
    pub state_fields: EmbyMigrationUserItemStateFields,
    pub item_matches: &'a [EmbyMigrationItemMatchBatch],
    pub states: &'a [EmbyMigrationUserItemStateBatch],
    pub import_records: &'a [EmbyMigrationImportRecordBatch],
    pub handled_items: &'a [EmbyMigrationHandledItemBatch],
    pub progress: EmbyMigrationJobProgress<'a>,
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
pub(crate) struct StoredEmbyMigrationPersonFavorite {
    pub job_id: String,
    pub emby_user_id: String,
    pub emby_person_id: String,
    pub emby_person_name: String,
    pub lux_user_id: Option<String>,
    pub lux_person_id: Option<String>,
    pub provider_ids_json: String,
    pub match_method: String,
    pub confidence: Option<i64>,
    pub status: String,
    pub state_hash: String,
    pub detail_json: String,
    pub error: Option<String>,
}

#[cfg(test)]
pub(crate) struct NewEmbyMigrationPersonFavorite<'a> {
    pub job_id: &'a str,
    pub emby_user_id: &'a str,
    pub emby_person_id: &'a str,
    pub emby_person_name: &'a str,
    pub lux_user_id: Option<&'a str>,
    pub lux_person_id: Option<&'a str>,
    pub provider_ids_json: &'a str,
    pub match_method: &'a str,
    pub confidence: Option<i64>,
    pub status: &'a str,
    pub state_hash: &'a str,
    pub detail_json: &'a str,
    pub error: Option<&'a str>,
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

#[cfg(test)]
pub(crate) struct NewImportedUserItemState<'a> {
    pub user_id: &'a str,
    pub item_id: &'a str,
    pub position_ticks: i64,
    pub is_played: bool,
    pub is_favorite: bool,
    pub play_count: i64,
    pub last_played_at: Option<i64>,
}

#[derive(Clone, Debug)]
pub(crate) struct EmbyMigrationItemMatchBatch {
    pub emby_item_id: String,
    pub emby_item_type: String,
    pub lux_item_id: Option<String>,
    pub match_method: String,
    pub confidence: Option<i64>,
    pub status: String,
    pub detail_json: String,
}

#[derive(Clone, Debug)]
pub(crate) struct EmbyMigrationUserItemStateBatch {
    pub user_id: String,
    pub item_id: String,
    pub position_ticks: i64,
    pub is_played: bool,
    pub is_favorite: bool,
    pub play_count: i64,
    pub last_played_at: Option<i64>,
}

/// Identifies the user-item state columns that a migration scope may change.
///
/// An Emby response contains a complete `UserData` object even when the job
/// requested one state category. The mask prevents non-selected values from
/// changing an existing Lux state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct EmbyMigrationUserItemStateFields {
    pub position_ticks: bool,
    pub is_played: bool,
    pub is_favorite: bool,
    pub play_count: bool,
    pub last_played_at: bool,
}

impl EmbyMigrationUserItemStateFields {
    #[cfg(test)]
    pub const fn all() -> Self {
        Self {
            position_ticks: true,
            is_played: true,
            is_favorite: true,
            play_count: true,
            last_played_at: true,
        }
    }

    #[cfg(test)]
    pub const fn favorite_only() -> Self {
        Self {
            position_ticks: false,
            is_played: false,
            is_favorite: true,
            play_count: false,
            last_played_at: false,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct EmbyMigrationImportRecordBatch {
    pub emby_user_id: String,
    pub emby_item_id: String,
    pub lux_user_id: String,
    pub lux_item_id: String,
    pub state_hash: String,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct EmbyMigrationHandledItemBatch {
    pub emby_user_id: String,
    pub emby_item_id: String,
}

#[derive(Clone, Debug)]
pub(crate) struct EmbyMigrationPersonFavoriteBatch {
    pub emby_user_id: String,
    pub emby_person_id: String,
    pub emby_person_name: String,
    pub lux_user_id: Option<String>,
    pub lux_person_id: Option<String>,
    pub provider_ids_json: String,
    pub match_method: String,
    pub confidence: Option<i64>,
    pub status: String,
    pub state_hash: String,
    pub detail_json: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct EmbyMigrationPersonFavoriteStateBatch {
    pub user_id: String,
    pub person_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredMigrationMediaIdentity {
    pub id: String,
    pub library_id: String,
    pub item_type: String,
    pub title: String,
    pub production_year: Option<i64>,
    pub provider_ids_json: Option<String>,
    pub series_id: Option<String>,
    pub season_number: Option<i64>,
    pub episode_number: Option<i64>,
}

/// Exact lookup keys extracted from one bounded Emby page.  Keeping the query
/// shape in storage avoids leaking application DTOs across the persistence
/// boundary while allowing the provider index to remain parameterized.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct MigrationMediaIdentityLookup {
    pub item_type: String,
    pub title: String,
    pub title_pattern: String,
    pub production_year: Option<i64>,
    pub season_number: Option<i64>,
    pub episode_number: Option<i64>,
    pub provider_ids: Vec<(String, String)>,
}

/// Exact person keys extracted from one bounded Emby favorites page. Provider
/// IDs and normalized names are resolved in batches so the migration runner
/// never needs to materialize the complete people identity table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MigrationPersonIdentityLookup {
    pub normalized_name: String,
    pub provider_ids: Vec<(String, String)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredMigrationPersonIdentity {
    pub id: String,
    pub display_name: String,
    pub provider: Option<String>,
    pub provider_id: Option<String>,
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
        job: &NewEmbyMigrationJob<'_>,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO emby_migration_jobs (
                 id, plugin_id, created_by_user_id, source_label, source_base_url,
                 secret_ref, status, phase, dry_run, merge_policy, scope_json, emby_user_ids_json
             ) VALUES (?, 'org.lux.emby-migration', ?, ?, ?, ?, 'PENDING', 'TESTING', ?, ?, ?, ?)",
        )
        .bind(job.id)
        .bind(job.created_by_user_id)
        .bind(job.source_label)
        .bind(job.source_base_url)
        .bind(job.secret_ref)
        .bind(if job.dry_run { 1_i64 } else { 0_i64 })
        .bind(job.merge_policy)
        .bind(job.scope_json)
        .bind(job.emby_user_ids_json)
        .execute(self.pool())
        .await
        .map(|_| ())
        .map_err(storage_error)
    }

    pub(crate) async fn has_active_emby_migration_job(&self) -> Result<bool, StorageError> {
        self.query_scalar::<i64>(
            "SELECT COUNT(*) FROM emby_migration_jobs
             WHERE status IN ('PENDING', 'RUNNING')",
        )
        .fetch_one(self.pool())
        .await
        .map(|count| count > 0)
        .map_err(storage_error)
    }

    pub(crate) async fn find_emby_migration_job(
        &self,
        id: &str,
    ) -> Result<Option<StoredEmbyMigrationJob>, StorageError> {
        self.query(
            "SELECT id, plugin_id, created_by_user_id, source_label, source_base_url,
                    secret_ref, status, phase, dry_run, merge_policy, scope_json, cursor_json,
                    emby_user_ids_json,
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
                    secret_ref, status, phase, dry_run, merge_policy, scope_json, cursor_json,
                    emby_user_ids_json,
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
        progress: &EmbyMigrationJobProgress<'_>,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE emby_migration_jobs
             SET cursor_json = ?, processed_count = ?, total_count = ?, matched_count = ?,
                 skipped_count = ?, failed_count = ?, updated_at = unixepoch()
             WHERE id = ? AND status IN ('PENDING', 'RUNNING')",
        )
        .bind(progress.cursor_json)
        .bind(progress.processed_count)
        .bind(progress.total_count)
        .bind(progress.matched_count)
        .bind(progress.skipped_count)
        .bind(progress.failed_count)
        .bind(progress.id)
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

    pub(crate) async fn emby_migration_cancel_requested(
        &self,
        id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar("SELECT cancel_requested FROM emby_migration_jobs WHERE id = ?")
            .bind(id)
            .fetch_optional(self.pool())
            .await
            .map(|value: Option<i64>| value.unwrap_or_default() != 0)
            .map_err(storage_error)
    }

    #[allow(dead_code)]
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

    /// Upsert a bounded batch of user-link reports in one transaction per
    /// 100 rows. User creation and profile synchronization remain outside this
    /// method because they have per-user password and administrator invariants.
    pub(crate) async fn upsert_emby_migration_user_links_batch(
        &self,
        links: &[StoredEmbyMigrationUserLink],
    ) -> Result<(), StorageError> {
        for links in links.chunks(EMBY_MIGRATION_WRITE_BATCH_SIZE) {
            let mut transaction = self.begin_metadata_write_transaction().await?;
            let mut sql = String::from(
                "INSERT INTO emby_migration_user_links (
                 job_id, emby_user_id, emby_username, lux_user_id, status, error
             ) VALUES ",
            );
            for index in 0..links.len() {
                if index > 0 {
                    sql.push(',');
                }
                sql.push_str(" (?, ?, ?, ?, ?, ?)");
            }
            let lux_user_id_changed = sql_is_distinct(
                "emby_migration_user_links.lux_user_id",
                "excluded.lux_user_id",
            );
            let error_changed =
                sql_is_distinct("emby_migration_user_links.error", "excluded.error");
            sql.push_str(&format!(
                " ON CONFLICT(job_id, emby_user_id) DO UPDATE SET
                     emby_username = excluded.emby_username,
                     lux_user_id = excluded.lux_user_id,
                     status = excluded.status,
                     error = excluded.error,
                     updated_at = unixepoch()
                 WHERE emby_migration_user_links.emby_username <> excluded.emby_username
                    OR {lux_user_id_changed}
                    OR emby_migration_user_links.status <> excluded.status
                    OR {error_changed}"
            ));
            let mut query = self.query(sqlx::AssertSqlSafe(sql));
            for link in links {
                query = query
                    .bind(&link.job_id)
                    .bind(&link.emby_user_id)
                    .bind(&link.emby_username)
                    .bind(&link.lux_user_id)
                    .bind(&link.status)
                    .bind(&link.error);
            }
            query
                .execute(&mut *transaction)
                .await
                .map_err(storage_error)?;
            transaction.commit().await.map_err(storage_error)?;
        }
        Ok(())
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

    /// Resolve only candidates referenced by the current source page. Provider
    /// IDs use the normalized projection table and title/year lookups remain a
    /// bounded fallback for older records without provider IDs.
    pub(crate) async fn list_migration_media_identity_candidates(
        &self,
        lookups: &[MigrationMediaIdentityLookup],
    ) -> Result<Vec<StoredMigrationMediaIdentity>, StorageError> {
        self.list_migration_media_identity_candidates_filtered(lookups, None)
            .await
    }

    /// Resolve page candidates while optionally restricting reads to the
    /// administrator-selected Lux libraries.  The unfiltered wrapper above
    /// preserves the legacy storage API used by older callers and tests.
    pub(crate) async fn list_migration_media_identity_candidates_filtered(
        &self,
        lookups: &[MigrationMediaIdentityLookup],
        selected_library_ids: Option<&[String]>,
    ) -> Result<Vec<StoredMigrationMediaIdentity>, StorageError> {
        if lookups.is_empty() {
            return Ok(Vec::new());
        }
        if selected_library_ids.is_some_and(<[String]>::is_empty) {
            return Ok(Vec::new());
        }
        let mut identities = Vec::new();
        let mut seen_ids = HashSet::new();
        let mut provider_candidates_by_key: HashMap<(String, String, String), HashSet<String>> =
            HashMap::new();
        let mut provider_keys = lookups
            .iter()
            .flat_map(|lookup| {
                lookup.provider_ids.iter().map(|(provider, provider_id)| {
                    (
                        lookup.item_type.clone(),
                        provider.clone(),
                        provider_id.clone(),
                    )
                })
            })
            .collect::<Vec<_>>();
        provider_keys.sort_unstable();
        provider_keys.dedup();

        for chunk in provider_keys.chunks(200) {
            if chunk.is_empty() {
                continue;
            }
            for library_chunk in migration_library_id_chunks(selected_library_ids) {
                let predicates = std::iter::repeat_n(
                    "(provider.item_type = ? AND provider.provider = ? AND provider.provider_id = ?)",
                    chunk.len(),
                )
                .collect::<Vec<_>>()
                .join(" OR ");
                let sql = format!(
                    "SELECT DISTINCT mi.id, mi.item_type, mi.title, mi.production_year,
                            mi.provider_ids_json, mi.series_id, mi.season_number,
                            mi.episode_number, mi.library_id,
                            provider.provider, provider.provider_id
                     FROM media_items AS mi
                     JOIN media_item_provider_ids AS provider
                       ON provider.media_item_id = mi.id
                     WHERE mi.removed_at IS NULL AND ({predicates}){}",
                    migration_library_filter_sql(library_chunk),
                );
                let mut query = self.query(sqlx::AssertSqlSafe(sql));
                for (item_type, provider, provider_id) in chunk {
                    query = query.bind(item_type).bind(provider).bind(provider_id);
                }
                if let Some(library_ids) = library_chunk {
                    for library_id in library_ids {
                        query = query.bind(library_id);
                    }
                }
                for row in query.fetch_all(self.pool()).await.map_err(storage_error)? {
                    let id: String = row.get("id");
                    let provider_key = (
                        row.get("item_type"),
                        row.get("provider"),
                        row.get("provider_id"),
                    );
                    provider_candidates_by_key
                        .entry(provider_key)
                        .or_default()
                        .insert(id.clone());
                    if seen_ids.insert(id.clone()) {
                        identities.push(StoredMigrationMediaIdentity {
                            id,
                            library_id: row.get("library_id"),
                            item_type: row.get("item_type"),
                            title: row.get("title"),
                            production_year: row.get("production_year"),
                            provider_ids_json: row.get("provider_ids_json"),
                            series_id: row.get("series_id"),
                            season_number: row.get("season_number"),
                            episode_number: row.get("episode_number"),
                        });
                    }
                }
            }
        }

        // Title-only matching is intentionally constrained by item type and a
        // one-year production window. The application applies its stricter
        // Unicode-normalized title and episode-key checks afterwards.
        let title_lookups = lookups
            .iter()
            .filter(|lookup| {
                // A punctuation-only title normalizes to an empty key and its
                // generated pattern is "%". Never issue that unbounded
                // fallback query; the application can only report it as
                // unmatched when no Provider ID resolved.
                if !lookup.title.chars().any(char::is_alphanumeric) {
                    return false;
                }
                let provider_candidates = lookup
                    .provider_ids
                    .iter()
                    .flat_map(|(provider, provider_id)| {
                        provider_candidates_by_key
                            .get(&(
                                lookup.item_type.clone(),
                                provider.clone(),
                                provider_id.clone(),
                            ))
                            .into_iter()
                            .flatten()
                    })
                    .collect::<HashSet<_>>();
                provider_candidates.is_empty()
            })
            .collect::<Vec<_>>();
        for chunk in title_lookups.chunks(100) {
            for library_chunk in migration_library_id_chunks(selected_library_ids) {
                let predicates = std::iter::repeat_n(
                    "(mi.item_type = ? AND lower(mi.title) LIKE lower(?) AND
                      (? IS NULL OR mi.production_year IS NULL OR abs(mi.production_year - ?) <= 1))",
                    chunk.len(),
                )
                .collect::<Vec<_>>()
                .join(" OR ");
                let sql = format!(
                    "SELECT DISTINCT mi.id, mi.item_type, mi.title, mi.production_year,
                            mi.provider_ids_json, mi.series_id, mi.season_number,
                            mi.episode_number, mi.library_id
                     FROM media_items AS mi
                     WHERE mi.removed_at IS NULL AND ({predicates}){}",
                    migration_library_filter_sql(library_chunk),
                );
                let mut query = self.query(sqlx::AssertSqlSafe(sql));
                for lookup in chunk {
                    query = query
                        .bind(&lookup.item_type)
                        .bind(&lookup.title_pattern)
                        .bind(lookup.production_year)
                        .bind(lookup.production_year);
                }
                if let Some(library_ids) = library_chunk {
                    for library_id in library_ids {
                        query = query.bind(library_id);
                    }
                }
                for row in query.fetch_all(self.pool()).await.map_err(storage_error)? {
                    let id: String = row.get("id");
                    if seen_ids.insert(id.clone()) {
                        identities.push(StoredMigrationMediaIdentity {
                            id,
                            library_id: row.get("library_id"),
                            item_type: row.get("item_type"),
                            title: row.get("title"),
                            production_year: row.get("production_year"),
                            provider_ids_json: row.get("provider_ids_json"),
                            series_id: row.get("series_id"),
                            season_number: row.get("season_number"),
                            episode_number: row.get("episode_number"),
                        });
                    }
                }
            }
        }

        identities.sort_unstable_by(|left, right| left.id.cmp(&right.id));
        Ok(identities)
    }

    pub(crate) async fn list_migration_person_identities(
        &self,
    ) -> Result<Vec<StoredMigrationPersonIdentity>, StorageError> {
        self.query(
            "SELECT p.id, p.display_name, pi.provider, pi.provider_id
             FROM people p
             LEFT JOIN person_identities pi ON pi.person_id = p.id
             WHERE p.status = 'ACTIVE'
             ORDER BY p.id, pi.provider, pi.provider_id",
        )
        .fetch_all(self.pool())
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredMigrationPersonIdentity {
                    id: row.get("id"),
                    display_name: row.get("display_name"),
                    provider: row.try_get::<Option<String>, _>("provider").ok().flatten(),
                    provider_id: row
                        .try_get::<Option<String>, _>("provider_id")
                        .ok()
                        .flatten(),
                })
                .collect()
        })
        .map_err(storage_error)
    }

    /// Resolve only the people referenced by the current source favorites
    /// page. Provider and normalized-name predicates are batched separately so
    /// both existing indexes (`person_identities` primary key and
    /// `idx_people_name`) remain usable.
    pub(crate) async fn list_migration_person_identity_candidates(
        &self,
        lookups: &[MigrationPersonIdentityLookup],
    ) -> Result<Vec<StoredMigrationPersonIdentity>, StorageError> {
        if lookups.is_empty() {
            return Ok(Vec::new());
        }
        let mut identities = Vec::new();
        let mut seen = HashSet::<(String, Option<String>, Option<String>)>::new();
        let mut provider_keys = lookups
            .iter()
            .flat_map(|lookup| lookup.provider_ids.iter().cloned())
            .collect::<Vec<_>>();
        provider_keys.sort_unstable();
        provider_keys.dedup();
        for chunk in provider_keys.chunks(200) {
            if chunk.is_empty() {
                continue;
            }
            let predicates =
                std::iter::repeat_n("(pi.provider = ? AND pi.provider_id = ?)", chunk.len())
                    .collect::<Vec<_>>()
                    .join(" OR ");
            let sql = format!(
                "SELECT p.id, p.display_name, pi.provider, pi.provider_id
                 FROM people p
                 JOIN person_identities pi ON pi.person_id = p.id
                 WHERE p.status = 'ACTIVE' AND ({predicates})"
            );
            let mut query = self.query(sqlx::AssertSqlSafe(sql));
            for (provider, provider_id) in chunk {
                query = query.bind(provider).bind(provider_id);
            }
            for row in query.fetch_all(self.pool()).await.map_err(storage_error)? {
                let identity = StoredMigrationPersonIdentity {
                    id: row.get("id"),
                    display_name: row.get("display_name"),
                    provider: row.try_get("provider").ok(),
                    provider_id: row.try_get("provider_id").ok(),
                };
                let key = (
                    identity.id.clone(),
                    identity.provider.clone(),
                    identity.provider_id.clone(),
                );
                if seen.insert(key) {
                    identities.push(identity);
                }
            }
        }

        let mut names = lookups
            .iter()
            .map(|lookup| lookup.normalized_name.as_str())
            .filter(|name| !name.is_empty())
            .collect::<Vec<_>>();
        names.sort_unstable();
        names.dedup();
        for chunk in names.chunks(200) {
            if chunk.is_empty() {
                continue;
            }
            let predicates = std::iter::repeat_n("p.normalized_name = ?", chunk.len())
                .collect::<Vec<_>>()
                .join(" OR ");
            let sql = format!(
                "SELECT p.id, p.display_name
                 FROM people p
                 WHERE p.status = 'ACTIVE' AND ({predicates})"
            );
            let mut query = self.query(sqlx::AssertSqlSafe(sql));
            for name in chunk {
                query = query.bind(name);
            }
            for row in query.fetch_all(self.pool()).await.map_err(storage_error)? {
                let identity = StoredMigrationPersonIdentity {
                    id: row.get("id"),
                    display_name: row.get("display_name"),
                    provider: None,
                    provider_id: None,
                };
                let key = (identity.id.clone(), None, None);
                if seen.insert(key) {
                    identities.push(identity);
                }
            }
        }
        identities.sort_unstable_by(|left, right| {
            left.id
                .cmp(&right.id)
                .then_with(|| left.provider.cmp(&right.provider))
                .then_with(|| left.provider_id.cmp(&right.provider_id))
        });
        Ok(identities)
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

    #[cfg(test)]
    pub(crate) async fn upsert_emby_migration_person_favorite(
        &self,
        record: &NewEmbyMigrationPersonFavorite<'_>,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO emby_migration_person_favorites (
                 job_id, emby_user_id, emby_person_id, emby_person_name,
                 lux_user_id, lux_person_id, provider_ids_json, match_method,
                 confidence, status, state_hash, detail_json, error
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(job_id, emby_user_id, emby_person_id) DO UPDATE SET
                 emby_person_name = excluded.emby_person_name,
                 lux_user_id = excluded.lux_user_id,
                 lux_person_id = excluded.lux_person_id,
                 provider_ids_json = excluded.provider_ids_json,
                 match_method = excluded.match_method,
                 confidence = excluded.confidence,
                 status = excluded.status,
                 state_hash = excluded.state_hash,
                 detail_json = excluded.detail_json,
                 error = excluded.error,
                 updated_at = unixepoch()",
        )
        .bind(record.job_id)
        .bind(record.emby_user_id)
        .bind(record.emby_person_id)
        .bind(record.emby_person_name)
        .bind(record.lux_user_id)
        .bind(record.lux_person_id)
        .bind(record.provider_ids_json)
        .bind(record.match_method)
        .bind(record.confidence)
        .bind(record.status)
        .bind(record.state_hash)
        .bind(record.detail_json)
        .bind(record.error)
        .execute(self.pool())
        .await
        .map(|_| ())
        .map_err(storage_error)
    }

    pub(crate) async fn list_emby_migration_person_favorites(
        &self,
        job_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredEmbyMigrationPersonFavorite>, StorageError> {
        self.query(
            "SELECT job_id, emby_user_id, emby_person_id, emby_person_name,
                    lux_user_id, lux_person_id, provider_ids_json, match_method,
                    confidence, status, state_hash, detail_json, error
             FROM emby_migration_person_favorites
             WHERE job_id = ?
             ORDER BY emby_user_id, emby_person_name, emby_person_id
             LIMIT ? OFFSET ?",
        )
        .bind(job_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool())
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredEmbyMigrationPersonFavorite {
                    job_id: row.get("job_id"),
                    emby_user_id: row.get("emby_user_id"),
                    emby_person_id: row.get("emby_person_id"),
                    emby_person_name: row.get("emby_person_name"),
                    lux_user_id: row.get("lux_user_id"),
                    lux_person_id: row.get("lux_person_id"),
                    provider_ids_json: row.get("provider_ids_json"),
                    match_method: row.get("match_method"),
                    confidence: row.get("confidence"),
                    status: row.get("status"),
                    state_hash: row.get("state_hash"),
                    detail_json: row.get("detail_json"),
                    error: row.get("error"),
                })
                .collect()
        })
        .map_err(storage_error)
    }

    /// Commits all writes produced for one source page atomically.
    ///
    /// A page is deliberately bounded by the service (currently 500 source
    /// records), so a single multi-row statement stays below SQLite's
    /// parameter limit while avoiding one transaction/commit per item.
    pub(crate) async fn commit_emby_migration_item_page(
        &self,
        page: EmbyMigrationItemPageBatch<'_>,
    ) -> Result<(), StorageError> {
        let EmbyMigrationItemPageBatch {
            job_id,
            merge_policy,
            state_fields,
            item_matches,
            states,
            import_records,
            handled_items,
            progress,
        } = page;
        let mut transaction = self.begin_metadata_write_transaction().await?;

        if !item_matches.is_empty() {
            for item_matches in item_matches.chunks(EMBY_MIGRATION_WRITE_BATCH_SIZE) {
                let mut sql = String::from(
                    "INSERT INTO emby_migration_item_matches (
                     job_id, emby_item_id, emby_item_type, lux_item_id, match_method,
                     confidence, status, detail_json
                 ) VALUES ",
                );
                for index in 0..item_matches.len() {
                    if index > 0 {
                        sql.push(',');
                    }
                    sql.push_str(" (?, ?, ?, ?, ?, ?, ?, ?)");
                }
                let lux_item_id_changed = sql_is_distinct(
                    "emby_migration_item_matches.lux_item_id",
                    "excluded.lux_item_id",
                );
                let confidence_changed = sql_is_distinct(
                    "emby_migration_item_matches.confidence",
                    "excluded.confidence",
                );
                sql.push_str(&format!(
                    " ON CONFLICT(job_id, emby_item_id) DO UPDATE SET
                     emby_item_type = excluded.emby_item_type,
                     lux_item_id = excluded.lux_item_id,
                     match_method = excluded.match_method,
                     confidence = excluded.confidence,
                     status = excluded.status,
                     detail_json = excluded.detail_json,
                     updated_at = unixepoch()
                 WHERE emby_migration_item_matches.emby_item_type <> excluded.emby_item_type
                    OR {lux_item_id_changed}
                    OR emby_migration_item_matches.match_method <> excluded.match_method
                    OR {confidence_changed}
                    OR emby_migration_item_matches.status <> excluded.status
                    OR emby_migration_item_matches.detail_json <> excluded.detail_json"
                ));
                let mut query = self.query(sqlx::AssertSqlSafe(sql));
                for item_match in item_matches {
                    query = query
                        .bind(job_id)
                        .bind(&item_match.emby_item_id)
                        .bind(&item_match.emby_item_type)
                        .bind(&item_match.lux_item_id)
                        .bind(&item_match.match_method)
                        .bind(item_match.confidence)
                        .bind(&item_match.status)
                        .bind(&item_match.detail_json);
                }
                query
                    .execute(&mut *transaction)
                    .await
                    .map_err(storage_error)?;
            }
        }

        if !states.is_empty() {
            for states in states.chunks(EMBY_MIGRATION_WRITE_BATCH_SIZE) {
                let (mut position_ticks, mut is_played, mut is_favorite, mut play_count, mut last_played_at) =
                    match merge_policy {
                    "OVERWRITE" => (
                        "excluded.position_ticks",
                        "excluded.is_played",
                        "excluded.is_favorite",
                        "excluded.play_count",
                        "excluded.last_played_at",
                    ),
                    "SKIP" => (
                        "user_item_state.position_ticks",
                        "user_item_state.is_played",
                        "user_item_state.is_favorite",
                        "user_item_state.play_count",
                        "user_item_state.last_played_at",
                    ),
                    _ => (
                        "CASE
                            WHEN excluded.last_played_at IS NOT NULL
                             AND user_item_state.last_played_at IS NULL
                                THEN excluded.position_ticks
                            WHEN excluded.last_played_at IS NOT NULL
                             AND user_item_state.last_played_at IS NOT NULL
                             AND excluded.last_played_at > user_item_state.last_played_at
                                THEN excluded.position_ticks
                            WHEN (excluded.last_played_at = user_item_state.last_played_at
                               OR (excluded.last_played_at IS NULL
                               AND user_item_state.last_played_at IS NULL))
                                THEN CASE WHEN excluded.position_ticks > user_item_state.position_ticks
                                          THEN excluded.position_ticks
                                          ELSE user_item_state.position_ticks END
                            ELSE user_item_state.position_ticks
                         END",
                        "CASE WHEN user_item_state.is_played = 1 OR excluded.is_played = 1
                              THEN 1 ELSE 0 END",
                        "CASE WHEN user_item_state.is_favorite = 1 OR excluded.is_favorite = 1
                              THEN 1 ELSE 0 END",
                        "CASE WHEN excluded.play_count > user_item_state.play_count
                              THEN excluded.play_count ELSE user_item_state.play_count END",
                        "CASE
                            WHEN user_item_state.last_played_at IS NULL
                                THEN excluded.last_played_at
                            WHEN excluded.last_played_at IS NULL
                                THEN user_item_state.last_played_at
                            WHEN excluded.last_played_at > user_item_state.last_played_at
                                THEN excluded.last_played_at
                            ELSE user_item_state.last_played_at
                         END",
                    ),
                };
                if state_fields.position_ticks
                    && !state_fields.last_played_at
                    && merge_policy == "MERGE"
                {
                    position_ticks =
                        "CASE WHEN excluded.position_ticks > user_item_state.position_ticks
                                          THEN excluded.position_ticks
                                          ELSE user_item_state.position_ticks END";
                }
                if !state_fields.position_ticks {
                    position_ticks = "user_item_state.position_ticks";
                }
                if !state_fields.is_played {
                    is_played = "user_item_state.is_played";
                }
                if !state_fields.is_favorite {
                    is_favorite = "user_item_state.is_favorite";
                }
                if !state_fields.play_count {
                    play_count = "user_item_state.play_count";
                }
                if !state_fields.last_played_at {
                    last_played_at = "user_item_state.last_played_at";
                }
                let last_played_changed =
                    sql_is_distinct(last_played_at, "user_item_state.last_played_at");
                let mut changed_fields = Vec::with_capacity(5);
                if state_fields.position_ticks {
                    changed_fields.push(format!(
                        "{position_ticks} != user_item_state.position_ticks"
                    ));
                }
                if state_fields.is_played {
                    changed_fields.push(format!("{is_played} != user_item_state.is_played"));
                }
                if state_fields.is_favorite {
                    changed_fields.push(format!("{is_favorite} != user_item_state.is_favorite"));
                }
                if state_fields.play_count {
                    changed_fields.push(format!("{play_count} != user_item_state.play_count"));
                }
                if state_fields.last_played_at {
                    changed_fields.push(last_played_changed.clone());
                }
                if changed_fields.is_empty() {
                    continue;
                }
                let mut columns = vec!["user_id", "item_id"];
                if state_fields.position_ticks {
                    columns.push("position_ticks");
                }
                if state_fields.is_played {
                    columns.push("is_played");
                }
                if state_fields.is_favorite {
                    columns.push("is_favorite");
                }
                if state_fields.play_count {
                    columns.push("play_count");
                }
                if state_fields.last_played_at {
                    columns.push("last_played_at");
                }
                let values = std::iter::repeat_n("?", columns.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut sql = format!(
                    "INSERT INTO user_item_state ({}) VALUES ",
                    columns.join(", ")
                );
                for index in 0..states.len() {
                    if index > 0 {
                        sql.push(',');
                    }
                    sql.push_str(&format!(" ({values})"));
                }
                let changed = changed_fields.join("\n                           OR ");
                sql.push_str(&format!(
                    " ON CONFLICT(user_id, item_id) DO UPDATE SET
                     position_ticks = {position_ticks},
                     is_played = {is_played},
                     is_favorite = {is_favorite},
                     play_count = {play_count},
                     last_played_at = {last_played_at},
                     version = user_item_state.version + CASE
                         WHEN {changed}
                         THEN 1 ELSE 0 END"
                ));
                sql.push_str(&format!(" WHERE {changed}"));
                let mut query = self.query(sqlx::AssertSqlSafe(sql));
                for state in states {
                    query = query.bind(&state.user_id).bind(&state.item_id);
                    if state_fields.position_ticks {
                        query = query.bind(state.position_ticks);
                    }
                    if state_fields.is_played {
                        query = query.bind(if state.is_played { 1_i64 } else { 0_i64 });
                    }
                    if state_fields.is_favorite {
                        query = query.bind(if state.is_favorite { 1_i64 } else { 0_i64 });
                    }
                    if state_fields.play_count {
                        query = query.bind(state.play_count);
                    }
                    if state_fields.last_played_at {
                        query = query.bind(state.last_played_at);
                    }
                }
                query
                    .execute(&mut *transaction)
                    .await
                    .map_err(storage_error)?;
            }
        }

        if !import_records.is_empty() {
            for import_records in import_records.chunks(EMBY_MIGRATION_WRITE_BATCH_SIZE) {
                let mut sql = String::from(
                    "INSERT INTO emby_migration_import_records (
                     job_id, emby_user_id, emby_item_id, lux_user_id, lux_item_id,
                     state_hash, status, error
                 ) VALUES ",
                );
                for index in 0..import_records.len() {
                    if index > 0 {
                        sql.push(',');
                    }
                    sql.push_str(" (?, ?, ?, ?, ?, ?, ?, ?)");
                }
                let error_changed =
                    sql_is_distinct("emby_migration_import_records.error", "excluded.error");
                sql.push_str(&format!(
                    " ON CONFLICT(job_id, emby_user_id, emby_item_id) DO UPDATE SET
                     lux_user_id = excluded.lux_user_id,
                     lux_item_id = excluded.lux_item_id,
                     state_hash = excluded.state_hash,
                     status = excluded.status,
                     error = excluded.error,
                     imported_at = unixepoch()
                 WHERE emby_migration_import_records.lux_user_id <> excluded.lux_user_id
                    OR emby_migration_import_records.lux_item_id <> excluded.lux_item_id
                    OR emby_migration_import_records.state_hash <> excluded.state_hash
                    OR emby_migration_import_records.status <> excluded.status
                    OR {error_changed}"
                ));
                let mut query = self.query(sqlx::AssertSqlSafe(sql));
                for record in import_records {
                    query = query
                        .bind(job_id)
                        .bind(&record.emby_user_id)
                        .bind(&record.emby_item_id)
                        .bind(&record.lux_user_id)
                        .bind(&record.lux_item_id)
                        .bind(&record.state_hash)
                        .bind(&record.status)
                        .bind(&record.error);
                }
                query
                    .execute(&mut *transaction)
                    .await
                    .map_err(storage_error)?;
            }
        }

        if !handled_items.is_empty() {
            for handled_items in handled_items.chunks(EMBY_MIGRATION_WRITE_BATCH_SIZE) {
                let mut sql = String::from(
                    "INSERT INTO emby_migration_handled_items (
                     job_id, emby_user_id, emby_item_id
                 ) VALUES ",
                );
                for index in 0..handled_items.len() {
                    if index > 0 {
                        sql.push(',');
                    }
                    sql.push_str(" (?, ?, ?)");
                }
                sql.push_str(" ON CONFLICT(job_id, emby_user_id, emby_item_id) DO NOTHING");
                let mut query = self.query(sqlx::AssertSqlSafe(sql));
                for handled_item in handled_items {
                    query = query
                        .bind(job_id)
                        .bind(&handled_item.emby_user_id)
                        .bind(&handled_item.emby_item_id);
                }
                query
                    .execute(&mut *transaction)
                    .await
                    .map_err(storage_error)?;
            }
        }

        let result = self
            .query(
                "UPDATE emby_migration_jobs
                 SET cursor_json = ?, processed_count = ?, total_count = ?, matched_count = ?,
                     skipped_count = ?, failed_count = ?, updated_at = unixepoch()
                 WHERE id = ? AND status IN ('PENDING', 'RUNNING')",
            )
            .bind(progress.cursor_json)
            .bind(progress.processed_count)
            .bind(progress.total_count)
            .bind(progress.matched_count)
            .bind(progress.skipped_count)
            .bind(progress.failed_count)
            .bind(progress.id)
            .execute(&mut *transaction)
            .await
            .map_err(storage_error)?;
        if result.rows_affected() != 1 {
            return Err(StorageError::Conflict(
                "Emby migration job progress could not be updated".to_owned(),
            ));
        }
        transaction.commit().await.map_err(storage_error)
    }

    pub(crate) async fn commit_emby_migration_person_page(
        &self,
        job_id: &str,
        favorites: &[EmbyMigrationPersonFavoriteBatch],
        states: &[EmbyMigrationPersonFavoriteStateBatch],
        progress: &EmbyMigrationJobProgress<'_>,
    ) -> Result<(), StorageError> {
        let mut transaction = self.begin_metadata_write_transaction().await?;
        if !favorites.is_empty() {
            for favorites in favorites.chunks(EMBY_MIGRATION_WRITE_BATCH_SIZE) {
                let mut sql = String::from(
                    "INSERT INTO emby_migration_person_favorites (
                     job_id, emby_user_id, emby_person_id, emby_person_name,
                     lux_user_id, lux_person_id, provider_ids_json, match_method,
                     confidence, status, state_hash, detail_json, error
                 ) VALUES ",
                );
                for index in 0..favorites.len() {
                    if index > 0 {
                        sql.push(',');
                    }
                    sql.push_str(" (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)");
                }
                let lux_user_id_changed = sql_is_distinct(
                    "emby_migration_person_favorites.lux_user_id",
                    "excluded.lux_user_id",
                );
                let lux_person_id_changed = sql_is_distinct(
                    "emby_migration_person_favorites.lux_person_id",
                    "excluded.lux_person_id",
                );
                let confidence_changed = sql_is_distinct(
                    "emby_migration_person_favorites.confidence",
                    "excluded.confidence",
                );
                let error_changed =
                    sql_is_distinct("emby_migration_person_favorites.error", "excluded.error");
                sql.push_str(&format!(
                " ON CONFLICT(job_id, emby_user_id, emby_person_id) DO UPDATE SET
                     emby_person_name = excluded.emby_person_name,
                     lux_user_id = excluded.lux_user_id,
                     lux_person_id = excluded.lux_person_id,
                     provider_ids_json = excluded.provider_ids_json,
                     match_method = excluded.match_method,
                     confidence = excluded.confidence,
                     status = excluded.status,
                     state_hash = excluded.state_hash,
                     detail_json = excluded.detail_json,
                     error = excluded.error,
                     updated_at = unixepoch()
                 WHERE emby_migration_person_favorites.emby_person_name <> excluded.emby_person_name
                    OR {lux_user_id_changed}
                    OR {lux_person_id_changed}
                    OR emby_migration_person_favorites.provider_ids_json <> excluded.provider_ids_json
                    OR emby_migration_person_favorites.match_method <> excluded.match_method
                    OR {confidence_changed}
                    OR emby_migration_person_favorites.status <> excluded.status
                    OR emby_migration_person_favorites.state_hash <> excluded.state_hash
                    OR emby_migration_person_favorites.detail_json <> excluded.detail_json
                    OR {error_changed}"
                ));
                let mut query = self.query(sqlx::AssertSqlSafe(sql));
                for favorite in favorites {
                    query = query
                        .bind(job_id)
                        .bind(&favorite.emby_user_id)
                        .bind(&favorite.emby_person_id)
                        .bind(&favorite.emby_person_name)
                        .bind(&favorite.lux_user_id)
                        .bind(&favorite.lux_person_id)
                        .bind(&favorite.provider_ids_json)
                        .bind(&favorite.match_method)
                        .bind(favorite.confidence)
                        .bind(&favorite.status)
                        .bind(&favorite.state_hash)
                        .bind(&favorite.detail_json)
                        .bind(&favorite.error);
                }
                query
                    .execute(&mut *transaction)
                    .await
                    .map_err(storage_error)?;
            }
        }
        if !states.is_empty() {
            for states in states.chunks(EMBY_MIGRATION_WRITE_BATCH_SIZE) {
                let mut sql = String::from(
                    "INSERT INTO user_person_state (user_id, person_id, is_favorite) VALUES ",
                );
                for index in 0..states.len() {
                    if index > 0 {
                        sql.push(',');
                    }
                    sql.push_str(" (?, ?, 1)");
                }
                sql.push_str(
                    " ON CONFLICT(user_id, person_id) DO UPDATE SET
                     is_favorite = excluded.is_favorite,
                     updated_at = unixepoch()
                 WHERE user_person_state.is_favorite <> excluded.is_favorite",
                );
                let mut query = self.query(sqlx::AssertSqlSafe(sql));
                for state in states {
                    query = query.bind(&state.user_id).bind(&state.person_id);
                }
                query
                    .execute(&mut *transaction)
                    .await
                    .map_err(storage_error)?;
            }
        }
        let result = self
            .query(
                "UPDATE emby_migration_jobs
                 SET cursor_json = ?, processed_count = ?, total_count = ?, matched_count = ?,
                     skipped_count = ?, failed_count = ?, updated_at = unixepoch()
                 WHERE id = ? AND status IN ('PENDING', 'RUNNING')",
            )
            .bind(progress.cursor_json)
            .bind(progress.processed_count)
            .bind(progress.total_count)
            .bind(progress.matched_count)
            .bind(progress.skipped_count)
            .bind(progress.failed_count)
            .bind(progress.id)
            .execute(&mut *transaction)
            .await
            .map_err(storage_error)?;
        if result.rows_affected() != 1 {
            return Err(StorageError::Conflict(
                "Emby migration job progress could not be updated".to_owned(),
            ));
        }
        transaction.commit().await.map_err(storage_error)
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

    pub(crate) async fn list_emby_migration_handled_item_ids(
        &self,
        job_id: &str,
        emby_user_id: &str,
        item_ids: &[String],
    ) -> Result<Vec<String>, StorageError> {
        if item_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat_n("?", item_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "SELECT emby_item_id
             FROM emby_migration_handled_items
             WHERE job_id = ? AND emby_user_id = ?
               AND emby_item_id IN ({placeholders})"
        );
        let mut query = self.query_scalar::<String>(sqlx::AssertSqlSafe(query));
        query = query.bind(job_id).bind(emby_user_id);
        for item_id in item_ids {
            query = query.bind(item_id);
        }
        query.fetch_all(self.pool()).await.map_err(storage_error)
    }

    pub(crate) async fn list_emby_migration_imported_library_ids(
        &self,
        job_id: &str,
        emby_user_id: &str,
    ) -> Result<Vec<String>, StorageError> {
        self.query_scalar(
            "SELECT DISTINCT media_items.library_id
             FROM emby_migration_import_records
             JOIN media_items ON media_items.id = emby_migration_import_records.lux_item_id
             WHERE emby_migration_import_records.job_id = ?
               AND emby_migration_import_records.emby_user_id = ?
               AND emby_migration_import_records.status = 'IMPORTED'
               AND media_items.removed_at IS NULL
             ORDER BY media_items.library_id",
        )
        .bind(job_id)
        .bind(emby_user_id)
        .fetch_all(self.pool())
        .await
        .map_err(storage_error)
    }

    #[cfg(test)]
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

    #[cfg(test)]
    pub(crate) async fn upsert_imported_user_item_state(
        &self,
        state: &NewImportedUserItemState<'_>,
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
        .bind(state.user_id)
        .bind(state.item_id)
        .bind(state.position_ticks)
        .bind(if state.is_played { 1_i64 } else { 0_i64 })
        .bind(if state.is_favorite { 1_i64 } else { 0_i64 })
        .bind(state.play_count)
        .bind(state.last_played_at)
        .execute(self.pool())
        .await
        .map(|_| ())
        .map_err(storage_error)
    }

    #[cfg(test)]
    pub(crate) async fn merge_imported_user_item_state(
        &self,
        state: &NewImportedUserItemState<'_>,
        merge_policy: &str,
    ) -> Result<(), StorageError> {
        let (position_ticks, is_played, is_favorite, play_count, last_played_at) =
            match merge_policy {
                "OVERWRITE" => (
                    "excluded.position_ticks",
                    "excluded.is_played",
                    "excluded.is_favorite",
                    "excluded.play_count",
                    "excluded.last_played_at",
                ),
                "SKIP" => (
                    "user_item_state.position_ticks",
                    "user_item_state.is_played",
                    "user_item_state.is_favorite",
                    "user_item_state.play_count",
                    "user_item_state.last_played_at",
                ),
                _ => (
                    "CASE
                        WHEN excluded.last_played_at IS NOT NULL
                         AND user_item_state.last_played_at IS NULL
                            THEN excluded.position_ticks
                        WHEN excluded.last_played_at IS NOT NULL
                         AND user_item_state.last_played_at IS NOT NULL
                         AND excluded.last_played_at > user_item_state.last_played_at
                            THEN excluded.position_ticks
                        WHEN (excluded.last_played_at = user_item_state.last_played_at
                           OR (excluded.last_played_at IS NULL
                           AND user_item_state.last_played_at IS NULL))
                            THEN CASE WHEN excluded.position_ticks > user_item_state.position_ticks
                                      THEN excluded.position_ticks
                                      ELSE user_item_state.position_ticks END
                        ELSE user_item_state.position_ticks
                     END",
                    "CASE WHEN user_item_state.is_played = 1 OR excluded.is_played = 1
                          THEN 1 ELSE 0 END",
                    "CASE WHEN user_item_state.is_favorite = 1 OR excluded.is_favorite = 1
                          THEN 1 ELSE 0 END",
                    "CASE WHEN excluded.play_count > user_item_state.play_count
                          THEN excluded.play_count ELSE user_item_state.play_count END",
                    "CASE
                        WHEN user_item_state.last_played_at IS NULL
                            THEN excluded.last_played_at
                        WHEN excluded.last_played_at IS NULL
                            THEN user_item_state.last_played_at
                        WHEN excluded.last_played_at > user_item_state.last_played_at
                            THEN excluded.last_played_at
                        ELSE user_item_state.last_played_at
                     END",
                ),
            };
        let last_played_changed = sql_is_distinct(last_played_at, "user_item_state.last_played_at");
        let query = format!(
            "INSERT INTO user_item_state (
                 user_id, item_id, position_ticks, is_played, is_favorite,
                 play_count, last_played_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(user_id, item_id) DO UPDATE SET
                 position_ticks = {position_ticks},
                 is_played = {is_played},
                 is_favorite = {is_favorite},
                 play_count = {play_count},
                 last_played_at = {last_played_at},
                 version = user_item_state.version + CASE
                     WHEN {position_ticks} != user_item_state.position_ticks
                       OR {is_played} != user_item_state.is_played
                       OR {is_favorite} != user_item_state.is_favorite
                       OR {play_count} != user_item_state.play_count
                       OR {last_played_changed}
                     THEN 1 ELSE 0 END",
        );
        self.query(sqlx::AssertSqlSafe(query))
            .bind(state.user_id)
            .bind(state.item_id)
            .bind(state.position_ticks)
            .bind(if state.is_played { 1_i64 } else { 0_i64 })
            .bind(if state.is_favorite { 1_i64 } else { 0_i64 })
            .bind(state.play_count)
            .bind(state.last_played_at)
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

const MIGRATION_LIBRARY_FILTER_CHUNK_SIZE: usize = 200;

fn migration_library_id_chunks(selected_library_ids: Option<&[String]>) -> Vec<Option<&[String]>> {
    selected_library_ids
        .map(|ids| {
            ids.chunks(MIGRATION_LIBRARY_FILTER_CHUNK_SIZE)
                .map(Some)
                .collect()
        })
        .unwrap_or_else(|| vec![None])
}

fn migration_library_filter_sql(library_ids: Option<&[String]>) -> String {
    let Some(library_ids) = library_ids else {
        return String::new();
    };
    let placeholders = std::iter::repeat_n("?", library_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    format!(" AND mi.library_id IN ({placeholders})")
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
        scope_json: row.get("scope_json"),
        emby_user_ids_json: row.get("emby_user_ids_json"),
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

fn sql_is_distinct(left: &str, right: &str) -> String {
    format!(
        "(({left} IS NULL AND {right} IS NOT NULL)
         OR ({left} IS NOT NULL AND {right} IS NULL)
         OR ({left} IS NOT NULL AND {right} IS NOT NULL AND {left} <> {right}))"
    )
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
    async fn migration_provider_index_backfills_and_resolves_only_page_candidates()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let (_user_id, item_id) = insert_test_user_and_item(&database).await?;
        sqlx::query("UPDATE media_items SET provider_ids_json = ? WHERE id = ?")
            .bind(r#"{"tmdb":"42"}"#)
            .bind(&item_id)
            .execute(database.pool())
            .await?;

        let indexed: (String, String, String) = sqlx::query_as(
            "SELECT media_item_id, provider, provider_id
             FROM media_item_provider_ids WHERE media_item_id = ?",
        )
        .bind(&item_id)
        .fetch_one(database.pool())
        .await?;
        assert_eq!(
            indexed,
            (item_id.clone(), "tmdb".to_owned(), "42".to_owned())
        );

        let identities = database
            .list_migration_media_identity_candidates(&[MigrationMediaIdentityLookup {
                item_type: "MOVIE".to_owned(),
                title: "Migration Item".to_owned(),
                title_pattern: "%migration%item%".to_owned(),
                production_year: None,
                season_number: None,
                episode_number: None,
                provider_ids: vec![("tmdb".to_owned(), "42".to_owned())],
            }])
            .await?;
        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].id, item_id);
        Ok(())
    }

    #[tokio::test]
    async fn migration_provider_match_skips_title_fallback_query()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let (_user_id, item_id) = insert_test_user_and_item(&database).await?;
        sqlx::query("UPDATE media_items SET provider_ids_json = ? WHERE id = ?")
            .bind(r#"{"tmdb":"42"}"#)
            .bind(&item_id)
            .execute(database.pool())
            .await?;

        database.reset_query_count();
        let identities = database
            .list_migration_media_identity_candidates(&[MigrationMediaIdentityLookup {
                item_type: "MOVIE".to_owned(),
                title: "A title that cannot match".to_owned(),
                title_pattern: "%a%title%that%cannot%match%".to_owned(),
                production_year: Some(1999),
                season_number: None,
                episode_number: None,
                provider_ids: vec![("tmdb".to_owned(), "42".to_owned())],
            }])
            .await?;

        assert_eq!(identities.len(), 1);
        assert_eq!(identities[0].id, item_id);
        assert_eq!(database.query_count(), 1);

        database.reset_query_count();
        let title_only = database
            .list_migration_media_identity_candidates(&[MigrationMediaIdentityLookup {
                item_type: "MOVIE".to_owned(),
                title: "Migration Item".to_owned(),
                title_pattern: "%migration%item%".to_owned(),
                production_year: None,
                season_number: None,
                episode_number: None,
                provider_ids: Vec::new(),
            }])
            .await?;
        assert_eq!(title_only.len(), 1);
        assert_eq!(title_only[0].id, item_id);
        assert_eq!(database.query_count(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn migration_provider_lookup_with_empty_title_skips_wildcard_fallback()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        database.reset_query_count();

        let identities = database
            .list_migration_media_identity_candidates(&[MigrationMediaIdentityLookup {
                item_type: "MOVIE".to_owned(),
                title: "!!!".to_owned(),
                title_pattern: "%".to_owned(),
                production_year: None,
                season_number: None,
                episode_number: None,
                provider_ids: vec![("tmdb".to_owned(), "missing".to_owned())],
            }])
            .await?;

        assert!(identities.is_empty());
        assert_eq!(database.query_count(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn migration_user_links_are_upserted_in_bounded_batches()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let (user_id, _item_id) = insert_test_user_and_item(&database).await?;
        let job_id = Uuid::now_v7().to_string();
        database
            .insert_emby_migration_job(&NewEmbyMigrationJob {
                id: &job_id,
                created_by_user_id: &user_id,
                source_label: "Test Emby",
                source_base_url: "https://emby.example.test/",
                secret_ref: "emby-migration/test",
                dry_run: false,
                merge_policy: "MERGE",
                scope_json: r#"{}"#,
                emby_user_ids_json: r#"["emby-user"]"#,
            })
            .await?;
        let links = (0..205)
            .map(|index| StoredEmbyMigrationUserLink {
                job_id: job_id.clone(),
                emby_user_id: format!("emby-{index}"),
                emby_username: format!("User {index}"),
                lux_user_id: Some(user_id.clone()),
                status: "LINKED".to_owned(),
                error: None,
            })
            .collect::<Vec<_>>();

        database.reset_query_count();
        database
            .upsert_emby_migration_user_links_batch(&links)
            .await?;

        assert_eq!(database.query_count(), 3);
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM emby_migration_user_links WHERE job_id = ?")
                .bind(&job_id)
                .fetch_one(database.pool())
                .await?;
        assert_eq!(count, 205);
        Ok(())
    }

    #[tokio::test]
    async fn migration_person_candidates_are_scoped_to_page_keys()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let provider_person = Uuid::now_v7().to_string();
        let name_person = Uuid::now_v7().to_string();
        for (id, name) in [(&provider_person, "Actor A"), (&name_person, "Actor B")] {
            sqlx::query(
                "INSERT INTO people (
                    id, display_name, directory_name, normalized_name, status, created_at, updated_at
                 ) VALUES (?, ?, ?, ?, 'ACTIVE', unixepoch(), unixepoch())",
            )
            .bind(id)
            .bind(name)
            .bind(name)
            .bind(name.to_lowercase().replace(' ', ""))
            .execute(database.pool())
            .await?;
        }
        sqlx::query(
            "INSERT INTO person_identities (
                person_id, provider, provider_id, match_method, confidence, evidence_json,
                created_at, updated_at
             ) VALUES (?, 'tmdb', '42', 'TEST', 100, '{}', unixepoch(), unixepoch())",
        )
        .bind(&provider_person)
        .execute(database.pool())
        .await?;

        database.reset_query_count();
        let identities = database
            .list_migration_person_identity_candidates(&[
                MigrationPersonIdentityLookup {
                    normalized_name: "actora".to_owned(),
                    provider_ids: vec![("tmdb".to_owned(), "42".to_owned())],
                },
                MigrationPersonIdentityLookup {
                    normalized_name: "actorb".to_owned(),
                    provider_ids: Vec::new(),
                },
            ])
            .await?;

        assert!(database.query_count() <= 3);
        assert_eq!(identities.len(), 3);
        assert!(
            identities
                .iter()
                .any(|identity| identity.id == provider_person)
        );
        assert!(identities.iter().any(|identity| identity.id == name_person));
        Ok(())
    }

    #[tokio::test]
    async fn migration_provider_candidates_respect_selected_library_filter()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let (_user_id, allowed_item_id) = insert_test_user_and_item(&database).await?;
        let allowed_library_id: String =
            sqlx::query_scalar("SELECT library_id FROM media_items WHERE id = ?")
                .bind(&allowed_item_id)
                .fetch_one(database.pool())
                .await?;
        let excluded_library_id = "migration-excluded-library";
        let excluded_item_id = "migration-excluded-item";
        sqlx::query("INSERT INTO libraries (id, name, kind) VALUES (?, ?, 'MOVIE')")
            .bind(excluded_library_id)
            .bind("Excluded migration library")
            .execute(database.pool())
            .await?;
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(excluded_item_id)
        .bind(excluded_library_id)
        .bind("Migration Item")
        .bind("migration item")
        .execute(database.pool())
        .await?;
        for item_id in [&allowed_item_id, excluded_item_id] {
            sqlx::query("UPDATE media_items SET provider_ids_json = ? WHERE id = ?")
                .bind(r#"{"tmdb":"42"}"#)
                .bind(item_id)
                .execute(database.pool())
                .await?;
        }

        let identities = database
            .list_migration_media_identity_candidates_filtered(
                &[MigrationMediaIdentityLookup {
                    item_type: "MOVIE".to_owned(),
                    title: "Migration Item".to_owned(),
                    title_pattern: "%migration%item%".to_owned(),
                    production_year: None,
                    season_number: None,
                    episode_number: None,
                    provider_ids: vec![("tmdb".to_owned(), "42".to_owned())],
                }],
                Some(std::slice::from_ref(&allowed_library_id)),
            )
            .await?;

        assert_eq!(
            identities.iter().map(|item| &item.id).collect::<Vec<_>>(),
            vec![&allowed_item_id]
        );
        Ok(())
    }

    #[tokio::test]
    async fn imported_state_and_history_are_idempotent() -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let (user_id, item_id) = insert_test_user_and_item(&database).await?;

        database
            .upsert_imported_user_item_state(&NewImportedUserItemState {
                user_id: &user_id,
                item_id: &item_id,
                position_ticks: 120,
                is_played: true,
                is_favorite: true,
                play_count: 3,
                last_played_at: Some(200),
            })
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
    async fn imported_state_merge_is_atomic_and_preserves_merge_semantics()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let (user_id, item_id) = insert_test_user_and_item(&database).await?;
        database
            .upsert_imported_user_item_state(&NewImportedUserItemState {
                user_id: &user_id,
                item_id: &item_id,
                position_ticks: 120,
                is_played: true,
                is_favorite: false,
                play_count: 3,
                last_played_at: Some(200),
            })
            .await?;

        database.reset_query_count();
        database
            .merge_imported_user_item_state(
                &NewImportedUserItemState {
                    user_id: &user_id,
                    item_id: &item_id,
                    position_ticks: 80,
                    is_played: false,
                    is_favorite: true,
                    play_count: 5,
                    last_played_at: Some(300),
                },
                "MERGE",
            )
            .await?;
        assert_eq!(database.query_count(), 1);

        let state = database
            .find_user_item_state_for_migration(&user_id, &item_id)
            .await?
            .expect("merged state should be stored");
        assert_eq!(state.position_ticks, 80);
        assert!(state.is_played);
        assert!(state.is_favorite);
        assert_eq!(state.play_count, 5);
        assert_eq!(state.last_played_at, Some(300));
        assert_eq!(state.version, 1);
        Ok(())
    }

    #[tokio::test]
    async fn batch_state_overwrite_clears_last_played_at() -> Result<(), Box<dyn std::error::Error>>
    {
        let (_temp_dir, database) = test_database().await?;
        let (user_id, item_id) = insert_test_user_and_item(&database).await?;
        database
            .upsert_imported_user_item_state(&NewImportedUserItemState {
                user_id: &user_id,
                item_id: &item_id,
                position_ticks: 120,
                is_played: true,
                is_favorite: false,
                play_count: 3,
                last_played_at: Some(200),
            })
            .await?;
        let job_id = Uuid::now_v7().to_string();
        database
            .insert_emby_migration_job(&NewEmbyMigrationJob {
                id: &job_id,
                created_by_user_id: &user_id,
                source_label: "Test Emby",
                source_base_url: "https://emby.example.test/",
                secret_ref: "emby-migration/test",
                dry_run: false,
                merge_policy: "OVERWRITE",
                scope_json: r#"{"userProfile":false,"libraryAccess":false,"itemState":true,"personFavorites":false}"#,
                emby_user_ids_json: r#"["emby-user"]"#,
            })
            .await?;
        database
            .update_emby_migration_job_status(&job_id, "RUNNING", "ITEMS", None)
            .await?;

        database
            .commit_emby_migration_item_page(EmbyMigrationItemPageBatch {
                job_id: &job_id,
                merge_policy: "OVERWRITE",
                state_fields: EmbyMigrationUserItemStateFields::all(),
                item_matches: &[],
                states: &[EmbyMigrationUserItemStateBatch {
                    user_id: user_id.clone(),
                    item_id: item_id.clone(),
                    position_ticks: 120,
                    is_played: true,
                    is_favorite: false,
                    play_count: 3,
                    last_played_at: None,
                }],
                import_records: &[],
                handled_items: &[],
                progress: EmbyMigrationJobProgress {
                    id: &job_id,
                    cursor_json: "{}",
                    processed_count: 1,
                    total_count: 1,
                    matched_count: 1,
                    skipped_count: 0,
                    failed_count: 0,
                },
            })
            .await?;

        let state = database
            .find_user_item_state_for_migration(&user_id, &item_id)
            .await?
            .expect("overwritten batch state should remain present");
        assert_eq!(state.last_played_at, None);
        assert_eq!(state.version, 1);
        Ok(())
    }

    #[tokio::test]
    async fn batch_favorite_only_overwrite_preserves_unselected_state_fields()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let (user_id, item_id) = insert_test_user_and_item(&database).await?;
        database
            .upsert_imported_user_item_state(&NewImportedUserItemState {
                user_id: &user_id,
                item_id: &item_id,
                position_ticks: 120,
                is_played: true,
                is_favorite: false,
                play_count: 3,
                last_played_at: Some(200),
            })
            .await?;
        let job_id = Uuid::now_v7().to_string();
        database
            .insert_emby_migration_job(&NewEmbyMigrationJob {
                id: &job_id,
                created_by_user_id: &user_id,
                source_label: "Test Emby",
                source_base_url: "https://emby.example.test/",
                secret_ref: "emby-migration/test",
                dry_run: false,
                merge_policy: "OVERWRITE",
                scope_json: r#"{"userProfile":false,"libraryAccess":false,"itemState":true,"itemStateFilters":["FAVORITE"],"personFavorites":false}"#,
                emby_user_ids_json: r#"["emby-user"]"#,
            })
            .await?;
        database
            .update_emby_migration_job_status(&job_id, "RUNNING", "ITEMS", None)
            .await?;

        database
            .commit_emby_migration_item_page(EmbyMigrationItemPageBatch {
                job_id: &job_id,
                merge_policy: "OVERWRITE",
                state_fields: EmbyMigrationUserItemStateFields::favorite_only(),
                item_matches: &[],
                states: &[EmbyMigrationUserItemStateBatch {
                    user_id: user_id.clone(),
                    item_id: item_id.clone(),
                    position_ticks: 0,
                    is_played: false,
                    is_favorite: true,
                    play_count: 0,
                    last_played_at: None,
                }],
                import_records: &[],
                handled_items: &[],
                progress: EmbyMigrationJobProgress {
                    id: &job_id,
                    cursor_json: "{}",
                    processed_count: 1,
                    total_count: 1,
                    matched_count: 1,
                    skipped_count: 0,
                    failed_count: 0,
                },
            })
            .await?;

        let state = database
            .find_user_item_state_for_migration(&user_id, &item_id)
            .await?
            .expect("favorite migration should retain the existing state");
        assert_eq!(state.position_ticks, 120);
        assert!(state.is_played);
        assert!(state.is_favorite);
        assert_eq!(state.play_count, 3);
        assert_eq!(state.last_played_at, Some(200));
        assert_eq!(state.version, 1);
        Ok(())
    }

    #[tokio::test]
    async fn batch_migration_reports_update_null_identity_fields()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let (user_id, item_id) = insert_test_user_and_item(&database).await?;
        let person_id = Uuid::now_v7().to_string();
        sqlx::query("INSERT INTO people (id, display_name, directory_name, normalized_name, status, created_at, updated_at) VALUES (?, ?, ?, ?, 'ACTIVE', unixepoch(), unixepoch())")
            .bind(&person_id)
            .bind("演员甲")
            .bind("演员甲")
            .bind("演员甲")
            .execute(database.pool())
            .await?;
        let job_id = Uuid::now_v7().to_string();
        database
            .insert_emby_migration_job(&NewEmbyMigrationJob {
                id: &job_id,
                created_by_user_id: &user_id,
                source_label: "Test Emby",
                source_base_url: "https://emby.example.test/",
                secret_ref: "emby-migration/test",
                dry_run: false,
                merge_policy: "MERGE",
                scope_json: r#"{"userProfile":false,"libraryAccess":false,"itemState":true,"personFavorites":true}"#,
                emby_user_ids_json: r#"["emby-user"]"#,
            })
            .await?;
        database
            .update_emby_migration_job_status(&job_id, "RUNNING", "ITEMS", None)
            .await?;

        let progress = || EmbyMigrationJobProgress {
            id: &job_id,
            cursor_json: "{}",
            processed_count: 1,
            total_count: 1,
            matched_count: 1,
            skipped_count: 0,
            failed_count: 0,
        };
        let initial_match = EmbyMigrationItemMatchBatch {
            emby_item_id: "emby-item".to_owned(),
            emby_item_type: "Movie".to_owned(),
            lux_item_id: None,
            match_method: "TMDB_ID".to_owned(),
            confidence: None,
            status: "MATCHED".to_owned(),
            detail_json: "{}".to_owned(),
        };
        database
            .commit_emby_migration_item_page(EmbyMigrationItemPageBatch {
                job_id: &job_id,
                merge_policy: "MERGE",
                state_fields: EmbyMigrationUserItemStateFields::all(),
                item_matches: std::slice::from_ref(&initial_match),
                states: &[],
                import_records: &[],
                handled_items: &[],
                progress: progress(),
            })
            .await?;
        let updated_match = EmbyMigrationItemMatchBatch {
            lux_item_id: Some(item_id.clone()),
            confidence: Some(95),
            ..initial_match
        };
        database
            .commit_emby_migration_item_page(EmbyMigrationItemPageBatch {
                job_id: &job_id,
                merge_policy: "MERGE",
                state_fields: EmbyMigrationUserItemStateFields::all(),
                item_matches: &[updated_match],
                states: &[],
                import_records: &[],
                handled_items: &[],
                progress: progress(),
            })
            .await?;
        let item_matches = database
            .list_emby_migration_item_matches(&job_id, 0, 10)
            .await?;
        assert_eq!(
            item_matches[0].lux_item_id.as_deref(),
            Some(item_id.as_str())
        );
        assert_eq!(item_matches[0].confidence, Some(95));

        let initial_import = EmbyMigrationImportRecordBatch {
            emby_user_id: "emby-user".to_owned(),
            emby_item_id: "emby-item".to_owned(),
            lux_user_id: user_id.clone(),
            lux_item_id: item_id.clone(),
            state_hash: "hash".to_owned(),
            status: "IMPORTED".to_owned(),
            error: None,
        };
        database
            .commit_emby_migration_item_page(EmbyMigrationItemPageBatch {
                job_id: &job_id,
                merge_policy: "MERGE",
                state_fields: EmbyMigrationUserItemStateFields::all(),
                item_matches: &[],
                states: &[],
                import_records: std::slice::from_ref(&initial_import),
                handled_items: &[],
                progress: progress(),
            })
            .await?;
        let updated_import = EmbyMigrationImportRecordBatch {
            error: Some("retry".to_owned()),
            ..initial_import
        };
        database
            .commit_emby_migration_item_page(EmbyMigrationItemPageBatch {
                job_id: &job_id,
                merge_policy: "MERGE",
                state_fields: EmbyMigrationUserItemStateFields::all(),
                item_matches: &[],
                states: &[],
                import_records: &[updated_import],
                handled_items: &[],
                progress: progress(),
            })
            .await?;
        let import_records = database
            .list_emby_migration_import_records(&job_id, 0, 10)
            .await?;
        assert_eq!(import_records[0].error.as_deref(), Some("retry"));

        let initial_favorite = EmbyMigrationPersonFavoriteBatch {
            emby_user_id: "emby-user".to_owned(),
            emby_person_id: "emby-person".to_owned(),
            emby_person_name: "演员甲".to_owned(),
            lux_user_id: None,
            lux_person_id: None,
            provider_ids_json: "{}".to_owned(),
            match_method: "NAME".to_owned(),
            confidence: None,
            status: "MATCHED".to_owned(),
            state_hash: "hash".to_owned(),
            detail_json: "{}".to_owned(),
            error: None,
        };
        database
            .commit_emby_migration_person_page(
                &job_id,
                std::slice::from_ref(&initial_favorite),
                &[],
                &progress(),
            )
            .await?;
        let updated_favorite = EmbyMigrationPersonFavoriteBatch {
            lux_user_id: Some(user_id.clone()),
            lux_person_id: Some(person_id.clone()),
            confidence: Some(90),
            ..initial_favorite
        };
        database
            .commit_emby_migration_person_page(&job_id, &[updated_favorite], &[], &progress())
            .await?;
        let person_favorites = database
            .list_emby_migration_person_favorites(&job_id, 0, 10)
            .await?;
        assert_eq!(person_favorites[0].lux_user_id, Some(user_id));
        assert_eq!(
            person_favorites[0].lux_person_id.as_deref(),
            Some(person_id.as_str())
        );
        assert_eq!(person_favorites[0].confidence, Some(90));
        Ok(())
    }

    #[tokio::test]
    async fn overwrite_imported_state_clears_last_played_at()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let (user_id, item_id) = insert_test_user_and_item(&database).await?;
        database
            .upsert_imported_user_item_state(&NewImportedUserItemState {
                user_id: &user_id,
                item_id: &item_id,
                position_ticks: 120,
                is_played: true,
                is_favorite: false,
                play_count: 3,
                last_played_at: Some(200),
            })
            .await?;

        database
            .merge_imported_user_item_state(
                &NewImportedUserItemState {
                    user_id: &user_id,
                    item_id: &item_id,
                    position_ticks: 120,
                    is_played: true,
                    is_favorite: false,
                    play_count: 3,
                    last_played_at: None,
                },
                "OVERWRITE",
            )
            .await?;

        let state = database
            .find_user_item_state_for_migration(&user_id, &item_id)
            .await?
            .expect("overwritten state should remain present");
        assert_eq!(state.position_ticks, 120);
        assert!(state.is_played);
        assert_eq!(state.play_count, 3);
        assert_eq!(state.last_played_at, None);
        assert_eq!(state.version, 1);
        Ok(())
    }

    #[tokio::test]
    async fn migration_item_page_commits_batch_and_progress_in_one_transaction()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let (user_id, item_id) = insert_test_user_and_item(&database).await?;
        let job_id = Uuid::now_v7().to_string();
        database
            .insert_emby_migration_job(&NewEmbyMigrationJob {
                id: &job_id,
                created_by_user_id: &user_id,
                source_label: "Test Emby",
                source_base_url: "https://emby.example.test/",
                secret_ref: "emby-migration/test",
                dry_run: false,
                merge_policy: "MERGE",
                scope_json: r#"{"userProfile":true,"libraryAccess":true,"itemState":true,"personFavorites":false}"#,
                emby_user_ids_json: r#"["emby-user"]"#,
            })
            .await?;
        database
            .update_emby_migration_job_status(&job_id, "RUNNING", "ITEMS", None)
            .await?;

        database.reset_query_count();
        database
            .commit_emby_migration_item_page(EmbyMigrationItemPageBatch {
                job_id: &job_id,
                merge_policy: "MERGE",
                state_fields: EmbyMigrationUserItemStateFields::all(),
                item_matches: &[EmbyMigrationItemMatchBatch {
                    emby_item_id: "emby-item".to_owned(),
                    emby_item_type: "Movie".to_owned(),
                    lux_item_id: Some(item_id.clone()),
                    match_method: "TMDB_ID".to_owned(),
                    confidence: Some(100),
                    status: "MATCHED".to_owned(),
                    detail_json: "{}".to_owned(),
                }],
                states: &[EmbyMigrationUserItemStateBatch {
                    user_id: user_id.clone(),
                    item_id: item_id.clone(),
                    position_ticks: 120,
                    is_played: true,
                    is_favorite: true,
                    play_count: 2,
                    last_played_at: Some(200),
                }],
                import_records: &[EmbyMigrationImportRecordBatch {
                    emby_user_id: "emby-user".to_owned(),
                    emby_item_id: "emby-item".to_owned(),
                    lux_user_id: user_id.clone(),
                    lux_item_id: item_id.clone(),
                    state_hash: "hash".to_owned(),
                    status: "IMPORTED".to_owned(),
                    error: None,
                }],
                handled_items: &[EmbyMigrationHandledItemBatch {
                    emby_user_id: "emby-user".to_owned(),
                    emby_item_id: "emby-item".to_owned(),
                }],
                progress: EmbyMigrationJobProgress {
                    id: &job_id,
                    cursor_json: r#"{"kind":"USER_STATE","userId":"emby-user","stateFilter":"PLAYED","startIndex":500}"#,
                    processed_count: 1,
                    total_count: 1,
                    matched_count: 1,
                    skipped_count: 0,
                    failed_count: 0,
                },
            })
            .await?;
        assert_eq!(database.query_count(), 5);

        let state = database
            .find_user_item_state_for_migration(&user_id, &item_id)
            .await?
            .expect("state should be committed");
        assert_eq!(state.position_ticks, 120);
        assert_eq!(state.version, 0);
        sqlx::query("CREATE TABLE migration_state_update_counts (count INTEGER NOT NULL)")
            .execute(database.pool())
            .await?;
        sqlx::query("INSERT INTO migration_state_update_counts (count) VALUES (0)")
            .execute(database.pool())
            .await?;
        sqlx::query(
            "CREATE TRIGGER count_repeated_migration_state_updates
             AFTER UPDATE ON user_item_state
             BEGIN
                 UPDATE migration_state_update_counts SET count = count + 1;
             END",
        )
        .execute(database.pool())
        .await?;
        database
            .commit_emby_migration_item_page(EmbyMigrationItemPageBatch {
                job_id: &job_id,
                merge_policy: "MERGE",
                state_fields: EmbyMigrationUserItemStateFields::all(),
                item_matches: &[EmbyMigrationItemMatchBatch {
                    emby_item_id: "emby-item".to_owned(),
                    emby_item_type: "Movie".to_owned(),
                    lux_item_id: Some(item_id.clone()),
                    match_method: "TMDB_ID".to_owned(),
                    confidence: Some(100),
                    status: "MATCHED".to_owned(),
                    detail_json: "{}".to_owned(),
                }],
                states: &[EmbyMigrationUserItemStateBatch {
                    user_id: user_id.clone(),
                    item_id: item_id.clone(),
                    position_ticks: 120,
                    is_played: true,
                    is_favorite: true,
                    play_count: 2,
                    last_played_at: Some(200),
                }],
                import_records: &[EmbyMigrationImportRecordBatch {
                    emby_user_id: "emby-user".to_owned(),
                    emby_item_id: "emby-item".to_owned(),
                    lux_user_id: user_id.clone(),
                    lux_item_id: item_id.clone(),
                    state_hash: "hash".to_owned(),
                    status: "IMPORTED".to_owned(),
                    error: None,
                }],
                handled_items: &[EmbyMigrationHandledItemBatch {
                    emby_user_id: "emby-user".to_owned(),
                    emby_item_id: "emby-item".to_owned(),
                }],
                progress: EmbyMigrationJobProgress {
                    id: &job_id,
                    cursor_json: r#"{"kind":"USER_STATE","userId":"emby-user","stateFilter":"FAVORITE","startIndex":0}"#,
                    processed_count: 1,
                    total_count: 1,
                    matched_count: 1,
                    skipped_count: 0,
                    failed_count: 0,
                },
            })
            .await?;
        let unchanged = database
            .find_user_item_state_for_migration(&user_id, &item_id)
            .await?
            .expect("state should remain present");
        assert_eq!(unchanged.version, 0);
        let repeated_state_updates: i64 =
            sqlx::query_scalar("SELECT count FROM migration_state_update_counts")
                .fetch_one(database.pool())
                .await?;
        assert_eq!(repeated_state_updates, 0);
        let imported_libraries = database
            .list_emby_migration_imported_library_ids(&job_id, "emby-user")
            .await?;
        assert_eq!(imported_libraries.len(), 1);
        let handled = database
            .list_emby_migration_handled_item_ids(
                &job_id,
                "emby-user",
                &["emby-item".to_owned(), "unhandled-item".to_owned()],
            )
            .await?;
        assert_eq!(handled, vec!["emby-item".to_owned()]);
        let job = database
            .find_emby_migration_job(&job_id)
            .await?
            .expect("job should be committed");
        assert_eq!(
            job.cursor_json,
            r#"{"kind":"USER_STATE","userId":"emby-user","stateFilter":"FAVORITE","startIndex":0}"#
        );
        Ok(())
    }

    #[tokio::test]
    async fn migration_person_page_batches_favorites_and_user_state()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let (user_id, _item_id) = insert_test_user_and_item(&database).await?;
        let person_id = Uuid::now_v7().to_string();
        sqlx::query("INSERT INTO people (id, display_name, directory_name, normalized_name, status, created_at, updated_at) VALUES (?, ?, ?, ?, 'ACTIVE', unixepoch(), unixepoch())")
            .bind(&person_id)
            .bind("演员甲")
            .bind("演员甲")
            .bind("演员甲")
            .execute(database.pool())
            .await?;
        let job_id = Uuid::now_v7().to_string();
        database
            .insert_emby_migration_job(&NewEmbyMigrationJob {
                id: &job_id,
                created_by_user_id: &user_id,
                source_label: "Test Emby",
                source_base_url: "https://emby.example.test/",
                secret_ref: "emby-migration/test",
                dry_run: false,
                merge_policy: "MERGE",
                scope_json: r#"{"userProfile":true,"libraryAccess":true,"itemState":false,"personFavorites":true}"#,
                emby_user_ids_json: r#"["emby-user"]"#,
            })
            .await?;
        database
            .update_emby_migration_job_status(&job_id, "RUNNING", "ITEMS", None)
            .await?;

        database.reset_query_count();
        database
            .commit_emby_migration_person_page(
                &job_id,
                &[EmbyMigrationPersonFavoriteBatch {
                    emby_user_id: "emby-user".to_owned(),
                    emby_person_id: "emby-person".to_owned(),
                    emby_person_name: "演员甲".to_owned(),
                    lux_user_id: Some(user_id.clone()),
                    lux_person_id: Some(person_id.clone()),
                    provider_ids_json: "{}".to_owned(),
                    match_method: "NAME".to_owned(),
                    confidence: Some(90),
                    status: "IMPORTED".to_owned(),
                    state_hash: "hash".to_owned(),
                    detail_json: "{}".to_owned(),
                    error: None,
                }],
                &[EmbyMigrationPersonFavoriteStateBatch {
                    user_id: user_id.clone(),
                    person_id: person_id.clone(),
                }],
                &EmbyMigrationJobProgress {
                    id: &job_id,
                    cursor_json: "{}",
                    processed_count: 1,
                    total_count: 1,
                    matched_count: 1,
                    skipped_count: 0,
                    failed_count: 0,
                },
            )
            .await?;
        assert_eq!(database.query_count(), 3);
        let favorite: i64 = sqlx::query_scalar(
            "SELECT is_favorite FROM user_person_state WHERE user_id = ? AND person_id = ?",
        )
        .bind(&user_id)
        .bind(&person_id)
        .fetch_one(database.pool())
        .await?;
        assert_eq!(favorite, 1);
        Ok(())
    }

    #[tokio::test]
    async fn migration_item_page_rolls_back_when_progress_cannot_advance()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let (user_id, item_id) = insert_test_user_and_item(&database).await?;
        let job_id = Uuid::now_v7().to_string();
        database
            .insert_emby_migration_job(&NewEmbyMigrationJob {
                id: &job_id,
                created_by_user_id: &user_id,
                source_label: "Test Emby",
                source_base_url: "https://emby.example.test/",
                secret_ref: "emby-migration/test",
                dry_run: false,
                merge_policy: "MERGE",
                scope_json: r#"{"userProfile":true,"libraryAccess":false,"itemState":true,"personFavorites":false}"#,
                emby_user_ids_json: r#"["emby-user"]"#,
            })
            .await?;
        database
            .update_emby_migration_job_status(&job_id, "RUNNING", "ITEMS", None)
            .await?;

        let result = database
            .commit_emby_migration_item_page(EmbyMigrationItemPageBatch {
                job_id: &job_id,
                merge_policy: "MERGE",
                state_fields: EmbyMigrationUserItemStateFields::all(),
                item_matches: &[EmbyMigrationItemMatchBatch {
                    emby_item_id: "emby-item".to_owned(),
                    emby_item_type: "Movie".to_owned(),
                    lux_item_id: Some(item_id.clone()),
                    match_method: "TMDB_ID".to_owned(),
                    confidence: Some(100),
                    status: "MATCHED".to_owned(),
                    detail_json: "{}".to_owned(),
                }],
                states: &[EmbyMigrationUserItemStateBatch {
                    user_id: user_id.clone(),
                    item_id: item_id.clone(),
                    position_ticks: 120,
                    is_played: true,
                    is_favorite: true,
                    play_count: 1,
                    last_played_at: Some(200),
                }],
                import_records: &[EmbyMigrationImportRecordBatch {
                    emby_user_id: "emby-user".to_owned(),
                    emby_item_id: "emby-item".to_owned(),
                    lux_user_id: user_id.clone(),
                    lux_item_id: item_id.clone(),
                    state_hash: "hash".to_owned(),
                    status: "IMPORTED".to_owned(),
                    error: None,
                }],
                handled_items: &[EmbyMigrationHandledItemBatch {
                    emby_user_id: "emby-user".to_owned(),
                    emby_item_id: "emby-item".to_owned(),
                }],
                progress: EmbyMigrationJobProgress {
                    id: "missing-job",
                    cursor_json: "{}",
                    processed_count: 1,
                    total_count: 1,
                    matched_count: 1,
                    skipped_count: 0,
                    failed_count: 0,
                },
            })
            .await;
        assert!(matches!(result, Err(StorageError::Conflict(_))));
        assert!(
            database
                .find_user_item_state_for_migration(&user_id, &item_id)
                .await?
                .is_none()
        );
        let matches = database
            .list_emby_migration_item_matches(&job_id, 0, 10)
            .await?;
        assert!(matches.is_empty());
        let records = database
            .list_emby_migration_import_records(&job_id, 0, 10)
            .await?;
        assert!(records.is_empty());
        Ok(())
    }

    #[tokio::test]
    #[ignore = "run manually to record the Emby migration page-write benchmark"]
    async fn migration_page_batch_benchmark_records_operation_counts()
    -> Result<(), Box<dyn std::error::Error>> {
        const PAGE_SIZE: usize = 500;

        let (_temp_dir, database) = test_database().await?;
        let (user_id, item_id) = insert_test_user_and_item(&database).await?;
        let job_id = Uuid::now_v7().to_string();
        database
            .insert_emby_migration_job(&NewEmbyMigrationJob {
                id: &job_id,
                created_by_user_id: &user_id,
                source_label: "Benchmark Emby",
                source_base_url: "https://emby.example.test/",
                secret_ref: "emby-migration/benchmark",
                dry_run: false,
                merge_policy: "MERGE",
                scope_json: r#"{"userProfile":false,"libraryAccess":false,"itemState":true,"personFavorites":false}"#,
                emby_user_ids_json: r#"["emby-user"]"#,
            })
            .await?;
        database
            .update_emby_migration_job_status(&job_id, "RUNNING", "ITEMS", None)
            .await?;

        let item_matches = (0..PAGE_SIZE)
            .map(|index| EmbyMigrationItemMatchBatch {
                emby_item_id: format!("emby-item-{index}"),
                emby_item_type: "Movie".to_owned(),
                lux_item_id: Some(item_id.clone()),
                match_method: "TMDB_ID".to_owned(),
                confidence: Some(100),
                status: "MATCHED".to_owned(),
                detail_json: "{}".to_owned(),
            })
            .collect::<Vec<_>>();
        let import_records = (0..PAGE_SIZE)
            .map(|index| EmbyMigrationImportRecordBatch {
                emby_user_id: "emby-user".to_owned(),
                emby_item_id: format!("emby-item-{index}"),
                lux_user_id: user_id.clone(),
                lux_item_id: item_id.clone(),
                state_hash: format!("hash-{index}"),
                status: "IMPORTED".to_owned(),
                error: None,
            })
            .collect::<Vec<_>>();
        let states = vec![EmbyMigrationUserItemStateBatch {
            user_id: user_id.clone(),
            item_id: item_id.clone(),
            position_ticks: 120,
            is_played: true,
            is_favorite: true,
            play_count: 1,
            last_played_at: Some(200),
        }];
        let handled_items = (0..PAGE_SIZE)
            .map(|index| EmbyMigrationHandledItemBatch {
                emby_user_id: "emby-user".to_owned(),
                emby_item_id: format!("emby-item-{index}"),
            })
            .collect::<Vec<_>>();

        let peak_rss_before = process_peak_rss_bytes();
        database.reset_query_count();
        let started = std::time::Instant::now();
        database
            .commit_emby_migration_item_page(EmbyMigrationItemPageBatch {
                job_id: &job_id,
                merge_policy: "MERGE",
                state_fields: EmbyMigrationUserItemStateFields::all(),
                item_matches: &item_matches,
                states: &states,
                import_records: &import_records,
                handled_items: &handled_items,
                progress: EmbyMigrationJobProgress {
                    id: &job_id,
                    cursor_json: r#"{"kind":"USER_STATE","userId":"emby-user","stateFilter":"PLAYED","startIndex":500}"#,
                    processed_count: PAGE_SIZE as i64,
                    total_count: PAGE_SIZE as i64,
                    matched_count: PAGE_SIZE as i64,
                    skipped_count: 0,
                    failed_count: 0,
                },
            })
            .await?;
        let elapsed = started.elapsed();
        let report = serde_json::json!({
            "benchmark": "emby_migration_page_batch",
            "os": std::env::consts::OS,
            "architecture": std::env::consts::ARCH,
            "sourceRpcCalls": 1,
            "effectiveItems": PAGE_SIZE,
            "skippedItems": 0,
            "matchedItems": PAGE_SIZE,
            "databaseStatements": database.query_count(),
            "databaseTransactions": 1,
            "elapsedMs": elapsed.as_millis(),
            "itemsPerSecond": PAGE_SIZE as f64 / elapsed.as_secs_f64().max(0.001),
            "peakRssBytes": process_peak_rss_bytes().max(peak_rss_before),
            "peakSourcePageRecords": PAGE_SIZE,
            "retryDuplicateSourceRequests": 0,
        });
        assert_eq!(database.query_count(), 17);
        println!("{report}");
        Ok(())
    }

    #[tokio::test]
    #[ignore = "run manually to record 500/5000/50000 item migration benchmarks"]
    async fn migration_page_batch_benchmark_records_scale() -> Result<(), Box<dyn std::error::Error>>
    {
        const PAGE_SIZE: usize = 500;

        let (_temp_dir, database) = test_database().await?;
        let (user_id, item_id) = insert_test_user_and_item(&database).await?;
        for total_items in [500_usize, 5_000, 50_000] {
            let job_id = Uuid::now_v7().to_string();
            database
                .insert_emby_migration_job(&NewEmbyMigrationJob {
                    id: &job_id,
                    created_by_user_id: &user_id,
                    source_label: "Benchmark Emby",
                    source_base_url: "https://emby.example.test/",
                    secret_ref: "emby-migration/benchmark-scale",
                    dry_run: false,
                    merge_policy: "MERGE",
                    scope_json: r#"{"userProfile":false,"libraryAccess":false,"itemState":true,"personFavorites":false}"#,
                    emby_user_ids_json: r#"["emby-user"]"#,
                })
                .await?;
            database
                .update_emby_migration_job_status(&job_id, "RUNNING", "ITEMS", None)
                .await?;

            database.reset_query_count();
            let started = std::time::Instant::now();
            let page_count = total_items.div_ceil(PAGE_SIZE);
            for page_index in 0..page_count {
                let page_start = page_index * PAGE_SIZE;
                let page_len = (total_items - page_start).min(PAGE_SIZE);
                let item_matches = (0..page_len)
                    .map(|index| {
                        let item_index = page_start + index;
                        EmbyMigrationItemMatchBatch {
                            emby_item_id: format!("emby-item-{total_items}-{item_index}"),
                            emby_item_type: "Movie".to_owned(),
                            lux_item_id: Some(item_id.clone()),
                            match_method: "TMDB_ID".to_owned(),
                            confidence: Some(100),
                            status: "MATCHED".to_owned(),
                            detail_json: "{}".to_owned(),
                        }
                    })
                    .collect::<Vec<_>>();
                let import_records = item_matches
                    .iter()
                    .map(|item_match| EmbyMigrationImportRecordBatch {
                        emby_user_id: "emby-user".to_owned(),
                        emby_item_id: item_match.emby_item_id.clone(),
                        lux_user_id: user_id.clone(),
                        lux_item_id: item_id.clone(),
                        state_hash: item_match.emby_item_id.clone(),
                        status: "IMPORTED".to_owned(),
                        error: None,
                    })
                    .collect::<Vec<_>>();
                let handled_items = item_matches
                    .iter()
                    .map(|item_match| EmbyMigrationHandledItemBatch {
                        emby_user_id: "emby-user".to_owned(),
                        emby_item_id: item_match.emby_item_id.clone(),
                    })
                    .collect::<Vec<_>>();
                let states = if page_index == 0 {
                    vec![EmbyMigrationUserItemStateBatch {
                        user_id: user_id.clone(),
                        item_id: item_id.clone(),
                        position_ticks: 120,
                        is_played: true,
                        is_favorite: true,
                        play_count: 1,
                        last_played_at: Some(200),
                    }]
                } else {
                    Vec::new()
                };
                database
                    .commit_emby_migration_item_page(EmbyMigrationItemPageBatch {
                        job_id: &job_id,
                        merge_policy: "MERGE",
                        state_fields: EmbyMigrationUserItemStateFields::all(),
                        item_matches: &item_matches,
                        states: &states,
                        import_records: &import_records,
                        handled_items: &handled_items,
                        progress: EmbyMigrationJobProgress {
                            id: &job_id,
                            cursor_json: "{}",
                            processed_count: (page_start + page_len) as i64,
                            total_count: total_items as i64,
                            matched_count: (page_start + page_len) as i64,
                            skipped_count: 0,
                            failed_count: 0,
                        },
                    })
                    .await?;
            }
            let elapsed = started.elapsed();
            let report = serde_json::json!({
                "benchmark": "emby_migration_page_batch_scale",
                "os": std::env::consts::OS,
                "architecture": std::env::consts::ARCH,
                "effectiveItems": total_items,
                "databaseStatements": database.query_count(),
                "databaseTransactions": page_count,
                "elapsedMs": elapsed.as_millis(),
                "itemsPerSecond": total_items as f64 / elapsed.as_secs_f64().max(0.001),
                "peakRssBytes": process_peak_rss_bytes(),
            });
            println!("{report}");
        }
        Ok(())
    }

    fn process_peak_rss_bytes() -> u64 {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: getrusage initializes the provided rusage structure on success.
        let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
        if result != 0 {
            return 0;
        }
        // SAFETY: result == 0 proves getrusage initialized usage.
        let usage = unsafe { usage.assume_init() };
        #[cfg(target_os = "macos")]
        let multiplier = 1_u64;
        #[cfg(not(target_os = "macos"))]
        let multiplier = 1024_u64;
        u64::try_from(usage.ru_maxrss).unwrap_or_default() * multiplier
    }

    #[tokio::test]
    async fn person_favorite_report_is_upserted_and_paginated()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let (user_id, _item_id) = insert_test_user_and_item(&database).await?;
        let job_id = Uuid::now_v7().to_string();
        database
            .insert_emby_migration_job(&NewEmbyMigrationJob {
                id: &job_id,
                created_by_user_id: &user_id,
                source_label: "Test Emby",
                source_base_url: "https://emby.example.test/",
                secret_ref: "emby-migration/test",
                dry_run: true,
                merge_policy: "MERGE",
                scope_json: r#"{"userProfile":true,"libraryAccess":true,"itemState":true,"personFavorites":true}"#,
                emby_user_ids_json: r#"["emby-user"]"#,
            })
            .await?;

        database
            .upsert_emby_migration_person_favorite(&NewEmbyMigrationPersonFavorite {
                job_id: &job_id,
                emby_user_id: "emby-user",
                emby_person_id: "person-1",
                emby_person_name: "演员甲",
                lux_user_id: Some(&user_id),
                lux_person_id: None,
                provider_ids_json: r#"{"tmdb":"123"}"#,
                match_method: "TMDB_ID",
                confidence: Some(100),
                status: "MATCHED",
                state_hash: "hash-1",
                detail_json: r#"{"sourceType":"Person"}"#,
                error: None,
            })
            .await?;
        let records = database
            .list_emby_migration_person_favorites(&job_id, 0, 10)
            .await?;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].emby_person_name, "演员甲");
        assert_eq!(records[0].provider_ids_json, r#"{"tmdb":"123"}"#);
        assert_eq!(records[0].status, "MATCHED");
        Ok(())
    }

    #[tokio::test]
    async fn migration_job_progress_and_cancellation_are_persisted()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let (user_id, _item_id) = insert_test_user_and_item(&database).await?;
        let job_id = Uuid::now_v7().to_string();

        database
            .insert_emby_migration_job(&NewEmbyMigrationJob {
                id: &job_id,
                created_by_user_id: &user_id,
                source_label: "Test Emby",
                source_base_url: "https://emby.example.test/",
                secret_ref: "emby-migration/test",
                dry_run: true,
                merge_policy: "MERGE",
                scope_json: r#"{"userProfile":true,"libraryAccess":true,"itemState":true,"personFavorites":true}"#,
                emby_user_ids_json: r#"["emby-user"]"#,
            })
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
                .update_emby_migration_job_progress(&EmbyMigrationJobProgress {
                    id: &job_id,
                    cursor_json: r#"{"page":1}"#,
                    processed_count: 5,
                    total_count: 10,
                    matched_count: 4,
                    skipped_count: 1,
                    failed_count: 0,
                })
                .await?
        );
        assert!(database.request_emby_migration_cancel(&job_id).await?);
        assert!(database.emby_migration_cancel_requested(&job_id).await?);

        let job = database
            .find_emby_migration_job(&job_id)
            .await?
            .expect("migration job should be stored");
        assert_eq!(job.status, "RUNNING");
        assert_eq!(job.phase, "ITEMS");
        assert_eq!(job.history_capability, "EVENT_HISTORY");
        assert_eq!(job.emby_user_ids_json, r#"["emby-user"]"#);
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
