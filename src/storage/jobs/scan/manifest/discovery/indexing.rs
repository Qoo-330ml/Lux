use super::*;

impl Database {
    pub(super) async fn apply_manifest_discovery_indexes_and_metadata<'a>(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Any>,
        chunk: &NewScanManifestDiscoveryChunk<'_>,
        workflow_version: i64,
        job_status: &str,
        cancel_requested: i64,
        library_id: &str,
        generation: &str,
        positive_indexes: &'a [&'a NewScanManifestPositiveIndex],
        entries: &'a [&'a NewScanManifestEntry],
        local_metadata_sequence_start: Option<i64>,
    ) -> Result<(ManifestDiscoveryPositiveIndexResult<'a>, bool), StorageError> {
        if !matches!(workflow_version, 2 | 3) && !positive_indexes.is_empty() {
            return Err(StorageError::Conflict(
                "legacy manifest cannot receive streamed positive indexes".to_owned(),
            ));
        }
        let mut positive_result = ManifestDiscoveryPositiveIndexResult::default();
        let positive_index_started = Instant::now();
        if matches!(workflow_version, 2 | 3) && !positive_indexes.is_empty() {
            if job_status != "RUNNING" || cancel_requested != 0 {
                return Err(StorageError::Conflict(
                    "streamed indexing requires an active manifest scan job".to_owned(),
                ));
            }
            positive_result = self
                .apply_scan_manifest_discovery_positive_indexes_in_transaction(
                    transaction,
                    ManifestDiscoveryPositiveIndexCommit {
                        job_id: chunk.job_id,
                        library_id,
                        library_root_id: chunk.library_root_id,
                        generation,
                        positives: positive_indexes,
                        observations: entries,
                    },
                )
                .await?;
        }
        record_manifest_storage_stage(
            "positive_index_apply",
            positive_index_started,
            positive_indexes.len(),
            positive_indexes.len(),
            0,
        );

        let local_metadata_batches_changed =
            workflow_version == 3 && !positive_result.local_metadata_refs.is_empty();
        if local_metadata_batches_changed {
            let local_metadata_started = Instant::now();
            let sequence_start = local_metadata_sequence_start.ok_or_else(|| {
                StorageError::Conflict(
                    "workflow 3 local metadata references require reserved batch sequences".into(),
                )
            })?;
            self.enqueue_manifest_local_metadata_refs_in_transaction(
                transaction,
                chunk.job_id,
                chunk.library_root_id,
                &positive_result.local_metadata_refs,
                sequence_start,
            )
            .await?;
            record_manifest_storage_stage(
                "local_metadata_outbox",
                local_metadata_started,
                positive_result.local_metadata_refs.len(),
                positive_result.local_metadata_refs.len(),
                0,
            );
        }

        Ok((positive_result, local_metadata_batches_changed))
    }
}
