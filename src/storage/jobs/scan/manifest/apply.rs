const MAX_SCAN_MANIFEST_APPLY_BATCH_SIZE: i64 = 500;

use super::*;
use deltas::ManifestDeltaApplyOutcome;

#[path = "apply/prepare.rs"]
mod prepare;

#[path = "apply/deltas.rs"]
mod deltas;

impl Database {
    pub(crate) async fn list_pending_scan_manifest_deltas(
        &self,
        manifest_id: &str,
        limit: i64,
    ) -> Result<Vec<StoredScanManifestDelta>, StorageError> {
        self.query(
            "SELECT delta.id, delta.library_root_id, delta.relative_path,
                    delta.observation_sequence, delta.delta_kind,
                    delta.base_filesystem_entry_id, delta.base_fingerprint,
                    observed.entry_kind, observed.size, observed.modified_at,
                    observed.device, observed.inode, observed.fingerprint
             FROM scan_manifest_deltas delta
             LEFT JOIN scan_manifest_entries observed
               ON observed.manifest_id = delta.manifest_id
              AND observed.library_root_id = delta.library_root_id
              AND observed.relative_path = delta.relative_path
              AND observed.observation_sequence = delta.observation_sequence
             WHERE delta.manifest_id = ? AND delta.state = 'PENDING'
             ORDER BY delta.library_root_id, delta.relative_path
             LIMIT ?",
        )
        .bind(manifest_id)
        .bind(limit.clamp(1, MAX_SCAN_MANIFEST_APPLY_BATCH_SIZE))
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredScanManifestDelta {
                    id: row.get("id"),
                    library_root_id: row.get("library_root_id"),
                    relative_path: row.get("relative_path"),
                    observation_sequence: row.get("observation_sequence"),
                    delta_kind: row.get("delta_kind"),
                    base_filesystem_entry_id: row.get("base_filesystem_entry_id"),
                    base_fingerprint: row.get("base_fingerprint"),
                    entry_kind: row.get("entry_kind"),
                    size: row.get("size"),
                    modified_at: row.get("modified_at"),
                    device: row.get("device"),
                    inode: row.get("inode"),
                    fingerprint: row.get("fingerprint"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn count_applied_scan_manifest_removals(
        &self,
        manifest_id: &str,
    ) -> Result<i64, StorageError> {
        self.query_scalar(
            "SELECT COUNT(*) FROM scan_manifest_deltas
             WHERE manifest_id = ? AND delta_kind = 'REMOVE' AND state = 'APPLIED'",
        )
        .bind(manifest_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn commit_scan_manifest_delta_batch(
        &self,
        batch: &ManifestDeltaBatchCommit<'_>,
    ) -> Result<ManifestDeltaBatchCommitResult, StorageError> {
        if batch.deltas.is_empty() {
            return Ok(ManifestDeltaBatchCommitResult::default());
        }
        if batch
            .deltas
            .iter()
            .any(|delta| delta.library_root_id != batch.library_root_id)
        {
            return Err(StorageError::Conflict(
                "manifest apply batch cannot span library roots".to_owned(),
            ));
        }
        let delta_ids = batch
            .deltas
            .iter()
            .map(|delta| delta.id.as_str())
            .collect::<std::collections::HashSet<_>>();
        if batch
            .unstable_delta_ids
            .iter()
            .any(|id| !delta_ids.contains(id.as_str()))
        {
            return Err(StorageError::Conflict(
                "manifest unstable set contains an entry outside the batch".to_owned(),
            ));
        }

        let mut transaction = self.begin_scan_write_transaction().await?;
        let preparation = self
            .prepare_manifest_delta_batch(&mut transaction, batch)
            .await?;
        let ManifestDeltaApplyOutcome {
            mut result,
            new_paths,
            changed_paths,
            removed_media_paths,
            changed_sidecar_paths,
            removed_sidecar_paths,
            removed_media_entry_ids,
            delta_updates,
        } = self
            .apply_manifest_delta_changes(&mut transaction, batch, preparation)
            .await?;
        for chunk in delta_updates.chunks(SCAN_MANIFEST_DELTA_BATCH_SIZE) {
            let state_cases = std::iter::repeat_n("WHEN ? THEN ?", chunk.len())
                .collect::<Vec<_>>()
                .join(" ");
            let error_cases = state_cases.clone();
            let ids = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "UPDATE scan_manifest_deltas
                 SET state = CASE id {state_cases} ELSE state END,
                     error = CASE id {error_cases} ELSE error END,
                     attempt_count = attempt_count + 1,
                     updated_at = unixepoch()
                 WHERE manifest_id = ? AND state = 'PENDING' AND id IN ({ids})"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for (id, state, _) in chunk {
                statement = statement.bind(id).bind(state);
            }
            for (id, _, error) in chunk {
                statement = statement.bind(id).bind(error);
            }
            statement = statement.bind(batch.manifest_id);
            for (id, _, _) in chunk {
                statement = statement.bind(id);
            }
            let updated = statement
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if usize::try_from(updated.rows_affected()).ok() != Some(chunk.len()) {
                return Err(StorageError::Conflict(
                    "manifest delta changed before its batch commit".to_owned(),
                ));
            }
        }

        let manifest_path_chunk_size = super::manifest_path_query_chunk_size(self.backend());
        for paths in new_paths.chunks(manifest_path_chunk_size) {
            result.metadata_targets_changed |= self
                .record_scan_job_targets_in_transaction(
                    &mut transaction,
                    batch.job_id,
                    batch.library_root_id,
                    paths,
                    "NEW",
                )
                .await?;
        }
        for paths in changed_paths.chunks(manifest_path_chunk_size) {
            result.metadata_targets_changed |= self
                .record_scan_job_targets_in_transaction(
                    &mut transaction,
                    batch.job_id,
                    batch.library_root_id,
                    paths,
                    "CHANGED",
                )
                .await?;
        }
        for paths in [&changed_sidecar_paths, &removed_sidecar_paths] {
            let directories = prune_sidecar_directories(
                paths
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
            result.metadata_targets_changed |= self
                .record_scan_job_sidecar_targets_in_transaction(
                    &mut transaction,
                    batch.job_id,
                    batch.library_root_id,
                    &directories,
                )
                .await?;
        }
        self.record_scan_job_removed_targets_for_entry_ids_in_transaction(
            &mut transaction,
            batch.job_id,
            &removed_media_entry_ids,
        )
        .await?;
        if !removed_media_paths.is_empty() {
            self.refresh_removed_media_items_in_transaction(
                &mut transaction,
                batch.library_root_id,
            )
            .await?;
        }
        let processed_count = result
            .applied_count
            .saturating_add(result.conflict_count)
            .saturating_add(result.unstable_count);
        if processed_count > 0 {
            let processed_count_i64 = i64::try_from(processed_count).map_err(|_| {
                StorageError::Conflict("manifest progress count overflow".to_owned())
            })?;
            self.query(
                "UPDATE scan_manifests
                 SET applied_delta_count = applied_delta_count + ?, updated_at = unixepoch()
                 WHERE id = ? AND state = 'APPLYING'",
            )
            .bind(
                i64::try_from(result.applied_count).map_err(|_| {
                    StorageError::Conflict("manifest apply count overflow".to_owned())
                })?,
            )
            .bind(batch.manifest_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
            let update = self
                .query(
                    "UPDATE scan_jobs
                     SET processed_count = processed_count + ?, updated_at = unixepoch()
                     WHERE id = ? AND status = 'RUNNING' AND cancel_requested = 0",
                )
                .bind(processed_count_i64)
                .bind(batch.job_id)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if update.rows_affected() != 1 {
                return Err(StorageError::Conflict(
                    "scan job stopped during manifest delta apply".to_owned(),
                ));
            }
        }
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result)
    }
}
