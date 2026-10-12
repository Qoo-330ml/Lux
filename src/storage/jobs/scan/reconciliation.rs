use super::*;

#[path = "reconciliation/batch.rs"]
mod batch;

impl Database {
    pub(crate) async fn list_reconciliation_missing_filesystem_entry_paths_page(
        &self,
        job_id: &str,
        library_root_id: &str,
        after_relative_path: Option<&str>,
        limit: i64,
    ) -> Result<Vec<String>, StorageError> {
        self.query_scalar(
            "SELECT fe.relative_path
             FROM filesystem_entries fe
             WHERE fe.library_root_id = ? AND fe.is_missing = 0
               AND NOT EXISTS (
                   SELECT 1
                   FROM reconciliation_scan_entries rse
                   WHERE rse.job_id = ?
                     AND rse.library_root_id = fe.library_root_id
                     AND rse.entry_type = 'FILE'
                     AND rse.relative_path = fe.relative_path
               )
               AND fe.relative_path > ?
             ORDER BY fe.relative_path
             LIMIT ?",
        )
        .bind(library_root_id)
        .bind(job_id)
        .bind(after_relative_path.unwrap_or_default())
        .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
        .fetch_all(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_reconciliation_scan_entries(
        &self,
        job_id: &str,
        entry_type: &str,
        limit: i64,
    ) -> Result<Vec<StoredReconciliationScanEntry>, StorageError> {
        self.query(
            "SELECT library_root_id, relative_path
             FROM reconciliation_scan_entries
             WHERE job_id = ? AND entry_type = ? AND status = 'PENDING'
             ORDER BY library_root_id, relative_path
             LIMIT ?",
        )
        .bind(job_id)
        .bind(entry_type)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(stored_reconciliation_scan_entry)
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn commit_reconciliation_discovery_chunk(
        &self,
        job_id: &str,
        library_root_id: &str,
        child_directories: &[String],
        media_files: &[String],
        completed_directory: Option<&str>,
    ) -> Result<i64, StorageError> {
        if child_directories.is_empty() && media_files.is_empty() && completed_directory.is_none() {
            return Ok(0);
        }
        let mut transaction = self.begin_scan_write_transaction().await?;
        let inserted_file_count = self
            .insert_reconciliation_directory_entries(
                &mut transaction,
                job_id,
                library_root_id,
                child_directories,
                media_files,
            )
            .await?;
        self.increment_reconciliation_total_count_in_transaction(
            &mut transaction,
            job_id,
            inserted_file_count,
        )
        .await?;
        if let Some(completed_directory) = completed_directory {
            self.query(
                "DELETE FROM reconciliation_scan_entries
                 WHERE job_id = ? AND library_root_id = ?
                   AND relative_path = ? AND entry_type = 'DIRECTORY'",
            )
            .bind(job_id)
            .bind(library_root_id)
            .bind(completed_directory)
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
        i64::try_from(inserted_file_count)
            .map_err(|_| StorageError::Conflict("reconciliation file count overflow".to_owned()))
    }

    async fn insert_reconciliation_directory_entries(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        job_id: &str,
        library_root_id: &str,
        child_directories: &[String],
        media_files: &[String],
    ) -> Result<u64, StorageError> {
        let mut inserted_file_count = 0_u64;
        for (entry_type, paths) in [("DIRECTORY", child_directories), ("FILE", media_files)] {
            for chunk in paths.chunks(SCAN_DML_CHUNK_SIZE) {
                if chunk.is_empty() {
                    continue;
                }
                let values = std::iter::repeat_n("(?, ?, ?, ?)", chunk.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                let query = format!(
                    "INSERT INTO reconciliation_scan_entries (
                         job_id, library_root_id, relative_path, entry_type
                     ) VALUES {values}
                     ON CONFLICT(job_id, entry_type, library_root_id, relative_path) DO NOTHING"
                );
                let mut statement = self.query(sqlx::AssertSqlSafe(query));
                for path in chunk {
                    statement = statement
                        .bind(job_id)
                        .bind(library_root_id)
                        .bind(path)
                        .bind(entry_type);
                }
                let result = statement
                    .execute(&mut **transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                if entry_type == "FILE" {
                    inserted_file_count = inserted_file_count
                        .checked_add(result.rows_affected())
                        .ok_or_else(|| {
                        StorageError::Conflict("reconciliation file count overflow".to_owned())
                    })?;
                }
            }
        }
        Ok(inserted_file_count)
    }

    async fn increment_reconciliation_total_count_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        job_id: &str,
        inserted_file_count: u64,
    ) -> Result<(), StorageError> {
        if inserted_file_count == 0 {
            return Ok(());
        }
        let inserted_file_count = i64::try_from(inserted_file_count)
            .map_err(|_| StorageError::Conflict("reconciliation file count overflow".to_owned()))?;
        self.query(
            "UPDATE scan_jobs
             SET total_count = CASE
                     WHEN total_count < processed_count
                         THEN processed_count + ?
                     ELSE total_count + ?
                 END,
                 updated_at = unixepoch()
             WHERE id = ? AND status = 'RUNNING' AND discovery_completed = 0",
        )
        .bind(inserted_file_count)
        .bind(inserted_file_count)
        .bind(job_id)
        .execute(&mut **transaction)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn finish_reconciliation_discovery(
        &self,
        job_id: &str,
    ) -> Result<i64, StorageError> {
        let mut transaction = self.begin_scan_write_transaction().await?;
        let discovered_file_count: i64 = self
            .query_scalar(
                "SELECT COUNT(*) FROM reconciliation_scan_entries
             WHERE job_id = ? AND entry_type = 'FILE'",
            )
            .bind(job_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let processed_count: i64 = self
            .query_scalar("SELECT processed_count FROM scan_jobs WHERE id = ?")
            .bind(job_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let total_count = discovered_file_count.max(processed_count);
        self.query(
            "UPDATE scan_jobs
             SET discovery_completed = 1, total_count = ?, updated_at = unixepoch()
             WHERE id = ? AND status = 'RUNNING'",
        )
        .bind(total_count)
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

    pub(crate) async fn discard_reconciliation_root_entries(
        &self,
        job_id: &str,
        library_root_id: &str,
    ) -> Result<i64, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let file_count: i64 = self
            .query_scalar(
                "SELECT COUNT(*) FROM reconciliation_scan_entries
             WHERE job_id = ? AND library_root_id = ?
               AND entry_type = 'FILE' AND status = 'PENDING'",
            )
            .bind(job_id)
            .bind(library_root_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.query(
            "DELETE FROM reconciliation_scan_entries
             WHERE job_id = ? AND library_root_id = ?",
        )
        .bind(job_id)
        .bind(library_root_id)
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
        Ok(file_count)
    }

    pub(crate) async fn clear_reconciliation_scan_entries(
        &self,
        job_id: &str,
    ) -> Result<(), StorageError> {
        self.query("DELETE FROM reconciliation_scan_entries WHERE job_id = ?")
            .bind(job_id)
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    /// Commits one reconciliation batch atomically.
    ///
    /// The batch is scoped to one library root. The caller must prepare all
    /// filesystem data before this call; all database changes are committed
    /// together or rolled back together. The returned counts describe
    /// `(confirmed_entries, created_items)`.
    pub(crate) async fn commit_reconciliation_batch(
        &self,
        batch: &ReconciliationBatchCommit<'_>,
    ) -> Result<ReconciliationBatchCommitResult, StorageError> {
        if batch.entries.is_empty()
            && batch.movie_files.is_empty()
            && batch.episode_files.is_empty()
            && batch.seen_entry_ids.is_empty()
            && batch.missing_paths.is_empty()
            && batch.new_paths.is_empty()
            && batch.changed_paths.is_empty()
            && batch.sidecar_paths.is_empty()
        {
            return Ok(ReconciliationBatchCommitResult {
                confirmed_entries: 0,
                created_items: 0,
                metadata_targets_changed: false,
            });
        }
        self.commit_reconciliation_batch_in_transaction(batch).await
    }

    async fn commit_reconciliation_batch_in_transaction(
        &self,
        batch: &ReconciliationBatchCommit<'_>,
    ) -> Result<ReconciliationBatchCommitResult, StorageError> {
        if batch
            .entries
            .iter()
            .any(|entry| entry.library_root_id != batch.library_root_id)
        {
            return Err(StorageError::Conflict(
                "reconciliation batch cannot span library roots".to_owned(),
            ));
        }

        let mut transaction = self.begin_scan_write_transaction().await?;
        let job_status: Option<String> = self
            .query_scalar("SELECT status FROM scan_jobs WHERE id = ?")
            .bind(batch.job_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if job_status.as_deref() != Some("RUNNING") {
            return Err(StorageError::Conflict(
                "reconciliation batch requires a running scan job".to_owned(),
            ));
        }

        let pending_paths = self
            .list_pending_reconciliation_paths_in_transaction(&mut transaction, batch)
            .await?;
        let (created_items, metadata_targets_changed) = self
            .apply_reconciliation_batch_media_and_targets(&mut transaction, batch, &pending_paths)
            .await?;

        let missing_count = self
            .discard_reconciliation_file_entries_in_transaction(&mut transaction, batch)
            .await?;
        let confirmed_entries = self
            .confirm_reconciliation_entries_in_transaction(&mut transaction, batch)
            .await?;
        let confirmed_count = i64::try_from(confirmed_entries).map_err(|_| {
            StorageError::Conflict("reconciliation batch confirmation count overflow".to_owned())
        })?;
        let missing_count = i64::try_from(missing_count).map_err(|_| {
            StorageError::Conflict("reconciliation batch confirmation count overflow".to_owned())
        })?;
        let confirmed_count = confirmed_count.checked_add(missing_count).ok_or_else(|| {
            StorageError::Conflict("reconciliation batch confirmation count overflow".to_owned())
        })?;
        let update = self
            .query(
                "UPDATE scan_jobs
                 SET cursor = CASE WHEN ? > 0 THEN ? ELSE cursor END,
                     processed_count = processed_count + ?,
                     total_count = CASE
                         WHEN total_count < processed_count + ?
                             THEN processed_count + ?
                         ELSE total_count
                     END,
                     updated_at = unixepoch()
                 WHERE id = ? AND status = 'RUNNING'",
            )
            .bind(confirmed_count)
            .bind(
                batch
                    .entries
                    .last()
                    .map(|entry| entry.relative_path.as_str()),
            )
            .bind(confirmed_count)
            .bind(confirmed_count)
            .bind(confirmed_count);
        let result = update
            .bind(batch.job_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if result.rows_affected() != 1 {
            return Err(StorageError::Conflict(
                "reconciliation scan job stopped during batch commit".to_owned(),
            ));
        }

        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(ReconciliationBatchCommitResult {
            confirmed_entries: usize::try_from(confirmed_count).map_err(|_| {
                StorageError::Conflict(
                    "reconciliation batch confirmation count overflow".to_owned(),
                )
            })?,
            created_items,
            metadata_targets_changed,
        })
    }

    async fn discard_reconciliation_file_entries_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        batch: &ReconciliationBatchCommit<'_>,
    ) -> Result<u64, StorageError> {
        if batch.missing_paths.is_empty() {
            return Ok(0);
        }
        let mut discarded = 0_u64;
        for paths in batch.missing_paths.chunks(SCAN_DML_CHUNK_SIZE) {
            let placeholders = std::iter::repeat_n("?", paths.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "DELETE FROM reconciliation_scan_entries
                 WHERE job_id = ? AND library_root_id = ?
                   AND entry_type = 'FILE' AND status = 'PENDING'
                   AND relative_path IN ({placeholders})"
            );
            let mut statement = self
                .query(sqlx::AssertSqlSafe(query))
                .bind(batch.job_id)
                .bind(batch.library_root_id);
            for path in paths {
                statement = statement.bind(path);
            }
            discarded = discarded.saturating_add(
                statement
                    .execute(&mut **transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?
                    .rows_affected(),
            );
        }
        Ok(discarded)
    }

    async fn confirm_reconciliation_entries_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        batch: &ReconciliationBatchCommit<'_>,
    ) -> Result<usize, StorageError> {
        let paths = batch
            .entries
            .iter()
            .map(|entry| entry.relative_path.as_str())
            .collect::<Vec<_>>();
        let mut confirmed = 0_u64;
        for chunk in paths.chunks(SCAN_DML_CHUNK_SIZE) {
            if chunk.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "UPDATE reconciliation_scan_entries
                 SET status = 'DONE'
                 WHERE job_id = ? AND library_root_id = ?
                   AND entry_type = 'FILE' AND status = 'PENDING'
                   AND relative_path IN ({placeholders})"
            );
            let mut statement = self
                .query(sqlx::AssertSqlSafe(query))
                .bind(batch.job_id)
                .bind(batch.library_root_id);
            for path in chunk {
                statement = statement.bind(path);
            }
            confirmed = confirmed.saturating_add(
                statement
                    .execute(&mut **transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?
                    .rows_affected(),
            );
        }
        usize::try_from(confirmed).map_err(|_| {
            StorageError::Conflict("reconciliation confirmation count overflow".to_owned())
        })
    }

    async fn list_pending_reconciliation_paths_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        batch: &ReconciliationBatchCommit<'_>,
    ) -> Result<HashSet<String>, StorageError> {
        let paths = batch
            .entries
            .iter()
            .map(|entry| entry.relative_path.as_str())
            .collect::<Vec<_>>();
        if paths.is_empty() {
            return Ok(HashSet::new());
        }
        let mut pending = HashSet::new();
        for chunk in paths.chunks(SCAN_DML_CHUNK_SIZE) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT relative_path
                 FROM reconciliation_scan_entries
                 WHERE job_id = ? AND library_root_id = ?
                   AND entry_type = 'FILE' AND status = 'PENDING'
                   AND relative_path IN ({placeholders})"
            );
            let mut statement = self
                .query(sqlx::AssertSqlSafe(query))
                .bind(batch.job_id)
                .bind(batch.library_root_id);
            for path in chunk {
                statement = statement.bind(path);
            }
            let rows = statement
                .fetch_all(&mut **transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            for row in rows {
                let path = row
                    .try_get::<String, _>("relative_path")
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                pending.insert(path);
            }
        }
        Ok(pending)
    }

    pub(crate) async fn finalize_reconciliation_root_page(
        &self,
        job_id: &str,
        library_root_id: &str,
        generation: &str,
        missing_paths: &[String],
        removed_media_paths: &[String],
        removed_sidecar_paths: &[String],
    ) -> Result<u64, StorageError> {
        if missing_paths.is_empty() {
            return Ok(0);
        }
        let mut transaction = self.begin_scan_write_transaction().await?;
        self.record_scan_job_removed_targets_in_transaction(
            &mut transaction,
            job_id,
            library_root_id,
            removed_media_paths,
        )
        .await?;
        let sidecar_directories = prune_sidecar_directories(
            removed_sidecar_paths
                .iter()
                .filter_map(|path| {
                    Path::new(path)
                        .parent()
                        .and_then(|parent| parent.to_str())
                        .map(|parent| {
                            if parent.is_empty() {
                                ".".to_owned()
                            } else {
                                parent.to_owned()
                            }
                        })
                })
                .collect(),
        );
        let _ = self
            .record_scan_job_sidecar_targets_in_transaction(
                &mut transaction,
                job_id,
                library_root_id,
                &sidecar_directories,
            )
            .await?;
        let missing_entries = self
            .mark_missing_filesystem_entry_paths_in_transaction(
                &mut transaction,
                library_root_id,
                generation,
                missing_paths,
            )
            .await?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(missing_entries)
    }

    pub(crate) async fn has_reconciliation_scan_entries(
        &self,
        job_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(
                 SELECT 1 FROM reconciliation_scan_entries
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
}
