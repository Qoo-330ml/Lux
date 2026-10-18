use super::prepare::ManifestDeltaBatchPreparation;
use super::*;

pub(super) struct ManifestDeltaApplyOutcome<'a> {
    pub(super) result: ManifestDeltaBatchCommitResult,
    pub(super) new_paths: Vec<String>,
    pub(super) changed_paths: Vec<String>,
    pub(super) removed_media_paths: Vec<String>,
    pub(super) changed_sidecar_paths: Vec<String>,
    pub(super) removed_sidecar_paths: Vec<String>,
    pub(super) removed_media_entry_ids: Vec<String>,
    pub(super) delta_updates: Vec<(&'a str, &'static str, Option<&'static str>)>,
}

impl Database {
    pub(super) async fn apply_manifest_delta_changes<'batch, 'data>(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Any>,
        batch: &'batch ManifestDeltaBatchCommit<'data>,
        preparation: ManifestDeltaBatchPreparation<'data>,
    ) -> Result<ManifestDeltaApplyOutcome<'data>, StorageError>
    where
        'data: 'batch,
    {
        let ManifestDeltaBatchPreparation {
            root_state,
            movie_files,
            episode_files,
            unresolved_files,
            sidecar_entries,
            unstable_ids,
            claimed_add_paths,
            mut result,
        } = preparation;
        let mut new_paths = Vec::new();
        let mut changed_paths = Vec::new();
        let mut removed_media_paths = Vec::new();
        let mut changed_sidecar_paths = Vec::new();
        let mut removed_sidecar_paths = Vec::new();
        let mut removed_media_entry_ids = Vec::new();
        let mut delta_updates = Vec::with_capacity(batch.deltas.len());
        let unresolved_files_to_materialize = batch
            .deltas
            .iter()
            .filter(|delta| {
                delta.delta_kind == "ADD"
                    && !unstable_ids.contains(delta.id.as_str())
                    && claimed_add_paths.contains(&delta.relative_path)
                    && root_state.as_deref() == Some("COMPLETE")
            })
            .filter_map(|delta| unresolved_files.get(delta.relative_path.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        self.materialize_manifest_unresolved_files_after_filesystem_insert_in_transaction(
            transaction,
            batch.library_id,
            batch.library_root_id,
            &unresolved_files_to_materialize,
        )
        .await?;
        result.created_items = result
            .created_items
            .saturating_add(unresolved_files_to_materialize.len());

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
                            delta.device,
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
                        let Some((entry_kind, _, _, _, _, _)) = observation else {
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
                            || unresolved_files.contains_key(delta.relative_path.as_str())
                        {
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
                        let Some((entry_kind, size, modified_at, device, inode, fingerprint)) =
                            observation
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
                                transaction,
                                ManifestExistingFileUpdate {
                                    filesystem_entry_id,
                                    library_root_id: batch.library_root_id,
                                    relative_path: &delta.relative_path,
                                    base_fingerprint: expected_fingerprint,
                                    expected_missing,
                                    size,
                                    modified_at,
                                    device,
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
                                transaction,
                                ManifestExistingFileUpdate {
                                    filesystem_entry_id,
                                    library_root_id: batch.library_root_id,
                                    relative_path: &delta.relative_path,
                                    base_fingerprint: expected_fingerprint,
                                    expected_missing,
                                    size,
                                    modified_at,
                                    device,
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
                                transaction,
                                ManifestExistingFileUpdate {
                                    filesystem_entry_id,
                                    library_root_id: batch.library_root_id,
                                    relative_path: &delta.relative_path,
                                    base_fingerprint: expected_fingerprint,
                                    expected_missing,
                                    size,
                                    modified_at,
                                    device,
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
                                SET size = ?, modified_at = ?, device = ?, inode = ?, fingerprint = ?,
                                     last_seen_generation = ?, is_missing = 0,
                                     updated_at = unixepoch()
                                 WHERE id = ? AND library_root_id = ? AND relative_path = ?
                                   AND entry_kind = 'FILE' AND is_missing = ?
                                   AND (fingerprint = ? OR (fingerprint IS NULL AND ? IS NULL))",
                            )
                            .bind(size)
                            .bind(modified_at)
                            .bind(device)
                            .bind(inode)
                            .bind(fingerprint)
                            .bind(batch.generation)
                            .bind(filesystem_entry_id)
                            .bind(batch.library_root_id)
                            .bind(&delta.relative_path)
                            .bind(database_flag(expected_missing))
                            .bind(expected_fingerprint)
                            .bind(expected_fingerprint)
                            .execute(&mut **transaction)
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
                            .execute(&mut **transaction)
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

        Ok(ManifestDeltaApplyOutcome {
            result,
            new_paths,
            changed_paths,
            removed_media_paths,
            changed_sidecar_paths,
            removed_sidecar_paths,
            removed_media_entry_ids,
            delta_updates,
        })
    }
}
