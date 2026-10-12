use super::*;

pub(super) struct ManifestDiscoveryCountersInput<'a> {
    pub(super) chunk: &'a NewScanManifestDiscoveryChunk<'a>,
    pub(super) workflow_version: i64,
    pub(super) inserted_directory_count: i64,
    pub(super) inserted_file_count: i64,
    pub(super) completed_directory_count: i64,
    pub(super) unchanged_file_count: usize,
    pub(super) add_count: i64,
    pub(super) change_count: i64,
    pub(super) reappeared_count: i64,
    pub(super) applied_count: i64,
}

impl Database {
    pub(super) async fn checkpoint_manifest_discovery_counters(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Any>,
        input: ManifestDiscoveryCountersInput<'_>,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE scan_manifests
             SET discovered_directory_count = discovered_directory_count + ?,
                 observed_file_count = observed_file_count + ?,
                 completed_directory_count = completed_directory_count + ?,
                 unchanged_count = unchanged_count + ?, add_count = add_count + ?,
                 change_count = change_count + ?, reappeared_count = reappeared_count + ?,
                 applied_delta_count = applied_delta_count + ?,
                 updated_at = unixepoch()
             WHERE id = ? AND state = 'DISCOVERING'",
        )
        .bind(input.inserted_directory_count)
        .bind(input.inserted_file_count)
        .bind(input.completed_directory_count)
        .bind(
            i64::try_from(input.unchanged_file_count).map_err(|_| {
                StorageError::Conflict("manifest unchanged count overflow".to_owned())
            })?,
        )
        .bind(input.add_count)
        .bind(input.change_count)
        .bind(input.reappeared_count)
        .bind(input.applied_count)
        .bind(input.chunk.manifest_id)
        .execute(&mut **transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;

        let accepting_discovery = self
            .query(
                "UPDATE scan_jobs
                 SET total_count = total_count + ?,
                     processed_count = processed_count + ?, updated_at = unixepoch()
                 WHERE id = ? AND status = 'RUNNING' AND cancel_requested = 0",
            )
            .bind(input.inserted_file_count)
            .bind(if matches!(input.workflow_version, 2 | 3) {
                input.inserted_file_count
            } else {
                0
            })
            .bind(input.chunk.job_id)
            .execute(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if accepting_discovery.rows_affected() != 1 {
            return Err(StorageError::Conflict(
                "scan job is no longer accepting manifest discovery chunks".to_owned(),
            ));
        }

        Ok(())
    }
}
