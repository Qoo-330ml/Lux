use super::*;

const DATABASE_CLEANUP_MARKER: &str = "database_lifecycle_cleanup_v1";
const DATABASE_CLEANUP_PENDING: &str = "PENDING";
const DATABASE_CLEANUP_RUNNING: &str = "RUNNING";
const DATABASE_CLEANUP_COMPLETED: &str = "COMPLETED";
const CLEANUP_BATCH_SIZE: i64 = 1_000;
const SCAN_EVENT_RETENTION_SECONDS: i64 = 7 * 24 * 60 * 60;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DatabaseLifecycleCleanupReport {
    pub scan_job_paths_deleted: u64,
    pub reconciliation_entries_deleted: u64,
    pub scan_job_targets_deleted: u64,
    pub scan_job_events_deleted: u64,
    pub scan_jobs_summarized: u64,
}

impl Database {
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
            self.prune_scan_job_events().await?;
            return Ok(None);
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
        Ok(DatabaseLifecycleCleanupReport {
            scan_job_paths_deleted: self.delete_completed_scan_job_paths().await?,
            reconciliation_entries_deleted: self.delete_completed_reconciliation_entries().await?,
            scan_job_targets_deleted: self.delete_non_retryable_scan_job_targets().await?,
            scan_job_events_deleted: self.prune_scan_job_events().await?,
            scan_jobs_summarized: self.summarize_terminal_scan_jobs().await?,
        })
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

    pub(crate) async fn prune_scan_job_events(&self) -> Result<u64, StorageError> {
        let mut deleted = 0_u64;
        loop {
            let count = self
                .query(
                    "DELETE FROM scan_job_events
                     WHERE id IN (
                         SELECT id
                         FROM scan_job_events
                         WHERE level = 'INFO'
                            OR (level IN ('WARN', 'ERROR')
                                AND created_at < unixepoch() - ?)
                         LIMIT ?
                     )",
                )
                .bind(SCAN_EVENT_RETENTION_SECONDS)
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
