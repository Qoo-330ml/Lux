use super::*;
use crate::storage::MetadataFillMissingRequest;

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
               AND status = 'QUEUED' AND cancel_requested = 0 AND total_count < 100
             ORDER BY created_at, id
             LIMIT ?{lock_clause}"
        );
        let queued_jobs = self
            .query_as::<(String, i64)>(sqlx::AssertSqlSafe(query))
            .bind(library_id)
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
                     id, status, total_count, mode, library_id, job_scope
                 ) VALUES (?, 'QUEUED', ?, 'FILL_MISSING', ?, 'ITEMS')",
            )
            .bind(&job_id)
            .bind(i64::try_from(chunk.len()).unwrap_or(i64::MAX))
            .bind(library_id)
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
        let mut active_by_item = HashMap::with_capacity(requests.len());
        let mut deferred_by_snapshot = HashMap::with_capacity(requests.len());
        let mut deferred_snapshots_by_item =
            HashMap::<String, Vec<(Option<Vec<u8>>, String)>>::new();
        let lock_clause = if self.backend == DatabaseBackend::Postgres {
            " FOR UPDATE OF jobs, job_items"
        } else {
            ""
        };
        for batch in requests.chunks(100) {
            if batch.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", batch.len())
                .collect::<Vec<_>>()
                .join(", ");
            let deferred_snapshot_predicates = batch
                .iter()
                .map(|request| {
                    if request.input_fingerprint.is_some() {
                        "(job_items.item_id = ? AND job_items.request_fingerprint IS NOT NULL)"
                    } else {
                        "(jobs.updated_at >= unixepoch() - 3600 AND job_items.item_id = ? AND job_items.request_fingerprint IS NULL AND job_items.request_capabilities_json = ?)"
                    }
                })
                .collect::<Vec<_>>()
                .join(" OR ");
            let query = format!(
                "SELECT jobs.id AS job_id, jobs.status AS job_status,
                        job_items.item_id, job_items.status AS item_status,
                        job_items.request_fingerprint,
                        job_items.request_capabilities_json,
                        CASE WHEN job_items.automatic_retry_count = 0
                                  AND jobs.status = 'DEFERRED'
                                  AND job_items.status = 'FAILED'
                                  AND job_items.error = 'SCRAPER_UNAVAILABLE'
                                  AND job_items.request_fingerprint IS NOT NULL
                            THEN 1 ELSE job_items.automatic_retry_count
                        END AS automatic_retry_count,
                        CASE WHEN COALESCE(
                                job_items.automatic_retry_after,
                                CASE WHEN job_items.automatic_retry_count = 0
                                          AND jobs.status = 'DEFERRED'
                                          AND job_items.status = 'FAILED'
                                          AND job_items.error = 'SCRAPER_UNAVAILABLE'
                                          AND job_items.request_fingerprint IS NOT NULL
                                     THEN CAST(legacy_retry.value AS BIGINT)
                                     ELSE NULL END
                             ) IS NULL
                            THEN 0 ELSE 1 END AS has_retry_deadline,
                        CASE WHEN COALESCE(
                                job_items.automatic_retry_after,
                                CASE WHEN job_items.automatic_retry_count = 0
                                          AND jobs.status = 'DEFERRED'
                                          AND job_items.status = 'FAILED'
                                          AND job_items.error = 'SCRAPER_UNAVAILABLE'
                                          AND job_items.request_fingerprint IS NOT NULL
                                     THEN CAST(legacy_retry.value AS BIGINT)
                                     ELSE NULL END
                             ) > unixepoch()
                            THEN 1 ELSE 0 END AS retry_pending,
                        CASE WHEN jobs.updated_at >= unixepoch() - 3600
                            THEN 1 ELSE 0 END AS recently_deferred
                 FROM metadata_reidentify_job_items job_items
                 JOIN metadata_reidentify_jobs jobs ON jobs.id = job_items.job_id
                 LEFT JOIN server_settings legacy_retry
                   ON legacy_retry.key = 'metadata_fill_missing_legacy_retry_after'
                 WHERE (
                     jobs.mode = 'FILL_MISSING'
                     AND jobs.status IN ('QUEUED', 'RUNNING')
                     AND jobs.cancel_requested = 0
                     AND (
                         (jobs.status = 'QUEUED'
                          AND job_items.status IN ('PENDING', 'RUNNING', 'COMPLETED'))
                         OR (jobs.status = 'RUNNING'
                             AND job_items.status IN ('PENDING', 'RUNNING'))
                     )
                     AND job_items.item_id IN ({placeholders})
                 ) OR (
                     jobs.mode = 'FILL_MISSING'
                     AND jobs.status = 'DEFERRED'
                     AND jobs.cancel_requested = 0
                     AND job_items.status = 'FAILED'
                     AND job_items.error = 'SCRAPER_UNAVAILABLE'
                     AND job_items.automatic_retry_consumed = 0
                     AND ({deferred_snapshot_predicates})
                 )
                 ORDER BY CASE WHEN jobs.status IN ('QUEUED', 'RUNNING') THEN 0 ELSE 1 END,
                          CASE WHEN job_items.status = 'RUNNING' THEN 0 ELSE 1 END,
                          jobs.created_at, jobs.id{lock_clause}"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for request in batch {
                statement = statement.bind(&request.item_id);
            }
            for request in batch {
                statement = statement.bind(&request.item_id);
                if request.input_fingerprint.is_none() {
                    statement = statement.bind(&request.capabilities_json);
                }
            }
            for row in statement
                .fetch_all(&mut **transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
            {
                let job_id: String = row.get("job_id");
                let item_id: String = row.get("item_id");
                let job_status: String = row.get("job_status");
                if job_status == "DEFERRED" {
                    let fingerprint: Option<Vec<u8>> = row.get("request_fingerprint");
                    let capabilities: String = row.get("request_capabilities_json");
                    let snapshot_key = (item_id.clone(), fingerprint.clone(), capabilities.clone());
                    let has_retry_deadline = row.get::<i64, _>("has_retry_deadline") != 0;
                    let retry_pending = row.get::<i64, _>("retry_pending") != 0;
                    let recently_deferred = row.get::<i64, _>("recently_deferred") != 0;
                    let retry = deferred_by_snapshot.entry(snapshot_key).or_insert(
                        DeferredFillMissingRetryState {
                            automatic_retry_count: 0,
                            has_retry_deadline: false,
                            retry_pending: false,
                            recently_deferred: false,
                            deferred_job_items: Vec::new(),
                        },
                    );
                    retry.automatic_retry_count = retry
                        .automatic_retry_count
                        .max(row.get("automatic_retry_count"));
                    retry.has_retry_deadline |= has_retry_deadline;
                    retry.retry_pending |= retry_pending;
                    retry.recently_deferred |= recently_deferred;
                    let snapshots = deferred_snapshots_by_item
                        .entry(item_id.clone())
                        .or_default();
                    let snapshot = (fingerprint, capabilities);
                    if !snapshots.contains(&snapshot) {
                        snapshots.push(snapshot);
                    }
                    retry.deferred_job_items.push((job_id, item_id));
                    continue;
                }
                active_by_item
                    .entry(item_id)
                    .or_insert_with(|| ActiveFillMissingItem {
                        job_id,
                        job_status,
                        item_status: row.get("item_status"),
                        request_fingerprint: row.get("request_fingerprint"),
                        request_capabilities_json: row.get("request_capabilities_json"),
                    });
            }
        }

        let mut queued_updates = HashMap::<String, Vec<MetadataFillMissingRequest>>::new();
        let mut completed_queued_updates = HashMap::<String, i64>::new();
        let mut remaining = Vec::new();
        let mut consumed_deferred_retry_job_items = Vec::new();
        for request in requests {
            if request.input_fingerprint.is_some()
                && let Some(snapshots) = deferred_snapshots_by_item.get(&request.item_id)
            {
                for (fingerprint, capabilities) in snapshots {
                    let matches_request = fingerprint.as_deref()
                        == request.input_fingerprint.as_deref()
                        && capabilities == &request.capabilities_json;
                    if matches_request {
                        continue;
                    }
                    if let Some(deferred) = deferred_by_snapshot.get(&(
                        request.item_id.clone(),
                        fingerprint.clone(),
                        capabilities.clone(),
                    )) {
                        consumed_deferred_retry_job_items
                            .extend(deferred.deferred_job_items.iter().cloned());
                    }
                }
            }
            if let Some(active_item) = active_by_item.get(&request.item_id) {
                let matches_snapshot = active_item.request_fingerprint.as_deref()
                    == request.input_fingerprint.as_deref()
                    && active_item.request_capabilities_json == request.capabilities_json;
                if matches_snapshot {
                    continue;
                }
                if active_item.job_status == "QUEUED" && active_item.item_status == "COMPLETED" {
                    *completed_queued_updates
                        .entry(active_item.job_id.clone())
                        .or_default() += 1;
                }
                queued_updates
                    .entry(active_item.job_id.clone())
                    .or_default()
                    .push(request.clone());
                continue;
            }

            let snapshot_key = (
                request.item_id.clone(),
                request.input_fingerprint.clone(),
                request.capabilities_json.clone(),
            );
            if let Some(deferred) = deferred_by_snapshot.get(&snapshot_key) {
                if request.input_fingerprint.is_some() {
                    if (deferred.has_retry_deadline && deferred.retry_pending)
                        || (!deferred.has_retry_deadline && deferred.recently_deferred)
                    {
                        continue;
                    }
                    let mut retry = request.clone();
                    retry.automatic_retry_count = deferred.automatic_retry_count;
                    remaining.push(retry);
                    consumed_deferred_retry_job_items
                        .extend(deferred.deferred_job_items.iter().cloned());
                } else if !deferred.recently_deferred {
                    remaining.push(request.clone());
                }
            } else {
                remaining.push(request.clone());
            }
        }

        consumed_deferred_retry_job_items.sort_unstable();
        consumed_deferred_retry_job_items.dedup();
        for batch in consumed_deferred_retry_job_items.chunks(BATCH_INSERT_CHUNK_SIZE) {
            let values = std::iter::repeat_n("(?, ?)", batch.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "UPDATE metadata_reidentify_job_items
                 SET automatic_retry_consumed = 1, updated_at = unixepoch()
                 WHERE automatic_retry_consumed = 0
                   AND status = 'FAILED' AND error = 'SCRAPER_UNAVAILABLE'
                   AND (job_id, item_id) IN ({values})"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for (job_id, item_id) in batch {
                statement = statement.bind(job_id).bind(item_id);
            }
            statement
                .execute(&mut **transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }

        for (job_id, updates) in queued_updates {
            for batch in updates.chunks(100) {
                let placeholders = std::iter::repeat_n("?", batch.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                let fingerprint_cases = std::iter::repeat_n("WHEN ? THEN ?", batch.len())
                    .collect::<Vec<_>>()
                    .join(" ");
                let capability_cases = std::iter::repeat_n("WHEN ? THEN ?", batch.len())
                    .collect::<Vec<_>>()
                    .join(" ");
                let query = format!(
                    "UPDATE metadata_reidentify_job_items
                     SET request_fingerprint = CASE item_id {fingerprint_cases}
                             ELSE request_fingerprint END,
                         request_capabilities_json = CASE item_id {capability_cases}
                             ELSE request_capabilities_json END,
                         status = CASE WHEN status = 'COMPLETED' THEN 'PENDING' ELSE status END,
                         candidate_count = CASE WHEN status = 'COMPLETED' THEN 0 ELSE candidate_count END,
                         error = CASE WHEN status = 'COMPLETED' THEN NULL ELSE error END,
                         automatic_retry_consumed = 0,
                         automatic_retry_count = 0,
                         automatic_retry_after = NULL,
                         updated_at = unixepoch()
                     WHERE job_id = ? AND status IN ('PENDING', 'RUNNING', 'COMPLETED')
                       AND item_id IN ({placeholders})"
                );
                let mut statement = self.query(sqlx::AssertSqlSafe(query));
                for request in batch {
                    statement = statement
                        .bind(&request.item_id)
                        .bind(request.input_fingerprint.as_deref());
                }
                for request in batch {
                    statement = statement
                        .bind(&request.item_id)
                        .bind(&request.capabilities_json);
                }
                statement = statement.bind(&job_id);
                for request in batch {
                    statement = statement.bind(&request.item_id);
                }
                statement
                    .execute(&mut **transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
            }
        }
        for (job_id, count) in completed_queued_updates {
            self.query(
                "UPDATE metadata_reidentify_jobs
                 SET processed_count = CASE WHEN processed_count >= ?
                     THEN processed_count - ? ELSE 0 END,
                     updated_at = unixepoch()
                 WHERE id = ? AND status = 'QUEUED'",
            )
            .bind(count)
            .bind(count)
            .bind(job_id)
            .execute(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }

        self.enqueue_fill_missing_jobs_in_transaction(transaction, library_id, &remaining)
            .await
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
            self.enqueue_fill_missing_jobs_in_transaction(&mut transaction, library_id, &requests)
                .await?
                .into_iter()
                .next()
                .ok_or_else(|| {
                    StorageError::Conflict("fill-missing job was not created".to_owned())
                })?
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
