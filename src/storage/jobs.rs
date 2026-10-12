use super::*;

#[path = "jobs/local_metadata.rs"]
mod local_metadata;
use local_metadata::MAX_SCAN_LOCAL_METADATA_BATCH_SOURCES;
pub(crate) use local_metadata::StoredScanLocalMetadataBackfillPage;
#[path = "jobs/metadata_reidentify.rs"]
mod metadata_reidentify;
use local_metadata::escape_sql_like_pattern;
#[path = "jobs/scan.rs"]
mod scan;
use scan::prune_sidecar_directories;
#[cfg(test)]
use scan::sidecar_target_query;
#[path = "jobs/shutdown.rs"]
mod shutdown;

#[path = "jobs/strm_probe.rs"]
mod strm_probe;

#[cfg(test)]
pub(crate) use super::media_sources::MAX_MEDIA_SOURCE_DELETE_BATCH_SIZE;
use std::{collections::HashMap, time::Instant};

const MAX_SCAN_MANIFEST_APPLY_BATCH_SIZE: i64 = 500;

impl Database {
    pub(crate) const LEGACY_SCAN_REQUIRES_NEW_MANIFEST: &'static str =
        "LEGACY_SCAN_REQUIRES_NEW_MANIFEST";
}

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
        let active_job: i64 = self
            .query_scalar(
                "SELECT COUNT(*) FROM scan_jobs
                 WHERE id = ? AND status = 'RUNNING' AND cancel_requested = 0",
            )
            .bind(batch.job_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let manifest_state: Option<String> = self
            .query_scalar("SELECT state FROM scan_manifests WHERE id = ? AND job_id = ?")
            .bind(batch.manifest_id)
            .bind(batch.job_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if active_job != 1 || manifest_state.as_deref() != Some("APPLYING") {
            return Err(StorageError::Conflict(
                "manifest batch requires an active applying scan".to_owned(),
            ));
        }
        let root_state: Option<String> = self
            .query_scalar(
                "SELECT state FROM scan_manifest_roots
                 WHERE manifest_id = ? AND library_root_id = ?",
            )
            .bind(batch.manifest_id)
            .bind(batch.library_root_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;

        let movie_files = batch
            .movie_files
            .iter()
            .map(|file| (file.relative_path.as_str(), file))
            .collect::<HashMap<_, _>>();
        let episode_files = batch
            .episode_files
            .iter()
            .map(|file| (file.relative_path.as_str(), file))
            .collect::<HashMap<_, _>>();
        let unresolved_files = batch
            .unresolved_files
            .iter()
            .map(|file| (file.relative_path.as_str(), file))
            .collect::<HashMap<_, _>>();
        let sidecar_entries = batch
            .sidecar_entries
            .iter()
            .map(|entry| (entry.relative_path.as_str(), entry))
            .collect::<HashMap<_, _>>();
        let unstable_ids = batch
            .unstable_delta_ids
            .iter()
            .map(String::as_str)
            .collect::<std::collections::HashSet<_>>();

        let mut add_filesystem_entries = Vec::new();
        if root_state.as_deref() == Some("COMPLETE") {
            for delta in batch.deltas.iter().filter(|delta| {
                delta.delta_kind == "ADD" && !unstable_ids.contains(delta.id.as_str())
            }) {
                if delta.observation_sequence.is_none() {
                    return Err(StorageError::Conflict(
                        "add delta is missing its observation".to_owned(),
                    ));
                }
                let entry_kind = delta.entry_kind.as_deref().ok_or_else(|| {
                    StorageError::Conflict(
                        "manifest delta observation is missing its entry kind".to_owned(),
                    )
                })?;
                let size = delta.size.ok_or_else(|| {
                    StorageError::Conflict(
                        "manifest delta observation is missing its size".to_owned(),
                    )
                })?;
                let modified_at = delta.modified_at.ok_or_else(|| {
                    StorageError::Conflict(
                        "manifest delta observation is missing its modified time".to_owned(),
                    )
                })?;
                let fingerprint = delta.fingerprint.as_deref().ok_or_else(|| {
                    StorageError::Conflict(
                        "manifest delta observation is missing its fingerprint".to_owned(),
                    )
                })?;
                if entry_kind != "FILE" || delta.base_filesystem_entry_id.is_some() {
                    return Err(StorageError::Conflict(
                        "add delta has an invalid observation or baseline".to_owned(),
                    ));
                }
                let filesystem_entry_id = movie_files
                    .get(delta.relative_path.as_str())
                    .map(|file| file.filesystem_entry_id.as_str())
                    .or_else(|| {
                        episode_files
                            .get(delta.relative_path.as_str())
                            .map(|file| file.filesystem_entry_id.as_str())
                    })
                    .or_else(|| {
                        unresolved_files
                            .get(delta.relative_path.as_str())
                            .map(|file| file.filesystem_entry_id.as_str())
                    })
                    .or_else(|| {
                        sidecar_entries
                            .get(delta.relative_path.as_str())
                            .map(|entry| entry.filesystem_entry_id.as_str())
                    })
                    .ok_or_else(|| {
                        StorageError::Conflict(
                            "manifest add delta has no prepared filesystem record".to_owned(),
                        )
                    })?;
                add_filesystem_entries.push(NewScanManifestFilesystemEntry {
                    id: filesystem_entry_id,
                    relative_path: &delta.relative_path,
                    size,
                    modified_at,
                    inode: delta.inode,
                    fingerprint,
                    last_seen_change_kind: None,
                });
            }
        }
        let claimed_add_paths = self
            .claim_manifest_add_filesystem_entries_in_transaction(
                &mut transaction,
                batch.library_root_id,
                batch.generation,
                &add_filesystem_entries,
            )
            .await?;
        let claimed_movie_files = batch
            .movie_files
            .iter()
            .filter(|file| claimed_add_paths.contains(&file.relative_path))
            .cloned()
            .collect::<Vec<_>>();
        let claimed_episode_files = batch
            .episode_files
            .iter()
            .filter(|file| claimed_add_paths.contains(&file.relative_path))
            .cloned()
            .collect::<Vec<_>>();

        let mut result = ManifestDeltaBatchCommitResult::default();
        result.created_items = self
            .insert_movie_files_without_filesystem_entries_in_transaction(
                &mut transaction,
                batch.library_id,
                batch.library_root_id,
                batch.generation,
                &claimed_movie_files,
            )
            .await?;
        result.created_items = result.created_items.saturating_add(
            self.insert_episode_files_without_filesystem_entries_in_transaction(
                &mut transaction,
                batch.library_id,
                batch.library_root_id,
                batch.generation,
                &claimed_episode_files,
            )
            .await?,
        );
        let mut new_paths = Vec::new();
        let mut changed_paths = Vec::new();
        let mut removed_media_paths = Vec::new();
        let mut changed_sidecar_paths = Vec::new();
        let mut removed_sidecar_paths = Vec::new();
        let mut removed_media_entry_ids = Vec::new();
        let mut delta_updates = Vec::with_capacity(batch.deltas.len());
        for delta in batch.deltas {
            let mut state = "APPLIED";
            let mut error = None;
            let force_unstable = unstable_ids.contains(delta.id.as_str())
                || root_state.as_deref() != Some("COMPLETE");
            if force_unstable {
                state = "UNSTABLE";
                error = Some(if root_state.as_deref() == Some("COMPLETE") {
                    "filesystem observation changed or became unavailable before apply"
                } else {
                    "library root is no longer complete and available"
                });
                result.unstable_count = result.unstable_count.saturating_add(1);
            } else {
                let expected_baseline = delta.base_filesystem_entry_id.as_deref();
                let expected_fingerprint = delta.base_fingerprint.as_deref();
                let expected_missing = delta.delta_kind == "REAPPEARED";
                let observation = delta
                    .observation_sequence
                    .map(|_| {
                        Ok((
                            delta.entry_kind.as_deref().ok_or_else(|| {
                                StorageError::Conflict(
                                    "manifest delta observation is missing its entry kind"
                                        .to_owned(),
                                )
                            })?,
                            delta.size.ok_or_else(|| {
                                StorageError::Conflict(
                                    "manifest delta observation is missing its size".to_owned(),
                                )
                            })?,
                            delta.modified_at.ok_or_else(|| {
                                StorageError::Conflict(
                                    "manifest delta observation is missing its modified time"
                                        .to_owned(),
                                )
                            })?,
                            delta.inode,
                            delta.fingerprint.as_deref().ok_or_else(|| {
                                StorageError::Conflict(
                                    "manifest delta observation is missing its fingerprint"
                                        .to_owned(),
                                )
                            })?,
                        ))
                    })
                    .transpose()?;

                let applied = match delta.delta_kind.as_str() {
                    "ADD" => {
                        let Some((entry_kind, _, _, _, _)) = observation else {
                            return Err(StorageError::Conflict(
                                "add delta is missing its observation".to_owned(),
                            ));
                        };
                        if entry_kind != "FILE" || expected_baseline.is_some() {
                            return Err(StorageError::Conflict(
                                "add delta has an invalid observation or baseline".to_owned(),
                            ));
                        }
                        if !claimed_add_paths.contains(&delta.relative_path) {
                            false
                        } else if movie_files.contains_key(delta.relative_path.as_str())
                            || episode_files.contains_key(delta.relative_path.as_str())
                        {
                            new_paths.push(delta.relative_path.clone());
                            true
                        } else if let Some(file) =
                            unresolved_files.get(delta.relative_path.as_str())
                        {
                            self.materialize_manifest_unresolved_file_after_filesystem_insert_in_transaction(
                                &mut transaction,
                                batch.library_id,
                                batch.library_root_id,
                                file,
                            )
                            .await?;
                            result.created_items = result.created_items.saturating_add(1);
                            new_paths.push(delta.relative_path.clone());
                            true
                        } else if sidecar_entries.contains_key(delta.relative_path.as_str()) {
                            changed_sidecar_paths.push(delta.relative_path.clone());
                            true
                        } else {
                            return Err(StorageError::Conflict(
                                "manifest add delta has no prepared filesystem record".to_owned(),
                            ));
                        }
                    }
                    "CHANGE" | "REAPPEARED" => {
                        let Some(filesystem_entry_id) = expected_baseline else {
                            return Err(StorageError::Conflict(
                                "change delta has no baseline entry".to_owned(),
                            ));
                        };
                        let Some((entry_kind, size, modified_at, inode, fingerprint)) = observation
                        else {
                            return Err(StorageError::Conflict(
                                "change delta is missing its observation".to_owned(),
                            ));
                        };
                        if entry_kind != "FILE" {
                            return Err(StorageError::Conflict(
                                "change delta observation is not a file".to_owned(),
                            ));
                        }
                        let applied = if let Some(file) =
                            movie_files.get(delta.relative_path.as_str())
                        {
                            self.apply_manifest_existing_file_in_transaction(
                                &mut transaction,
                                ManifestExistingFileUpdate {
                                    filesystem_entry_id,
                                    library_root_id: batch.library_root_id,
                                    relative_path: &delta.relative_path,
                                    base_fingerprint: expected_fingerprint,
                                    expected_missing,
                                    size,
                                    modified_at,
                                    inode,
                                    fingerprint,
                                    generation: batch.generation,
                                    last_seen_change_kind: None,
                                    source_kind: &file.source_kind,
                                    edition_name: file.edition_name.as_deref(),
                                    quality_label: file.quality_label.as_deref(),
                                    container: &file.container,
                                    external_url: file.external_url.as_deref(),
                                    strm_target_kind: file.strm_target_kind.as_deref(),
                                },
                            )
                            .await?
                        } else if let Some(file) = episode_files.get(delta.relative_path.as_str()) {
                            self.apply_manifest_existing_file_in_transaction(
                                &mut transaction,
                                ManifestExistingFileUpdate {
                                    filesystem_entry_id,
                                    library_root_id: batch.library_root_id,
                                    relative_path: &delta.relative_path,
                                    base_fingerprint: expected_fingerprint,
                                    expected_missing,
                                    size,
                                    modified_at,
                                    inode,
                                    fingerprint,
                                    generation: batch.generation,
                                    last_seen_change_kind: None,
                                    source_kind: &file.source_kind,
                                    edition_name: file.edition_name.as_deref(),
                                    quality_label: file.quality_label.as_deref(),
                                    container: &file.container,
                                    external_url: file.external_url.as_deref(),
                                    strm_target_kind: file.strm_target_kind.as_deref(),
                                },
                            )
                            .await?
                        } else if let Some(file) =
                            unresolved_files.get(delta.relative_path.as_str())
                        {
                            self.apply_manifest_existing_file_in_transaction(
                                &mut transaction,
                                ManifestExistingFileUpdate {
                                    filesystem_entry_id,
                                    library_root_id: batch.library_root_id,
                                    relative_path: &delta.relative_path,
                                    base_fingerprint: expected_fingerprint,
                                    expected_missing,
                                    size,
                                    modified_at,
                                    inode,
                                    fingerprint,
                                    generation: batch.generation,
                                    last_seen_change_kind: None,
                                    source_kind: &file.source_kind,
                                    edition_name: None,
                                    quality_label: None,
                                    container: &file.container,
                                    external_url: file.external_url.as_deref(),
                                    strm_target_kind: file.strm_target_kind.as_deref(),
                                },
                            )
                            .await?
                        } else {
                            self.query(
                                "UPDATE filesystem_entries
                                 SET size = ?, modified_at = ?, inode = ?, fingerprint = ?,
                                     last_seen_generation = ?, is_missing = 0,
                                     updated_at = unixepoch()
                                 WHERE id = ? AND library_root_id = ? AND relative_path = ?
                                   AND entry_kind = 'FILE' AND is_missing = ?
                                   AND (fingerprint = ? OR (fingerprint IS NULL AND ? IS NULL))",
                            )
                            .bind(size)
                            .bind(modified_at)
                            .bind(inode)
                            .bind(fingerprint)
                            .bind(batch.generation)
                            .bind(filesystem_entry_id)
                            .bind(batch.library_root_id)
                            .bind(&delta.relative_path)
                            .bind(database_flag(expected_missing))
                            .bind(expected_fingerprint)
                            .bind(expected_fingerprint)
                            .execute(&mut *transaction)
                            .await
                            .map_err(|source| StorageError::Sqlx {
                                path: self.path.clone(),
                                source,
                            })?
                            .rows_affected()
                                == 1
                        };
                        if applied {
                            if movie_files.contains_key(delta.relative_path.as_str())
                                || episode_files.contains_key(delta.relative_path.as_str())
                                || unresolved_files.contains_key(delta.relative_path.as_str())
                            {
                                changed_paths.push(delta.relative_path.clone());
                            } else {
                                changed_sidecar_paths.push(delta.relative_path.clone());
                            }
                        }
                        applied
                    }
                    "REMOVE" => {
                        let Some(filesystem_entry_id) = expected_baseline else {
                            return Err(StorageError::Conflict(
                                "remove delta has no baseline entry".to_owned(),
                            ));
                        };
                        let updated = self
                            .query(
                                "UPDATE filesystem_entries
                                 SET is_missing = 1, updated_at = unixepoch()
                                 WHERE id = ? AND library_root_id = ? AND relative_path = ?
                                   AND entry_kind = 'FILE' AND is_missing = 0
                                   AND (fingerprint = ? OR (fingerprint IS NULL AND ? IS NULL))",
                            )
                            .bind(filesystem_entry_id)
                            .bind(batch.library_root_id)
                            .bind(&delta.relative_path)
                            .bind(expected_fingerprint)
                            .bind(expected_fingerprint)
                            .execute(&mut *transaction)
                            .await
                            .map_err(|source| StorageError::Sqlx {
                                path: self.path.clone(),
                                source,
                            })?
                            .rows_affected()
                            == 1;
                        if updated {
                            if batch.removed_media_paths.contains(&delta.relative_path) {
                                removed_media_paths.push(delta.relative_path.clone());
                                removed_media_entry_ids.push(filesystem_entry_id.to_owned());
                            }
                            if batch.removed_sidecar_paths.contains(&delta.relative_path) {
                                removed_sidecar_paths.push(delta.relative_path.clone());
                            }
                            result.removed_count = result.removed_count.saturating_add(1);
                        }
                        updated
                    }
                    _ => {
                        return Err(StorageError::Conflict(
                            "manifest delta has an unknown kind".to_owned(),
                        ));
                    }
                };
                if applied {
                    result.applied_count = result.applied_count.saturating_add(1);
                } else {
                    state = "CONFLICT";
                    error = Some("filesystem entry changed after manifest diff was computed");
                    result.conflict_count = result.conflict_count.saturating_add(1);
                }
            }

            delta_updates.push((delta.id.as_str(), state, error));
        }

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

    pub(crate) async fn restore_media_item(&self, item_id: &str) -> Result<(), StorageError> {
        self.query(
            "UPDATE media_items
             SET removed_at = NULL, updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(item_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }
}

#[cfg(test)]
#[path = "jobs/tests.rs"]
mod tests;
