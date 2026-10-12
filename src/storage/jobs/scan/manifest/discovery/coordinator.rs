use super::*;
use super::{
    checkpoint::{ManifestDiscoveryCheckpointInput, ManifestDiscoverySequenceReservations},
    counters::ManifestDiscoveryCountersInput,
    indexing::ManifestDiscoveryIndexingInput,
    input::ManifestDiscoveryInput,
    state::ManifestDiscoveryStateSnapshot,
};

struct ManifestDiscoveryObservationInput<'slice, 'data> {
    chunk: &'slice NewScanManifestDiscoveryChunk<'data>,
    workflow_version: i64,
    discovery_format_version: i64,
    discovery_mode: &'slice str,
    lite_mode: bool,
    entries: &'slice [&'data NewScanManifestEntry],
    positive_index_count: usize,
    inserted_directory_count: u64,
    inserted_directory_count_i64: i64,
    inserted_file_count: usize,
    inserted_file_count_i64: i64,
    child_directory_count: usize,
    completed_directories: &'slice [&'data str],
    completed_directory_count: i64,
}

impl Database {
    #[allow(dead_code)] // Kept for focused storage contract tests.
    pub(crate) async fn commit_scan_manifest_discovery_chunk(
        &self,
        chunk: &NewScanManifestDiscoveryChunk<'_>,
    ) -> Result<i64, StorageError> {
        self.commit_scan_manifest_discovery_chunks(std::slice::from_ref(chunk))
            .await
            .map(|result| result.observed_file_count)
    }

    pub(crate) async fn commit_scan_manifest_discovery_chunks(
        &self,
        chunks: &[NewScanManifestDiscoveryChunk<'_>],
    ) -> Result<ManifestDiscoveryCommitResult, StorageError> {
        let input_validation_started = Instant::now();
        let Some(input) = ManifestDiscoveryInput::collect(chunks)? else {
            return Ok(ManifestDiscoveryCommitResult::default());
        };
        if input.child_directories.is_empty()
            && input.entries.is_empty()
            && input.completed_directories.is_empty()
        {
            return Ok(ManifestDiscoveryCommitResult::default());
        }

        let transaction_item_count = input
            .entries
            .len()
            .saturating_add(input.child_directories.len());
        let transaction_file_count = input
            .entries
            .iter()
            .filter(|entry| entry.entry_kind == "FILE")
            .count();
        let transaction_directory_count = input.child_directories.len();
        record_manifest_storage_stage(
            "transaction_input_validation",
            input_validation_started,
            transaction_item_count,
            transaction_file_count,
            transaction_directory_count,
        );

        let transaction_total_started = Instant::now();
        let transaction_begin_started = Instant::now();
        let mut transaction = self.begin_scan_write_transaction().await?;
        record_manifest_storage_stage(
            "transaction_begin",
            transaction_begin_started,
            transaction_item_count,
            transaction_file_count,
            transaction_directory_count,
        );

        let result = self
            .commit_manifest_discovery_transaction(&mut transaction, input)
            .await?;
        let transaction_commit_started = Instant::now();
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        record_manifest_storage_stage(
            "transaction_commit",
            transaction_commit_started,
            transaction_item_count,
            transaction_file_count,
            transaction_directory_count,
        );
        record_manifest_storage_stage(
            "transaction_total",
            transaction_total_started,
            transaction_item_count,
            transaction_file_count,
            transaction_directory_count,
        );
        Ok(result)
    }

    async fn commit_manifest_discovery_transaction<'slice, 'data>(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Any>,
        input: ManifestDiscoveryInput<'slice, 'data>,
    ) -> Result<ManifestDiscoveryCommitResult, StorageError>
    where
        'data: 'slice,
    {
        let ManifestDiscoveryInput {
            chunk,
            child_directories,
            entries,
            completed_directories,
            positive_indexes,
            unchanged_paths,
            seen_filesystem_entries,
        } = input;
        let (state, inserted_directory_count) = self
            .load_state_and_insert_frontier(transaction, chunk, &child_directories)
            .await?;
        let ManifestDiscoveryStateSnapshot {
            workflow_version,
            discovery_format_version,
            discovery_mode,
            library_id,
            generation,
            job_status,
            cancel_requested,
            lite_mode,
        } = state;

        let file_counts = self
            .query_manifest_discovery_file_paths(
                transaction,
                chunk,
                discovery_format_version,
                &generation,
                &entries,
                &unchanged_paths,
            )
            .await?;
        let inserted_file_count = file_counts.inserted_file_count;
        let unchanged_file_count = file_counts.unchanged_file_count;
        let inserted_file_count_i64 = i64::try_from(inserted_file_count)
            .map_err(|_| StorageError::Conflict("manifest file count overflow".to_owned()))?;
        let inserted_directory_count_i64 = i64::try_from(inserted_directory_count)
            .map_err(|_| StorageError::Conflict("manifest directory count overflow".to_owned()))?;

        let completed_directory_count = self
            .complete_and_record_manifest_frontier(
                transaction,
                chunk,
                &completed_directories,
                lite_mode,
            )
            .await?;

        let reservations = self
            .checkpoint_and_insert_manifest_observations(
                transaction,
                ManifestDiscoveryObservationInput {
                    chunk,
                    workflow_version,
                    discovery_format_version,
                    discovery_mode: &discovery_mode,
                    lite_mode,
                    entries: &entries,
                    positive_index_count: positive_indexes.len(),
                    inserted_directory_count,
                    inserted_directory_count_i64,
                    inserted_file_count,
                    inserted_file_count_i64,
                    child_directory_count: child_directories.len(),
                    completed_directories: &completed_directories,
                    completed_directory_count,
                },
            )
            .await?;

        let (mut positive_result, local_metadata_batches_changed) = self
            .apply_manifest_discovery_indexes_and_metadata(
                transaction,
                chunk,
                ManifestDiscoveryIndexingInput {
                    workflow_version,
                    job_status: &job_status,
                    cancel_requested,
                    library_id: &library_id,
                    generation: &generation,
                    positive_indexes: &positive_indexes,
                    entries: &entries,
                    local_metadata_sequence_start: reservations.local_metadata_sequence_start,
                },
            )
            .await?;

        let presence_ledger_started = Instant::now();
        let presence_ledger_path_count = self
            .update_manifest_presence_ledger(
                transaction,
                chunk,
                discovery_format_version,
                &generation,
                &seen_filesystem_entries,
                &file_counts.file_paths,
                &mut positive_result.indexed_paths,
            )
            .await?;
        record_manifest_storage_stage(
            "presence_ledger",
            presence_ledger_started,
            presence_ledger_path_count,
            presence_ledger_path_count,
            0,
        );

        let manifest_counters_started = Instant::now();
        self.checkpoint_manifest_discovery_counters(
            transaction,
            ManifestDiscoveryCountersInput {
                chunk,
                workflow_version,
                inserted_directory_count: inserted_directory_count_i64,
                inserted_file_count: inserted_file_count_i64,
                completed_directory_count,
                unchanged_file_count,
                add_count: positive_result.add_count,
                change_count: positive_result.change_count,
                reappeared_count: positive_result.reappeared_count,
                applied_count: positive_result.applied_count,
            },
        )
        .await?;
        record_manifest_storage_stage(
            "manifest_counter_checkpoint",
            manifest_counters_started,
            inserted_file_count
                .saturating_add(completed_directories.len())
                .saturating_add(positive_indexes.len()),
            inserted_file_count,
            completed_directories.len(),
        );

        Ok(ManifestDiscoveryCommitResult {
            observed_file_count: i64::try_from(inserted_file_count)
                .map_err(|_| StorageError::Conflict("manifest file count overflow".to_owned()))?,
            created_items: positive_result.created_items,
            metadata_targets_changed: positive_result.metadata_targets_changed,
            local_metadata_batches_changed,
        })
    }

    async fn load_state_and_insert_frontier(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Any>,
        chunk: &NewScanManifestDiscoveryChunk<'_>,
        child_directories: &[&str],
    ) -> Result<(ManifestDiscoveryStateSnapshot, u64), StorageError> {
        let manifest_state_started = Instant::now();
        let state = self
            .load_manifest_discovery_state_snapshot(transaction, chunk)
            .await?;
        record_manifest_storage_stage("manifest_state_check", manifest_state_started, 1, 0, 0);

        let directory_frontier_started = Instant::now();
        let inserted_directory_count = self
            .insert_manifest_directory_frontier(
                transaction,
                chunk,
                child_directories,
                state.lite_mode,
            )
            .await?;
        record_manifest_storage_stage(
            "directory_frontier_insert",
            directory_frontier_started,
            child_directories.len(),
            0,
            child_directories.len(),
        );

        Ok((state, inserted_directory_count))
    }

    async fn complete_and_record_manifest_frontier(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Any>,
        chunk: &NewScanManifestDiscoveryChunk<'_>,
        completed_directories: &[&str],
        lite_mode: bool,
    ) -> Result<i64, StorageError> {
        let started = Instant::now();
        let completed_directory_count = self
            .complete_manifest_directory_frontier(
                transaction,
                chunk,
                completed_directories,
                lite_mode,
            )
            .await?;
        record_manifest_storage_stage(
            "directory_frontier_completion",
            started,
            completed_directories.len(),
            0,
            completed_directories.len(),
        );
        Ok(completed_directory_count)
    }

    async fn checkpoint_and_insert_manifest_observations<'slice, 'data>(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Any>,
        input: ManifestDiscoveryObservationInput<'slice, 'data>,
    ) -> Result<ManifestDiscoverySequenceReservations, StorageError>
    where
        'data: 'slice,
    {
        let root_checkpoint_started = Instant::now();
        let observation_entries = if input.discovery_format_version == 3 {
            input
                .entries
                .iter()
                .copied()
                .filter(|entry| {
                    entry.entry_kind == "DIRECTORY"
                        && (!input.lite_mode || entry.relative_path.is_empty())
                })
                .collect::<Vec<_>>()
        } else {
            input.entries.to_vec()
        };
        let reservations = self
            .checkpoint_manifest_discovery_root(
                transaction,
                ManifestDiscoveryCheckpointInput {
                    chunk: input.chunk,
                    workflow_version: input.workflow_version,
                    discovery_mode: input.discovery_mode,
                    lite_mode: input.lite_mode,
                    observation_entry_count: observation_entries.len(),
                    positive_index_count: input.positive_index_count,
                    inserted_directory_count_i64: input.inserted_directory_count_i64,
                    inserted_file_count: input.inserted_file_count_i64,
                    child_directory_count: input.child_directory_count,
                    completed_directory_count: input.completed_directory_count,
                    completed_directory_input_count: input.completed_directories.len(),
                },
            )
            .await?;
        record_manifest_storage_stage(
            "root_checkpoint",
            root_checkpoint_started,
            input
                .inserted_directory_count
                .try_into()
                .unwrap_or(usize::MAX)
                .saturating_add(input.completed_directories.len())
                .saturating_add(input.inserted_file_count),
            input.inserted_file_count,
            input.completed_directories.len(),
        );

        let observation_batch_size = if matches!(input.workflow_version, 2 | 3) {
            super::super::manifest_path_query_chunk_size(self.backend())
        } else {
            80
        };
        let observation_started = Instant::now();
        self.insert_manifest_discovery_observations(
            transaction,
            input.chunk,
            &observation_entries,
            observation_batch_size,
            reservations.observation_sequence_start,
        )
        .await?;
        record_manifest_storage_stage(
            "manifest_observation_insert",
            observation_started,
            observation_entries.len(),
            observation_entries
                .iter()
                .filter(|entry| entry.entry_kind == "FILE")
                .count(),
            observation_entries
                .iter()
                .filter(|entry| entry.entry_kind == "DIRECTORY")
                .count(),
        );

        Ok(reservations)
    }
}
