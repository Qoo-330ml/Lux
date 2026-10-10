use super::*;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

const DATABASE_CLEANUP_MARKER: &str = "database_lifecycle_cleanup_v1";
const DATABASE_CLEANUP_PENDING: &str = "PENDING";
const DATABASE_CLEANUP_RUNNING: &str = "RUNNING";
const DATABASE_CLEANUP_COMPLETED: &str = "COMPLETED";
const SCAN_JOB_EVENTS_LOG_MIGRATION_MARKER: &str = "scan_job_events_config_log_migration_v1";
const AUDIT_EVENTS_LOG_MIGRATION_MARKER: &str = "audit_events_config_log_migration_v1";
const CLEANUP_BATCH_SIZE: i64 = 1_000;
/// Grace period so a batch is never removed while its worker is still finishing it.
const LOCAL_METADATA_BATCH_RETENTION_SECONDS: i64 = 3_600;
const MAX_LOG_MIGRATION_BATCH_BYTES: u64 = crate::observability::logs::LOG_SEGMENT_BYTES / 2;

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
