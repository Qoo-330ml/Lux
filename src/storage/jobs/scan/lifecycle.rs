use super::*;

impl Database {
    pub(crate) async fn create_scan_job(
        &self,
        id: &str,
        library_id: &str,
        job_type: &str,
        generation: &str,
        total_count: i64,
        auto_metadata_match: bool,
    ) -> Result<(), StorageError> {
        for _ in 0..50 {
            let mut transaction = self.begin_scan_write_transaction().await?;
            if !self
                .lock_scan_fingerprint_migration_state(&mut transaction, library_id)
                .await?
            {
                transaction
                    .rollback()
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                continue;
            }
            self.query(
                "INSERT INTO scan_jobs (
                    id, library_id, job_type, status, generation, total_count,
                    auto_metadata_match
                 ) VALUES (?, ?, ?, 'PENDING', ?, ?, ?)",
            )
            .bind(id)
            .bind(library_id)
            .bind(job_type)
            .bind(generation)
            .bind(total_count)
            .bind(database_flag(auto_metadata_match))
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
            transaction
                .commit()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(());
        }
        Err(StorageError::Conflict(
            "scan fingerprint migration lease did not clear in time".to_owned(),
        ))
    }

    pub(crate) async fn enable_scan_job_auto_metadata_match(
        &self,
        job_id: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE scan_jobs
             SET auto_metadata_match = 1, updated_at = unixepoch()
             WHERE id = ? AND status IN ('PENDING', 'RUNNING')",
        )
        .bind(job_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn finish_scan_job_if_idle(&self, id: &str) -> Result<bool, StorageError> {
        let result = self
            .query(
                "UPDATE scan_jobs
             SET status = 'COMPLETED', cursor = NULL, current_item = NULL,
                 scan_phase = 'IDLE',
                 finished_at = unixepoch(), updated_at = unixepoch()
             WHERE id = ? AND status IN ('PENDING', 'RUNNING')
               AND NOT EXISTS (
                   SELECT 1 FROM scan_job_paths
                   WHERE job_id = ? AND processed_at IS NULL
               )",
            )
            .bind(id)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn find_scan_job(
        &self,
        id: &str,
    ) -> Result<Option<StoredScanJob>, StorageError> {
        self.query(
            "SELECT id, library_id, job_type, status, generation, cursor,
                    processed_count, total_count, processed_media_count,
                    cancel_requested, error,
                    created_at, started_at, finished_at,
                    discovery_completed, auto_metadata_match,
                    current_item, scan_phase
             FROM scan_jobs WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_scan_job))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_scan_jobs(
        &self,
        status: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredScanJob>, StorageError> {
        let rows = if let Some(status) = status {
            self.query(
                "SELECT id, library_id, job_type, status, generation, cursor,
                        processed_count, total_count, processed_media_count,
                        cancel_requested, error,
                        created_at, started_at, finished_at,
                        discovery_completed, auto_metadata_match,
                        current_item, scan_phase
                 FROM scan_jobs WHERE status = ?
                 ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
            )
            .bind(status)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        } else {
            self.query(
                "SELECT id, library_id, job_type, status, generation, cursor,
                        processed_count, total_count, processed_media_count,
                        cancel_requested, error,
                        created_at, started_at, finished_at,
                        discovery_completed, auto_metadata_match,
                        current_item, scan_phase
                 FROM scan_jobs
                 ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
            )
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        };
        rows.map(|rows| rows.into_iter().map(stored_scan_job).collect())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_scan_jobs_for_library_deletion(
        &self,
        library_id: &str,
    ) -> Result<Vec<StoredScanJob>, StorageError> {
        self.query(
            "SELECT id, library_id, job_type, status, generation, cursor,
                    processed_count, total_count, processed_media_count,
                    cancel_requested, error,
                    created_at, started_at, finished_at,
                    discovery_completed, auto_metadata_match,
                    current_item, scan_phase
             FROM scan_jobs
             WHERE library_id = ?
               AND (status IN ('PENDING', 'RUNNING')
                    OR (status = 'COMPLETED' AND scan_phase = 'POSTPROCESSING'))
             ORDER BY created_at DESC, id DESC",
        )
        .bind(library_id)
        .fetch_all(&self.pool)
        .await
        .map(|rows| rows.into_iter().map(stored_scan_job).collect())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn count_scan_jobs_by_status(
        &self,
    ) -> Result<StoredScanJobCounts, StorageError> {
        self.query(
            "SELECT
                (SELECT COUNT(*) FROM scan_jobs
                 WHERE status IN ('PENDING', 'RUNNING')) AS running,
                (SELECT COUNT(*) FROM scan_jobs WHERE status = 'FAILED') AS failed",
        )
        .fetch_one(&self.pool)
        .await
        .map(|row| StoredScanJobCounts {
            running: row.get("running"),
            failed: row.get("failed"),
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_scan_jobs_for_activity(
        &self,
        limit: i64,
    ) -> Result<Vec<StoredScanJob>, StorageError> {
        self.query(
            "SELECT id, library_id, job_type, status, generation, cursor,
                    processed_count, total_count, processed_media_count,
                    cancel_requested, error,
                    created_at, started_at, finished_at,
                    discovery_completed, auto_metadata_match,
                    current_item, scan_phase
             FROM scan_jobs
             WHERE status IN ('PENDING', 'RUNNING')
                OR (status = 'COMPLETED' AND scan_phase = 'POSTPROCESSING')
             ORDER BY created_at DESC, id DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map(|rows| rows.into_iter().map(stored_scan_job).collect())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_scan_job_ids_needing_resume(
        &self,
    ) -> Result<Vec<String>, StorageError> {
        self.query_scalar(
            "SELECT id FROM scan_jobs
             WHERE status IN ('PENDING', 'RUNNING')
                OR (status = 'COMPLETED' AND scan_phase = 'POSTPROCESSING')
             ORDER BY created_at, id LIMIT 10000",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_active_scan_job_for_library(
        &self,
        library_id: &str,
    ) -> Result<Option<StoredScanJob>, StorageError> {
        self.query(
            "SELECT id, library_id, job_type, status, generation, cursor,
                    processed_count, total_count, processed_media_count,
                    cancel_requested, error,
                    created_at, started_at, finished_at,
                    discovery_completed, auto_metadata_match,
                    current_item, scan_phase
             FROM scan_jobs
             WHERE library_id = ? AND status IN ('PENDING', 'RUNNING')
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(library_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_scan_job))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_active_scan_job(
        &self,
        library_id: &str,
        job_type: &str,
    ) -> Result<Option<StoredScanJob>, StorageError> {
        self.query(
            "SELECT id, library_id, job_type, status, generation, cursor,
                    processed_count, total_count, processed_media_count,
                    cancel_requested, error,
                    created_at, started_at, finished_at,
                    discovery_completed, auto_metadata_match,
                    current_item, scan_phase
             FROM scan_jobs
             WHERE library_id = ? AND job_type = ? AND status IN ('PENDING', 'RUNNING')
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(library_id)
        .bind(job_type)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_scan_job))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn has_active_scan_job_type(
        &self,
        job_type: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(
                 SELECT 1 FROM scan_jobs
                 WHERE job_type = ? AND status IN ('PENDING', 'RUNNING')
             ) THEN 1 ELSE 0 END",
        )
        .bind(job_type)
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn has_running_scan_job_type(
        &self,
        job_type: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS (
                 SELECT 1 FROM scan_jobs WHERE job_type = ? AND status = 'RUNNING'
             ) THEN 1 ELSE 0 END",
        )
        .bind(job_type)
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn claim_scan_job(&self, id: &str) -> Result<bool, StorageError> {
        self.query(
            "UPDATE scan_jobs
             SET status = 'RUNNING', started_at = COALESCE(started_at, unixepoch()),
                 updated_at = unixepoch()
             WHERE id = ? AND status = 'PENDING'",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn update_scan_job_progress(
        &self,
        id: &str,
        cursor: Option<&str>,
        processed_count: i64,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE scan_jobs
             SET cursor = ?, processed_count = ?, updated_at = unixepoch()
             WHERE id = ? AND status = 'RUNNING'",
        )
        .bind(cursor)
        .bind(processed_count)
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn update_scan_job_activity(
        &self,
        id: &str,
        current_item: Option<&str>,
        scan_phase: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE scan_jobs
             SET current_item = ?, scan_phase = ?, updated_at = unixepoch()
             WHERE id = ? AND (status IN ('PENDING', 'RUNNING')
                OR (status = 'COMPLETED' AND scan_phase = 'POSTPROCESSING'))",
        )
        .bind(current_item)
        .bind(scan_phase)
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn scan_job_cancel_requested(&self, id: &str) -> Result<bool, StorageError> {
        self.query_scalar("SELECT cancel_requested FROM scan_jobs WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .map(|value: i64| value != 0)
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn request_scan_job_cancel(&self, id: &str) -> Result<(), StorageError> {
        self.query(
            "UPDATE scan_jobs SET cancel_requested = 1, updated_at = unixepoch()
             WHERE id = ? AND (status IN ('PENDING', 'RUNNING')
                  OR (status = 'COMPLETED' AND scan_phase = 'POSTPROCESSING'))",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn finish_scan_job(
        &self,
        id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE scan_jobs
             SET status = CASE WHEN cancel_requested = 1 THEN 'CANCELLED' ELSE ? END,
                 error = CASE WHEN cancel_requested = 1 THEN NULL ELSE ? END,
                 cursor = NULL, current_item = NULL,
                 scan_phase = 'IDLE',
                 finished_at = unixepoch(), updated_at = unixepoch()
             WHERE id = ? AND (status IN ('PENDING', 'RUNNING')
                  OR (status = 'COMPLETED' AND scan_phase = 'POSTPROCESSING'))",
        )
        .bind(status)
        .bind(error)
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn mark_scan_job_postprocessing(&self, id: &str) -> Result<(), StorageError> {
        self.query(
            "UPDATE scan_jobs
             SET status = 'COMPLETED', cursor = NULL, current_item = NULL,
                 scan_phase = 'POSTPROCESSING',
                 error = NULL, finished_at = COALESCE(finished_at, unixepoch()),
                 updated_at = unixepoch()
             WHERE id = ? AND status = 'RUNNING'",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn complete_scan_job_postprocessing(
        &self,
        id: &str,
    ) -> Result<bool, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let result = self
            .query(
                "UPDATE scan_jobs
                 SET status = 'COMPLETED', error = NULL, cursor = NULL,
                     current_item = NULL, cancel_requested = 0,
                     scan_phase = 'IDLE', finished_at = COALESCE(finished_at, unixepoch()),
                     updated_at = unixepoch()
                 WHERE id = ? AND status IN ('RUNNING', 'COMPLETED')
                   AND scan_phase = 'POSTPROCESSING'
                   AND NOT EXISTS (
                       SELECT 1 FROM scan_job_targets
                       WHERE job_id = ?
                         AND (
                             probe_state IN ('PENDING', 'FAILED')
                             OR metadata_state IN ('PENDING', 'FAILED')
                             OR thumbnail_state IN ('PENDING', 'FAILED')
                         )
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM scan_manifests
                       WHERE job_id = ? AND workflow_version IN (2, 3)
                         AND discovery_format_version = 3
                         AND postprocessing_targets_ready = 0
                   )",
            )
            .bind(id)
            .bind(id)
            .bind(id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if result.rows_affected() == 1 {
            self.query(
                "UPDATE scan_manifests
                 SET state = 'COMPLETED', resume_state = NULL,
                     completed_at = COALESCE(completed_at, unixepoch()),
                     updated_at = unixepoch()
                 WHERE job_id = ? AND state = 'POSTPROCESSING'",
            )
            .bind(id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn fail_scan_job_postprocessing(
        &self,
        id: &str,
    ) -> Result<bool, StorageError> {
        let result = self
            .query(
                "UPDATE scan_jobs
                 SET status = 'COMPLETED', error = 'POSTPROCESSING_FAILED', cursor = NULL,
                     current_item = NULL, cancel_requested = 0,
                     scan_phase = 'IDLE', finished_at = COALESCE(finished_at, unixepoch()),
                     updated_at = unixepoch()
                 WHERE id = ? AND status IN ('RUNNING', 'COMPLETED')
                   AND scan_phase = 'POSTPROCESSING'
                   AND (
                     EXISTS (
                       SELECT 1 FROM scan_job_targets
                       WHERE job_id = ?
                         AND (
                             probe_state IN ('PENDING', 'FAILED')
                             OR metadata_state IN ('PENDING', 'FAILED')
                             OR thumbnail_state IN ('PENDING', 'FAILED')
                         )
                     ) OR EXISTS (
                       SELECT 1 FROM scan_manifests
                       WHERE job_id = ? AND workflow_version IN (2, 3)
                         AND discovery_format_version = 3
                         AND postprocessing_targets_ready = 0
                     )
                   )",
            )
            .bind(id)
            .bind(id)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn retry_scan_job_postprocessing(
        &self,
        id: &str,
    ) -> Result<bool, StorageError> {
        let mut transaction = self.begin_scan_write_transaction().await?;
        let library_id = self
            .query_scalar::<Option<String>>("SELECT library_id FROM scan_jobs WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .flatten();
        let Some(library_id) = library_id else {
            transaction
                .rollback()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(false);
        };
        if !self
            .lock_scan_fingerprint_migration_state(&mut transaction, &library_id)
            .await?
        {
            transaction
                .rollback()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(false);
        }
        let result = self
            .query(
                "UPDATE scan_jobs
                 SET status = CASE WHEN status = 'COMPLETED' THEN 'COMPLETED' ELSE 'RUNNING' END,
                     cancel_requested = 0, error = NULL,
                     current_item = NULL, scan_phase = 'POSTPROCESSING',
                     started_at = COALESCE(started_at, unixepoch()),
                     finished_at = CASE WHEN status = 'COMPLETED' THEN finished_at ELSE NULL END,
                     updated_at = unixepoch()
                 WHERE id = ? AND status IN ('COMPLETED', 'FAILED', 'CANCELLED')
                   AND job_type = 'RECONCILE_LIBRARY'
                   AND scan_phase = 'IDLE'
                   AND NOT EXISTS (
                       SELECT 1 FROM reconciliation_scan_entries
                       WHERE job_id = ?
                   )",
            )
            .bind(id)
            .bind(id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn retry_scan_job(&self, id: &str) -> Result<bool, StorageError> {
        let mut transaction = self.begin_scan_write_transaction().await?;
        let library_id = self
            .query_scalar::<Option<String>>("SELECT library_id FROM scan_jobs WHERE id = ?")
            .bind(id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .flatten();
        let Some(library_id) = library_id else {
            transaction
                .rollback()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(false);
        };
        if !self
            .lock_scan_fingerprint_migration_state(&mut transaction, &library_id)
            .await?
        {
            transaction
                .rollback()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(false);
        }
        let result = self
            .query(
                "UPDATE scan_jobs
             SET status = 'PENDING', cancel_requested = 0, error = NULL,
                 current_item = NULL, scan_phase = 'IDLE',
                 started_at = NULL, finished_at = NULL, updated_at = unixepoch()
             WHERE id = ? AND status IN ('FAILED', 'CANCELLED')",
            )
            .bind(id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result.rows_affected() == 1)
    }
}
