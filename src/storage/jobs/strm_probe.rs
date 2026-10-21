use super::*;

#[cfg(test)]
mod playback_scope_tests {
    use super::*;

    #[tokio::test]
    async fn source_scoped_job_round_trips_without_changing_library_jobs()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let config = crate::config::Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let library = crate::application::libraries::LibraryService::new(database.clone())
            .create_library("Movies", crate::library::LibraryKind::Movie, false)
            .await?;
        sqlx::query(
            "INSERT INTO strm_probe_jobs
             (id, operation_id, library_id, status, concurrency, include_ready,
              write_sidecars, total_count, target_source_id)
             VALUES ('playback', 'operation', ?, 'PENDING', 1, 0, 0, 1, 'source')",
        )
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;
        let target: Option<String> = sqlx::query_scalar(
            "SELECT target_source_id FROM strm_probe_jobs WHERE id = 'playback'",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(target.as_deref(), Some("source"));
        let job = database.find_strm_probe_job("playback").await?;
        assert_eq!(
            job.as_ref().and_then(|job| job.target_source_id.as_deref()),
            Some("source")
        );
        assert_eq!(job.map(|job| job.total_count), Some(1));
        database.close().await;
        Ok(())
    }
}

impl Database {
    pub(crate) async fn create_strm_probe_job(
        &self,
        job: NewStrmProbeJob<'_>,
    ) -> Result<(), StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.query(
            "INSERT INTO strm_probe_jobs (
                id, operation_id, library_id, status, concurrency,
                include_ready, write_sidecars, media_info_enabled,
                thumbnail_enabled, thumbnail_position_percent, target_scan_job_id,
                target_source_id, total_count
             ) VALUES (?, ?, ?, 'PENDING', ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(job.id)
        .bind(job.operation_id)
        .bind(job.library_id)
        .bind(job.concurrency)
        .bind(database_flag(job.include_ready))
        .bind(database_flag(job.write_sidecars))
        .bind(database_flag(job.media_info_enabled))
        .bind(database_flag(job.thumbnail_enabled))
        .bind(job.thumbnail_position_percent)
        .bind(job.target_scan_job_id)
        .bind(job.target_source_id)
        .bind(job.total_count)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        if job.total_count > 0 {
            self.mark_strm_probe_sources_pending_in_transaction(
                &mut transaction,
                job.library_id,
                job.target_scan_job_id,
            )
            .await?;
        }
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn has_active_strm_probe_jobs(&self) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(
                SELECT 1 FROM strm_probe_jobs WHERE status IN ('PENDING', 'RUNNING')
            ) THEN 1 ELSE 0 END",
        )
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn has_active_strm_probe_jobs_for_operation(
        &self,
        operation_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(
                SELECT 1 FROM strm_probe_jobs
                WHERE operation_id = ? AND status IN ('PENDING', 'RUNNING')
            ) THEN 1 ELSE 0 END",
        )
        .bind(operation_id)
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_strm_probe_job(
        &self,
        id: &str,
    ) -> Result<Option<StoredStrmProbeJob>, StorageError> {
        self.query(
            "SELECT id, operation_id, library_id, status, concurrency,
                    include_ready, write_sidecars, media_info_enabled,
                    thumbnail_enabled, thumbnail_position_percent, target_scan_job_id,
                    target_source_id, cursor, processed_count,
                    total_count, cancel_requested, error,
                    created_at, started_at, finished_at
             FROM strm_probe_jobs WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_strm_probe_job))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_strm_probe_jobs(
        &self,
        status: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredStrmProbeJob>, StorageError> {
        let rows = if let Some(status) = status {
            self.query(
                "SELECT id, operation_id, library_id, status, concurrency,
                        include_ready, write_sidecars, media_info_enabled,
                        thumbnail_enabled, thumbnail_position_percent, target_scan_job_id,
                        target_source_id, cursor, processed_count,
                        total_count, cancel_requested, error,
                        created_at, started_at, finished_at
                 FROM strm_probe_jobs WHERE status = ?
                 ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
            )
            .bind(status)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        } else {
            self.query(
                "SELECT id, operation_id, library_id, status, concurrency,
                        include_ready, write_sidecars, media_info_enabled,
                        thumbnail_enabled, thumbnail_position_percent, target_scan_job_id,
                        target_source_id, cursor, processed_count,
                        total_count, cancel_requested, error,
                        created_at, started_at, finished_at
                 FROM strm_probe_jobs
                 ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
            )
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        };
        rows.map(|rows| rows.into_iter().map(stored_strm_probe_job).collect())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_active_strm_probe_job_ids(&self) -> Result<Vec<String>, StorageError> {
        self.query_scalar(
            "SELECT id FROM strm_probe_jobs
             WHERE status IN ('PENDING', 'RUNNING')
             ORDER BY created_at, id LIMIT 10000",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn claim_strm_probe_job(&self, id: &str) -> Result<bool, StorageError> {
        self.query(
            "UPDATE strm_probe_jobs
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

    pub(crate) async fn update_strm_probe_job_progress(
        &self,
        id: &str,
        cursor: Option<&str>,
        processed_count: i64,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE strm_probe_jobs
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

    pub(crate) async fn strm_probe_job_cancel_requested(
        &self,
        id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar("SELECT cancel_requested FROM strm_probe_jobs WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .map(|value: i64| value != 0)
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn request_strm_probe_job_cancel(&self, id: &str) -> Result<(), StorageError> {
        self.query(
            "UPDATE strm_probe_jobs SET cancel_requested = 1, updated_at = unixepoch()
             WHERE id = ? AND status IN ('PENDING', 'RUNNING')",
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

    pub(crate) async fn finish_strm_probe_job(
        &self,
        id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<(), StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let cancelled_scope: Option<(String, Option<String>)> = self
            .query_as(
                "SELECT library_id, target_source_id FROM strm_probe_jobs
                 WHERE id = ? AND cancel_requested = 1
                   AND status IN ('PENDING', 'RUNNING')",
            )
            .bind(id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if let Some((library_id, target_source_id)) = cancelled_scope {
            let reset = if let Some(source_id) = target_source_id {
                self.query(
                    "UPDATE media_sources
                     SET probe_status = 'NOT_CONFIGURED', probe_error = NULL,
                         updated_at = unixepoch()
                     WHERE id = ? AND source_kind = 'STRM_URL' AND probe_status = 'PENDING'
                       AND item_id IN (
                           SELECT id FROM media_items WHERE library_id = ?
                       )",
                )
                .bind(source_id)
                .bind(library_id)
                .execute(&mut *transaction)
                .await
            } else {
                self.query(
                    "UPDATE media_sources
                     SET probe_status = 'NOT_CONFIGURED', probe_error = NULL,
                         updated_at = unixepoch()
                     WHERE source_kind = 'STRM_URL' AND probe_status = 'PENDING'
                       AND item_id IN (
                           SELECT id FROM media_items WHERE library_id = ?
                       )",
                )
                .bind(library_id)
                .execute(&mut *transaction)
                .await
            };
            reset.map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        self.query(
            "UPDATE strm_probe_jobs
             SET status = CASE WHEN cancel_requested = 1 THEN 'CANCELLED' ELSE ? END,
                 error = CASE WHEN cancel_requested = 1 THEN NULL ELSE ? END,
                 finished_at = unixepoch(), updated_at = unixepoch()
             WHERE id = ? AND status IN ('PENDING', 'RUNNING')",
        )
        .bind(status)
        .bind(error)
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
            })
    }
}
