use super::*;

impl Database {
    pub(super) async fn apply_reconciliation_batch_media_and_targets(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        batch: &ReconciliationBatchCommit<'_>,
        pending_paths: &std::collections::HashSet<String>,
    ) -> Result<(usize, bool), StorageError> {
        let movie_files = batch
            .movie_files
            .iter()
            .filter(|file| pending_paths.contains(&file.relative_path))
            .cloned()
            .collect::<Vec<_>>();
        let episode_files = batch
            .episode_files
            .iter()
            .filter(|file| pending_paths.contains(&file.relative_path))
            .cloned()
            .collect::<Vec<_>>();

        let mut created_items = 0_usize;
        created_items = created_items.saturating_add(
            self.insert_movie_files_batch_in_transaction(
                transaction,
                batch.library_id,
                batch.library_root_id,
                batch.generation,
                &movie_files,
            )
            .await?,
        );
        created_items = created_items.saturating_add(
            self.insert_episode_files_batch_in_transaction(
                transaction,
                batch.library_id,
                batch.library_root_id,
                batch.generation,
                &episode_files,
            )
            .await?,
        );

        let mut metadata_targets_changed = self
            .record_scan_job_targets_in_transaction(
                transaction,
                batch.job_id,
                batch.library_root_id,
                batch.new_paths,
                "NEW",
            )
            .await?;
        metadata_targets_changed |= self
            .record_scan_job_targets_in_transaction(
                transaction,
                batch.job_id,
                batch.library_root_id,
                batch.changed_paths,
                "CHANGED",
            )
            .await?;
        let sidecar_directories = prune_sidecar_directories(
            batch
                .sidecar_paths
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
        metadata_targets_changed |= self
            .record_scan_job_sidecar_targets_in_transaction(
                transaction,
                batch.job_id,
                batch.library_root_id,
                &sidecar_directories,
            )
            .await?;
        self.restore_filesystem_entries_batch_in_transaction(transaction, batch.seen_entry_ids)
            .await?;

        Ok((created_items, metadata_targets_changed))
    }
}
