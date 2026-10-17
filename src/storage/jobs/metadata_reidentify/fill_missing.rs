use super::*;
use crate::storage::MetadataFillMissingRequest;

#[path = "fill_missing/requests.rs"]
mod request_flow;

const FILL_MISSING_DISPATCH_SOURCE_EXPLICIT: &str = "EXPLICIT";
const FILL_MISSING_DISPATCH_SOURCE_AUTOMATIC: &str = "AUTOMATIC";
const FILL_MISSING_RETRY_PAGE_MAX: i64 = 100;

struct ActiveFillMissingItem {
    job_id: String,
    job_status: String,
    item_status: String,
    request_fingerprint: Option<Vec<u8>>,
    request_capabilities_json: String,
}

struct DeferredFillMissingRetryState {
    automatic_retry_count: i64,
    has_retry_deadline: bool,
    retry_pending: bool,
    recently_deferred: bool,
    deferred_job_items: Vec<(String, String)>,
}

impl Database {
    pub(crate) async fn enqueue_fill_missing_jobs_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        library_id: &str,
        dispatch_source: &str,
        requests: &[MetadataFillMissingRequest],
    ) -> Result<Vec<String>, StorageError> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }

        let mut job_ids = Vec::new();
        let mut remaining = requests.to_vec();
        let lock_clause = if self.backend == DatabaseBackend::Postgres {
            " FOR UPDATE"
        } else {
            ""
        };
        let query = format!(
            "SELECT id, total_count
             FROM metadata_reidentify_jobs
             WHERE library_id = ? AND mode = 'FILL_MISSING'
               AND dispatch_source = ?
               AND status = 'QUEUED' AND cancel_requested = 0 AND total_count < 100
             ORDER BY created_at, id
             LIMIT ?{lock_clause}"
        );
        let queued_jobs = self
            .query_as::<(String, i64)>(sqlx::AssertSqlSafe(query))
            .bind(library_id)
            .bind(dispatch_source)
            .bind(i64::try_from(remaining.len()).unwrap_or(i64::MAX))
            .fetch_all(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        for (queued_job_id, queued_count) in queued_jobs {
            if remaining.is_empty() {
                break;
            }
            let capacity = 100usize.saturating_sub(usize::try_from(queued_count).unwrap_or(100));
            let take = capacity.min(remaining.len());
            if take > 0 {
                let queued_items = remaining.drain(..take).collect::<Vec<_>>();
                self.insert_fill_missing_job_items(transaction, &queued_job_id, &queued_items)
                    .await?;
                self.query(
                    "UPDATE metadata_reidentify_jobs
                     SET total_count = total_count + ?, updated_at = unixepoch()
                     WHERE id = ? AND status = 'QUEUED'",
                )
                .bind(i64::try_from(queued_items.len()).unwrap_or(i64::MAX))
                .bind(&queued_job_id)
                .execute(&mut **transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
                job_ids.push(queued_job_id);
            }
        }
        for chunk in remaining.chunks(BATCH_INSERT_CHUNK_SIZE) {
            if chunk.is_empty() {
                continue;
            }
            let job_id = Uuid::now_v7().to_string();
            self.query(
                "INSERT INTO metadata_reidentify_jobs (
                     id, status, total_count, mode, library_id, job_scope, dispatch_source
                 ) VALUES (?, 'QUEUED', ?, 'FILL_MISSING', ?, 'ITEMS', ?)",
            )
            .bind(&job_id)
            .bind(i64::try_from(chunk.len()).unwrap_or(i64::MAX))
            .bind(library_id)
            .bind(dispatch_source)
            .execute(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;

            self.insert_fill_missing_job_items(transaction, &job_id, chunk)
                .await?;
            job_ids.push(job_id);
        }
        Ok(job_ids)
    }

    pub(crate) async fn enqueue_or_update_fill_missing_requests_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        library_id: &str,
        requests: &[MetadataFillMissingRequest],
    ) -> Result<Vec<String>, StorageError> {
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        let state = self
            .load_fill_missing_request_state(transaction, requests)
            .await?;
        let plan = request_flow::plan_fill_missing_request_updates(requests, state);
        self.persist_fill_missing_request_plan(transaction, library_id, plan)
            .await
    }

    pub(crate) async fn enqueue_due_automatic_fill_missing_retries(
        &self,
        library_id: &str,
        limit: i64,
    ) -> Result<Vec<String>, StorageError> {
        if library_id.trim().is_empty() {
            return Err(StorageError::Conflict(
                "automatic fill-missing retry requires a library".to_owned(),
            ));
        }
        let limit = limit.clamp(1, FILL_MISSING_RETRY_PAGE_MAX);
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let rows = self
            .query(
                "SELECT DISTINCT items.item_id
                 FROM metadata_reidentify_job_items items
                 JOIN metadata_reidentify_jobs jobs ON jobs.id = items.job_id
                 LEFT JOIN server_settings legacy_retry
                   ON legacy_retry.key = 'metadata_fill_missing_legacy_retry_after'
                 WHERE jobs.library_id = ? AND jobs.mode = 'FILL_MISSING'
                   AND jobs.dispatch_source = 'AUTOMATIC'
                   AND jobs.status = 'DEFERRED' AND jobs.cancel_requested = 0
                   AND items.status = 'FAILED'
                   AND items.error = 'SCRAPER_UNAVAILABLE'
                   AND items.automatic_retry_consumed = 0
                   AND items.request_fingerprint IS NOT NULL
                   AND COALESCE(
                       items.automatic_retry_after,
                       CASE WHEN items.automatic_retry_count = 0
                            THEN CAST(legacy_retry.value AS BIGINT)
                            ELSE NULL END
                   ) <= unixepoch()
                 ORDER BY items.item_id LIMIT ?",
            )
            .bind(library_id)
            .bind(limit)
            .fetch_all(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if rows.is_empty() {
            transaction
                .commit()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(Vec::new());
        }
        let item_ids = rows
            .into_iter()
            .map(|row| row.get::<String, _>("item_id"))
            .collect::<Vec<_>>();
        self.lock_media_items_for_update(&mut transaction, &item_ids)
            .await?;
        let requests = self
            .build_metadata_fill_missing_requests(&mut transaction, library_id, &item_ids, &[])
            .await?;
        let current_request_item_ids = requests
            .iter()
            .map(|request| request.item_id.as_str())
            .collect::<std::collections::HashSet<_>>();
        let obsolete_item_ids = item_ids
            .iter()
            .filter(|item_id| !current_request_item_ids.contains(item_id.as_str()))
            .collect::<Vec<_>>();
        for batch in obsolete_item_ids.chunks(BATCH_INSERT_CHUNK_SIZE) {
            if batch.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", batch.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "UPDATE metadata_reidentify_job_items
                 SET automatic_retry_consumed = 1, updated_at = unixepoch()
                 WHERE status = 'FAILED' AND error = 'SCRAPER_UNAVAILABLE'
                   AND automatic_retry_consumed = 0
                   AND item_id IN ({placeholders})
                   AND EXISTS (
                       SELECT 1 FROM metadata_reidentify_jobs jobs
                       WHERE jobs.id = metadata_reidentify_job_items.job_id
                         AND jobs.library_id = ? AND jobs.mode = 'FILL_MISSING'
                         AND jobs.dispatch_source = 'AUTOMATIC' AND jobs.status = 'DEFERRED'
                   )"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for item_id in batch {
                statement = statement.bind(item_id.as_str());
            }
            statement
                .bind(library_id)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }
        let job_ids = self
            .enqueue_or_update_fill_missing_requests_in_transaction(
                &mut transaction,
                library_id,
                &requests,
            )
            .await?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(job_ids)
    }

    pub(crate) async fn create_or_merge_fill_missing_job(
        &self,
        library_id: &str,
        item_ids: &[String],
    ) -> Result<String, StorageError> {
        if library_id.trim().is_empty() || item_ids.is_empty() || item_ids.len() > 100 {
            return Err(StorageError::Conflict(
                "invalid fill-missing job request".to_owned(),
            ));
        }
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        self.lock_media_items_for_update(&mut transaction, item_ids)
            .await?;
        let placeholders = std::iter::repeat_n("?", item_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let mut statement = self
            .query(sqlx::AssertSqlSafe(format!(
                "SELECT jobs.id, job_items.item_id
             FROM metadata_reidentify_job_items job_items
             JOIN metadata_reidentify_jobs jobs ON jobs.id = job_items.job_id
             WHERE jobs.library_id = ? AND jobs.mode = 'FILL_MISSING'
               AND jobs.status IN ('QUEUED', 'RUNNING', 'DEFERRED')
               AND (jobs.status <> 'DEFERRED' OR jobs.updated_at >= unixepoch() - 3600)
               AND jobs.cancel_requested = 0
               AND (
                   job_items.status IN ('PENDING', 'RUNNING')
                   OR (jobs.status = 'DEFERRED'
                       AND job_items.status = 'FAILED'
                       AND job_items.error = 'SCRAPER_UNAVAILABLE')
               )
               AND job_items.item_id IN ({placeholders})"
            )))
            .bind(library_id);
        for item_id in item_ids {
            statement = statement.bind(item_id);
        }
        let active_rows = statement
            .fetch_all(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let mut active_items = std::collections::HashSet::with_capacity(active_rows.len());
        let mut existing_job_id = None;
        for row in active_rows {
            existing_job_id.get_or_insert_with(|| row.get("id"));
            active_items.insert(row.get::<String, _>("item_id"));
        }
        let remaining = item_ids
            .iter()
            .filter(|item_id| !active_items.contains(*item_id))
            .cloned()
            .collect::<Vec<_>>();
        let job_id = if remaining.is_empty() {
            existing_job_id.ok_or_else(|| {
                StorageError::Conflict("fill-missing job has no schedulable items".to_owned())
            })?
        } else {
            let requests = remaining
                .iter()
                .map(|item_id| MetadataFillMissingRequest {
                    item_id: item_id.clone(),
                    input_fingerprint: None,
                    capabilities_json: "[]".to_owned(),
                    automatic_retry_count: 0,
                })
                .collect::<Vec<_>>();
            self.enqueue_fill_missing_jobs_in_transaction(
                &mut transaction,
                library_id,
                FILL_MISSING_DISPATCH_SOURCE_EXPLICIT,
                &requests,
            )
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| StorageError::Conflict("fill-missing job was not created".to_owned()))?
        };
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(job_id)
    }

    async fn insert_fill_missing_job_items(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        job_id: &str,
        requests: &[MetadataFillMissingRequest],
    ) -> Result<(), StorageError> {
        let values = std::iter::repeat_n(
            format!(
                "(?, ?, 'PENDING', (SELECT {METADATA_REIDENTIFY_PRIORITY_CASE}
                 FROM media_items WHERE id = ?), ?, ?, ?)"
            ),
            requests.len(),
        )
        .collect::<Vec<_>>()
        .join(", ");
        let query = format!(
            "INSERT INTO metadata_reidentify_job_items
                 (job_id, item_id, status, priority, request_fingerprint,
                  request_capabilities_json, automatic_retry_count)
             VALUES {values}"
        );
        let mut statement = self.query(sqlx::AssertSqlSafe(query));
        for request in requests {
            statement = statement
                .bind(job_id)
                .bind(&request.item_id)
                .bind(&request.item_id)
                .bind(request.input_fingerprint.as_deref())
                .bind(&request.capabilities_json)
                .bind(request.automatic_retry_count);
        }
        statement
            .execute(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(())
    }
}
