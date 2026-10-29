use super::*;

type FillMissingRequestSnapshot = (Option<Vec<u8>>, String);

pub(super) struct FillMissingRequestState {
    active_by_item: HashMap<String, ActiveFillMissingItem>,
    explicit_items: std::collections::HashSet<String>,
    deferred_by_snapshot: HashMap<(String, Option<Vec<u8>>, String), DeferredFillMissingRetryState>,
    deferred_snapshots_by_item: HashMap<String, Vec<FillMissingRequestSnapshot>>,
}

pub(super) struct FillMissingRequestPlan {
    queued_updates: HashMap<String, Vec<MetadataFillMissingRequest>>,
    completed_queued_updates: HashMap<String, i64>,
    remaining: Vec<MetadataFillMissingRequest>,
    consumed_deferred_retry_job_items: Vec<(String, String)>,
}

impl Database {
    pub(super) async fn load_fill_missing_request_state(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        requests: &[MetadataFillMissingRequest],
    ) -> Result<FillMissingRequestState, StorageError> {
        let mut active_by_item = HashMap::with_capacity(requests.len());
        let mut explicit_items = std::collections::HashSet::with_capacity(requests.len());
        let mut deferred_by_snapshot = HashMap::with_capacity(requests.len());
        let mut deferred_snapshots_by_item =
            HashMap::<String, Vec<FillMissingRequestSnapshot>>::new();
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
                        jobs.dispatch_source,
                        job_items.item_id, job_items.status AS item_status,
                        job_items.request_fingerprint,
                        job_items.request_capabilities_json,
                        CASE WHEN job_items.automatic_retry_count = 0
                                  AND jobs.status IN ('DEFERRED', 'COMPLETED_WITH_ISSUES')
                                  AND job_items.status = 'FAILED'
                                  AND job_items.error = 'SCRAPER_UNAVAILABLE'
                                  AND job_items.request_fingerprint IS NOT NULL
                            THEN 1 ELSE job_items.automatic_retry_count
                        END AS automatic_retry_count,
                        CASE WHEN COALESCE(
                                job_items.automatic_retry_after,
                                CASE WHEN job_items.automatic_retry_count = 0
                                          AND jobs.status IN ('DEFERRED', 'COMPLETED_WITH_ISSUES')
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
                                          AND jobs.status IN ('DEFERRED', 'COMPLETED_WITH_ISSUES')
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
                     AND jobs.dispatch_source = 'AUTOMATIC'
                     AND jobs.status IN ('DEFERRED', 'COMPLETED_WITH_ISSUES')
                     AND jobs.cancel_requested = 0
                     AND job_items.status = 'FAILED'
                     AND job_items.error = 'SCRAPER_UNAVAILABLE'
                     AND job_items.automatic_retry_consumed = 0
                     AND ({deferred_snapshot_predicates})
                 ) OR (
                     jobs.mode = 'FILL_MISSING'
                     AND jobs.dispatch_source = 'EXPLICIT'
                     AND jobs.status IN ('DEFERRED', 'COMPLETED_WITH_ISSUES')
                     AND jobs.cancel_requested = 0
                     AND job_items.status = 'FAILED'
                     AND job_items.error = 'SCRAPER_UNAVAILABLE'
                     AND job_items.automatic_retry_consumed = 0
                     AND job_items.item_id IN ({placeholders})
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
            for request in batch {
                statement = statement.bind(&request.item_id);
            }
            for row in statement
                .fetch_all(&mut **transaction)
                .await
                .map_err(self.sqlx_err())?
            {
                let job_id: String = row.get("job_id");
                let item_id: String = row.get("item_id");
                let job_status: String = row.get("job_status");
                let dispatch_source: String = row.get("dispatch_source");
                if dispatch_source == FILL_MISSING_DISPATCH_SOURCE_EXPLICIT {
                    explicit_items.insert(item_id);
                    continue;
                }
                if matches!(job_status.as_str(), "DEFERRED" | "COMPLETED_WITH_ISSUES") {
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

        Ok(FillMissingRequestState {
            active_by_item,
            explicit_items,
            deferred_by_snapshot,
            deferred_snapshots_by_item,
        })
    }
}

pub(super) fn plan_fill_missing_request_updates(
    requests: &[MetadataFillMissingRequest],
    state: FillMissingRequestState,
) -> FillMissingRequestPlan {
    let FillMissingRequestState {
        active_by_item,
        explicit_items,
        deferred_by_snapshot,
        deferred_snapshots_by_item,
    } = state;
    let mut queued_updates = HashMap::<String, Vec<MetadataFillMissingRequest>>::new();
    let mut completed_queued_updates = HashMap::<String, i64>::new();
    let mut remaining = Vec::new();
    let mut consumed_deferred_retry_job_items = Vec::new();
    for request in requests {
        if explicit_items.contains(&request.item_id) {
            continue;
        }
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

    FillMissingRequestPlan {
        queued_updates,
        completed_queued_updates,
        remaining,
        consumed_deferred_retry_job_items,
    }
}

impl Database {
    pub(super) async fn persist_fill_missing_request_plan(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        library_id: &str,
        plan: FillMissingRequestPlan,
    ) -> Result<Vec<String>, StorageError> {
        let FillMissingRequestPlan {
            queued_updates,
            completed_queued_updates,
            remaining,
            mut consumed_deferred_retry_job_items,
        } = plan;
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
                .map_err(self.sqlx_err())?;
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
                    .map_err(self.sqlx_err())?;
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
            .map_err(self.sqlx_err())?;
        }

        self.enqueue_fill_missing_jobs_in_transaction(
            transaction,
            library_id,
            FILL_MISSING_DISPATCH_SOURCE_AUTOMATIC,
            &remaining,
        )
        .await
    }
}
