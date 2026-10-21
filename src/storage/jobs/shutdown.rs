const SHUTDOWN_JOB_ERROR_CODE: &str = "SERVER_SHUTDOWN";

use super::*;

impl Database {
    pub(crate) const LEGACY_SCAN_REQUIRES_NEW_MANIFEST: &'static str =
        "LEGACY_SCAN_REQUIRES_NEW_MANIFEST";

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

        let legacy_scan_rows = self
            .query(
                "UPDATE scan_jobs
                 SET status = 'CANCELLED', cancel_requested = 0, error = ?, cursor = NULL,
                     current_item = NULL, scan_phase = 'IDLE', finished_at = unixepoch(),
                     updated_at = unixepoch()
                 WHERE job_type = 'RECONCILE_LIBRARY'
                   AND status IN ('PENDING', 'RUNNING', 'COMPLETED')
                   AND (
                       status IN ('PENDING', 'RUNNING')
                       OR (status = 'COMPLETED' AND scan_phase = 'POSTPROCESSING')
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM scan_manifests WHERE job_id = scan_jobs.id
                   )
                 RETURNING id",
            )
            .bind(Self::LEGACY_SCAN_REQUIRES_NEW_MANIFEST)
            .fetch_all(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        cancelled = cancelled.saturating_add(legacy_scan_rows.len() as u64);
        legacy_scan_events.extend(
            legacy_scan_rows
                .into_iter()
                .map(|row| row.get::<String, _>("id")),
        );

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
            "UPDATE library_deletion_jobs
             SET status = 'CANCELLED', error_code = ?,
                 finished_at = unixepoch(), updated_at = unixepoch()
             WHERE status IN ('PENDING', 'RUNNING')",
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
