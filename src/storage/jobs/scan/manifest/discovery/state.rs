use super::*;

pub(super) struct ManifestDiscoveryStateSnapshot {
    pub(super) workflow_version: i64,
    pub(super) discovery_format_version: i64,
    pub(super) discovery_mode: String,
    pub(super) library_id: String,
    pub(super) generation: String,
    pub(super) job_status: String,
    pub(super) cancel_requested: i64,
    pub(super) lite_mode: bool,
}

impl Database {
    pub(super) async fn load_manifest_discovery_state_snapshot(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Any>,
        chunk: &NewScanManifestDiscoveryChunk<'_>,
    ) -> Result<ManifestDiscoveryStateSnapshot, StorageError> {
        let (
            workflow_version,
            discovery_format_version,
            discovery_mode,
            library_id,
            generation,
            job_status,
            cancel_requested,
        ): (i64, i64, String, String, String, String, i64) = self
            .query_as(
                "SELECT manifest.workflow_version, manifest.discovery_format_version,
                        manifest.discovery_mode, manifest.library_id, job.generation,
                        job.status, job.cancel_requested
                 FROM scan_manifests manifest
                 JOIN scan_jobs job ON job.id = manifest.job_id
                 WHERE manifest.id = ? AND manifest.state = 'DISCOVERING'
                   AND job.id = ?",
            )
            .bind(chunk.manifest_id)
            .bind(chunk.job_id)
            .fetch_one(&mut **transaction)
            .await
            .map_err(self.sqlx_err())?;

        let lite_mode =
            is_lite_manifest_discovery(workflow_version, discovery_format_version, &discovery_mode);

        Ok(ManifestDiscoveryStateSnapshot {
            workflow_version,
            discovery_format_version,
            discovery_mode,
            library_id,
            generation,
            job_status,
            cancel_requested,
            lite_mode,
        })
    }
}
