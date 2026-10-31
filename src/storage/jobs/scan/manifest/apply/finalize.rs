use super::deltas::ManifestDeltaApplyOutcome;
use super::*;
use crate::storage::sql_templates::manifest_delta_finalize_template;
use std::path::Path;

impl Database {
    pub(super) async fn finalize_manifest_delta_batch<'data>(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Any>,
        batch: &ManifestDeltaBatchCommit<'data>,
        outcome: ManifestDeltaApplyOutcome<'data>,
    ) -> Result<ManifestDeltaBatchCommitResult, StorageError> {
        let ManifestDeltaApplyOutcome {
            mut result,
            new_paths,
            changed_paths,
            removed_media_paths,
            changed_sidecar_paths,
            removed_sidecar_paths,
            removed_media_entry_ids,
            delta_updates,
        } = outcome;

        let mut file_change_events = Vec::new();
        for path in &changed_sidecar_paths {
            if !super::super::indexes::is_file_change_only_sidecar_path(path) {
                continue;
            }
            let delta = batch
                .deltas
                .iter()
                .find(|delta| delta.relative_path == *path)
                .ok_or_else(|| {
                    StorageError::Conflict(
                        "manifest sidecar event has no matching delta".to_owned(),
                    )
                })?;
            let change_kind = if matches!(delta.delta_kind.as_str(), "ADD" | "REAPPEARED") {
                "ADD"
            } else {
                "CHANGE"
            };
            file_change_events.push((path.as_str(), change_kind));
        }
        file_change_events.extend(
            removed_sidecar_paths
                .iter()
                .filter(|path| super::super::indexes::is_file_change_only_sidecar_path(path))
                .map(|path| (path.as_str(), "REMOVE")),
        );
        self.record_manifest_file_changes_in_transaction(
            transaction,
            batch.job_id,
            batch.library_root_id,
            &file_change_events,
        )
        .await?;

        for chunk in delta_updates.chunks(SCAN_MANIFEST_DELTA_BATCH_SIZE) {
            let mut statement = match manifest_delta_finalize_template(
                &self.filesystem_path_templates,
                self.backend(),
                chunk.len(),
            ) {
                Ok(sql) => self.query(sqlx::AssertSqlSafe(sql)),
                Err(_) => {
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
                    self.query(sqlx::AssertSqlSafe(query))
                }
            };
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
                .execute(&mut **transaction)
                .await
                .map_err(self.sqlx_err())?;
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
                    transaction,
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
                    transaction,
                    batch.job_id,
                    batch.library_root_id,
                    paths,
                    "CHANGED",
                )
                .await?;
        }
        let legacy_changed_sidecar_paths = changed_sidecar_paths
            .iter()
            .filter(|path| !super::super::indexes::is_file_change_only_sidecar_path(path))
            .collect::<Vec<_>>();
        let legacy_removed_sidecar_paths = removed_sidecar_paths
            .iter()
            .filter(|path| !super::super::indexes::is_file_change_only_sidecar_path(path))
            .collect::<Vec<_>>();
        for paths in [&legacy_changed_sidecar_paths, &legacy_removed_sidecar_paths] {
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
                    transaction,
                    batch.job_id,
                    batch.library_root_id,
                    &directories,
                )
                .await?;
        }
        self.record_scan_job_removed_targets_for_entry_ids_in_transaction(
            transaction,
            batch.job_id,
            &removed_media_entry_ids,
        )
        .await?;
        if !removed_media_paths.is_empty() {
            self.refresh_removed_media_items_in_transaction(transaction, batch.library_root_id)
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
            .execute(&mut **transaction)
            .await
            .map_err(self.sqlx_err())?;
            let update = self
                .query(
                    "UPDATE scan_jobs
                     SET processed_count = processed_count + ?, updated_at = unixepoch()
                     WHERE id = ? AND status = 'RUNNING' AND cancel_requested = 0",
                )
                .bind(processed_count_i64)
                .bind(batch.job_id)
                .execute(&mut **transaction)
                .await
                .map_err(self.sqlx_err())?;
            if update.rows_affected() != 1 {
                return Err(StorageError::Conflict(
                    "scan job stopped during manifest delta apply".to_owned(),
                ));
            }
        }
        Ok(result)
    }
}
