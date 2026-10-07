use super::*;
use crate::storage::{
    ItemMetadataCompletenessCommit, MetadataAutoMatchPolicy, MetadataFillMissingRequest,
    NewItemMetadataCompletenessCheck, NewItemMetadataCompletenessResult,
};
use std::collections::BTreeSet;

const MAX_ITEM_METADATA_COMPLETENESS_CAPABILITY_LENGTH: usize = 64;
const MAX_ITEM_METADATA_COMPLETENESS_FINGERPRINT_BYTES: usize = 256;
const MAX_ITEM_METADATA_COMPLETENESS_ERROR_BYTES: usize = 4096;
const MAX_ITEM_METADATA_COMPLETENESS_PAGE_SIZE: i64 = 100;
const MAX_ITEM_METADATA_COMPLETENESS_CHECK_BATCH_SIZE: usize = 512;
const ITEM_METADATA_COMPLETENESS_WRITE_BATCH_SIZE: usize = 100;

fn validate_item_metadata_completeness_key(
    item_id: &str,
    capability: &str,
    fingerprint: &[u8],
) -> Result<(), StorageError> {
    let capability_length = capability.trim().chars().count();
    if item_id.trim().is_empty()
        || !(1..=MAX_ITEM_METADATA_COMPLETENESS_CAPABILITY_LENGTH).contains(&capability_length)
        || fingerprint.is_empty()
        || fingerprint.len() > MAX_ITEM_METADATA_COMPLETENESS_FINGERPRINT_BYTES
    {
        return Err(StorageError::Conflict(
            "invalid item metadata completeness key or fingerprint".into(),
        ));
    }
    Ok(())
}

#[allow(dead_code)] // The local scan worker consumes these transitions in the next phase task.
impl Database {
    pub(crate) async fn prepare_and_claim_item_metadata_completeness_checks(
        &self,
        checks: &[NewItemMetadataCompletenessCheck<'_>],
    ) -> Result<Vec<usize>, StorageError> {
        self.prepare_and_claim_item_metadata_completeness_checks_with_due_fill_missing_retries(
            checks,
            "",
            &[],
        )
        .await
        .map(|(claimed_indices, _)| claimed_indices)
    }

    pub(crate) async fn prepare_and_claim_item_metadata_completeness_checks_with_due_fill_missing_retries(
        &self,
        checks: &[NewItemMetadataCompletenessCheck<'_>],
        library_id: &str,
        due_fill_missing_retry_candidates: &[String],
    ) -> Result<(Vec<usize>, Vec<String>), StorageError> {
        if checks.len() > MAX_ITEM_METADATA_COMPLETENESS_CHECK_BATCH_SIZE {
            return Err(StorageError::Conflict(
                "metadata completeness check batch exceeds the storage limit".into(),
            ));
        }
        if due_fill_missing_retry_candidates.len() > MAX_ITEM_METADATA_COMPLETENESS_CHECK_BATCH_SIZE
        {
            return Err(StorageError::Conflict(
                "metadata fill-missing retry candidate batch exceeds the storage limit".into(),
            ));
        }
        let mut unique_checks = HashSet::with_capacity(checks.len());
        let mut checked_item_ids = HashSet::with_capacity(checks.len());
        for check in checks {
            validate_item_metadata_completeness_key(
                check.item_id,
                check.capability,
                check.input_fingerprint,
            )?;
            if !unique_checks.insert((check.item_id, check.capability.trim())) {
                return Err(StorageError::Conflict(
                    "metadata completeness check batch contains a duplicate item capability".into(),
                ));
            }
            checked_item_ids.insert(check.item_id);
        }
        let mut unique_retry_candidates =
            HashSet::with_capacity(due_fill_missing_retry_candidates.len());
        for item_id in due_fill_missing_retry_candidates {
            if item_id.trim().is_empty() || !checked_item_ids.contains(item_id.as_str()) {
                return Err(StorageError::Conflict(
                    "metadata fill-missing retry candidates must belong to the check batch".into(),
                ));
            }
            unique_retry_candidates.insert(item_id.clone());
        }
        if checks.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }

        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let item_ids = checks
            .iter()
            .map(|check| check.item_id.to_owned())
            .collect::<Vec<_>>();
        self.lock_media_items_for_update(&mut transaction, &item_ids)
            .await?;
        let mut claimed_indices = Vec::with_capacity(checks.len());
        for (batch_index, batch) in checks
            .chunks(ITEM_METADATA_COMPLETENESS_WRITE_BATCH_SIZE)
            .enumerate()
        {
            let values = std::iter::repeat_n("(?, ?, 'PENDING', NULL, ?)", batch.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "INSERT INTO item_metadata_completeness (
                     item_id, capability, local_state, is_missing, input_fingerprint
                 ) VALUES {values}
                 ON CONFLICT(item_id, capability) DO UPDATE SET
                     local_state = 'PENDING', is_missing = NULL,
                     input_fingerprint = excluded.input_fingerprint,
                     checked_at = NULL, retry_after = NULL, error = NULL,
                     updated_at = unixepoch()
                 WHERE item_metadata_completeness.input_fingerprint IS NULL
                    OR item_metadata_completeness.input_fingerprint <> excluded.input_fingerprint
                    OR item_metadata_completeness.local_state = 'CANCELLED'
                    OR (item_metadata_completeness.local_state = 'FAILED'
                        AND (item_metadata_completeness.retry_after IS NULL
                             OR item_metadata_completeness.retry_after <= unixepoch()))"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for check in batch {
                statement = statement
                    .bind(check.item_id)
                    .bind(check.capability.trim())
                    .bind(check.input_fingerprint.to_vec());
            }
            statement
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;

            let claim_values = std::iter::repeat_n("(?, ?, ?)", batch.len())
                .collect::<Vec<_>>()
                .join(", ");
            let claim_query = format!(
                "WITH requested(item_id, capability, input_fingerprint) AS (VALUES {claim_values})
                 UPDATE item_metadata_completeness
                 SET local_state = 'RUNNING', is_missing = NULL, checked_at = NULL,
                     retry_after = NULL, error = NULL, updated_at = unixepoch()
                 WHERE local_state = 'PENDING'
                   AND EXISTS (
                       SELECT 1 FROM requested
                       WHERE requested.item_id = item_metadata_completeness.item_id
                         AND requested.capability = item_metadata_completeness.capability
                         AND requested.input_fingerprint = item_metadata_completeness.input_fingerprint
                   )
                 RETURNING item_id, capability"
            );
            let mut claim_statement = self.query(sqlx::AssertSqlSafe(claim_query));
            for check in batch {
                claim_statement = claim_statement
                    .bind(check.item_id)
                    .bind(check.capability.trim())
                    .bind(check.input_fingerprint.to_vec());
            }
            let claimed_keys = claim_statement
                .fetch_all(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
                .into_iter()
                .map(|row| {
                    (
                        row.get::<String, _>("item_id"),
                        row.get::<String, _>("capability"),
                    )
                })
                .collect::<HashSet<_>>();
            for (offset, check) in batch.iter().enumerate() {
                if claimed_keys
                    .contains(&(check.item_id.to_owned(), check.capability.trim().to_owned()))
                {
                    claimed_indices
                        .push(batch_index * ITEM_METADATA_COMPLETENESS_WRITE_BATCH_SIZE + offset);
                }
            }
        }
        let mut due_fill_missing_retry_item_ids = Vec::new();
        if !unique_retry_candidates.is_empty() {
            if library_id.trim().is_empty() {
                return Err(StorageError::Conflict(
                    "metadata fill-missing retry lookup requires a library".into(),
                ));
            }
            let candidate_ids = unique_retry_candidates.into_iter().collect::<Vec<_>>();
            let requested_values = std::iter::repeat_n("(?)", candidate_ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "WITH requested(item_id) AS (VALUES {requested_values})
                 SELECT requested.item_id
                 FROM requested
                 JOIN media_items ON media_items.id = requested.item_id
                 LEFT JOIN server_settings legacy_retry
                   ON legacy_retry.key = 'metadata_fill_missing_legacy_retry_after'
                 WHERE media_items.library_id = ? AND media_items.removed_at IS NULL
                   AND EXISTS (
                       SELECT 1
                       FROM metadata_reidentify_job_items retry_items
                       JOIN metadata_reidentify_jobs jobs ON jobs.id = retry_items.job_id
                       WHERE retry_items.item_id = requested.item_id
                         AND jobs.library_id = ? AND jobs.mode = 'FILL_MISSING'
                         AND jobs.status = 'DEFERRED' AND jobs.cancel_requested = 0
                         AND retry_items.status = 'FAILED'
                         AND retry_items.error = 'SCRAPER_UNAVAILABLE'
                         AND retry_items.request_fingerprint IS NOT NULL
                         AND COALESCE(
                             retry_items.automatic_retry_after,
                             CASE WHEN retry_items.automatic_retry_count = 0
                                  THEN CAST(legacy_retry.value AS BIGINT)
                                  ELSE NULL END
                         ) <= unixepoch()
                   )
                 ORDER BY requested.item_id"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for item_id in &candidate_ids {
                statement = statement.bind(item_id);
            }
            due_fill_missing_retry_item_ids = statement
                .bind(library_id)
                .bind(library_id)
                .fetch_all(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
                .into_iter()
                .map(|row| row.get::<String, _>("item_id"))
                .collect();
        }
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok((claimed_indices, due_fill_missing_retry_item_ids))
    }

    pub(crate) async fn complete_local_metadata_and_enqueue_fill_missing(
        &self,
        library_id: &str,
        results: &[NewItemMetadataCompletenessResult<'_>],
        eligible_fill_missing_item_ids: &[String],
    ) -> Result<ItemMetadataCompletenessCommit, StorageError> {
        self.complete_local_metadata_and_enqueue_fill_missing_with_policy(
            library_id,
            results,
            eligible_fill_missing_item_ids,
            MetadataAutoMatchPolicy::UseLibrarySetting,
        )
        .await
    }

    pub(crate) async fn complete_local_metadata_and_enqueue_fill_missing_with_policy(
        &self,
        library_id: &str,
        results: &[NewItemMetadataCompletenessResult<'_>],
        eligible_fill_missing_item_ids: &[String],
        auto_match_policy: MetadataAutoMatchPolicy,
    ) -> Result<ItemMetadataCompletenessCommit, StorageError> {
        if library_id.trim().is_empty() || eligible_fill_missing_item_ids.len() > 256 {
            return Err(StorageError::Conflict(
                "invalid library or fill-missing item count".into(),
            ));
        }
        let mut unique_results = HashSet::with_capacity(results.len());
        for result in results {
            validate_item_metadata_completeness_key(
                result.item_id,
                result.capability,
                result.input_fingerprint,
            )?;
            if !unique_results.insert((result.item_id, result.capability.trim())) {
                return Err(StorageError::Conflict(
                    "metadata completeness result contains a duplicate item capability".into(),
                ));
            }
        }
        let mut eligible_ids = eligible_fill_missing_item_ids.to_vec();
        if eligible_ids.iter().any(|item_id| item_id.trim().is_empty()) {
            return Err(StorageError::Conflict(
                "fill-missing plan contains an empty item id".into(),
            ));
        }
        eligible_ids.sort_unstable();
        eligible_ids.dedup();

        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        if self.backend == crate::config::DatabaseBackend::Postgres {
            let library_exists = self
                .query_scalar::<String>("SELECT id FROM libraries WHERE id = ? FOR UPDATE")
                .bind(library_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if library_exists.is_none() {
                return Err(StorageError::Conflict(
                    "metadata completeness library was not found".into(),
                ));
            }
        } else {
            let library_exists = self
                .query_scalar::<String>("SELECT id FROM libraries WHERE id = ?")
                .bind(library_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if library_exists.is_none() {
                return Err(StorageError::Conflict(
                    "metadata completeness library was not found".into(),
                ));
            }
        }

        let lock_ids = results
            .iter()
            .map(|result| result.item_id.to_owned())
            .chain(eligible_ids.iter().cloned())
            .collect::<Vec<_>>();
        self.lock_media_items_for_update(&mut transaction, &lock_ids)
            .await?;

        let mut commit = ItemMetadataCompletenessCommit::default();
        let mut confirmed_missing = HashSet::new();
        for batch in results.chunks(ITEM_METADATA_COMPLETENESS_WRITE_BATCH_SIZE) {
            let values = std::iter::repeat_n("(?, ?, ?, ?, ?)", batch.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "WITH requested(item_id, capability, input_fingerprint, is_missing, checked_at) AS (VALUES {values})
                 UPDATE item_metadata_completeness
                 SET local_state = 'READY',
                     is_missing = (
                         SELECT requested.is_missing FROM requested
                         WHERE requested.item_id = item_metadata_completeness.item_id
                           AND requested.capability = item_metadata_completeness.capability
                           AND requested.input_fingerprint = item_metadata_completeness.input_fingerprint
                     ),
                     checked_at = (
                         SELECT requested.checked_at FROM requested
                         WHERE requested.item_id = item_metadata_completeness.item_id
                           AND requested.capability = item_metadata_completeness.capability
                           AND requested.input_fingerprint = item_metadata_completeness.input_fingerprint
                     ),
                     retry_after = NULL, error = NULL, updated_at = unixepoch()
                 WHERE local_state = 'RUNNING'
                   AND EXISTS (
                       SELECT 1 FROM requested
                       WHERE requested.item_id = item_metadata_completeness.item_id
                         AND requested.capability = item_metadata_completeness.capability
                         AND requested.input_fingerprint = item_metadata_completeness.input_fingerprint
                   )
                   AND EXISTS (
                       SELECT 1 FROM media_items
                       WHERE id = item_metadata_completeness.item_id
                         AND library_id = ? AND removed_at IS NULL
                   )
                 RETURNING item_id, is_missing"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for result in batch {
                statement = statement
                    .bind(result.item_id)
                    .bind(result.capability.trim())
                    .bind(result.input_fingerprint.to_vec())
                    .bind(database_flag(result.is_missing))
                    .bind(result.checked_at);
            }
            let rows = statement
                .bind(library_id)
                .fetch_all(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            for row in rows {
                let item_id = row.get::<String, _>("item_id");
                commit.updated_count = commit.updated_count.saturating_add(1);
                if row.get::<i64, _>("is_missing") != 0 {
                    confirmed_missing.insert(item_id);
                }
            }
        }

        let auto_match_enabled = match auto_match_policy {
            MetadataAutoMatchPolicy::Enabled => true,
            MetadataAutoMatchPolicy::Disabled => false,
            MetadataAutoMatchPolicy::UseLibrarySetting => {
                self.query_scalar::<i64>(
                    "SELECT scan_missing_metadata_auto_match_enabled
                     FROM libraries WHERE id = ?",
                )
                .bind(library_id)
                .fetch_one(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })? != 0
            }
        };
        if auto_match_enabled {
            for ids in eligible_ids.chunks(100) {
                if ids.is_empty() {
                    continue;
                }
                let placeholders = std::iter::repeat_n("?", ids.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                let query = format!(
                    "SELECT DISTINCT completeness.item_id
                 FROM item_metadata_completeness completeness
                 JOIN media_items item ON item.id = completeness.item_id
                 WHERE item.library_id = ? AND item.removed_at IS NULL
                   AND item.item_type IN ('MOVIE', 'SERIES', 'SEASON', 'EPISODE')
                   AND completeness.local_state = 'READY' AND completeness.is_missing = 1
                   AND completeness.item_id IN ({placeholders})"
                );
                let mut statement = self.query(sqlx::AssertSqlSafe(query)).bind(library_id);
                for item_id in ids {
                    statement = statement.bind(item_id);
                }
                confirmed_missing.extend(
                    statement
                        .fetch_all(&mut *transaction)
                        .await
                        .map_err(|source| StorageError::Sqlx {
                            path: self.path.clone(),
                            source,
                        })?
                        .into_iter()
                        .map(|row| row.get::<String, _>("item_id")),
                );
            }
        }
        if auto_match_enabled && !eligible_ids.is_empty() && !confirmed_missing.is_empty() {
            let candidate_ids = eligible_ids
                .into_iter()
                .filter(|item_id| confirmed_missing.contains(item_id))
                .collect::<Vec<_>>();
            let mut schedulable_ids = Vec::new();
            for ids in candidate_ids.chunks(100) {
                if ids.is_empty() {
                    continue;
                }
                let placeholders = std::iter::repeat_n("?", ids.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                let query = format!(
                    "SELECT id FROM media_items
                     WHERE library_id = ? AND removed_at IS NULL
                       AND item_type IN ('MOVIE', 'SERIES', 'SEASON', 'EPISODE')
                       AND id IN ({placeholders})
                     ORDER BY id"
                );
                let mut statement = self.query(sqlx::AssertSqlSafe(query)).bind(library_id);
                for item_id in ids {
                    statement = statement.bind(item_id);
                }
                schedulable_ids.extend(
                    statement
                        .fetch_all(&mut *transaction)
                        .await
                        .map_err(|source| StorageError::Sqlx {
                            path: self.path.clone(),
                            source,
                        })?
                        .into_iter()
                        .map(|row| row.get::<String, _>("id")),
                );
            }
            let requests = self
                .build_metadata_fill_missing_requests(
                    &mut transaction,
                    library_id,
                    &schedulable_ids,
                    results,
                )
                .await?;
            commit.scheduled_job_ids = self
                .enqueue_or_update_fill_missing_requests_in_transaction(
                    &mut transaction,
                    library_id,
                    &requests,
                )
                .await?;
        }

        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(commit)
    }

    async fn build_metadata_fill_missing_requests(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        library_id: &str,
        item_ids: &[String],
        results: &[NewItemMetadataCompletenessResult<'_>],
    ) -> Result<Vec<MetadataFillMissingRequest>, StorageError> {
        let requested_item_ids = item_ids.iter().map(String::as_str).collect::<HashSet<_>>();
        let mut fingerprints = HashMap::<String, Vec<u8>>::with_capacity(item_ids.len());
        for result in results {
            if !requested_item_ids.contains(result.item_id) {
                continue;
            }
            if let Some(existing) = fingerprints.get(result.item_id) {
                if existing.as_slice() != result.input_fingerprint {
                    return Err(StorageError::Conflict(
                        "fill-missing item has inconsistent completeness fingerprints".into(),
                    ));
                }
            } else {
                fingerprints.insert(result.item_id.to_owned(), result.input_fingerprint.to_vec());
            }
        }

        let mut missing_by_item = HashMap::<String, Vec<(String, Option<Vec<u8>>)>>::new();
        for item_batch in item_ids.chunks(100) {
            if item_batch.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", item_batch.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT completeness.item_id, completeness.capability,
                        completeness.input_fingerprint
                 FROM item_metadata_completeness completeness
                 JOIN media_items item ON item.id = completeness.item_id
                 WHERE item.library_id = ? AND item.removed_at IS NULL
                   AND item.item_type IN ('MOVIE', 'SERIES', 'SEASON', 'EPISODE')
                   AND completeness.local_state = 'READY'
                   AND completeness.is_missing = 1
                   AND completeness.item_id IN ({placeholders})
                 ORDER BY completeness.item_id, completeness.updated_at DESC,
                          completeness.checked_at DESC, completeness.capability"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query)).bind(library_id);
            for item_id in item_batch {
                statement = statement.bind(item_id);
            }
            for row in statement
                .fetch_all(&mut **transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
            {
                missing_by_item
                    .entry(row.get("item_id"))
                    .or_default()
                    .push((row.get("capability"), row.get("input_fingerprint")));
            }
        }

        let mut requests = Vec::with_capacity(item_ids.len());
        for item_id in item_ids {
            let Some(missing) = missing_by_item.get(item_id) else {
                continue;
            };
            let fingerprint = fingerprints.get(item_id).cloned().or_else(|| {
                missing
                    .iter()
                    .find_map(|(_, fingerprint)| fingerprint.clone())
            });
            let Some(fingerprint) = fingerprint else {
                continue;
            };
            let capabilities = missing
                .iter()
                .filter(|(_, candidate_fingerprint)| {
                    candidate_fingerprint.as_deref() == Some(fingerprint.as_slice())
                })
                .map(|(capability, _)| capability.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            if capabilities.is_empty() {
                continue;
            }
            let capabilities_json = serde_json::to_string(&capabilities).map_err(|_| {
                StorageError::Conflict("fill-missing capability set could not be encoded".into())
            })?;
            requests.push(MetadataFillMissingRequest {
                item_id: item_id.clone(),
                input_fingerprint: Some(fingerprint),
                capabilities_json,
                automatic_retry_count: 0,
            });
        }
        Ok(requests)
    }

    pub(crate) async fn prepare_item_metadata_completeness_check(
        &self,
        item_id: &str,
        capability: &str,
        input_fingerprint: &[u8],
    ) -> Result<bool, StorageError> {
        validate_item_metadata_completeness_key(item_id, capability, input_fingerprint)?;
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let item_ids = vec![item_id.to_owned()];
        self.lock_media_items_for_update(&mut transaction, &item_ids)
            .await?;
        let changed = self
            .query_scalar::<String>(
                "INSERT INTO item_metadata_completeness (
                     item_id, capability, local_state, is_missing, input_fingerprint
                 ) VALUES (?, ?, 'PENDING', NULL, ?)
                 ON CONFLICT(item_id, capability) DO UPDATE SET
                     local_state = 'PENDING', is_missing = NULL,
                     input_fingerprint = excluded.input_fingerprint,
                     checked_at = NULL, retry_after = NULL, error = NULL,
                     updated_at = unixepoch()
                 WHERE item_metadata_completeness.input_fingerprint IS NULL
                    OR item_metadata_completeness.input_fingerprint <> excluded.input_fingerprint
                    OR item_metadata_completeness.local_state = 'CANCELLED'
                    OR (item_metadata_completeness.local_state = 'FAILED'
                        AND (item_metadata_completeness.retry_after IS NULL
                             OR item_metadata_completeness.retry_after <= unixepoch()))
                 RETURNING item_id",
            )
            .bind(item_id)
            .bind(capability.trim())
            .bind(input_fingerprint.to_vec())
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
        Ok(changed.is_some())
    }

    pub(crate) async fn claim_item_metadata_completeness_check(
        &self,
        item_id: &str,
        capability: &str,
        input_fingerprint: &[u8],
    ) -> Result<bool, StorageError> {
        validate_item_metadata_completeness_key(item_id, capability, input_fingerprint)?;
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let changed = self
            .query_scalar::<String>(
                "UPDATE item_metadata_completeness
                 SET local_state = 'RUNNING', is_missing = NULL, checked_at = NULL,
                     retry_after = NULL, error = NULL, updated_at = unixepoch()
                 WHERE item_id = ? AND capability = ? AND input_fingerprint = ?
                   AND local_state = 'PENDING'
                 RETURNING item_id",
            )
            .bind(item_id)
            .bind(capability.trim())
            .bind(input_fingerprint.to_vec())
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
        Ok(changed.is_some())
    }

    pub(crate) async fn finish_item_metadata_completeness_check(
        &self,
        item_id: &str,
        capability: &str,
        input_fingerprint: &[u8],
        is_missing: bool,
        checked_at: i64,
    ) -> Result<bool, StorageError> {
        validate_item_metadata_completeness_key(item_id, capability, input_fingerprint)?;
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let changed = self
            .query_scalar::<String>(
                "UPDATE item_metadata_completeness
                 SET local_state = 'READY', is_missing = ?, checked_at = ?, retry_after = NULL,
                     error = NULL, updated_at = unixepoch()
                 WHERE item_id = ? AND capability = ? AND input_fingerprint = ?
                   AND local_state = 'RUNNING'
                 RETURNING item_id",
            )
            .bind(database_flag(is_missing))
            .bind(checked_at)
            .bind(item_id)
            .bind(capability.trim())
            .bind(input_fingerprint.to_vec())
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
        Ok(changed.is_some())
    }

    pub(crate) async fn fail_item_metadata_completeness_check(
        &self,
        item_id: &str,
        capability: &str,
        input_fingerprint: &[u8],
        retry_after: Option<i64>,
        error: &str,
    ) -> Result<bool, StorageError> {
        validate_item_metadata_completeness_key(item_id, capability, input_fingerprint)?;
        if error.len() > MAX_ITEM_METADATA_COMPLETENESS_ERROR_BYTES {
            return Err(StorageError::Conflict(
                "item metadata completeness error exceeds the storage limit".into(),
            ));
        }
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let changed = self
            .query_scalar::<String>(
                "UPDATE item_metadata_completeness
                 SET local_state = 'FAILED', is_missing = NULL, checked_at = NULL,
                     retry_after = ?, error = ?, updated_at = unixepoch()
                 WHERE item_id = ? AND capability = ? AND input_fingerprint = ?
                   AND local_state = 'RUNNING'
                 RETURNING item_id",
            )
            .bind(retry_after)
            .bind(error)
            .bind(item_id)
            .bind(capability.trim())
            .bind(input_fingerprint.to_vec())
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
        Ok(changed.is_some())
    }

    pub(crate) async fn cancel_item_metadata_completeness_check(
        &self,
        item_id: &str,
        capability: &str,
        input_fingerprint: &[u8],
    ) -> Result<bool, StorageError> {
        validate_item_metadata_completeness_key(item_id, capability, input_fingerprint)?;
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let changed = self
            .query_scalar::<String>(
                "UPDATE item_metadata_completeness
                 SET local_state = 'CANCELLED', is_missing = NULL, checked_at = NULL,
                     retry_after = NULL, error = NULL, updated_at = unixepoch()
                 WHERE item_id = ? AND capability = ? AND input_fingerprint = ?
                   AND local_state IN ('PENDING', 'RUNNING')
                 RETURNING item_id",
            )
            .bind(item_id)
            .bind(capability.trim())
            .bind(input_fingerprint.to_vec())
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
        Ok(changed.is_some())
    }

    /// Call once during startup, before local metadata workers begin claiming checks.
    pub(crate) async fn requeue_interrupted_item_metadata_completeness_checks(
        &self,
    ) -> Result<u64, StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let result = self
            .query(
                "UPDATE item_metadata_completeness
                 SET local_state = 'PENDING', is_missing = NULL, checked_at = NULL,
                     retry_after = NULL, error = NULL, updated_at = unixepoch()
                 WHERE local_state = 'RUNNING'",
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

    pub(crate) async fn find_item_metadata_completeness(
        &self,
        item_id: &str,
        capability: &str,
    ) -> Result<Option<StoredItemMetadataCompleteness>, StorageError> {
        if item_id.trim().is_empty()
            || capability.trim().is_empty()
            || capability.trim().chars().count() > MAX_ITEM_METADATA_COMPLETENESS_CAPABILITY_LENGTH
        {
            return Err(StorageError::Conflict(
                "invalid item metadata completeness key".into(),
            ));
        }
        self.query(
            "SELECT item_id, capability, local_state, is_missing, input_fingerprint,
                    checked_at, retry_after, error, updated_at
             FROM item_metadata_completeness WHERE item_id = ? AND capability = ?",
        )
        .bind(item_id)
        .bind(capability.trim())
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_item_metadata_completeness))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_confirmed_missing_metadata(
        &self,
        capability: &str,
        after_item_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<StoredItemMetadataCompleteness>, StorageError> {
        let capability_length = capability.trim().chars().count();
        let limit = i64::try_from(limit).map_err(|_| {
            StorageError::Conflict("invalid metadata completeness page size".into())
        })?;
        if !(1..=MAX_ITEM_METADATA_COMPLETENESS_CAPABILITY_LENGTH).contains(&capability_length)
            || !(1..=MAX_ITEM_METADATA_COMPLETENESS_PAGE_SIZE).contains(&limit)
            || after_item_id.is_some_and(|item_id| item_id.trim().is_empty())
        {
            return Err(StorageError::Conflict(
                "invalid metadata completeness filter or page size".into(),
            ));
        }
        let rows = if let Some(after_item_id) = after_item_id {
            self.query(
                "SELECT completeness.item_id, completeness.capability, completeness.local_state,
                        completeness.is_missing, completeness.input_fingerprint,
                        completeness.checked_at, completeness.retry_after, completeness.error,
                        completeness.updated_at
                 FROM item_metadata_completeness completeness
                 JOIN media_items ON media_items.id = completeness.item_id
                 WHERE completeness.capability = ? AND completeness.local_state = 'READY'
                   AND completeness.is_missing = 1
                   AND media_items.removed_at IS NULL
                   AND completeness.item_id > ?
                 ORDER BY completeness.item_id LIMIT ?",
            )
            .bind(capability.trim())
            .bind(after_item_id)
            .bind(limit)
            .fetch_all(&self.pool)
            .await
        } else {
            self.query(
                "SELECT completeness.item_id, completeness.capability, completeness.local_state,
                        completeness.is_missing, completeness.input_fingerprint,
                        completeness.checked_at, completeness.retry_after, completeness.error,
                        completeness.updated_at
                 FROM item_metadata_completeness completeness
                 JOIN media_items ON media_items.id = completeness.item_id
                 WHERE completeness.capability = ? AND completeness.local_state = 'READY'
                   AND completeness.is_missing = 1
                   AND media_items.removed_at IS NULL
                 ORDER BY completeness.item_id LIMIT ?",
            )
            .bind(capability.trim())
            .bind(limit)
            .fetch_all(&self.pool)
            .await
        }
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        Ok(rows
            .into_iter()
            .map(stored_item_metadata_completeness)
            .collect())
    }
}

impl Database {
    pub(crate) async fn count_pending_metadata_candidates(&self) -> Result<i64, StorageError> {
        self.query_scalar("SELECT COUNT(*) FROM metadata_candidates WHERE status = 'PENDING'")
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_pending_metadata_item_ids(
        &self,
        item_ids: &[String],
    ) -> Result<HashSet<String>, StorageError> {
        let mut pending = HashSet::new();
        for chunk in item_ids.chunks(500) {
            if chunk.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT DISTINCT item_id FROM metadata_candidates
                 WHERE status = 'PENDING' AND item_id IN ({placeholders})"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for item_id in chunk {
                statement = statement.bind(item_id);
            }
            let rows =
                statement
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
            pending.extend(rows.into_iter().map(|row| row.get("item_id")));
        }
        Ok(pending)
    }

    pub(crate) async fn insert_metadata_candidates(
        &self,
        candidates: &[NewMetadataCandidate<'_>],
    ) -> Result<(), StorageError> {
        if candidates.is_empty() {
            return Ok(());
        }

        // A multi-row upsert cannot target the same pending identity more than once in
        // PostgreSQL. Preserve the old sequential-upsert behavior by keeping the first ID
        // while selecting the highest-scoring payload (later payload wins ties).
        let mut unique_candidates = Vec::with_capacity(candidates.len());
        let mut candidate_indices = HashMap::with_capacity(candidates.len());
        for candidate in candidates {
            let identity = (candidate.item_id, candidate.provider, candidate.provider_id);
            if let Some(index) = candidate_indices.get(&identity).copied() {
                let existing: &mut NewMetadataCandidate<'_> = &mut unique_candidates[index];
                if candidate.score >= existing.score {
                    let id = existing.id;
                    *existing = *candidate;
                    existing.id = id;
                }
            } else {
                candidate_indices.insert(identity, unique_candidates.len());
                unique_candidates.push(*candidate);
            }
        }

        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        for chunk in unique_candidates.chunks(100) {
            let values = std::iter::repeat_n("(?, ?, ?, ?, ?, ?, 'PENDING', ?)", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "INSERT INTO metadata_candidates (
                    id, item_id, provider, provider_id, candidate_json, score, status, expires_at
                ) VALUES {values}
                ON CONFLICT (item_id, provider, provider_id) WHERE status = 'PENDING'
                DO UPDATE SET
                    candidate_json = CASE
                        WHEN excluded.score >= metadata_candidates.score
                        THEN excluded.candidate_json
                        ELSE metadata_candidates.candidate_json
                    END,
                    score = CASE
                        WHEN excluded.score >= metadata_candidates.score
                        THEN excluded.score
                        ELSE metadata_candidates.score
                    END,
                    expires_at = CASE
                        WHEN excluded.score >= metadata_candidates.score
                        THEN excluded.expires_at
                        ELSE metadata_candidates.expires_at
                    END,
                    updated_at = unixepoch()"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for candidate in chunk {
                statement = statement
                    .bind(candidate.id)
                    .bind(candidate.item_id)
                    .bind(candidate.provider)
                    .bind(candidate.provider_id)
                    .bind(candidate.candidate_json)
                    .bind(candidate.score)
                    .bind(candidate.expires_at);
            }
            statement
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
            })
    }

    pub(crate) async fn update_pending_metadata_candidate_json(
        &self,
        item_id: &str,
        candidate_id: &str,
        candidate_json: &str,
    ) -> Result<bool, StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let result = self
            .query(
                "UPDATE metadata_candidates
                 SET candidate_json = ?, updated_at = unixepoch()
                 WHERE id = ? AND item_id = ? AND status = 'PENDING'",
            )
            .bind(candidate_json)
            .bind(candidate_id)
            .bind(item_id)
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

    pub(crate) async fn list_metadata_attempts(
        &self,
        item_id: &str,
    ) -> Result<
        (
            Vec<StoredMetadataCapabilityAttempt>,
            Vec<StoredMetadataImageAttempt>,
        ),
        StorageError,
    > {
        let rows = self
            .query(
                "SELECT 'CAPABILITY' AS attempt_type, provider, provider_id, capability,
                        status, next_retry_at, NULL AS image_type, NULL AS candidate_key
                 FROM metadata_capability_attempts
                 WHERE item_id = ?
                 UNION ALL
                 SELECT 'IMAGE' AS attempt_type, NULL AS provider, NULL AS provider_id,
                        NULL AS capability, status, NULL AS next_retry_at,
                        image_type, candidate_key
                 FROM metadata_image_attempts
                 WHERE item_id = ?",
            )
            .bind(item_id)
            .bind(item_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let mut capability_attempts = Vec::new();
        let mut image_attempts = Vec::new();
        for row in rows {
            if row.get::<String, _>("attempt_type") == "CAPABILITY" {
                capability_attempts.push(StoredMetadataCapabilityAttempt {
                    provider: row.get("provider"),
                    provider_id: row.get("provider_id"),
                    capability: row.get("capability"),
                    status: row.get("status"),
                    next_retry_at: row.get("next_retry_at"),
                });
            } else {
                image_attempts.push(StoredMetadataImageAttempt {
                    image_type: row.get("image_type"),
                    candidate_key: row.get("candidate_key"),
                    status: row.get("status"),
                });
            }
        }
        Ok((capability_attempts, image_attempts))
    }

    pub(crate) async fn list_metadata_attempts_by_item_ids(
        &self,
        item_ids: &[String],
    ) -> Result<
        HashMap<
            String,
            (
                Vec<StoredMetadataCapabilityAttempt>,
                Vec<StoredMetadataImageAttempt>,
            ),
        >,
        StorageError,
    > {
        let mut attempts_by_item = HashMap::with_capacity(item_ids.len());
        for chunk in item_ids.chunks(500) {
            if chunk.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT item_id, 'CAPABILITY' AS attempt_type, provider, provider_id,
                        capability, status, next_retry_at, NULL AS image_type,
                        NULL AS candidate_key
                 FROM metadata_capability_attempts
                 WHERE item_id IN ({placeholders})
                 UNION ALL
                 SELECT item_id, 'IMAGE' AS attempt_type, NULL AS provider,
                        NULL AS provider_id, NULL AS capability, status,
                        NULL AS next_retry_at, image_type, candidate_key
                 FROM metadata_image_attempts
                 WHERE item_id IN ({placeholders})"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for item_id in chunk {
                statement = statement.bind(item_id);
            }
            for item_id in chunk {
                statement = statement.bind(item_id);
            }
            let rows =
                statement
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
            for row in rows {
                let item_id = row.get::<String, _>("item_id");
                let entry = attempts_by_item
                    .entry(item_id)
                    .or_insert_with(|| (Vec::<StoredMetadataCapabilityAttempt>::new(), Vec::new()));
                if row.get::<String, _>("attempt_type") == "CAPABILITY" {
                    entry.0.push(StoredMetadataCapabilityAttempt {
                        provider: row.get("provider"),
                        provider_id: row.get("provider_id"),
                        capability: row.get("capability"),
                        status: row.get("status"),
                        next_retry_at: row.get("next_retry_at"),
                    });
                } else {
                    entry.1.push(StoredMetadataImageAttempt {
                        image_type: row.get("image_type"),
                        candidate_key: row.get("candidate_key"),
                        status: row.get("status"),
                    });
                }
            }
        }
        Ok(attempts_by_item)
    }

    pub(crate) async fn record_metadata_capability_results(
        &self,
        item_id: &str,
        provider: &str,
        provider_id: &str,
        results: &[MetadataCapabilityResult<'_>],
        now: i64,
    ) -> Result<(), StorageError> {
        if results.is_empty() {
            return Ok(());
        }
        let mut unique_results = Vec::with_capacity(results.len());
        let mut result_indices = HashMap::with_capacity(results.len());
        for result in results {
            if let Some(index) = result_indices.get(result.capability).copied() {
                unique_results[index] = result;
            } else {
                result_indices.insert(result.capability, unique_results.len());
                unique_results.push(result);
            }
        }
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        for chunk in unique_results.chunks(100) {
            let values = std::iter::repeat_n("(?, ?, ?, ?, ?, 1, ?, NULL, ?, ?)", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "INSERT INTO metadata_capability_attempts (
                    item_id, provider, provider_id, capability, status, attempt_count,
                    last_attempt_at, next_retry_at, error_code, updated_at
                ) VALUES {values}
                ON CONFLICT(item_id, provider, provider_id, capability) DO UPDATE SET
                    status = excluded.status,
                    attempt_count = 1,
                    last_attempt_at = excluded.last_attempt_at,
                    next_retry_at = NULL,
                    error_code = excluded.error_code,
                    updated_at = excluded.updated_at"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for result in chunk {
                let status = if result.has_data {
                    "AVAILABLE"
                } else {
                    "UNAVAILABLE"
                };
                let error_code = (!result.has_data).then_some("NO_DATA");
                statement = statement
                    .bind(item_id)
                    .bind(provider)
                    .bind(provider_id)
                    .bind(result.capability)
                    .bind(status)
                    .bind(now)
                    .bind(error_code)
                    .bind(now);
            }
            statement
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
            })
    }

    pub(crate) async fn record_metadata_capability_failures(
        &self,
        item_id: &str,
        provider: &str,
        provider_id: &str,
        capabilities: &[&str],
        now: i64,
    ) -> Result<(), StorageError> {
        if capabilities.is_empty() {
            return Ok(());
        }
        let mut unique_capabilities = Vec::with_capacity(capabilities.len());
        let mut seen = HashSet::with_capacity(capabilities.len());
        for capability in capabilities {
            if seen.insert(*capability) {
                unique_capabilities.push(*capability);
            }
        }
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let mut attempts = HashMap::with_capacity(unique_capabilities.len());
        for chunk in unique_capabilities.chunks(100) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT capability, attempt_count
                 FROM metadata_capability_attempts
                 WHERE item_id = ? AND provider = ? AND provider_id = ?
                   AND capability IN ({placeholders})"
            );
            let mut statement = self.query_as::<(String, i64)>(sqlx::AssertSqlSafe(query));
            statement = statement.bind(item_id).bind(provider).bind(provider_id);
            for capability in chunk {
                statement = statement.bind(*capability);
            }
            let rows = statement
                .fetch_all(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            attempts.extend(rows);
        }
        let retry_rows = unique_capabilities
            .iter()
            .map(|capability| {
                let previous_attempt_count = attempts.get(*capability).copied().unwrap_or_default();
                let exponent = previous_attempt_count.clamp(0, 5) as u32;
                let delay = 300_i64
                    .saturating_mul(1_i64.checked_shl(exponent).unwrap_or(i64::MAX))
                    .min(86_400);
                (*capability, now.saturating_add(delay))
            })
            .collect::<Vec<_>>();
        for chunk in retry_rows.chunks(100) {
            let values = std::iter::repeat_n(
                "(?, ?, ?, ?, 'FAILED', 1, ?, ?, 'TRANSIENT_FAILURE', ?)",
                chunk.len(),
            )
            .collect::<Vec<_>>()
            .join(", ");
            let query = format!(
                "INSERT INTO metadata_capability_attempts (
                    item_id, provider, provider_id, capability, status, attempt_count,
                    last_attempt_at, next_retry_at, error_code, updated_at
                ) VALUES {values}
                ON CONFLICT(item_id, provider, provider_id, capability) DO UPDATE SET
                    status = 'FAILED',
                    attempt_count = metadata_capability_attempts.attempt_count + 1,
                    last_attempt_at = excluded.last_attempt_at,
                    next_retry_at = excluded.next_retry_at,
                    error_code = excluded.error_code,
                    updated_at = excluded.updated_at"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for (capability, next_retry_at) in chunk {
                statement = statement
                    .bind(item_id)
                    .bind(provider)
                    .bind(provider_id)
                    .bind(capability)
                    .bind(now)
                    .bind(next_retry_at)
                    .bind(now);
            }
            statement
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
            })
    }

    pub(crate) async fn list_pending_metadata_candidates(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredMetadataCandidate>, StorageError> {
        self.query(
            "SELECT mc.id, mc.item_id, mc.provider, mc.provider_id,
                    mc.candidate_json, mc.score, mc.status, mc.expires_at,
                    mi.title AS item_title
             FROM metadata_candidates mc
             JOIN media_items mi ON mi.id = mc.item_id
             WHERE mc.status = 'PENDING' AND mi.removed_at IS NULL
             ORDER BY mc.created_at, mc.id
             LIMIT ? OFFSET ?",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map(|rows| rows.into_iter().map(stored_metadata_candidate).collect())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn count_pending_metadata_candidates_for_item(
        &self,
        item_id: &str,
        search: Option<&str>,
    ) -> Result<i64, StorageError> {
        let count = if let Some(search) = search.map(str::trim).filter(|value| !value.is_empty()) {
            let pattern = format!("%{search}%");
            self.query_scalar::<i64>(
                "SELECT COUNT(*) FROM metadata_candidates
                 WHERE item_id = ? AND status = 'PENDING'
                   AND (provider_id LIKE ? OR candidate_json LIKE ?)",
            )
            .bind(item_id)
            .bind(&pattern)
            .bind(&pattern)
            .fetch_one(&self.pool)
            .await
        } else {
            self.query_scalar::<i64>(
                "SELECT COUNT(*) FROM metadata_candidates
                 WHERE item_id = ? AND status = 'PENDING'",
            )
            .bind(item_id)
            .fetch_one(&self.pool)
            .await
        };
        count.map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_pending_metadata_candidates_for_item(
        &self,
        item_id: &str,
        search: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredMetadataCandidate>, StorageError> {
        let rows = if let Some(search) = search.map(str::trim).filter(|value| !value.is_empty()) {
            let pattern = format!("%{search}%");
            self.query(
                "SELECT mc.id, mc.item_id, mc.provider, mc.provider_id,
                        mc.candidate_json, mc.score, mc.status, mc.expires_at,
                        mi.title AS item_title
                 FROM metadata_candidates mc
                 JOIN media_items mi ON mi.id = mc.item_id
                 WHERE mc.item_id = ? AND mc.status = 'PENDING' AND mi.removed_at IS NULL
                   AND (mc.provider_id LIKE ? OR mc.candidate_json LIKE ?)
                 ORDER BY mc.created_at, mc.id LIMIT ? OFFSET ?",
            )
            .bind(item_id)
            .bind(&pattern)
            .bind(&pattern)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        } else {
            self.query(
                "SELECT mc.id, mc.item_id, mc.provider, mc.provider_id,
                        mc.candidate_json, mc.score, mc.status, mc.expires_at,
                        mi.title AS item_title
                 FROM metadata_candidates mc
                 JOIN media_items mi ON mi.id = mc.item_id
                 WHERE mc.item_id = ? AND mc.status = 'PENDING' AND mi.removed_at IS NULL
                 ORDER BY mc.created_at, mc.id LIMIT ? OFFSET ?",
            )
            .bind(item_id)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        };
        rows.map(|rows| rows.into_iter().map(stored_metadata_candidate).collect())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_pending_metadata_candidates_for_item_with_count(
        &self,
        item_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<StoredMetadataCandidate>, i64), StorageError> {
        let rows = self
            .query(
                "SELECT mc.id, mc.item_id, mc.provider, mc.provider_id,
                        mc.candidate_json, mc.score, mc.status, mc.expires_at,
                        mi.title AS item_title, COUNT(*) OVER() AS total_count
                 FROM metadata_candidates mc
                 JOIN media_items mi ON mi.id = mc.item_id
                 WHERE mc.item_id = ? AND mc.status = 'PENDING' AND mi.removed_at IS NULL
                 ORDER BY mc.created_at, mc.id
                 LIMIT ? OFFSET ?",
            )
            .bind(item_id)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let total = rows.first().map(|row| row.get("total_count")).unwrap_or(0);
        let candidates = rows.into_iter().map(stored_metadata_candidate).collect();
        Ok((candidates, total))
    }

    pub(crate) async fn list_best_pending_metadata_candidates_for_item(
        &self,
        item_id: &str,
        page_window: i64,
        result_limit: i64,
    ) -> Result<(Vec<StoredMetadataCandidate>, i64), StorageError> {
        const MAX_PAGE_WINDOW: i64 = 50;
        const MAX_RESULT_LIMIT: i64 = 2;

        let page_window = page_window.clamp(1, MAX_PAGE_WINDOW);
        let result_limit = result_limit.clamp(1, MAX_RESULT_LIMIT);
        let rows = self
            .query(
                "WITH pending AS (
                     SELECT mc.id, mc.item_id, mc.score, mc.created_at,
                            COUNT(*) OVER() AS total_count,
                            ROW_NUMBER() OVER(ORDER BY mc.created_at, mc.id) AS page_position
                     FROM metadata_candidates mc
                     JOIN media_items mi ON mi.id = mc.item_id
                     WHERE mc.item_id = ? AND mc.status = 'PENDING' AND mi.removed_at IS NULL
                 ), ranked AS (
                     SELECT id, item_id, score, created_at, total_count,
                            ROW_NUMBER() OVER(ORDER BY score DESC, created_at, id) AS score_position
                     FROM pending
                     WHERE page_position <= ?
                 )
                 SELECT mc.id, mc.item_id, mc.provider, mc.provider_id,
                        mc.candidate_json, mc.score, mc.status, mc.expires_at,
                        mi.title AS item_title, ranked.total_count
                 FROM ranked
                 JOIN metadata_candidates mc ON mc.id = ranked.id
                 JOIN media_items mi ON mi.id = mc.item_id
                 WHERE ranked.score_position <= ?
                 ORDER BY ranked.score DESC, ranked.created_at, ranked.id",
            )
            .bind(item_id)
            .bind(page_window)
            .bind(result_limit)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let total = rows.first().map(|row| row.get("total_count")).unwrap_or(0);
        let candidates = rows.into_iter().map(stored_metadata_candidate).collect();
        Ok((candidates, total))
    }

    pub(crate) async fn find_metadata_candidate(
        &self,
        item_id: &str,
        candidate_id: &str,
    ) -> Result<Option<StoredMetadataCandidate>, StorageError> {
        self.query(
            "SELECT mc.id, mc.item_id, mc.provider, mc.provider_id,
                    mc.candidate_json, mc.score, mc.status, mc.expires_at,
                    mi.title AS item_title
             FROM metadata_candidates mc
             JOIN media_items mi ON mi.id = mc.item_id
             WHERE mc.id = ? AND mc.item_id = ?
               AND mi.removed_at IS NULL
             LIMIT 1",
        )
        .bind(candidate_id)
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_metadata_candidate))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_best_pending_metadata_candidate(
        &self,
        item_id: &str,
    ) -> Result<Option<StoredMetadataCandidate>, StorageError> {
        self.query(
            "SELECT mc.id, mc.item_id, mc.provider, mc.provider_id,
                    mc.candidate_json, mc.score, mc.status, mc.expires_at,
                    mi.title AS item_title
             FROM metadata_candidates mc
             JOIN media_items mi ON mi.id = mc.item_id
             WHERE mc.item_id = ? AND mc.status = 'PENDING'
               AND mi.removed_at IS NULL
             ORDER BY mc.score DESC, mc.created_at, mc.id
             LIMIT 1",
        )
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_metadata_candidate))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_unexpired_pending_metadata_candidates_for_item(
        &self,
        item_id: &str,
        provider_key: &str,
        limit: i64,
    ) -> Result<Vec<StoredMetadataCandidate>, StorageError> {
        let escaped_provider_key = provider_key
            .replace('!', "!!")
            .replace('%', "!%")
            .replace('_', "!_");
        let suffix_patterns =
            [".", ":", "/"].map(|separator| format!("%{separator}{escaped_provider_key}"));
        self.query(
            "SELECT mc.id, mc.item_id, mc.provider, mc.provider_id,
                    mc.candidate_json, mc.score, mc.status, mc.expires_at,
                    mi.title AS item_title
             FROM metadata_candidates mc
             JOIN media_items mi ON mi.id = mc.item_id
             WHERE mc.item_id = ? AND mc.status = 'PENDING'
               AND mi.removed_at IS NULL
               AND (mc.expires_at IS NULL OR mc.expires_at > unixepoch())
               AND (
                   lower(mc.provider) = lower(?)
                   OR lower(mc.provider) LIKE ? ESCAPE '!'
                   OR lower(mc.provider) LIKE ? ESCAPE '!'
                   OR lower(mc.provider) LIKE ? ESCAPE '!'
               )
             ORDER BY mc.score DESC, mc.created_at, mc.id
             LIMIT ?",
        )
        .bind(item_id)
        .bind(provider_key)
        .bind(&suffix_patterns[0])
        .bind(&suffix_patterns[1])
        .bind(&suffix_patterns[2])
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map(|rows| rows.into_iter().map(stored_metadata_candidate).collect())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn select_metadata_candidate(
        &self,
        update: SelectedMetadataUpdate<'_>,
    ) -> Result<bool, StorageError> {
        let sort_title = update.title.to_lowercase();
        let _write_guard = self.acquire_metadata_write_lock().await;
        // SQLite WAL can reject a deferred read-to-write upgrade with
        // SQLITE_BUSY_SNAPSHOT; reserve the single writer before this short
        // metadata transaction performs its updates.
        let mut transaction = self.begin_metadata_write_transaction().await?;
        self.query(
            "UPDATE media_items
             SET title = ?, sort_title = ?, original_title = ?, overview = ?, production_year = ?,
                 premiere_date = COALESCE(?, premiere_date),
                 last_air_date = COALESCE(?, last_air_date),
                 status = COALESCE(?, status),
                 original_language = COALESCE(?, original_language),
                 rating = CASE WHEN ? = 1 THEN ? ELSE rating END,
                 rating_source = CASE WHEN ? IS NULL THEN rating_source ELSE ? END,
                 provider_ids_json = ?,
                 metadata_scraper_id = CASE WHEN ? IS NULL THEN metadata_scraper_id ELSE ? END,
                 identification_status = CASE WHEN ? = 1 THEN 'PENDING' ELSE 'ONLINE_CONFIRMED' END,
                 metadata_fingerprint = ?, metadata_provenance_json = ?, locked_fields_json = ?,
                 poster_fallback_required = ?, updated_at = unixepoch()
             WHERE id = ? AND removed_at IS NULL",
        )
        .bind(update.title)
        .bind(sort_title)
        .bind(update.original_title)
        .bind(update.overview)
        .bind(update.production_year)
        .bind(update.premiere_date)
        .bind(update.last_air_date)
        .bind(update.status)
        .bind(update.original_language)
        .bind(database_flag(update.rating.is_some()))
        .bind(update.rating.unwrap_or_default())
        .bind(update.rating_source)
        .bind(update.rating_source)
        .bind(update.provider_ids_json)
        .bind(update.metadata_scraper_id)
        .bind(update.metadata_scraper_id)
        .bind(database_flag(update.keep_pending))
        .bind(update.metadata_fingerprint)
        .bind(update.provenance_json)
        .bind(update.locked_fields_json)
        .bind(database_flag(update.poster_fallback_required))
        .bind(update.item_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        let selected = self
            .query(
                "UPDATE metadata_candidates
             SET status = CASE WHEN ? = 1 THEN 'PENDING' ELSE 'SELECTED' END,
                 updated_at = unixepoch()
             WHERE id = ? AND item_id = ? AND status = 'PENDING'",
            )
            .bind(database_flag(update.keep_pending))
            .bind(update.candidate_id)
            .bind(update.item_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if selected.rows_affected() != 1 {
            transaction
                .rollback()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(false);
        }
        self.query(
            "UPDATE metadata_candidates
             SET status = 'REJECTED', updated_at = unixepoch()
             WHERE item_id = ? AND status = 'PENDING' AND id <> ?",
        )
        .bind(update.item_id)
        .bind(update.candidate_id)
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
        Ok(true)
    }
}
