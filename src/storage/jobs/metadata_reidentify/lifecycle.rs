use super::*;

const AUTOMATIC_FILL_MISSING_DISPATCH_PAGE_MAX: i64 = 100;

impl Database {
    pub(crate) async fn create_metadata_reidentify_job(
        &self,
        job_id: &str,
        item_ids: &[String],
        mode: &str,
    ) -> Result<(), StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        self.lock_media_items_for_update(&mut transaction, item_ids)
            .await?;
        self.query(
            "INSERT INTO metadata_reidentify_jobs (
                id, status, total_count, mode, library_id, job_scope
             ) VALUES (?, 'QUEUED', ?, ?, NULL, 'ITEMS')",
        )
        .bind(job_id)
        .bind(i64::try_from(item_ids.len()).unwrap_or(i64::MAX))
        .bind(mode)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        for chunk in item_ids.chunks(BATCH_INSERT_CHUNK_SIZE) {
            let values = std::iter::repeat_n(
                format!(
                    "(?, ?, 'PENDING', (SELECT {METADATA_REIDENTIFY_PRIORITY_CASE}
                     FROM media_items WHERE id = ?))"
                ),
                chunk.len(),
            )
            .collect::<Vec<_>>()
            .join(", ");
            let query = format!(
                "INSERT INTO metadata_reidentify_job_items
                     (job_id, item_id, status, priority)
                 VALUES {values}"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for item_id in chunk {
                statement = statement.bind(job_id).bind(item_id).bind(item_id);
            }
            statement
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }
        self.query(
            "UPDATE metadata_reidentify_jobs
             SET library_id = (
                 SELECT CASE
                     WHEN MIN(media_items.library_id) = MAX(media_items.library_id)
                         THEN MIN(media_items.library_id)
                     ELSE NULL
                 END
                 FROM metadata_reidentify_job_items
                 JOIN media_items ON media_items.id = metadata_reidentify_job_items.item_id
                 WHERE metadata_reidentify_job_items.job_id = ?
             )
             WHERE id = ?",
        )
        .bind(job_id)
        .bind(job_id)
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
            })
    }

    pub(crate) async fn create_metadata_reidentify_library_job(
        &self,
        job_id: &str,
        library_id: &str,
        mode: &str,
    ) -> Result<i64, StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        self.query(
            "INSERT INTO metadata_reidentify_jobs (
                id, status, total_count, mode, library_id, job_scope
             )
             SELECT ?, 'CANCELLED', COUNT(*), ?, ?, 'LIBRARY'
             FROM media_items
             WHERE library_id = ? AND removed_at IS NULL
               AND item_type IN ('MOVIE', 'SERIES', 'SEASON', 'EPISODE')",
        )
        .bind(job_id)
        .bind(mode)
        .bind(library_id)
        .bind(library_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        let total_count: i64 = self
            .query_scalar("SELECT total_count FROM metadata_reidentify_jobs WHERE id = ?")
            .bind(job_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if total_count == 0 {
            transaction
                .rollback()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(0);
        }
        self.query(
            "INSERT INTO metadata_reidentify_job_items
                 (job_id, item_id, status, priority)
             SELECT ?, id, 'PENDING', CASE
                 WHEN item_type IN ('MOVIE', 'SERIES') THEN 0
                 WHEN item_type = 'SEASON' THEN 1
                 WHEN item_type = 'EPISODE' THEN 2
                 ELSE 3
             END
             FROM media_items
             WHERE library_id = ? AND removed_at IS NULL
               AND item_type IN ('MOVIE', 'SERIES', 'SEASON', 'EPISODE')",
        )
        .bind(job_id)
        .bind(library_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query(
            "UPDATE metadata_reidentify_jobs
             SET status = 'QUEUED', updated_at = unixepoch()
             WHERE id = ? AND status = 'CANCELLED'",
        )
        .bind(job_id)
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
        Ok(total_count)
    }

    pub(crate) async fn find_metadata_reidentify_job(
        &self,
        job_id: &str,
    ) -> Result<Option<StoredMetadataReidentifyJob>, StorageError> {
        self.query(
            "WITH pending_counts AS (
                 SELECT job_items.job_id, COUNT(DISTINCT candidates.item_id) AS pending_count
                 FROM metadata_reidentify_job_items job_items
                 JOIN metadata_candidates candidates
                   ON candidates.item_id = job_items.item_id
                 WHERE job_items.job_id = ?
                   AND candidates.status = 'PENDING'
                 GROUP BY job_items.job_id
             )
             SELECT jobs.id, jobs.status, jobs.processed_count, jobs.total_count,
                    jobs.error, jobs.created_at, jobs.updated_at, jobs.started_at,
                    jobs.finished_at, jobs.mode, jobs.cancel_requested,
                    jobs.library_id, jobs.job_scope,
                    COALESCE(pending_counts.pending_count, 0) AS pending_count
             FROM metadata_reidentify_jobs jobs
             LEFT JOIN pending_counts ON pending_counts.job_id = jobs.id
             WHERE jobs.id = ?",
        )
        .bind(job_id)
        .bind(job_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_metadata_reidentify_job))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_metadata_reidentify_jobs(
        &self,
        status: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredMetadataReidentifyJob>, StorageError> {
        let rows = if let Some(status) = status {
            self.query(
                "WITH selected_jobs AS (
                     SELECT id, status, processed_count, total_count, error,
                            created_at, updated_at, started_at, finished_at, mode,
                            cancel_requested, library_id, job_scope
                     FROM metadata_reidentify_jobs
                     WHERE status = ?
                     ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?
                 ), pending_counts AS (
                     SELECT job_items.job_id, COUNT(DISTINCT candidates.item_id) AS pending_count
                     FROM metadata_reidentify_job_items job_items
                     JOIN selected_jobs ON selected_jobs.id = job_items.job_id
                     JOIN metadata_candidates candidates
                       ON candidates.item_id = job_items.item_id
                      AND candidates.status = 'PENDING'
                     GROUP BY job_items.job_id
                 )
                 SELECT selected_jobs.id, selected_jobs.status,
                        selected_jobs.processed_count, selected_jobs.total_count,
                        selected_jobs.error, selected_jobs.created_at,
                        selected_jobs.updated_at, selected_jobs.started_at,
                        selected_jobs.finished_at, selected_jobs.mode,
                        selected_jobs.cancel_requested, selected_jobs.library_id,
                        selected_jobs.job_scope,
                        COALESCE(pending_counts.pending_count, 0) AS pending_count
                 FROM selected_jobs
                 LEFT JOIN pending_counts ON pending_counts.job_id = selected_jobs.id
                 ORDER BY selected_jobs.created_at DESC, selected_jobs.id DESC",
            )
            .bind(status)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        } else {
            self.query(
                "WITH selected_jobs AS (
                     SELECT id, status, processed_count, total_count, error,
                            created_at, updated_at, started_at, finished_at, mode,
                            cancel_requested, library_id, job_scope
                     FROM metadata_reidentify_jobs
                     ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?
                 ), pending_counts AS (
                     SELECT job_items.job_id, COUNT(DISTINCT candidates.item_id) AS pending_count
                     FROM metadata_reidentify_job_items job_items
                     JOIN selected_jobs ON selected_jobs.id = job_items.job_id
                     JOIN metadata_candidates candidates
                       ON candidates.item_id = job_items.item_id
                      AND candidates.status = 'PENDING'
                     GROUP BY job_items.job_id
                 )
                 SELECT selected_jobs.id, selected_jobs.status,
                        selected_jobs.processed_count, selected_jobs.total_count,
                        selected_jobs.error, selected_jobs.created_at,
                        selected_jobs.updated_at, selected_jobs.started_at,
                        selected_jobs.finished_at, selected_jobs.mode,
                        selected_jobs.cancel_requested, selected_jobs.library_id,
                        selected_jobs.job_scope,
                        COALESCE(pending_counts.pending_count, 0) AS pending_count
                 FROM selected_jobs
                 LEFT JOIN pending_counts ON pending_counts.job_id = selected_jobs.id
                 ORDER BY selected_jobs.created_at DESC, selected_jobs.id DESC",
            )
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        };
        rows.map(|rows| {
            rows.into_iter()
                .map(stored_metadata_reidentify_job)
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_metadata_reidentify_jobs_for_activity(
        &self,
        limit: i64,
    ) -> Result<Vec<StoredMetadataReidentifyJob>, StorageError> {
        self.query(
            "SELECT id, status, processed_count, total_count, error,
                    created_at, updated_at, started_at, finished_at, mode,
                    cancel_requested, library_id, job_scope, 0 AS pending_count
             FROM metadata_reidentify_jobs
             WHERE status IN ('QUEUED', 'RUNNING')
             ORDER BY created_at DESC, id DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(stored_metadata_reidentify_job)
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn active_library_metadata_reidentify_job_id(
        &self,
    ) -> Result<Option<String>, StorageError> {
        self.query_scalar(
            "SELECT id
             FROM metadata_reidentify_jobs
             WHERE job_scope = 'LIBRARY'
               AND status IN ('QUEUED', 'RUNNING')
             ORDER BY created_at DESC, id DESC
             LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn claim_metadata_reidentify_job(
        &self,
        job_id: &str,
    ) -> Result<bool, StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let result = self
            .query(
                "UPDATE metadata_reidentify_jobs
             SET status = 'RUNNING', started_at = COALESCE(started_at, unixepoch()),
                 updated_at = unixepoch()
             WHERE id = ? AND status = 'QUEUED' AND cancel_requested = 0",
            )
            .bind(job_id)
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

    pub(crate) async fn metadata_reidentify_job_dispatch_source(
        &self,
        job_id: &str,
    ) -> Result<Option<String>, StorageError> {
        self.query_scalar("SELECT dispatch_source FROM metadata_reidentify_jobs WHERE id = ?")
            .bind(job_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_queued_automatic_fill_missing_job_ids(
        &self,
        library_id: &str,
        after_created_at: Option<i64>,
        after_job_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<(String, i64)>, StorageError> {
        let limit = limit.clamp(1, AUTOMATIC_FILL_MISSING_DISPATCH_PAGE_MAX);
        let rows = if let (Some(created_at), Some(job_id)) = (after_created_at, after_job_id) {
            self.query_as::<(String, i64)>(
                "SELECT id, created_at FROM metadata_reidentify_jobs
                 WHERE library_id = ? AND mode = 'FILL_MISSING'
                   AND dispatch_source = 'AUTOMATIC' AND status = 'QUEUED'
                   AND (created_at > ? OR (created_at = ? AND id > ?))
                 ORDER BY created_at, id LIMIT ?",
            )
            .bind(library_id)
            .bind(created_at)
            .bind(created_at)
            .bind(job_id)
            .bind(limit)
            .fetch_all(&self.pool)
            .await
        } else {
            self.query_as::<(String, i64)>(
                "SELECT id, created_at FROM metadata_reidentify_jobs
                 WHERE library_id = ? AND mode = 'FILL_MISSING'
                   AND dispatch_source = 'AUTOMATIC' AND status = 'QUEUED'
                 ORDER BY created_at, id LIMIT ?",
            )
            .bind(library_id)
            .bind(limit)
            .fetch_all(&self.pool)
            .await
        };
        rows.map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_queued_automatic_fill_missing_library_ids(
        &self,
        after_library_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<String>, StorageError> {
        let limit = limit.clamp(1, AUTOMATIC_FILL_MISSING_DISPATCH_PAGE_MAX);
        let rows = if let Some(after_library_id) = after_library_id {
            self.query_scalar(
                "SELECT DISTINCT library_id FROM metadata_reidentify_jobs
                 WHERE library_id IS NOT NULL AND library_id > ?
                   AND mode = 'FILL_MISSING' AND dispatch_source = 'AUTOMATIC'
                   AND status = 'QUEUED'
                 ORDER BY library_id LIMIT ?",
            )
            .bind(after_library_id)
            .bind(limit)
            .fetch_all(&self.pool)
            .await
        } else {
            self.query_scalar(
                "SELECT DISTINCT library_id FROM metadata_reidentify_jobs
                 WHERE library_id IS NOT NULL AND mode = 'FILL_MISSING'
                   AND dispatch_source = 'AUTOMATIC' AND status = 'QUEUED'
                 ORDER BY library_id LIMIT ?",
            )
            .bind(limit)
            .fetch_all(&self.pool)
            .await
        };
        rows.map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn finish_metadata_reidentify_job(
        &self,
        job_id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<(), StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let cancel_requested: i64 = self
            .query_scalar("SELECT cancel_requested FROM metadata_reidentify_jobs WHERE id = ?")
            .bind(job_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if cancel_requested != 0 || status == "CANCELLED" {
            self.query(
                "UPDATE metadata_reidentify_job_items
                 SET status = 'FAILED', candidate_count = 0,
                     error = 'JOB_CANCELLED',
                     claimed_request_fingerprint = NULL,
                     claimed_request_capabilities_json = '[]',
                     updated_at = unixepoch()
                 WHERE job_id = ? AND status IN ('PENDING', 'RUNNING')",
            )
            .bind(job_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        self.query(
            "UPDATE metadata_reidentify_jobs
             SET status = CASE WHEN cancel_requested = 1 THEN 'CANCELLED' ELSE ? END,
                 error = CASE WHEN cancel_requested = 1 THEN NULL ELSE ? END,
                 finished_at = unixepoch(), updated_at = unixepoch()
             WHERE id = ? AND status IN ('QUEUED', 'RUNNING')",
        )
        .bind(status)
        .bind(error)
        .bind(job_id)
        .execute(&mut *transaction)
        .await
        .map(|_| ())
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
            })
    }

    pub(crate) async fn metadata_reidentify_job_cancel_requested(
        &self,
        job_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar("SELECT cancel_requested FROM metadata_reidentify_jobs WHERE id = ?")
            .bind(job_id)
            .fetch_one(&self.pool)
            .await
            .map(|value: i64| value != 0)
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn request_metadata_reidentify_job_cancel(
        &self,
        job_id: &str,
    ) -> Result<bool, StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let result = self
            .query(
                "UPDATE metadata_reidentify_jobs
             SET cancel_requested = 1, updated_at = unixepoch()
             WHERE id = ? AND status IN ('QUEUED', 'RUNNING')",
            )
            .bind(job_id)
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

    pub(crate) async fn retry_metadata_reidentify_job(
        &self,
        job_id: &str,
    ) -> Result<bool, StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let result = self
            .query(
                "UPDATE metadata_reidentify_jobs
             SET status = 'QUEUED', dispatch_source = 'EXPLICIT',
                 processed_count = (
                     SELECT COUNT(*) FROM metadata_reidentify_job_items
                     WHERE job_id = ? AND status = 'COMPLETED'
                 ),
                 cancel_requested = 0, error = NULL, started_at = NULL, finished_at = NULL,
                 updated_at = unixepoch()
             WHERE id = ? AND status IN ('FAILED', 'CANCELLED', 'COMPLETED_WITH_ISSUES', 'DEFERRED')",
            )
            .bind(job_id)
            .bind(job_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if result.rows_affected() == 1 {
            self.query(
                "UPDATE metadata_reidentify_job_items
                 SET status = 'PENDING', candidate_count = 0, error = NULL,
                     automatic_retry_after = NULL,
                     automatic_retry_consumed = 0,
                     claimed_request_fingerprint = NULL,
                     claimed_request_capabilities_json = '[]',
                     updated_at = unixepoch()
                 WHERE job_id = ? AND status IN ('FAILED', 'RUNNING', 'PENDING', 'CANCELLED')",
            )
            .bind(job_id)
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

    pub(crate) async fn list_shutdown_interrupted_metadata_reidentify_job_ids(
        &self,
        after_job_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<String>, StorageError> {
        self.query_scalar(
            "SELECT jobs.id
             FROM metadata_reidentify_jobs jobs
             WHERE jobs.status = 'CANCELLED'
               AND jobs.error = 'SERVER_SHUTDOWN'
               AND (? IS NULL OR jobs.id > ?)
               AND EXISTS (
                   SELECT 1 FROM metadata_reidentify_job_items items
                   WHERE items.job_id = jobs.id
                     AND (items.status IN ('PENDING', 'RUNNING')
                          OR (items.status = 'FAILED' AND items.error = 'SERVER_SHUTDOWN'))
               )
             ORDER BY jobs.id LIMIT ?",
        )
        .bind(after_job_id)
        .bind(after_job_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn resume_metadata_reidentify_job_after_shutdown(
        &self,
        job_id: &str,
    ) -> Result<bool, StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let result = self
            .query(
                "UPDATE metadata_reidentify_jobs
                 SET status = 'QUEUED',
                     processed_count = (
                         SELECT COUNT(*) FROM metadata_reidentify_job_items
                         WHERE job_id = ? AND status = 'COMPLETED'
                     ),
                     cancel_requested = 0, error = NULL, started_at = NULL,
                     finished_at = NULL, updated_at = unixepoch()
                 WHERE id = ? AND status = 'CANCELLED' AND error = 'SERVER_SHUTDOWN'
                   AND EXISTS (
                       SELECT 1 FROM metadata_reidentify_job_items
                       WHERE job_id = metadata_reidentify_jobs.id
                         AND (status IN ('PENDING', 'RUNNING')
                              OR (status = 'FAILED' AND error = 'SERVER_SHUTDOWN'))
                   )",
            )
            .bind(job_id)
            .bind(job_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if result.rows_affected() == 1 {
            self.query(
                "UPDATE metadata_reidentify_job_items
                 SET status = 'PENDING', candidate_count = 0, error = NULL,
                     automatic_retry_after = NULL, automatic_retry_consumed = 0,
                     claimed_request_fingerprint = NULL,
                     claimed_request_capabilities_json = '[]', updated_at = unixepoch()
                 WHERE job_id = ? AND (
                     status IN ('PENDING', 'RUNNING')
                     OR (status = 'FAILED' AND error = 'SERVER_SHUTDOWN')
                 )",
            )
            .bind(job_id)
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

    pub(crate) async fn list_metadata_reidentify_items(
        &self,
        job_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredMetadataReidentifyItem>, StorageError> {
        self.query(
            "SELECT job_id, item_id, status, candidate_count, error, updated_at
             FROM metadata_reidentify_job_items
             WHERE job_id = ? ORDER BY item_id LIMIT ? OFFSET ?",
        )
        .bind(job_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(stored_metadata_reidentify_item)
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn metadata_reidentify_job_has_failed_items(
        &self,
        job_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(
                 SELECT 1 FROM metadata_reidentify_job_items
                 WHERE job_id = ? AND status = 'FAILED'
             ) THEN 1 ELSE 0 END",
        )
        .bind(job_id)
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn metadata_reidentify_job_has_pending_items(
        &self,
        job_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(
                 SELECT 1 FROM metadata_reidentify_job_items
                 WHERE job_id = ? AND status = 'PENDING'
             ) THEN 1 ELSE 0 END",
        )
        .bind(job_id)
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn metadata_reidentify_job_has_item_error(
        &self,
        job_id: &str,
        error: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(
                 SELECT 1 FROM metadata_reidentify_job_items
                 WHERE job_id = ? AND error = ?
             ) THEN 1 ELSE 0 END",
        )
        .bind(job_id)
        .bind(error)
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_active_metadata_reidentify_job_ids(
        &self,
    ) -> Result<Vec<String>, StorageError> {
        self.query_scalar(
            "SELECT id FROM metadata_reidentify_jobs
             WHERE status IN ('QUEUED', 'RUNNING')
             ORDER BY created_at, id LIMIT 10000",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }
}
