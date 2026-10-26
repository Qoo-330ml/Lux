use super::*;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

const DATABASE_CLEANUP_MARKER: &str = "database_lifecycle_cleanup_v1";
const MEDIA_SOURCE_DEFAULT_REPAIR_MARKER: &str = "startup_repair_media_source_defaults_v1";
const DATABASE_CLEANUP_PENDING: &str = "PENDING";
const DATABASE_CLEANUP_RUNNING: &str = "RUNNING";
const DATABASE_CLEANUP_COMPLETED: &str = "COMPLETED";
const SCAN_JOB_EVENTS_LOG_MIGRATION_MARKER: &str = "scan_job_events_config_log_migration_v1";
const AUDIT_EVENTS_LOG_MIGRATION_MARKER: &str = "audit_events_config_log_migration_v1";
const CLEANUP_BATCH_SIZE: i64 = 1_000;
/// Grace period so a batch is never removed while its worker is still finishing it.
const LOCAL_METADATA_BATCH_RETENTION_SECONDS: i64 = 3_600;
const MAX_LOG_MIGRATION_BATCH_BYTES: u64 = crate::observability::logs::LOG_SEGMENT_BYTES / 2;

/// Expired tombstones that can be dropped without losing anything the user earned.
///
/// Merge targets still block the delete. User state and playback history no longer do when the item
/// carries a provider identity: that identity is what lets the archived copies find the item again,
/// so both are archived instead of pinning the row forever. An unidentified item keeps its row,
/// because there would be nothing left to match its history by later.
const PURGEABLE_REMOVED_ITEM_SELECT: &str = "
SELECT mi.id FROM media_items mi
WHERE mi.removed_at IS NOT NULL
  AND mi.removed_at < unixepoch() - ?
  AND mi.item_type IN ('MOVIE', 'EPISODE', 'VIDEO', 'UNRESOLVED')
  AND NOT EXISTS (
      SELECT 1 FROM media_items m2 WHERE m2.merged_into_item_id = mi.id)
  AND (
      (
          NOT EXISTS (SELECT 1 FROM user_item_state s WHERE s.item_id = mi.id)
          AND NOT EXISTS (SELECT 1 FROM playback_history_events e WHERE e.item_id = mi.id)
      )
      OR EXISTS (SELECT 1 FROM media_item_provider_ids p WHERE p.media_item_id = mi.id)
  )
ORDER BY mi.removed_at
LIMIT ?";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DatabaseLifecycleCleanupReport {
    pub scan_job_paths_deleted: u64,
    pub reconciliation_entries_deleted: u64,
    pub scan_manifest_deltas_deleted: u64,
    pub scan_manifest_entries_deleted: u64,
    pub scan_manifest_directories_deleted: u64,
    pub scan_local_metadata_batches_deleted: u64,
    pub scan_job_targets_deleted: u64,
    pub scan_jobs_summarized: u64,
}

impl Database {
    pub async fn migrate_legacy_scan_job_events_to_logs(&self) -> Result<u64, StorageError> {
        if self
            .is_config_log_migration_complete(SCAN_JOB_EVENTS_LOG_MIGRATION_MARKER)
            .await?
        {
            return Ok(0);
        }
        let source_event_count: i64 = self
            .query_scalar("SELECT COUNT(*) FROM scan_job_events")
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if source_event_count == 0 {
            self.mark_config_log_migration_complete(SCAN_JOB_EVENTS_LOG_MIGRATION_MARKER)
                .await?;
            return Ok(0);
        }
        let mut migrated = 0_u64;
        loop {
            let rows = self
                .query(
                    "SELECT id, job_id, level, event_code, message, details_json, created_at
                     FROM scan_job_events
                     ORDER BY created_at, id
                     LIMIT ?",
                )
                .bind(CLEANUP_BATCH_SIZE)
                .fetch_all(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if rows.is_empty() {
                break;
            }

            // Keep a batch within one UTC day so archive retention cannot prune an
            // earlier part of the batch before its database rows are acknowledged.
            let batch_date = rows.first().and_then(|row| {
                let created_at: i64 = row.get("created_at");
                OffsetDateTime::from_unix_timestamp(created_at)
                    .ok()
                    .map(|timestamp| timestamp.date())
            });
            let mut ids = Vec::with_capacity(rows.len());
            let mut records = Vec::with_capacity(rows.len());
            let mut batch_bytes = 0_u64;
            for row in rows {
                let id: String = row.get("id");
                let job_id: String = row.get("job_id");
                let level: String = row.get("level");
                let event_code: String = row.get("event_code");
                let message: String = row.get("message");
                let details_json: String = row.get("details_json");
                let created_at: i64 = row.get("created_at");
                let event_date = OffsetDateTime::from_unix_timestamp(created_at)
                    .ok()
                    .map(|timestamp| timestamp.date());
                if batch_date.is_some() && event_date != batch_date
                    || batch_date.is_none() && !ids.is_empty()
                {
                    break;
                }

                let details = Self::redact_legacy_log_json(&details_json, "task event details")?;
                let message = crate::observability::logs::redact_sensitive_log_values(
                    serde_json::Value::String(message),
                )
                .as_str()
                .unwrap_or_default()
                .to_owned();
                let timestamp = OffsetDateTime::from_unix_timestamp(created_at)
                    .ok()
                    .and_then(|timestamp| timestamp.format(&Rfc3339).ok())
                    .unwrap_or_else(|| created_at.to_string());
                let record = serde_json::json!({
                    "recordType": "scan_job_event",
                    "id": id.clone(),
                    "jobId": job_id,
                    "level": level,
                    "eventCode": event_code,
                    "message": message,
                    "details": details,
                    "timestamp": timestamp,
                    "createdAt": created_at,
                });
                let record_bytes = u64::try_from(
                    serde_json::to_vec(&record)
                        .map_err(|source| StorageError::Io {
                            path: self.path.clone(),
                            source: std::io::Error::other(source),
                        })?
                        .len()
                        .saturating_add(1),
                )
                .unwrap_or(u64::MAX);
                if !ids.is_empty()
                    && batch_bytes.saturating_add(record_bytes) > MAX_LOG_MIGRATION_BATCH_BYTES
                {
                    break;
                }
                ids.push(id);
                batch_bytes = batch_bytes.saturating_add(record_bytes);
                records.push(record);
            }

            self.log_store
                .suspend_archive_pruning()
                .await
                .map_err(|source| StorageError::Io {
                    path: self.path.clone(),
                    source,
                })?;
            let existing_event_ids = self
                .log_store
                .existing_scan_job_event_ids(&ids)
                .await
                .map_err(|source| StorageError::Io {
                    path: self.path.clone(),
                    source,
                })?;
            records.retain(|record| {
                record["id"]
                    .as_str()
                    .is_none_or(|id| !existing_event_ids.contains(id))
            });
            self.log_store
                .append_json_batch_and_sync(records)
                .await
                .map_err(|source| StorageError::Io {
                    path: self.path.clone(),
                    source,
                })?;
            self.log_store
                .verify_scan_job_event_ids(&ids)
                .await
                .map_err(|source| StorageError::Io {
                    path: self.path.clone(),
                    source,
                })?;

            let placeholders = std::iter::repeat_n("?", ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            let delete_query = format!("DELETE FROM scan_job_events WHERE id IN ({placeholders})");
            let mut statement = self.query(sqlx::AssertSqlSafe(delete_query));
            for id in &ids {
                statement = statement.bind(id);
            }
            let deleted = statement
                .execute(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
                .rows_affected();
            self.log_store
                .prune_archives()
                .await
                .map_err(|source| StorageError::Io {
                    path: self.path.clone(),
                    source,
                })?;
            migrated = migrated.saturating_add(deleted);
        }
        self.mark_config_log_migration_complete(SCAN_JOB_EVENTS_LOG_MIGRATION_MARKER)
            .await?;
        Ok(migrated)
    }

    pub async fn migrate_legacy_audit_events_to_logs(&self) -> Result<u64, StorageError> {
        if self
            .is_config_log_migration_complete(AUDIT_EVENTS_LOG_MIGRATION_MARKER)
            .await?
        {
            return Ok(0);
        }
        let source_event_count: i64 = self
            .query_scalar("SELECT COUNT(*) FROM audit_events")
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if source_event_count == 0 {
            self.mark_config_log_migration_complete(AUDIT_EVENTS_LOG_MIGRATION_MARKER)
                .await?;
            return Ok(0);
        }

        let mut migrated = 0_u64;
        loop {
            let rows = self
                .query(
                    "SELECT ae.id, ae.actor_user_id, users.username_normalized AS actor_username,
                            ae.event_type, ae.target_type, ae.target_id,
                            ae.metadata_json, ae.created_at
                     FROM audit_events ae
                     LEFT JOIN users ON users.id = ae.actor_user_id
                     ORDER BY ae.created_at, ae.id
                     LIMIT ?",
                )
                .bind(CLEANUP_BATCH_SIZE)
                .fetch_all(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if rows.is_empty() {
                break;
            }

            let batch_date = rows.first().and_then(|row| {
                let created_at: i64 = row.get("created_at");
                OffsetDateTime::from_unix_timestamp(created_at)
                    .ok()
                    .map(|timestamp| timestamp.date())
            });
            let mut ids = Vec::with_capacity(rows.len());
            let mut records = Vec::with_capacity(rows.len());
            let mut batch_bytes = 0_u64;
            for row in rows {
                let id: String = row.get("id");
                let actor_user_id: Option<String> = row.get("actor_user_id");
                let actor_username: Option<String> = row.get("actor_username");
                let event_type: String = row.get("event_type");
                let target_type: Option<String> = row.get("target_type");
                let target_id: Option<String> = row.get("target_id");
                let metadata_json: String = row.get("metadata_json");
                let created_at: i64 = row.get("created_at");
                let event_date = OffsetDateTime::from_unix_timestamp(created_at)
                    .ok()
                    .map(|timestamp| timestamp.date());
                if batch_date.is_some() && event_date != batch_date
                    || batch_date.is_none() && !ids.is_empty()
                {
                    break;
                }

                let metadata = Self::redact_legacy_log_json(&metadata_json, "audit metadata")?;
                let timestamp = OffsetDateTime::from_unix_timestamp(created_at)
                    .ok()
                    .and_then(|timestamp| timestamp.format(&Rfc3339).ok())
                    .unwrap_or_else(|| created_at.to_string());
                let record = serde_json::json!({
                    "recordType": "admin_audit_event",
                    "id": id.clone(),
                    "actorUserId": actor_user_id,
                    "actorUsername": actor_username,
                    "eventType": event_type,
                    "targetType": target_type,
                    "targetId": target_id,
                    "metadata": metadata,
                    "timestamp": timestamp,
                    "createdAt": created_at,
                });
                let record_bytes = u64::try_from(
                    serde_json::to_vec(&record)
                        .map_err(|source| StorageError::Io {
                            path: self.path.clone(),
                            source: std::io::Error::other(source),
                        })?
                        .len()
                        .saturating_add(1),
                )
                .unwrap_or(u64::MAX);
                if !ids.is_empty()
                    && batch_bytes.saturating_add(record_bytes) > MAX_LOG_MIGRATION_BATCH_BYTES
                {
                    break;
                }
                ids.push(id);
                batch_bytes = batch_bytes.saturating_add(record_bytes);
                records.push(record);
            }

            self.log_store
                .suspend_archive_pruning()
                .await
                .map_err(|source| StorageError::Io {
                    path: self.path.clone(),
                    source,
                })?;
            let existing_event_ids = self
                .log_store
                .existing_audit_event_ids(&ids)
                .await
                .map_err(|source| StorageError::Io {
                    path: self.path.clone(),
                    source,
                })?;
            records.retain(|record| {
                record["id"]
                    .as_str()
                    .is_none_or(|id| !existing_event_ids.contains(id))
            });
            self.log_store
                .append_json_batch_and_sync(records)
                .await
                .map_err(|source| StorageError::Io {
                    path: self.path.clone(),
                    source,
                })?;
            self.log_store
                .verify_audit_event_ids(&ids)
                .await
                .map_err(|source| StorageError::Io {
                    path: self.path.clone(),
                    source,
                })?;

            let placeholders = std::iter::repeat_n("?", ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            let delete_query = format!("DELETE FROM audit_events WHERE id IN ({placeholders})");
            let mut statement = self.query(sqlx::AssertSqlSafe(delete_query));
            for id in &ids {
                statement = statement.bind(id);
            }
            let deleted = statement
                .execute(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
                .rows_affected();
            self.log_store
                .prune_archives()
                .await
                .map_err(|source| StorageError::Io {
                    path: self.path.clone(),
                    source,
                })?;
            migrated = migrated.saturating_add(deleted);
        }
        self.mark_config_log_migration_complete(AUDIT_EVENTS_LOG_MIGRATION_MARKER)
            .await?;
        Ok(migrated)
    }

    async fn is_config_log_migration_complete(&self, key: &str) -> Result<bool, StorageError> {
        let value: Option<String> = self
            .query_scalar("SELECT value FROM lux_meta WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(value.as_deref() == Some(DATABASE_CLEANUP_COMPLETED))
    }

    async fn mark_config_log_migration_complete(&self, key: &str) -> Result<(), StorageError> {
        let updated = self
            .query("UPDATE lux_meta SET value = ? WHERE key = ?")
            .bind(DATABASE_CLEANUP_COMPLETED)
            .bind(key)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if updated.rows_affected() == 0 {
            self.query("INSERT INTO lux_meta (key, value) VALUES (?, ?)")
                .bind(key)
                .bind(DATABASE_CLEANUP_COMPLETED)
                .execute(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }
        Ok(())
    }

    fn redact_legacy_log_json(
        value: &str,
        description: &str,
    ) -> Result<serde_json::Value, StorageError> {
        serde_json::from_str::<serde_json::Value>(value)
            .map(crate::observability::logs::redact_sensitive_log_values)
            .map_err(|_| {
                StorageError::Conflict(format!(
                    "legacy {description} is not valid JSON; source row was retained"
                ))
            })
    }

    /// Runs the one-time cleanup installed by the database lifecycle migration.
    ///
    /// The marker is claimed atomically and only marked completed after every
    /// bounded batch has committed. A failed or interrupted run can therefore
    /// be retried by the next container start without losing resumable work.
    pub async fn run_database_lifecycle_cleanup(
        &self,
    ) -> Result<Option<DatabaseLifecycleCleanupReport>, StorageError> {
        self.reset_interrupted_database_cleanup().await?;
        if !self.claim_database_cleanup().await? {
            let report = self.cleanup_completed_scan_manifest_payloads().await?;
            return if report.scan_manifest_deltas_deleted > 0
                || report.scan_manifest_entries_deleted > 0
                || report.scan_manifest_directories_deleted > 0
                || report.scan_local_metadata_batches_deleted > 0
            {
                Ok(Some(report))
            } else {
                Ok(None)
            };
        }

        let cleanup_result = self.perform_database_cleanup().await;
        match cleanup_result {
            Ok(report) => {
                if let Err(error) = self.mark_database_cleanup_completed().await {
                    let _ = self.reset_database_cleanup_marker().await;
                    return Err(error);
                }
                Ok(Some(report))
            }
            Err(error) => {
                let _ = self.reset_database_cleanup_marker().await;
                Err(error)
            }
        }
    }

    async fn reset_interrupted_database_cleanup(&self) -> Result<(), StorageError> {
        self.query(
            "UPDATE lux_meta
             SET value = ?
             WHERE key = ? AND value = ?",
        )
        .bind(DATABASE_CLEANUP_PENDING)
        .bind(DATABASE_CLEANUP_MARKER)
        .bind(DATABASE_CLEANUP_RUNNING)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    async fn claim_database_cleanup(&self) -> Result<bool, StorageError> {
        let result = self
            .query(
                "UPDATE lux_meta
                 SET value = ?
                 WHERE key = ? AND value = ?",
            )
            .bind(DATABASE_CLEANUP_RUNNING)
            .bind(DATABASE_CLEANUP_MARKER)
            .bind(DATABASE_CLEANUP_PENDING)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result.rows_affected() == 1)
    }

    async fn reset_database_cleanup_marker(&self) -> Result<(), StorageError> {
        self.query(
            "UPDATE lux_meta
             SET value = ?
             WHERE key = ? AND value = ?",
        )
        .bind(DATABASE_CLEANUP_PENDING)
        .bind(DATABASE_CLEANUP_MARKER)
        .bind(DATABASE_CLEANUP_RUNNING)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    async fn mark_database_cleanup_completed(&self) -> Result<(), StorageError> {
        self.query(
            "UPDATE lux_meta
             SET value = ?
             WHERE key = ? AND value = ?",
        )
        .bind(DATABASE_CLEANUP_COMPLETED)
        .bind(DATABASE_CLEANUP_MARKER)
        .bind(DATABASE_CLEANUP_RUNNING)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    async fn perform_database_cleanup(
        &self,
    ) -> Result<DatabaseLifecycleCleanupReport, StorageError> {
        let manifest_payload = self.cleanup_completed_scan_manifest_payloads().await?;
        Ok(DatabaseLifecycleCleanupReport {
            scan_job_paths_deleted: self.delete_completed_scan_job_paths().await?,
            reconciliation_entries_deleted: self.delete_completed_reconciliation_entries().await?,
            scan_manifest_deltas_deleted: manifest_payload.scan_manifest_deltas_deleted,
            scan_manifest_entries_deleted: manifest_payload.scan_manifest_entries_deleted,
            scan_manifest_directories_deleted: manifest_payload.scan_manifest_directories_deleted,
            scan_local_metadata_batches_deleted: manifest_payload
                .scan_local_metadata_batches_deleted,
            scan_job_targets_deleted: self.delete_non_retryable_scan_job_targets().await?,
            scan_jobs_summarized: self.summarize_terminal_scan_jobs().await?,
        })
    }

    pub(crate) async fn cleanup_completed_scan_manifest_payloads(
        &self,
    ) -> Result<DatabaseLifecycleCleanupReport, StorageError> {
        let scan_manifest_deltas_deleted = self.delete_completed_scan_manifest_deltas().await?;
        let scan_manifest_entries_deleted = self
            .delete_completed_scan_manifest_entries()
            .await?
            .saturating_add(self.delete_completed_scan_manifest_seen_paths().await?);
        self.query(
            "UPDATE scan_manifest_roots SET postprocessing_target_cursor = NULL
             WHERE postprocessing_target_cursor IS NOT NULL
               AND manifest_id IN (
                   SELECT id FROM scan_manifests WHERE state = 'COMPLETED'
               )",
        )
        .execute(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        let scan_manifest_directories_deleted =
            self.delete_completed_scan_manifest_directories().await?;
        let scan_local_metadata_batches_deleted =
            self.delete_terminal_scan_local_metadata_batches().await?;
        Ok(DatabaseLifecycleCleanupReport {
            scan_manifest_deltas_deleted,
            scan_manifest_entries_deleted,
            scan_manifest_directories_deleted,
            scan_local_metadata_batches_deleted,
            ..DatabaseLifecycleCleanupReport::default()
        })
    }

    /// Local-metadata batches carry the full source id list of up to 256 sources. Once
    /// a batch is COMPLETED (or CANCELLED) and its scan job is no longer running nothing
    /// reads it again: the batch id is only consulted while a job is still enqueueing.
    async fn delete_terminal_scan_local_metadata_batches(&self) -> Result<u64, StorageError> {
        let mut deleted = 0_u64;
        loop {
            let count = self
                .query(
                    "DELETE FROM scan_local_metadata_batches
                     WHERE id IN (
                         SELECT batch.id
                         FROM scan_local_metadata_batches batch
                         WHERE batch.status IN ('COMPLETED', 'CANCELLED')
                           AND batch.updated_at < unixepoch() - ?
                           AND NOT EXISTS (
                               SELECT 1 FROM scan_jobs sj
                               WHERE sj.id = batch.job_id
                                 AND NOT (
                                     (sj.status IN ('COMPLETED', 'FAILED')
                                      AND sj.scan_phase = 'IDLE')
                                     OR sj.status = 'CANCELLED'
                                 )
                           )
                         LIMIT ?
                     )",
                )
                .bind(LOCAL_METADATA_BATCH_RETENTION_SECONDS)
                .bind(CLEANUP_BATCH_SIZE)
                .execute(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
                .rows_affected();
            if count == 0 {
                break;
            }
            deleted = deleted.saturating_add(count);
        }
        Ok(deleted)
    }

    async fn delete_completed_scan_manifest_deltas(&self) -> Result<u64, StorageError> {
        let mut deleted = 0_u64;
        loop {
            let count = self
                .query(
                    "DELETE FROM scan_manifest_deltas
                     WHERE id IN (
                         SELECT delta.id
                         FROM scan_manifest_deltas delta
                         JOIN scan_manifests manifest ON manifest.id = delta.manifest_id
                         WHERE manifest.state = 'COMPLETED'
                         LIMIT ?
                     )",
                )
                .bind(CLEANUP_BATCH_SIZE)
                .execute(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
                .rows_affected();
            if count == 0 {
                break;
            }
            deleted = deleted.saturating_add(count);
        }
        Ok(deleted)
    }

    async fn delete_completed_scan_manifest_seen_paths(&self) -> Result<u64, StorageError> {
        let mut deleted = 0_u64;
        loop {
            let count = self
                .query(
                    "DELETE FROM scan_manifest_seen_paths
                     WHERE (manifest_id, library_root_id, relative_path) IN (
                         SELECT seen.manifest_id, seen.library_root_id, seen.relative_path
                         FROM scan_manifest_seen_paths seen
                         JOIN scan_manifests manifest ON manifest.id = seen.manifest_id
                         WHERE manifest.state = 'COMPLETED'
                         LIMIT ?
                     )",
                )
                .bind(CLEANUP_BATCH_SIZE)
                .execute(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
                .rows_affected();
            if count == 0 {
                break;
            }
            deleted = deleted.saturating_add(count);
        }
        Ok(deleted)
    }

    async fn delete_completed_scan_manifest_entries(&self) -> Result<u64, StorageError> {
        let mut deleted = 0_u64;
        loop {
            let count = self
                .query(
                    "DELETE FROM scan_manifest_entries
                     WHERE (manifest_id, library_root_id, relative_path, observation_sequence) IN (
                         SELECT entry.manifest_id, entry.library_root_id, entry.relative_path,
                                entry.observation_sequence
                         FROM scan_manifest_entries entry
                         JOIN scan_manifests manifest ON manifest.id = entry.manifest_id
                         WHERE manifest.state = 'COMPLETED'
                         LIMIT ?
                     )",
                )
                .bind(CLEANUP_BATCH_SIZE)
                .execute(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
                .rows_affected();
            if count == 0 {
                break;
            }
            deleted = deleted.saturating_add(count);
        }
        Ok(deleted)
    }

    async fn delete_completed_scan_manifest_directories(&self) -> Result<u64, StorageError> {
        let mut deleted = 0_u64;
        loop {
            let count = self
                .query(
                    "DELETE FROM scan_manifest_directories
                     WHERE (manifest_id, library_root_id, relative_path) IN (
                         SELECT directory.manifest_id, directory.library_root_id,
                                directory.relative_path
                         FROM scan_manifest_directories directory
                         JOIN scan_manifests manifest ON manifest.id = directory.manifest_id
                         WHERE manifest.state = 'COMPLETED'
                         LIMIT ?
                     )",
                )
                .bind(CLEANUP_BATCH_SIZE)
                .execute(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
                .rows_affected();
            if count == 0 {
                break;
            }
            deleted = deleted.saturating_add(count);
        }
        Ok(deleted)
    }

    async fn delete_completed_scan_job_paths(&self) -> Result<u64, StorageError> {
        let mut deleted = 0_u64;
        loop {
            let count = self
                .query(
                    "DELETE FROM scan_job_paths
                     WHERE (job_id, library_root_id, relative_path) IN (
                         SELECT sjp.job_id, sjp.library_root_id, sjp.relative_path
                         FROM scan_job_paths sjp
                         JOIN scan_jobs sj ON sj.id = sjp.job_id
                         WHERE (sj.status = 'COMPLETED' AND sj.scan_phase = 'IDLE')
                            OR sj.status = 'CANCELLED'
                         LIMIT ?
                     )",
                )
                .bind(CLEANUP_BATCH_SIZE)
                .execute(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
                .rows_affected();
            if count == 0 {
                break;
            }
            deleted = deleted.saturating_add(count);
        }
        Ok(deleted)
    }

    async fn delete_completed_reconciliation_entries(&self) -> Result<u64, StorageError> {
        let mut deleted = 0_u64;
        loop {
            let count = self
                .query(
                    "DELETE FROM reconciliation_scan_entries
                     WHERE (job_id, library_root_id, entry_type, relative_path) IN (
                         SELECT rse.job_id, rse.library_root_id, rse.entry_type, rse.relative_path
                         FROM reconciliation_scan_entries rse
                         JOIN scan_jobs sj ON sj.id = rse.job_id
                         WHERE (sj.status = 'COMPLETED' AND sj.scan_phase = 'IDLE')
                            OR (sj.status = 'CANCELLED'
                                AND COALESCE(sj.error, '') <>
                                    'LEGACY_SCAN_REQUIRES_NEW_MANIFEST')
                         LIMIT ?
                     )",
                )
                .bind(CLEANUP_BATCH_SIZE)
                .execute(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
                .rows_affected();
            if count == 0 {
                break;
            }
            deleted = deleted.saturating_add(count);
        }
        Ok(deleted)
    }

    async fn delete_non_retryable_scan_job_targets(&self) -> Result<u64, StorageError> {
        let mut deleted = 0_u64;
        loop {
            let count = self
                .query(
                    "DELETE FROM scan_job_targets
                     WHERE (job_id, target_type, target_id) IN (
                         SELECT target.job_id, target.target_type, target.target_id
                         FROM scan_job_targets target
                         JOIN scan_jobs sj ON sj.id = target.job_id
                         WHERE (
                             (sj.status IN ('COMPLETED', 'FAILED') AND sj.scan_phase = 'IDLE')
                             OR sj.status = 'CANCELLED'
                         )
                         AND target.probe_state NOT IN ('PENDING', 'FAILED')
                         AND target.metadata_state NOT IN ('PENDING', 'FAILED')
                         AND target.thumbnail_state NOT IN ('PENDING', 'FAILED')
                         LIMIT ?
                     )",
                )
                .bind(CLEANUP_BATCH_SIZE)
                .execute(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
                .rows_affected();
            if count == 0 {
                break;
            }
            deleted = deleted.saturating_add(count);
        }
        Ok(deleted)
    }

    async fn summarize_terminal_scan_jobs(&self) -> Result<u64, StorageError> {
        let result = self
            .query(
                "UPDATE scan_jobs
                 SET cursor = NULL,
                     current_item = NULL,
                     cancel_requested = CASE
                         WHEN status IN ('COMPLETED', 'FAILED', 'CANCELLED') THEN 0
                         ELSE cancel_requested
                     END,
                     updated_at = unixepoch()
                 WHERE (
                         status IN ('FAILED', 'CANCELLED')
                         OR (status = 'COMPLETED' AND scan_phase = 'IDLE')
                       )
                   AND (cursor IS NOT NULL OR current_item IS NOT NULL OR cancel_requested <> 0)",
            )
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result.rows_affected())
    }
}

const REMOVED_ITEM_RETENTION_ENV: &str = "LUX_REMOVED_ITEM_RETENTION_DAYS";
const DEFAULT_REMOVED_ITEM_RETENTION_DAYS: i64 = 30;
const REMOVED_ITEM_PURGE_BATCH_SIZE: i64 = 200;
const REMOVED_ITEM_PURGE_MAX_BATCHES: u32 = 500;
const REMOVED_ITEM_PURGE_PAUSE: std::time::Duration = std::time::Duration::from_millis(200);
const SECONDS_PER_DAY: i64 = 86_400;
const TASK_HISTORY_RETENTION_ENV: &str = "LUX_TASK_HISTORY_RETENTION_DAYS";
const DEFAULT_TASK_HISTORY_RETENTION_DAYS: i64 = 14;
const TASK_HISTORY_RETENTION_INITIAL_DELAY: std::time::Duration =
    std::time::Duration::from_secs(15 * 60);
const TASK_HISTORY_RETENTION_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(24 * 60 * 60);
const TASK_HISTORY_PRUNE_BATCH: i64 = 500;
const TASK_HISTORY_PRUNE_MAX_BATCHES: u32 = 100;
const METADATA_CANDIDATE_PRUNE_BATCH: i64 = 2_000;
/// Orphaned targets are tiny rows, so a larger batch drains a leftover backlog within a few rounds
/// while each statement still stops as soon as it has collected its batch.
const ORPHAN_TARGET_PRUNE_BATCH: i64 = 2_000;
/// Wall-clock budget for one retention round. The batch count alone does not bound the work: a
/// single unbounded batch can run for minutes, so the round also stops once this budget is spent
/// and resumes on the next scheduled run.
const TASK_HISTORY_PRUNE_TIME_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);
/// Per-statement cap for one retention batch. It matches the round budget so a statement that is
/// unexpectedly slow is cancelled inside its own batch instead of holding a pooled connection for
/// the connection-wide 30 minute statement timeout.
const RETENTION_STATEMENT_TIMEOUT: &str = "SET LOCAL statement_timeout = '30s'";

/// Retention statements, one table each.
///
/// Every statement must be able to satisfy its `ORDER BY ... LIMIT` from an index range. The
/// metadata candidate backlog previously used one statement that OR'ed a status filter with a
/// correlated `EXISTS`, which made the planner switch to a bitmap scan plus a full sort of the
/// whole backlog for every batch; on a 1.3M row backlog that exceeded the database statement
/// timeout even for the first batch.
mod prune_sql {
    pub(super) const STRM_PROBE_JOBS: &str = "DELETE FROM strm_probe_jobs
         WHERE id IN (
             SELECT id FROM strm_probe_jobs
             WHERE status IN ('COMPLETED', 'CANCELLED', 'FAILED')
               AND created_at < ?
             ORDER BY created_at, id
             LIMIT ?
         )";
    pub(super) const CHAPTER_DETECTION_JOBS: &str = "DELETE FROM chapter_detection_jobs
         WHERE id IN (
             SELECT id FROM chapter_detection_jobs
             WHERE status IN ('COMPLETED', 'CANCELLED', 'FAILED')
               AND created_at < ?
             ORDER BY created_at, id
             LIMIT ?
         )";
    pub(super) const DANMAKU_MATCH_JOBS: &str = "DELETE FROM danmaku_match_jobs
         WHERE id IN (
             SELECT id FROM danmaku_match_jobs
             WHERE status IN ('COMPLETED', 'CANCELLED', 'FAILED')
               AND created_at < ?
             ORDER BY created_at, id
             LIMIT ?
         )";
    pub(super) const METADATA_REIDENTIFY_JOBS: &str = "DELETE FROM metadata_reidentify_jobs
         WHERE id IN (
             SELECT id FROM metadata_reidentify_jobs
             WHERE status IN ('COMPLETED', 'CANCELLED', 'FAILED')
               AND created_at < ?
             ORDER BY created_at, id
             LIMIT ?
         )";
    pub(super) const SCAN_JOBS: &str = "DELETE FROM scan_jobs
         WHERE id IN (
             SELECT job.id FROM scan_jobs job
             WHERE job.status IN ('COMPLETED', 'CANCELLED', 'FAILED')
               AND job.scan_phase <> 'POSTPROCESSING'
               AND job.created_at < ?
               AND NOT EXISTS (
                   SELECT 1 FROM strm_probe_jobs probe
                   WHERE probe.target_scan_job_id = job.id
               )
             ORDER BY job.created_at, job.id
             LIMIT ?
         )";
    /// Rejected or expired candidates are gated by status and age only, so one ordered index range
    /// answers the whole batch.
    pub(super) const METADATA_CANDIDATE_REJECTIONS: &str = "DELETE FROM metadata_candidates
         WHERE id IN (
             SELECT id FROM metadata_candidates
             WHERE status IN ('REJECTED', 'EXPIRED')
               AND created_at < ?
             ORDER BY created_at, id
             LIMIT ?
         )";
    /// Pending candidates past their expiry can never be selected, but they keep their payload
    /// until the rejection retention window. Delete them after a one-hour grace so a slow worker
    /// can still finish a just-expired page.
    pub(super) const METADATA_CANDIDATE_EXPIRED_PENDING: &str = "DELETE FROM metadata_candidates
         WHERE id IN (
             SELECT id FROM metadata_candidates
             WHERE status = 'PENDING'
               AND expires_at IS NOT NULL
               AND expires_at < ?
             ORDER BY expires_at, id
             LIMIT ?
         )";
    /// Orphaned targets belong to a job that no longer exists. The anti-join is bounded by the
    /// batch size, so the sweep finishes its first batches on the pages that still hold orphans.
    pub(super) const ORPHAN_SCAN_JOB_TARGETS: &str = "DELETE FROM scan_job_targets
         WHERE (job_id, target_type, target_id) IN (
             SELECT target.job_id, target.target_type, target.target_id
             FROM scan_job_targets target
             LEFT JOIN scan_jobs job ON job.id = target.job_id
             WHERE job.id IS NULL
             LIMIT ?
         )";
    /// Superseded selections are gated by one status plus the `item_id` lookup, which keeps the
    /// driving scan on the status range and lets the correlated lookup stay index-only.
    pub(super) const METADATA_CANDIDATE_SUPERSEDED: &str = "DELETE FROM metadata_candidates
         WHERE id IN (
             SELECT candidate.id FROM metadata_candidates candidate
             WHERE candidate.status = 'SELECTED'
               AND candidate.created_at < ?
               AND EXISTS (
                   SELECT 1 FROM metadata_candidates newer
                   WHERE newer.item_id = candidate.item_id
                     AND newer.status = 'SELECTED'
                     AND (
                         newer.created_at > candidate.created_at
                         OR (
                             newer.created_at = candidate.created_at
                             AND newer.id > candidate.id
                         )
                     )
               )
             ORDER BY candidate.created_at, candidate.id
             LIMIT ?
         )";
}

/// Terminal account records (invitations, mail, merge history) are kept this long for audits.
const ACCOUNT_RECORD_RETENTION_SECONDS: i64 = 90 * 24 * 60 * 60;
/// Devices that have not been seen for this long are dropped so the list stays bounded.
const USER_DEVICE_RETENTION_SECONDS: i64 = 180 * 24 * 60 * 60;

#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct AccountRecordPurge {
    pub user_invitations: u64,
    pub mail_outbox: u64,
    pub mail_challenges: u64,
    pub user_devices: u64,
    pub media_merge_records: u64,
}

impl Database {
    /// Drops terminal invitation/mail/merge rows and stale devices in bounded batches.
    #[allow(clippy::field_reassign_with_default)]
    pub(crate) async fn purge_expired_account_records(
        &self,
        now: i64,
        limit: i64,
    ) -> Result<AccountRecordPurge, StorageError> {
        let cutoff = now.saturating_sub(ACCOUNT_RECORD_RETENTION_SECONDS);
        let device_cutoff = now.saturating_sub(USER_DEVICE_RETENTION_SECONDS);
        let batch = limit.clamp(1, 5_000);
        let mut purge = AccountRecordPurge::default();
        purge.user_invitations = self
            .query(
                "DELETE FROM user_invitations WHERE id IN (
                     SELECT id FROM user_invitations
                     WHERE status <> 'PENDING' AND expires_at < ?
                     ORDER BY expires_at, id LIMIT ?
                 )",
            )
            .bind(cutoff)
            .bind(batch)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .rows_affected();
        purge.mail_outbox = self
            .query(
                "DELETE FROM mail_outbox WHERE id IN (
                     SELECT id FROM mail_outbox
                     WHERE status IN ('SENT', 'FAILED') AND updated_at < ?
                     ORDER BY updated_at, id LIMIT ?
                 )",
            )
            .bind(cutoff)
            .bind(batch)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .rows_affected();
        purge.mail_challenges = self
            .query(
                "DELETE FROM mail_challenges WHERE id IN (
                     SELECT id FROM mail_challenges
                     WHERE expires_at < ?
                       AND (consumed_at IS NOT NULL OR revoked_at IS NOT NULL)
                     ORDER BY expires_at, id LIMIT ?
                 )",
            )
            .bind(cutoff)
            .bind(batch)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .rows_affected();
        purge.user_devices = self
            .query(
                "DELETE FROM user_devices WHERE (user_id, device_id) IN (
                     SELECT user_id, device_id FROM user_devices
                     WHERE last_seen_at < ?
                     ORDER BY last_seen_at, user_id, device_id LIMIT ?
                 )",
            )
            .bind(device_cutoff)
            .bind(batch)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .rows_affected();
        purge.media_merge_records = self
            .query(
                "DELETE FROM media_merge_records WHERE id IN (
                     SELECT id FROM media_merge_records
                     WHERE undone_at IS NOT NULL AND undone_at < ?
                     ORDER BY undone_at, id LIMIT ?
                 )",
            )
            .bind(cutoff)
            .bind(batch)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .rows_affected();
        Ok(purge)
    }
}

/// Session and playback rows older than this are not needed for any active client.
const SESSION_HISTORY_RETENTION_SECONDS: i64 = 30 * 24 * 60 * 60;

#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct SessionHistoryPurge {
    pub web_sessions: u64,
    pub access_tokens: u64,
    pub web_playback_sessions: u64,
    pub playback_sessions: u64,
    pub playback_admissions: u64,
}

impl Database {
    /// Drops expired/revoked session rows and finished playback history in bounded batches.
    ///
    /// These tables are append-only today: nothing removed them, so a NAS that has been running
    /// for a year keeps every login and playback row. Only terminal rows older than the retention
    /// window are touched; active sessions and running playbacks are never deleted.
    #[allow(clippy::field_reassign_with_default)]
    pub(crate) async fn purge_expired_session_history(
        &self,
        now: i64,
        limit: i64,
    ) -> Result<SessionHistoryPurge, StorageError> {
        let cutoff = now.saturating_sub(SESSION_HISTORY_RETENTION_SECONDS);
        let batch = limit.clamp(1, 5_000);
        let mut purge = SessionHistoryPurge::default();
        purge.web_sessions = self
            .query(
                "DELETE FROM web_sessions WHERE id IN (
                     SELECT id FROM web_sessions
                     WHERE expires_at < ?
                        OR (revoked_at IS NOT NULL AND revoked_at < ?)
                     ORDER BY expires_at, id LIMIT ?
                 )",
            )
            .bind(cutoff)
            .bind(cutoff)
            .bind(batch)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .rows_affected();
        purge.access_tokens = self
            .query(
                "DELETE FROM access_tokens WHERE id IN (
                     SELECT id FROM access_tokens
                     WHERE revoked_at IS NOT NULL AND revoked_at < ?
                     ORDER BY revoked_at, id LIMIT ?
                 )",
            )
            .bind(cutoff)
            .bind(batch)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .rows_affected();
        purge.web_playback_sessions = self
            .query(
                "DELETE FROM web_playback_sessions WHERE id IN (
                     SELECT id FROM web_playback_sessions
                     WHERE state <> 'ACTIVE' AND expires_at < ?
                     ORDER BY expires_at, id LIMIT ?
                 )",
            )
            .bind(cutoff)
            .bind(batch)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .rows_affected();
        purge.playback_sessions = self
            .query(
                "DELETE FROM playback_sessions WHERE id IN (
                     SELECT id FROM playback_sessions
                     WHERE state = 'STOPPED' AND last_event_at < ?
                     ORDER BY last_event_at, id LIMIT ?
                 )",
            )
            .bind(cutoff)
            .bind(batch)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .rows_affected();
        purge.playback_admissions = self
            .query(
                "DELETE FROM playback_admissions WHERE (user_id, play_session_id) IN (
                     SELECT user_id, play_session_id FROM playback_admissions
                     WHERE state = 'RELEASED' AND last_seen_at < ?
                     ORDER BY last_seen_at, user_id, play_session_id LIMIT ?
                 )",
            )
            .bind(cutoff)
            .bind(batch)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .rows_affected();
        Ok(purge)
    }
}

static REMOVED_ITEM_PURGE_RUNNING: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Retention window for soft-deleted media records, in days. `0` disables the purges.
pub fn removed_item_retention_days() -> i64 {
    std::env::var(REMOVED_ITEM_RETENTION_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<i64>().ok())
        .filter(|days| *days >= 0)
        .unwrap_or(DEFAULT_REMOVED_ITEM_RETENTION_DAYS)
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TaskHistoryRetentionReport {
    pub scan_jobs: u64,
    pub metadata_reidentify_jobs: u64,
    pub chapter_detection_jobs: u64,
    pub danmaku_match_jobs: u64,
    pub strm_probe_jobs: u64,
    pub metadata_candidates: u64,
    /// Targets left behind by jobs that no longer exist. `scan_job_targets` has no foreign key to
    /// `scan_jobs`, so these rows are never removed by a cascade.
    pub scan_job_targets: u64,
    /// Tables whose prune statement failed in this round. A failing table never stops the other
    /// tables: the previous implementation propagated the first error and left every later table
    /// unpruned, which silently accumulated expired history.
    pub failed: Vec<&'static str>,
}

/// Retention window for finished background-job history, in days. `0` disables pruning.
pub fn task_history_retention_days() -> i64 {
    std::env::var(TASK_HISTORY_RETENTION_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<i64>().ok())
        .filter(|days| *days >= 0)
        .unwrap_or(DEFAULT_TASK_HISTORY_RETENTION_DAYS)
}

impl Database {
    /// Runs one bounded purge pass in the background unless one is already running.
    pub fn spawn_removed_media_item_purge(&self) {
        let retention_days = removed_item_retention_days();
        if retention_days == 0 {
            return;
        }
        if REMOVED_ITEM_PURGE_RUNNING.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return;
        }
        let database = self.clone();
        tokio::spawn(async move {
            let retention_seconds = retention_days.saturating_mul(SECONDS_PER_DAY);
            let result = database
                .purge_expired_removed_media_items(
                    retention_seconds,
                    REMOVED_ITEM_PURGE_BATCH_SIZE,
                    REMOVED_ITEM_PURGE_MAX_BATCHES,
                    REMOVED_ITEM_PURGE_PAUSE,
                )
                .await;
            match result {
                Ok(0) => {}
                Ok(purged) => tracing::info!(purged, "expired soft-deleted media items purged"),
                Err(error) => tracing::warn!(%error, "soft-deleted media item purge failed"),
            }
            let result = database
                .purge_expired_missing_filesystem_entries(
                    retention_seconds,
                    REMOVED_ITEM_PURGE_BATCH_SIZE,
                    REMOVED_ITEM_PURGE_MAX_BATCHES,
                    REMOVED_ITEM_PURGE_PAUSE,
                )
                .await;
            match result {
                Ok(0) => {}
                Ok(purged) => {
                    tracing::info!(purged, "expired orphaned missing filesystem entries purged")
                }
                Err(error) => tracing::warn!(
                    %error,
                    "orphaned missing filesystem entry purge failed"
                ),
            }
            REMOVED_ITEM_PURGE_RUNNING.store(false, std::sync::atomic::Ordering::Release);
        });
    }

    /// Hard-deletes soft-deleted media items older than `retention_seconds` in small
    /// batches, letting `ON DELETE CASCADE` remove images, credits and search rows.
    /// Items that are a merge target are kept.
    ///
    /// User state and playback history no longer block the delete: an identified item has both
    /// copied into their archives first, so the history survives and is reattached to whichever
    /// item carries the same provider identity later. Only items without a provider identity keep
    /// their rows, because there would be nothing to match their history by afterwards.
    pub async fn purge_expired_removed_media_items(
        &self,
        retention_seconds: i64,
        batch_size: i64,
        max_batches: u32,
        pause: std::time::Duration,
    ) -> Result<u64, StorageError> {
        if batch_size <= 0 {
            return Ok(0);
        }
        let mut purged = 0_u64;
        for _ in 0..max_batches {
            let candidates = self
                .query_scalar::<String>(PURGEABLE_REMOVED_ITEM_SELECT)
                .bind(retention_seconds)
                .bind(batch_size)
                .fetch_all(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if candidates.is_empty() {
                break;
            }
            // Archive and delete in one transaction: a crash in between must not leave either an
            // item whose state was already consumed or a state row whose item is already gone.
            let mut transaction = self.begin_metadata_write_transaction().await?;
            self.archive_watch_state_for_items(&mut transaction, &candidates)
                .await?;
            self.archive_playback_history_for_items(&mut transaction, &candidates)
                .await?;
            let placeholders = std::iter::repeat_n("?", candidates.len())
                .collect::<Vec<_>>()
                .join(", ");
            let delete = format!("DELETE FROM media_items WHERE id IN ({placeholders})");
            let mut statement = self.query(sqlx::AssertSqlSafe(delete));
            for id in &candidates {
                statement = statement.bind(id.as_str());
            }
            let count = statement
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
                .rows_affected();
            transaction
                .commit()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if count == 0 {
                break;
            }
            purged = purged.saturating_add(count);
            if !pause.is_zero() {
                tokio::time::sleep(pause).await;
            }
        }
        Ok(purged)
    }

    /// Hard-deletes expired missing file entries that no longer have a media source.
    /// Entries with a source remain as identity tombstones so scans can restore them;
    /// active libraries are skipped while a scan may still hold a baseline reference.
    pub async fn purge_expired_missing_filesystem_entries(
        &self,
        retention_seconds: i64,
        batch_size: i64,
        max_batches: u32,
        pause: std::time::Duration,
    ) -> Result<u64, StorageError> {
        let mut purged = 0_u64;
        for _ in 0..max_batches {
            let count = self
                .query(
                    "DELETE FROM filesystem_entries
                     WHERE id IN (
                         SELECT entry.id
                         FROM filesystem_entries entry
                         JOIN library_roots library_root
                           ON library_root.id = entry.library_root_id
                         WHERE entry.entry_kind = 'FILE'
                           AND entry.is_missing = 1
                           AND entry.updated_at < unixepoch() - ?
                           AND NOT EXISTS (
                               SELECT 1 FROM media_sources source
                               WHERE source.filesystem_entry_id = entry.id
                           )
                           AND NOT EXISTS (
                               SELECT 1 FROM scan_jobs job
                               WHERE job.library_id = library_root.library_id
                                 AND job.status IN ('PENDING', 'RUNNING')
                           )
                         ORDER BY entry.updated_at, entry.id
                         LIMIT ?
                     )",
                )
                .bind(retention_seconds)
                .bind(batch_size)
                .execute(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
                .rows_affected();
            if count == 0 {
                break;
            }
            purged = purged.saturating_add(count);
            if !pause.is_zero() {
                tokio::time::sleep(pause).await;
            }
        }
        Ok(purged)
    }
}

const SOURCE_DEFAULT_REPAIR_BATCH_SIZE: i64 = 500;

impl Database {
    /// Gives every media item exactly one default source. Older scans marked every new
    /// source as default, so items that gained a second version ended up with several
    /// defaults (and merges or removals could leave none). The oldest of the current
    /// defaults wins, which keeps today's effective choice; with no default the oldest
    /// source becomes one. Returns the number of repaired items.
    pub(crate) async fn startup_repair_completed(&self, key: &str) -> Result<bool, StorageError> {
        let value: Option<String> = self
            .query_scalar("SELECT value FROM lux_meta WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(value.as_deref() == Some("1"))
    }

    pub(crate) async fn mark_startup_repair_completed(
        &self,
        key: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO lux_meta (key, value) VALUES (?, '1')
             ON CONFLICT(key) DO UPDATE SET value = '1'",
        )
        .bind(key)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub async fn normalize_media_source_defaults(&self) -> Result<u64, StorageError> {
        if self
            .startup_repair_completed(MEDIA_SOURCE_DEFAULT_REPAIR_MARKER)
            .await?
        {
            return Ok(0);
        }
        let mut repaired = 0_u64;
        loop {
            let item_ids = self
                .query_scalar::<String>(
                    "SELECT item_id FROM media_sources
                     GROUP BY item_id
                     HAVING SUM(is_default) <> 1
                     LIMIT ?",
                )
                .bind(SOURCE_DEFAULT_REPAIR_BATCH_SIZE)
                .fetch_all(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if item_ids.is_empty() {
                break;
            }
            let placeholders = std::iter::repeat_n("?", item_ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            let mut statement = self.query(sqlx::AssertSqlSafe(format!(
                "UPDATE media_sources
                 SET is_default = CASE WHEN id = (
                     SELECT chosen.id FROM media_sources chosen
                     WHERE chosen.item_id = media_sources.item_id
                     ORDER BY chosen.is_default DESC, chosen.id
                     LIMIT 1
                 ) THEN 1 ELSE 0 END,
                     updated_at = unixepoch()
                 WHERE item_id IN ({placeholders})"
            )));
            for item_id in &item_ids {
                statement = statement.bind(item_id);
            }
            statement
                .execute(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            repaired = repaired.saturating_add(item_ids.len() as u64);
            if repaired > 100_000_000 {
                break;
            }
        }
        self.mark_startup_repair_completed(MEDIA_SOURCE_DEFAULT_REPAIR_MARKER)
            .await?;
        Ok(repaired)
    }

    /// Runs one prune batch with a statement timeout that cannot exceed the round's remaining
    /// budget.
    ///
    /// The connection setting is a 30 minute timeout, which is far longer than a retention round
    /// should ever need: one unindexed statement could hold a connection for half an hour before it
    /// was cancelled. PostgreSQL gets the remaining budget as `SET LOCAL`, so a slow statement fails
    /// inside its own batch and the other tables still run.
    async fn run_prune_batch(
        &self,
        sql: &'static str,
        cutoff: Option<i64>,
        batch: i64,
    ) -> Result<u64, StorageError> {
        let mut query = self.query(sql);
        if let Some(cutoff) = cutoff {
            query = query.bind(cutoff);
        }
        let query = query.bind(batch);
        if self.backend != DatabaseBackend::Postgres {
            return query
                .execute(&self.pool)
                .await
                .map(|result| result.rows_affected())
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                });
        }
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        sqlx::query(RETENTION_STATEMENT_TIMEOUT)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let deleted = query
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .rows_affected();
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(deleted)
    }

    async fn prune_task_history_batches(
        &self,
        sql: &'static str,
        cutoff: i64,
        batch: i64,
        max_batches: u32,
        deadline: std::time::Instant,
    ) -> Result<u64, StorageError> {
        let mut total = 0_u64;
        for _ in 0..max_batches {
            if std::time::Instant::now() >= deadline {
                break;
            }
            let deleted = self.run_prune_batch(sql, Some(cutoff), batch).await?;
            total = total.saturating_add(deleted);
            if deleted < u64::try_from(batch).unwrap_or(u64::MAX) {
                break;
            }
        }
        Ok(total)
    }

    /// Same bounded loop as `prune_task_history_batches`, for statements that only take the batch
    /// size; the orphan sweep has no retention cutoff because an orphan is already expired.
    async fn prune_orphan_batches(
        &self,
        sql: &'static str,
        batch: i64,
        max_batches: u32,
        deadline: std::time::Instant,
    ) -> Result<u64, StorageError> {
        let mut total = 0_u64;
        for _ in 0..max_batches {
            if std::time::Instant::now() >= deadline {
                break;
            }
            let deleted = self.run_prune_batch(sql, None, batch).await?;
            total = total.saturating_add(deleted);
            if deleted < u64::try_from(batch).unwrap_or(u64::MAX) {
                break;
            }
        }
        Ok(total)
    }

    async fn prune_orphan_table(
        &self,
        failed: &mut Vec<&'static str>,
        table: &'static str,
        sql: &'static str,
        batch: i64,
        deadline: std::time::Instant,
    ) -> u64 {
        match self
            .prune_orphan_batches(sql, batch, TASK_HISTORY_PRUNE_MAX_BATCHES, deadline)
            .await
        {
            Ok(deleted) => deleted,
            Err(error) => {
                tracing::warn!(
                    %error,
                    table,
                    "task history retention could not prune a table; continuing with the others"
                );
                failed.push(table);
                0
            }
        }
    }

    /// Prunes one table and records the table name when its statement fails.
    ///
    /// The round must survive a single slow or broken table: reporting the failure and moving on
    /// keeps every later table pruned, while the returned count stays usable for logging.
    async fn prune_table(
        &self,
        failed: &mut Vec<&'static str>,
        table: &'static str,
        sql: &'static str,
        cutoff: i64,
        batch: i64,
        deadline: std::time::Instant,
    ) -> u64 {
        match self
            .prune_task_history_batches(
                sql,
                cutoff,
                batch,
                TASK_HISTORY_PRUNE_MAX_BATCHES,
                deadline,
            )
            .await
        {
            Ok(deleted) => deleted,
            Err(error) => {
                tracing::warn!(
                    %error,
                    table,
                    "task history retention could not prune a table; continuing with the others"
                );
                failed.push(table);
                0
            }
        }
    }

    /// Deletes one bounded pass of finished job history and resolved candidates.
    ///
    /// Each table is pruned independently within a shared wall-clock budget, so one failing table
    /// neither aborts the round nor hides which table failed.
    pub async fn prune_task_history(
        &self,
        retention_seconds: i64,
        batch_limit: i64,
    ) -> Result<TaskHistoryRetentionReport, StorageError> {
        let cutoff = current_unix_timestamp().saturating_sub(retention_seconds.max(0));
        let batch = batch_limit.clamp(1, 5_000);
        let deadline = std::time::Instant::now() + TASK_HISTORY_PRUNE_TIME_BUDGET;
        let mut failed = Vec::new();
        let strm_probe_jobs = self
            .prune_table(
                &mut failed,
                "strm_probe_jobs",
                prune_sql::STRM_PROBE_JOBS,
                cutoff,
                batch,
                deadline,
            )
            .await;
        let chapter_detection_jobs = self
            .prune_table(
                &mut failed,
                "chapter_detection_jobs",
                prune_sql::CHAPTER_DETECTION_JOBS,
                cutoff,
                batch,
                deadline,
            )
            .await;
        let danmaku_match_jobs = self
            .prune_table(
                &mut failed,
                "danmaku_match_jobs",
                prune_sql::DANMAKU_MATCH_JOBS,
                cutoff,
                batch,
                deadline,
            )
            .await;
        let metadata_reidentify_jobs = self
            .prune_table(
                &mut failed,
                "metadata_reidentify_jobs",
                prune_sql::METADATA_REIDENTIFY_JOBS,
                cutoff,
                batch,
                deadline,
            )
            .await;
        let scan_jobs = self
            .prune_table(
                &mut failed,
                "scan_jobs",
                prune_sql::SCAN_JOBS,
                cutoff,
                batch,
                deadline,
            )
            .await;
        let scan_job_targets = self
            .prune_orphan_table(
                &mut failed,
                "scan_job_targets",
                prune_sql::ORPHAN_SCAN_JOB_TARGETS,
                ORPHAN_TARGET_PRUNE_BATCH.max(batch),
                deadline,
            )
            .await;
        let rejected = self
            .prune_table(
                &mut failed,
                "metadata_candidate_rejections",
                prune_sql::METADATA_CANDIDATE_REJECTIONS,
                cutoff,
                METADATA_CANDIDATE_PRUNE_BATCH.max(batch),
                deadline,
            )
            .await;
        let superseded = self
            .prune_table(
                &mut failed,
                "metadata_candidate_superseded",
                prune_sql::METADATA_CANDIDATE_SUPERSEDED,
                cutoff,
                METADATA_CANDIDATE_PRUNE_BATCH.max(batch),
                deadline,
            )
            .await;
        let expired_pending = self
            .prune_table(
                &mut failed,
                "metadata_candidate_expired_pending",
                prune_sql::METADATA_CANDIDATE_EXPIRED_PENDING,
                current_unix_timestamp().saturating_sub(60 * 60),
                METADATA_CANDIDATE_PRUNE_BATCH.max(batch),
                deadline,
            )
            .await;
        Ok(TaskHistoryRetentionReport {
            strm_probe_jobs,
            chapter_detection_jobs,
            danmaku_match_jobs,
            metadata_reidentify_jobs,
            scan_jobs,
            scan_job_targets,
            metadata_candidates: rejected
                .saturating_add(superseded)
                .saturating_add(expired_pending),
            failed,
        })
    }

    /// Starts the daily retention pass for finished job history. This is
    /// independent from database maintenance so disabling VACUUM does not
    /// retain every historical scan and metadata job forever.
    pub fn spawn_task_history_retention(&self) {
        let retention_days = task_history_retention_days();
        if retention_days == 0 {
            return;
        }
        let database = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(TASK_HISTORY_RETENTION_INITIAL_DELAY).await;
            let mut ticker = tokio::time::interval(TASK_HISTORY_RETENTION_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                match database
                    .prune_task_history(
                        retention_days.saturating_mul(SECONDS_PER_DAY),
                        TASK_HISTORY_PRUNE_BATCH,
                    )
                    .await
                {
                    Ok(report) => {
                        if !report.failed.is_empty() {
                            tracing::warn!(
                                failed = ?report.failed,
                                "task history retention left some tables unpruned"
                            );
                        }
                        if report.scan_jobs > 0
                            || report.metadata_reidentify_jobs > 0
                            || report.chapter_detection_jobs > 0
                            || report.danmaku_match_jobs > 0
                            || report.strm_probe_jobs > 0
                            || report.metadata_candidates > 0
                            || report.scan_job_targets > 0
                        {
                            tracing::info!(
                                scan_jobs = report.scan_jobs,
                                metadata_reidentify_jobs = report.metadata_reidentify_jobs,
                                chapter_detection_jobs = report.chapter_detection_jobs,
                                danmaku_match_jobs = report.danmaku_match_jobs,
                                strm_probe_jobs = report.strm_probe_jobs,
                                metadata_candidates = report.metadata_candidates,
                                scan_job_targets = report.scan_job_targets,
                                failed = report.failed.len(),
                                "pruned expired task history"
                            );
                        }
                    }
                    Err(error) => tracing::warn!(%error, "task history retention failed"),
                }
            }
        });
    }
}

#[cfg(test)]
mod task_history_retention_tests {
    use super::*;
    use crate::config::Config;

    #[tokio::test]
    async fn retention_deletes_only_expired_terminal_scan_jobs()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        })
        .await?;
        database
            .query("INSERT INTO libraries (id, name, kind) VALUES ('retention-library', 'Retention', 'MOVIE')")
            .execute(&database.pool)
            .await?;
        let now = current_unix_timestamp();
        database
            .query(
                "INSERT INTO scan_jobs (id, library_id, job_type, status, generation, created_at)
                 VALUES ('old-completed', 'retention-library', 'RECONCILE_LIBRARY', 'COMPLETED', 'g1', 1),
                        ('new-completed', 'retention-library', 'RECONCILE_LIBRARY', 'COMPLETED', 'g2', ?),
                        ('old-running', 'retention-library', 'RECONCILE_LIBRARY', 'RUNNING', 'g3', 1)",
            )
            .bind(now)
            .execute(&database.pool)
            .await?;

        let report = database.prune_task_history(100, 10).await?;
        assert!(report.scan_jobs >= 1);
        let old_count: i64 = database
            .query_scalar("SELECT COUNT(*) FROM scan_jobs WHERE id = 'old-completed'")
            .fetch_one(&database.pool)
            .await?;
        let new_count: i64 = database
            .query_scalar("SELECT COUNT(*) FROM scan_jobs WHERE id = 'new-completed'")
            .fetch_one(&database.pool)
            .await?;
        let running_count: i64 = database
            .query_scalar("SELECT COUNT(*) FROM scan_jobs WHERE id = 'old-running'")
            .fetch_one(&database.pool)
            .await?;
        assert_eq!(old_count, 0);
        assert_eq!(new_count, 1);
        assert_eq!(running_count, 1);
        Ok(())
    }

    /// Candidates were previously pruned by one statement that OR'ed two unrelated predicates, so
    /// the planner could not keep `created_at` ordering and had to sort the whole backlog for every
    /// batch. The passes are now separate so each can follow its own index range.
    #[tokio::test]
    async fn retention_prunes_rejected_and_superseded_candidates_in_separate_passes()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        })
        .await?;
        database
            .query("INSERT INTO libraries (id, name, kind) VALUES ('retention-library', 'Retention', 'MOVIE')")
            .execute(&database.pool)
            .await?;
        database
            .query(
                "INSERT INTO media_items (id, library_id, item_type, title, sort_title, identification_status)
                 VALUES ('retention-item', 'retention-library', 'MOVIE', 'Retention', 'retention', 'LOCAL_CONFIRMED')",
            )
            .execute(&database.pool)
            .await?;
        database
            .query(
                "INSERT INTO metadata_candidates (
                     id, item_id, provider, provider_id, candidate_json, score, status, created_at
                 ) VALUES
                     ('old-selected', 'retention-item', 'TMDB', '1', '{}', 90, 'SELECTED', 1),
                     ('mid-selected', 'retention-item', 'TMDB', '2', '{}', 91, 'SELECTED', 2),
                     ('new-selected', 'retention-item', 'TMDB', '3', '{}', 92, 'SELECTED', 3),
                     ('old-rejected', 'retention-item', 'TMDB', '4', '{}', 10, 'REJECTED', 1)",
            )
            .execute(&database.pool)
            .await?;

        let report = database.prune_task_history(100, 10).await?;
        assert!(report.failed.is_empty());
        assert_eq!(report.metadata_candidates, 3);
        let remaining: Vec<String> = database
            .query_scalar("SELECT id FROM metadata_candidates ORDER BY id")
            .fetch_all(&database.pool)
            .await?;
        assert_eq!(remaining, vec!["new-selected".to_owned()]);
        Ok(())
    }

    /// One failing table must not stop the other tables from being pruned, and the caller has to be
    /// able to see which table failed instead of only a single warn line.
    #[tokio::test]
    async fn retention_records_a_failing_table_and_keeps_pruning_others()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        })
        .await?;
        database
            .query("INSERT INTO libraries (id, name, kind) VALUES ('retention-library', 'Retention', 'MOVIE')")
            .execute(&database.pool)
            .await?;
        database
            .query(
                "INSERT INTO scan_jobs (id, library_id, job_type, status, generation, created_at)
                 VALUES ('old-completed', 'retention-library', 'RECONCILE_LIBRARY', 'COMPLETED', 'g1', 1)",
            )
            .execute(&database.pool)
            .await?;

        let mut failed = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let broken = database
            .prune_table(
                &mut failed,
                "broken",
                "DELETE FROM retention_missing_table WHERE id IN (SELECT id FROM retention_missing_table LIMIT ?)",
                1,
                10,
                deadline,
            )
            .await;
        assert_eq!(broken, 0);
        assert_eq!(failed, vec!["broken"]);

        let deleted = database
            .prune_table(
                &mut failed,
                "scan_jobs",
                super::prune_sql::SCAN_JOBS,
                100,
                10,
                deadline,
            )
            .await;
        assert_eq!(deleted, 1);
        assert_eq!(failed, vec!["broken"]);
        let remaining: i64 = database
            .query_scalar("SELECT COUNT(*) FROM scan_jobs")
            .fetch_one(&database.pool)
            .await?;
        assert_eq!(remaining, 0);
        Ok(())
    }

    /// `scan_job_targets` carries no foreign key to `scan_jobs` in the current schema, so deleting
    /// a finished job leaves its targets behind. The retention round has to sweep those orphans
    /// instead of relying on a cascade that does not exist.
    #[tokio::test]
    async fn retention_sweeps_orphaned_scan_job_targets() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        })
        .await?;
        database
            .query("INSERT INTO libraries (id, name, kind) VALUES ('retention-library', 'Retention', 'MOVIE')")
            .execute(&database.pool)
            .await?;
        let now = current_unix_timestamp();
        database
            .query(
                "INSERT INTO scan_jobs (id, library_id, job_type, status, generation, created_at)
                 VALUES ('live-job', 'retention-library', 'RECONCILE_LIBRARY', 'RUNNING', 'g1', ?)",
            )
            .bind(now)
            .execute(&database.pool)
            .await?;
        database
            .query(
                "INSERT INTO scan_job_targets (job_id, target_type, target_id, item_id, change_kind)
                 VALUES ('live-job', 'ITEM', 'live-target', 'live-item', 'CHANGED')",
            )
            .execute(&database.pool)
            .await?;
        // SQLite enforces the foreign key that PostgreSQL deliberately drops on `job_id`, so the
        // orphan row is created on a connection with constraint enforcement disabled.
        let mut connection = database.pool.acquire().await?;
        sqlx::query("PRAGMA foreign_keys = OFF")
            .execute(&mut *connection)
            .await?;
        sqlx::query(
            "INSERT INTO scan_job_targets (job_id, target_type, target_id, item_id, change_kind)
             VALUES ('gone-job', 'ITEM', 'orphan-target', 'orphan-item', 'NEW')",
        )
        .execute(&mut *connection)
        .await?;
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&mut *connection)
            .await?;
        drop(connection);

        let report = database.prune_task_history(100, 10).await?;
        assert!(report.failed.is_empty());
        assert_eq!(report.scan_job_targets, 1);
        let remaining: Vec<String> = database
            .query_scalar("SELECT target_id FROM scan_job_targets ORDER BY target_id")
            .fetch_all(&database.pool)
            .await?;
        assert_eq!(remaining, vec!["live-target".to_owned()]);
        let jobs: i64 = database
            .query_scalar("SELECT COUNT(*) FROM scan_jobs")
            .fetch_one(&database.pool)
            .await?;
        assert_eq!(jobs, 1);
        Ok(())
    }
}
