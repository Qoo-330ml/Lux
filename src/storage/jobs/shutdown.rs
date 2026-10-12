const SHUTDOWN_JOB_ERROR_CODE: &str = "SERVER_SHUTDOWN";

use super::*;

impl Database {
    /// Marks every unfinished persistent background job as cancelled.
    ///
    /// This is deliberately one transaction so startup and shutdown never
    /// leave a mixed set of job tables eligible for automatic recovery.
    pub async fn cancel_incomplete_jobs_for_shutdown(&self) -> Result<u64, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let mut cancelled = 0_u64;
        let mut legacy_scan_events = Vec::new();

        let legacy_scan_ids: Vec<String> = self
            .query_scalar(
                "SELECT id FROM scan_jobs
                 WHERE job_type = 'RECONCILE_LIBRARY'
                   AND (
                       status IN ('PENDING', 'RUNNING')
                       OR (status = 'COMPLETED' AND scan_phase = 'POSTPROCESSING')
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM scan_manifests WHERE job_id = scan_jobs.id
                   )",
            )
            .fetch_all(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        for job_id in &legacy_scan_ids {
            let result = self
                .query(
                    "UPDATE scan_jobs
                     SET status = 'CANCELLED', cancel_requested = 0, error = ?, cursor = NULL,
                         current_item = NULL, scan_phase = 'IDLE', finished_at = unixepoch(),
                         updated_at = unixepoch()
                     WHERE id = ? AND status IN ('PENDING', 'RUNNING', 'COMPLETED')",
                )
                .bind(Self::LEGACY_SCAN_REQUIRES_NEW_MANIFEST)
                .bind(job_id)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            cancelled = cancelled.saturating_add(result.rows_affected());
            if result.rows_affected() == 1 {
                legacy_scan_events.push(job_id.clone());
            }
        }

        for query in [
            "UPDATE scan_jobs
             SET status = 'CANCELLED', cancel_requested = 0, error = ?, cursor = NULL,
                 current_item = NULL, scan_phase = 'IDLE', finished_at = unixepoch(),
                 updated_at = unixepoch()
             WHERE (status IN ('PENDING', 'RUNNING')
                OR (status = 'COMPLETED' AND scan_phase = 'POSTPROCESSING'))",
            "UPDATE strm_probe_jobs
             SET status = 'CANCELLED', cancel_requested = 0, error = ?, finished_at = unixepoch(),
                 updated_at = unixepoch()
             WHERE status IN ('PENDING', 'RUNNING')",
            "UPDATE chapter_detection_jobs
             SET status = 'CANCELLED', cancel_requested = 0, error = ?, finished_at = unixepoch(),
                 updated_at = unixepoch()
             WHERE status IN ('PENDING', 'RUNNING')",
            "UPDATE library_cover_jobs
             SET status = 'CANCELLED', error = ?, finished_at = unixepoch(), updated_at = unixepoch()
             WHERE status IN ('PENDING', 'RUNNING')",
            "UPDATE danmaku_match_jobs
             SET status = 'CANCELLED', cancel_requested = 0, error = ?, finished_at = unixepoch(),
                 updated_at = unixepoch()
             WHERE status IN ('PENDING', 'RUNNING')",
            "UPDATE metadata_reidentify_jobs
             SET status = 'CANCELLED', cancel_requested = 0, error = ?, finished_at = unixepoch(),
                 updated_at = unixepoch()
             WHERE status IN ('QUEUED', 'RUNNING')",
            "UPDATE emby_migration_jobs
             SET status = 'CANCELLED', cancel_requested = 0, error = ?, finished_at = unixepoch(),
                 updated_at = unixepoch()
             WHERE status IN ('PENDING', 'RUNNING')",
            "UPDATE person_index_rebuild_jobs
             SET status = 'CANCELLED', cancel_requested = 0, run_token = NULL, error = ?,
                 finished_at = unixepoch(), updated_at = unixepoch()
             WHERE status IN ('QUEUED', 'RUNNING')",
        ] {
            let result = self
                .query(query)
                .bind(SHUTDOWN_JOB_ERROR_CODE)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            cancelled = cancelled.saturating_add(result.rows_affected());
        }

        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        for job_id in legacy_scan_events {
            let event_id = uuid::Uuid::now_v7().to_string();
            self.log_store
                .append_scan_job_event(
                    &event_id,
                    &job_id,
                    "WARN",
                    Self::LEGACY_SCAN_REQUIRES_NEW_MANIFEST,
                    "旧版全量扫描没有 Manifest 检查点；请重试以创建新的 Manifest 扫描",
                    "{}",
                )
                .await
                .map_err(|source| StorageError::Io {
                    path: self.path.clone(),
                    source,
                })?;
        }
        Ok(cancelled)
    }
}
