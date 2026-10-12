// Six bound values per row keep each insert under SQLite's conservative bind limit.
const MAX_SCAN_LOCAL_METADATA_BATCH_INSERT_ROWS: usize = 64;
#[allow(dead_code)] // The test-only batch listing API enforces this storage contract bound.
const MAX_SCAN_LOCAL_METADATA_BATCH_PAGE_SIZE: i64 = 100;
use super::*;

impl Database {
    #[allow(dead_code)] // Storage contract tests cover idempotent single-batch enqueue.
    pub(crate) async fn enqueue_scan_local_metadata_batch(
        &self,
        batch: NewScanLocalMetadataBatch<'_>,
    ) -> Result<bool, StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let inserted = self
            .enqueue_scan_local_metadata_batch_in_transaction(&mut transaction, batch)
            .await?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(inserted)
    }

    #[allow(dead_code)] // Shared by the test-only single-batch enqueue contract.
    async fn enqueue_scan_local_metadata_batch_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        batch: NewScanLocalMetadataBatch<'_>,
    ) -> Result<bool, StorageError> {
        if batch.id.trim().is_empty()
            || batch.job_id.trim().is_empty()
            || batch.library_root_id.trim().is_empty()
            || batch.batch_sequence < 0
            || batch.source_ids.is_empty()
            || batch.source_ids.len() > MAX_SCAN_LOCAL_METADATA_BATCH_SOURCES
        {
            return Err(StorageError::Conflict(
                "scan local metadata batch has invalid identity, sequence, or source count".into(),
            ));
        }
        let mut unique_sources = std::collections::HashSet::with_capacity(batch.source_ids.len());
        if batch
            .source_ids
            .iter()
            .any(|source_id| source_id.trim().is_empty() || !unique_sources.insert(source_id))
        {
            return Err(StorageError::Conflict(
                "scan local metadata batch contains an empty or duplicate source".into(),
            ));
        }
        let source_refs_json = serde_json::to_string(batch.source_ids)
            .map_err(|error| StorageError::Serialization(error.to_string()))?;
        let source_count = i64::try_from(batch.source_ids.len()).map_err(|_| {
            StorageError::Conflict("scan local metadata source count overflow".into())
        })?;

        let inserted = self
            .query(
                "INSERT INTO scan_local_metadata_batches (
                     id, job_id, library_root_id, batch_sequence, source_refs_json, source_count
                 ) VALUES (?, ?, ?, ?, ?, ?)
                 ON CONFLICT(job_id, library_root_id, batch_sequence) DO NOTHING
                 RETURNING id",
            )
            .bind(batch.id)
            .bind(batch.job_id)
            .bind(batch.library_root_id)
            .bind(batch.batch_sequence)
            .bind(&source_refs_json)
            .bind(source_count)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if inserted.is_some() {
            return Ok(true);
        }

        let existing = self
            .query_as::<(String, String, i64)>(
                "SELECT id, source_refs_json, source_count
                 FROM scan_local_metadata_batches
                 WHERE job_id = ? AND library_root_id = ? AND batch_sequence = ?",
            )
            .bind(batch.job_id)
            .bind(batch.library_root_id)
            .bind(batch.batch_sequence)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let Some((existing_id, existing_sources, existing_count)) = existing else {
            return Err(StorageError::Conflict(
                "scan local metadata batch id already belongs to another batch".into(),
            ));
        };
        if existing_id != batch.id
            || existing_sources != source_refs_json
            || existing_count != source_count
        {
            return Err(StorageError::Conflict(
                "scan local metadata batch sequence already has different input".into(),
            ));
        }
        Ok(false)
    }

    pub(in crate::storage::repository::jobs) async fn enqueue_manifest_local_metadata_refs_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        job_id: &str,
        library_root_id: &str,
        local_metadata_refs: &[(&str, &str)],
        sequence_start: i64,
    ) -> Result<(), StorageError> {
        let mut ordered_refs = local_metadata_refs.to_vec();
        ordered_refs.sort_unstable();
        let mut unique_refs = std::collections::HashSet::with_capacity(ordered_refs.len());
        ordered_refs.retain(|(_, reference_id)| unique_refs.insert(*reference_id));

        let mut batches = Vec::with_capacity(
            ordered_refs
                .len()
                .div_ceil(MAX_SCAN_LOCAL_METADATA_BATCH_SOURCES),
        );
        for (batch_index, refs) in ordered_refs
            .chunks(MAX_SCAN_LOCAL_METADATA_BATCH_SOURCES)
            .enumerate()
        {
            let batch_index = i64::try_from(batch_index).map_err(|_| {
                StorageError::Conflict("local metadata batch sequence overflow".into())
            })?;
            let batch_sequence = sequence_start.checked_add(batch_index).ok_or_else(|| {
                StorageError::Conflict("local metadata batch sequence overflow".into())
            })?;
            let source_ids = refs
                .iter()
                .map(|(_, reference_id)| (*reference_id).to_owned())
                .collect::<Vec<_>>();
            let id = format!("{job_id}:{library_root_id}:{batch_sequence}");
            if source_ids
                .iter()
                .any(|source_id| source_id.trim().is_empty())
            {
                return Err(StorageError::Conflict(
                    "scan local metadata batch contains an empty source".into(),
                ));
            }
            let source_count = i64::try_from(source_ids.len()).map_err(|_| {
                StorageError::Conflict("scan local metadata source count overflow".into())
            })?;
            let source_refs_json = serde_json::to_string(&source_ids)
                .map_err(|error| StorageError::Serialization(error.to_string()))?;
            batches.push((batch_sequence, id, source_refs_json, source_count));
        }

        for batch_rows in batches.chunks(MAX_SCAN_LOCAL_METADATA_BATCH_INSERT_ROWS) {
            let values = std::iter::repeat_n("(?, ?, ?, ?, ?, ?)", batch_rows.len())
                .collect::<Vec<_>>()
                .join(", ");
            let mut insert = self.query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO scan_local_metadata_batches (
                     id, job_id, library_root_id, batch_sequence, source_refs_json, source_count
                 ) VALUES {values}
                 ON CONFLICT(job_id, library_root_id, batch_sequence) DO NOTHING
                 RETURNING batch_sequence"
            )));
            for (batch_sequence, id, source_refs_json, source_count) in batch_rows {
                insert = insert
                    .bind(id)
                    .bind(job_id)
                    .bind(library_root_id)
                    .bind(*batch_sequence)
                    .bind(source_refs_json)
                    .bind(*source_count);
            }
            let inserted_sequences = insert
                .fetch_all(&mut **transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
                .into_iter()
                .map(|row| row.get::<i64, _>(0))
                .collect::<std::collections::HashSet<_>>();
            if inserted_sequences.len() == batch_rows.len() {
                continue;
            }

            let existing_sequences = batch_rows
                .iter()
                .map(|(sequence, _, _, _)| *sequence)
                .filter(|sequence| !inserted_sequences.contains(sequence))
                .collect::<Vec<_>>();
            let placeholders = std::iter::repeat_n("?", existing_sequences.len())
                .collect::<Vec<_>>()
                .join(", ");
            let mut select = self
                .query_as::<(i64, String, String, i64)>(sqlx::AssertSqlSafe(format!(
                    "SELECT batch_sequence, id, source_refs_json, source_count
                     FROM scan_local_metadata_batches
                     WHERE job_id = ? AND library_root_id = ?
                       AND batch_sequence IN ({placeholders})"
                )))
                .bind(job_id)
                .bind(library_root_id);
            for batch_sequence in &existing_sequences {
                select = select.bind(batch_sequence);
            }
            let existing_batches =
                select
                    .fetch_all(&mut **transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
            if existing_batches.len() != existing_sequences.len()
                || existing_batches
                    .iter()
                    .any(|(sequence, id, source_refs_json, source_count)| {
                        !batch_rows.iter().any(
                            |(expected_sequence, expected_id, expected_sources, expected_count)| {
                                expected_sequence == sequence
                                    && expected_id == id
                                    && expected_sources == source_refs_json
                                    && expected_count == source_count
                            },
                        )
                    })
            {
                return Err(StorageError::Conflict(
                    "scan local metadata batch sequence already has different input".into(),
                ));
            }
        }
        Ok(())
    }

    #[allow(dead_code)] // Storage tests inspect pagination and persisted batch state.
    pub(crate) async fn list_scan_local_metadata_batches(
        &self,
        after_created_at: Option<i64>,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<StoredScanLocalMetadataBatch>, StorageError> {
        let limit = i64::try_from(limit)
            .map_err(|_| StorageError::Conflict("invalid scan metadata page size".into()))?;
        if !(1..=MAX_SCAN_LOCAL_METADATA_BATCH_PAGE_SIZE).contains(&limit)
            || after_created_at.is_some() != after_id.is_some()
        {
            return Err(StorageError::Conflict(
                "invalid scan metadata page size or cursor".into(),
            ));
        }
        let rows = match (after_created_at, after_id) {
            (None, None) => {
                self.query(
                    "SELECT * FROM scan_local_metadata_batches
                     ORDER BY created_at, id LIMIT ?",
                )
                .bind(limit)
                .fetch_all(&self.pool)
                .await
            }
            (Some(created_at), Some(id)) => {
                self.query(
                    "SELECT * FROM scan_local_metadata_batches
                     WHERE created_at > ? OR (created_at = ? AND id > ?)
                     ORDER BY created_at, id LIMIT ?",
                )
                .bind(created_at)
                .bind(created_at)
                .bind(id)
                .bind(limit)
                .fetch_all(&self.pool)
                .await
            }
            _ => {
                return Err(StorageError::Conflict(
                    "invalid scan metadata page cursor".into(),
                ));
            }
        }
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        Ok(rows
            .into_iter()
            .map(stored_scan_local_metadata_batch)
            .collect())
    }

    pub(crate) async fn mark_scan_local_metadata_images_complete(
        &self,
        batch_id: &str,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE scan_local_metadata_batches
             SET images_completed_at = unixepoch(), updated_at = unixepoch()
             WHERE id = ? AND status = 'RUNNING'",
        )
        .bind(batch_id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn has_pending_scan_local_metadata_images(
        &self,
        job_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS (
                 SELECT 1 FROM scan_local_metadata_batches
                 WHERE job_id = ? AND images_completed_at IS NULL
                   AND status IN ('PENDING', 'RUNNING')
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

    pub(crate) async fn claim_next_scan_local_metadata_batch(
        &self,
    ) -> Result<Option<StoredScanLocalMetadataBatch>, StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let next_id = self
            .query_scalar::<String>(
                "SELECT id FROM scan_local_metadata_batches
                 WHERE status IN ('PENDING', 'FAILED')
                   AND (next_attempt_at IS NULL OR next_attempt_at <= unixepoch())
                 ORDER BY created_at, id LIMIT 1",
            )
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let Some(next_id) = next_id else {
            transaction
                .commit()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(None);
        };
        let claimed = self
            .query(
                "UPDATE scan_local_metadata_batches
                 SET status = 'RUNNING', attempts = attempts + 1, next_attempt_at = NULL,
                     error = NULL, updated_at = unixepoch()
                 WHERE id = ? AND status IN ('PENDING', 'FAILED')
                   AND (next_attempt_at IS NULL OR next_attempt_at <= unixepoch())
                 RETURNING *",
            )
            .bind(next_id)
            .fetch_optional(&mut *transaction)
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
        Ok(claimed.map(stored_scan_local_metadata_batch))
    }

    #[allow(dead_code)] // Exercised by storage tests for the default completion transition.
    pub(crate) async fn complete_scan_local_metadata_batch(
        &self,
        batch_id: &str,
    ) -> Result<bool, StorageError> {
        self.transition_scan_local_metadata_batch(batch_id, None, None)
            .await
    }

    pub(crate) async fn complete_scan_local_metadata_batch_with_outcome(
        &self,
        batch_id: &str,
        issue: Option<&str>,
        non_retryable_item_ids: &[String],
    ) -> Result<bool, StorageError> {
        if issue.is_some_and(|issue| issue.len() > MAX_SCAN_LOCAL_METADATA_BATCH_ERROR_BYTES) {
            return Err(StorageError::Conflict(
                "scan local metadata issue exceeds the storage limit".into(),
            ));
        }
        let non_retryable_item_ids_json = serialize_scan_local_metadata_non_retryable_item_ids(
            non_retryable_item_ids,
            MAX_SCAN_LOCAL_METADATA_BATCH_SOURCES,
        )?;
        let _write_guard = self.acquire_metadata_write_lock().await;
        let result = self
            .query(
                "UPDATE scan_local_metadata_batches
                 SET status = 'COMPLETED', next_attempt_at = NULL, error = ?,
                     non_retryable_item_ids_json = ?, updated_at = unixepoch()
                 WHERE id = ? AND status = 'RUNNING'",
            )
            .bind(issue)
            .bind(non_retryable_item_ids_json)
            .bind(batch_id)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn fail_scan_local_metadata_batch(
        &self,
        batch_id: &str,
        error: &str,
        next_attempt_at: Option<i64>,
    ) -> Result<bool, StorageError> {
        if error.len() > MAX_SCAN_LOCAL_METADATA_BATCH_ERROR_BYTES {
            return Err(StorageError::Conflict(
                "scan local metadata error exceeds the storage limit".into(),
            ));
        }
        self.transition_scan_local_metadata_batch(batch_id, next_attempt_at, Some(error))
            .await
    }

    pub(crate) async fn fail_scan_local_metadata_batch_with_non_retryable_item_ids(
        &self,
        batch_id: &str,
        error: &str,
        non_retryable_item_ids: &[String],
        next_attempt_at: Option<i64>,
    ) -> Result<bool, StorageError> {
        if error.len() > MAX_SCAN_LOCAL_METADATA_BATCH_ERROR_BYTES {
            return Err(StorageError::Conflict(
                "scan local metadata error exceeds the storage limit".into(),
            ));
        }
        let non_retryable_item_ids_json = serialize_scan_local_metadata_non_retryable_item_ids(
            non_retryable_item_ids,
            MAX_SCAN_LOCAL_METADATA_BATCH_SOURCES,
        )?;
        let _write_guard = self.acquire_metadata_write_lock().await;
        let result = self
            .query(
                "UPDATE scan_local_metadata_batches
                 SET status = 'FAILED', next_attempt_at = ?, error = ?,
                     non_retryable_item_ids_json = ?, updated_at = unixepoch()
                 WHERE id = ? AND status = 'RUNNING'",
            )
            .bind(next_attempt_at)
            .bind(error)
            .bind(non_retryable_item_ids_json)
            .bind(batch_id)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result.rows_affected() == 1)
    }

    async fn transition_scan_local_metadata_batch(
        &self,
        batch_id: &str,
        next_attempt_at: Option<i64>,
        error: Option<&str>,
    ) -> Result<bool, StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let result = if let Some(error) = error {
            self.query(
                "UPDATE scan_local_metadata_batches
                 SET status = 'FAILED', next_attempt_at = ?, error = ?, updated_at = unixepoch()
                 WHERE id = ? AND status = 'RUNNING'",
            )
            .bind(next_attempt_at)
            .bind(error)
            .bind(batch_id)
            .execute(&mut *transaction)
            .await
        } else {
            self.query(
                "UPDATE scan_local_metadata_batches
                 SET status = 'COMPLETED', next_attempt_at = NULL, error = NULL,
                     updated_at = unixepoch()
                 WHERE id = ? AND status = 'RUNNING'",
            )
            .bind(batch_id)
            .execute(&mut *transaction)
            .await
        }
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

    #[allow(dead_code)] // Exercised by storage tests for cancellation state transitions.
    pub(crate) async fn cancel_scan_local_metadata_batches(
        &self,
        job_id: &str,
    ) -> Result<u64, StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let result = self
            .query(
                "UPDATE scan_local_metadata_batches
                 SET status = 'CANCELLED', next_attempt_at = NULL, updated_at = unixepoch()
                 WHERE job_id = ? AND status IN ('PENDING', 'FAILED', 'RUNNING')",
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
        Ok(result.rows_affected())
    }

    /// Call once during startup, before outbox workers begin claiming work.
    pub(crate) async fn requeue_interrupted_scan_local_metadata_batches(
        &self,
    ) -> Result<u64, StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let result = self
            .query(
                "UPDATE scan_local_metadata_batches
                 SET status = 'PENDING', next_attempt_at = NULL, updated_at = unixepoch()
                 WHERE status = 'RUNNING'",
            )
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
        Ok(result.rows_affected())
    }
}
