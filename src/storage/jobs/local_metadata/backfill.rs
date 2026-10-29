const MAX_SCAN_LOCAL_METADATA_BACKFILL_PAGE_SIZE: usize = 16;

#[derive(Debug)]
pub(crate) struct StoredScanLocalMetadataBackfillPage {
    pub(crate) library_root_id: String,
    pub(crate) cursor_entry_id: Option<String>,
    pub(crate) entry_ids: Vec<String>,
    pub(crate) non_retryable_item_ids: Vec<String>,
    pub(crate) next_cursor_entry_id: String,
    pub(crate) has_more: bool,
    pub(crate) attempts: i64,
}

use super::*;

impl Database {
    pub(crate) async fn ensure_scan_local_metadata_backfill_roots(
        &self,
    ) -> Result<u64, StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        self.query(
            "INSERT INTO scan_local_metadata_backfills (library_root_id)
             SELECT id FROM library_roots
             WHERE TRUE
             ORDER BY id
             ON CONFLICT(library_root_id) DO NOTHING",
        )
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected())
        .map_err(self.sqlx_err())
    }

    #[allow(dead_code)] // Exercised by storage tests for backfill-root idempotency.
    pub(crate) async fn ensure_scan_local_metadata_backfill_root(
        &self,
        library_root_id: &str,
    ) -> Result<bool, StorageError> {
        if library_root_id.trim().is_empty() {
            return Err(StorageError::Conflict(
                "scan metadata backfill root id is empty".into(),
            ));
        }
        let _write_guard = self.acquire_metadata_write_lock().await;
        self.query(
            "INSERT INTO scan_local_metadata_backfills (library_root_id)
             VALUES (?) ON CONFLICT(library_root_id) DO NOTHING",
        )
        .bind(library_root_id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn claim_next_scan_local_metadata_backfill_page(
        &self,
        page_size: usize,
    ) -> Result<Option<StoredScanLocalMetadataBackfillPage>, StorageError> {
        if !(1..=MAX_SCAN_LOCAL_METADATA_BACKFILL_PAGE_SIZE).contains(&page_size) {
            return Err(StorageError::Conflict(
                "invalid scan metadata backfill page size".into(),
            ));
        }
        let query_limit = i64::try_from(page_size.saturating_add(1)).map_err(|_| {
            StorageError::Conflict("scan metadata backfill page size overflow".into())
        })?;

        loop {
            let _write_guard = self.acquire_metadata_write_lock().await;
            let mut transaction = self.begin_metadata_write_transaction().await?;
            let next_root = self
                .query_scalar::<String>(
                    "SELECT library_root_id FROM scan_local_metadata_backfills
                     WHERE status IN ('PENDING', 'FAILED')
                       AND (next_attempt_at IS NULL OR next_attempt_at <= unixepoch())
                     ORDER BY updated_at, library_root_id LIMIT 1",
                )
                .fetch_optional(&mut *transaction)
                .await
                .map_err(self.sqlx_err())?;
            let Some(library_root_id) = next_root else {
                transaction.commit().await.map_err(self.sqlx_err())?;
                return Ok(None);
            };
            let claimed: Option<(Option<String>, i64, String)> = self
                .query_as(
                    "UPDATE scan_local_metadata_backfills
                     SET status = 'RUNNING', attempts = attempts + 1,
                         next_attempt_at = NULL, error = NULL, updated_at = unixepoch()
                     WHERE library_root_id = ? AND status IN ('PENDING', 'FAILED')
                       AND (next_attempt_at IS NULL OR next_attempt_at <= unixepoch())
                     RETURNING cursor_entry_id, attempts, non_retryable_item_ids_json",
                )
                .bind(&library_root_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(self.sqlx_err())?;
            let Some((cursor_entry_id, attempts, non_retryable_item_ids_json)) = claimed else {
                transaction.commit().await.map_err(self.sqlx_err())?;
                continue;
            };
            let non_retryable_item_ids = parse_scan_local_metadata_non_retryable_item_ids(
                &non_retryable_item_ids_json,
                MAX_SCAN_LOCAL_METADATA_BACKFILL_PAGE_SIZE,
            )?;
            let mut entry_ids = self
                .query_scalar::<String>(
                    "SELECT DISTINCT entry.id
                     FROM filesystem_entries entry
                     WHERE entry.library_root_id = ? AND entry.entry_kind = 'FILE'
                       AND entry.is_missing = 0
                       AND entry.id > COALESCE(?, '')
                       AND EXISTS (
                           SELECT 1 FROM media_sources source
                           JOIN media_items item ON item.id = source.item_id
                           WHERE source.filesystem_entry_id = entry.id
                             AND item.removed_at IS NULL
                       )
                     ORDER BY entry.id LIMIT ?",
                )
                .bind(&library_root_id)
                .bind(cursor_entry_id.as_deref())
                .bind(query_limit)
                .fetch_all(&mut *transaction)
                .await
                .map_err(self.sqlx_err())?;
            if entry_ids.is_empty() {
                self.query(
                    "UPDATE scan_local_metadata_backfills
                     SET status = 'COMPLETED', next_attempt_at = NULL, error = NULL,
                         non_retryable_item_ids_json = '[]',
                         updated_at = unixepoch()
                     WHERE library_root_id = ? AND status = 'RUNNING' AND attempts = ?
                       AND cursor_entry_id IS NOT DISTINCT FROM ?",
                )
                .bind(&library_root_id)
                .bind(attempts)
                .bind(cursor_entry_id.as_deref())
                .execute(&mut *transaction)
                .await
                .map_err(self.sqlx_err())?;
                transaction.commit().await.map_err(self.sqlx_err())?;
                continue;
            }

            let has_more = entry_ids.len() > page_size;
            if has_more {
                entry_ids.pop();
            }
            let next_cursor_entry_id = entry_ids.last().cloned().ok_or_else(|| {
                StorageError::Conflict("scan metadata backfill returned an empty page".into())
            })?;
            transaction.commit().await.map_err(self.sqlx_err())?;
            return Ok(Some(StoredScanLocalMetadataBackfillPage {
                library_root_id,
                cursor_entry_id,
                entry_ids,
                non_retryable_item_ids,
                next_cursor_entry_id,
                has_more,
                attempts,
            }));
        }
    }

    #[allow(dead_code)] // Exercised by storage tests for the default completion transition.
    pub(crate) async fn complete_scan_local_metadata_backfill_page(
        &self,
        page: &StoredScanLocalMetadataBackfillPage,
    ) -> Result<bool, StorageError> {
        self.complete_scan_local_metadata_backfill_page_with_optional_issue(page, None)
            .await
    }

    pub(crate) async fn complete_scan_local_metadata_backfill_page_with_optional_issue(
        &self,
        page: &StoredScanLocalMetadataBackfillPage,
        issue: Option<&str>,
    ) -> Result<bool, StorageError> {
        if issue.is_some_and(|issue| issue.len() > MAX_SCAN_LOCAL_METADATA_BATCH_ERROR_BYTES) {
            return Err(StorageError::Conflict(
                "scan metadata backfill issue exceeds the storage limit".into(),
            ));
        }
        let status = if page.has_more {
            "PENDING"
        } else {
            "COMPLETED"
        };
        let _write_guard = self.acquire_metadata_write_lock().await;
        let result = self
            .query(
                "UPDATE scan_local_metadata_backfills
                 SET cursor_entry_id = ?, status = ?, next_attempt_at = NULL, error = ?,
                     non_retryable_item_ids_json = '[]',
                     updated_at = unixepoch()
                 WHERE library_root_id = ? AND status = 'RUNNING' AND attempts = ?
                   AND cursor_entry_id IS NOT DISTINCT FROM ?",
            )
            .bind(&page.next_cursor_entry_id)
            .bind(status)
            .bind(issue)
            .bind(&page.library_root_id)
            .bind(page.attempts)
            .bind(page.cursor_entry_id.as_deref())
            .execute(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        Ok(result.rows_affected() == 1)
    }

    #[allow(dead_code)] // Exercised by storage tests for the default failure transition.
    pub(crate) async fn fail_scan_local_metadata_backfill_page(
        &self,
        page: &StoredScanLocalMetadataBackfillPage,
        error: &str,
        next_attempt_at: Option<i64>,
    ) -> Result<bool, StorageError> {
        self.fail_scan_local_metadata_backfill_page_with_exclusions(
            page,
            error,
            &[],
            next_attempt_at,
        )
        .await
    }

    pub(crate) async fn fail_scan_local_metadata_backfill_page_with_exclusions(
        &self,
        page: &StoredScanLocalMetadataBackfillPage,
        error: &str,
        newly_non_retryable_item_ids: &[String],
        next_attempt_at: Option<i64>,
    ) -> Result<bool, StorageError> {
        if error.len() > MAX_SCAN_LOCAL_METADATA_BATCH_ERROR_BYTES {
            return Err(StorageError::Conflict(
                "scan metadata backfill error exceeds the storage limit".into(),
            ));
        }
        let mut non_retryable_item_ids = page.non_retryable_item_ids.clone();
        let mut unique_item_ids = non_retryable_item_ids
            .iter()
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        for item_id in newly_non_retryable_item_ids {
            if item_id.trim().is_empty() {
                return Err(StorageError::Conflict(
                    "scan metadata backfill exclusion item id is empty".into(),
                ));
            }
            if unique_item_ids.insert(item_id.clone()) {
                non_retryable_item_ids.push(item_id.clone());
            }
        }
        if non_retryable_item_ids.len() > MAX_SCAN_LOCAL_METADATA_BACKFILL_PAGE_SIZE {
            return Err(StorageError::Conflict(
                "scan metadata backfill exclusion count exceeds the page limit".into(),
            ));
        }
        let non_retryable_item_ids_json = serialize_scan_local_metadata_non_retryable_item_ids(
            &non_retryable_item_ids,
            MAX_SCAN_LOCAL_METADATA_BACKFILL_PAGE_SIZE,
        )?;
        let _write_guard = self.acquire_metadata_write_lock().await;
        let result = self
            .query(
                "UPDATE scan_local_metadata_backfills
                 SET status = 'FAILED', next_attempt_at = ?, error = ?,
                     non_retryable_item_ids_json = ?,
                     updated_at = unixepoch()
                 WHERE library_root_id = ? AND status = 'RUNNING' AND attempts = ?
                   AND cursor_entry_id IS NOT DISTINCT FROM ?",
            )
            .bind(next_attempt_at)
            .bind(error)
            .bind(non_retryable_item_ids_json)
            .bind(&page.library_root_id)
            .bind(page.attempts)
            .bind(page.cursor_entry_id.as_deref())
            .execute(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn requeue_interrupted_scan_local_metadata_backfills(
        &self,
    ) -> Result<u64, StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let result = self
            .query(
                "UPDATE scan_local_metadata_backfills
                 SET status = 'PENDING', next_attempt_at = NULL,
                     error = COALESCE(error, 'worker interrupted'), updated_at = unixepoch()
                 WHERE status = 'RUNNING'",
            )
            .execute(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        Ok(result.rows_affected())
    }
}
