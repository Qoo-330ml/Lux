use super::*;

pub(super) struct ManifestDeltaBatchPreparation<'a> {
    pub(super) root_state: Option<String>,
    pub(super) movie_files: HashMap<&'a str, &'a NewMovieFile>,
    pub(super) episode_files: HashMap<&'a str, &'a NewEpisodeFile>,
    pub(super) unresolved_files: HashMap<&'a str, &'a NewScanManifestUnresolvedFile>,
    pub(super) sidecar_entries: HashMap<&'a str, &'a NewScanManifestSidecarEntry>,
    pub(super) unstable_ids: std::collections::HashSet<&'a str>,
    pub(super) claimed_add_paths: std::collections::HashSet<String>,
    pub(super) result: ManifestDeltaBatchCommitResult,
}

impl Database {
    pub(super) async fn prepare_manifest_delta_batch<'a>(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Any>,
        batch: &ManifestDeltaBatchCommit<'a>,
    ) -> Result<ManifestDeltaBatchPreparation<'a>, StorageError> {
        let active_job: i64 = self
            .query_scalar(
                "SELECT COUNT(*) FROM scan_jobs
                 WHERE id = ? AND status = 'RUNNING' AND cancel_requested = 0",
            )
            .bind(batch.job_id)
            .fetch_one(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let manifest_state: Option<String> = self
            .query_scalar("SELECT state FROM scan_manifests WHERE id = ? AND job_id = ?")
            .bind(batch.manifest_id)
            .bind(batch.job_id)
            .fetch_optional(&mut **transaction)
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
            .fetch_optional(&mut **transaction)
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
                    device: delta.device,
                    inode: delta.inode,
                    fingerprint,
                    last_seen_change_kind: None,
                });
            }
        }
        let claimed_add_paths = self
            .claim_manifest_add_filesystem_entries_in_transaction(
                transaction,
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
                transaction,
                batch.library_id,
                batch.library_root_id,
                batch.generation,
                &claimed_movie_files,
            )
            .await?;
        result.created_items = result.created_items.saturating_add(
            self.insert_episode_files_without_filesystem_entries_in_transaction(
                transaction,
                batch.library_id,
                batch.library_root_id,
                batch.generation,
                &claimed_episode_files,
            )
            .await?,
        );

        Ok(ManifestDeltaBatchPreparation {
            root_state,
            movie_files,
            episode_files,
            unresolved_files,
            sidecar_entries,
            unstable_ids,
            claimed_add_paths,
            result,
        })
    }
}
