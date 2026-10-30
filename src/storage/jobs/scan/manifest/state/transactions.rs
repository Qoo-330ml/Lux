use super::*;

const SCAN_MANIFEST_ROOT_BATCH_SIZE: usize = 100;

impl Database {
    pub(crate) async fn create_full_scan_manifest_job(
        &self,
        id: &str,
        generation: &str,
        auto_metadata_match: bool,
        manifest: &NewScanManifest<'_>,
        legacy_retry_job_id: Option<&str>,
    ) -> Result<(), StorageError> {
        if manifest.job_id != id {
            return Err(StorageError::Conflict(
                "manifest job id does not match the scan job".to_owned(),
            ));
        }
        let root_count = validate_scan_manifest(manifest)?;
        let mut transaction = self.begin_scan_write_transaction().await?;
        if !self
            .lock_scan_fingerprint_migration_state(&mut transaction, manifest.library_id)
            .await?
        {
            return Err(StorageError::Conflict(
                "scan fingerprint migration lease is active".to_owned(),
            ));
        }
        self.query(
            "INSERT INTO scan_jobs (
                id, library_id, job_type, status, generation, total_count,
                discovery_completed, auto_metadata_match
             ) VALUES (?, ?, 'RECONCILE_LIBRARY', 'PENDING', ?, 0, 0, ?)",
        )
        .bind(id)
        .bind(manifest.library_id)
        .bind(generation)
        .bind(database_flag(auto_metadata_match))
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;
        let created_manifest = self
            .query(
                "INSERT INTO scan_manifests (
                     id, job_id, library_id, state, workflow_version,
                     discovery_format_version, discovery_mode, root_count,
                     postprocessing_targets_ready
                 )
                 SELECT ?, sj.id, sj.library_id, 'DISCOVERING', 3, 3, 'LITE', ?, 0
                 FROM scan_jobs sj
                 WHERE sj.id = ? AND sj.library_id = ?
                   AND sj.job_type = 'RECONCILE_LIBRARY' AND sj.status = 'PENDING'",
            )
            .bind(manifest.id)
            .bind(root_count)
            .bind(id)
            .bind(manifest.library_id)
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        if created_manifest.rows_affected() != 1 {
            return Err(StorageError::Conflict(
                "manifest must reference its new full-scan job".to_owned(),
            ));
        }
        self.initialize_full_scan_manifest_roots(&mut transaction, manifest, root_count)
            .await?;
        self.copy_legacy_scan_manifest_targets(&mut transaction, id, legacy_retry_job_id)
            .await?;
        transaction.commit().await.map_err(self.sqlx_err())
    }

    async fn initialize_full_scan_manifest_roots(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        manifest: &NewScanManifest<'_>,
        root_count: i64,
    ) -> Result<(), StorageError> {
        for chunk in manifest.roots.chunks(SCAN_MANIFEST_ROOT_BATCH_SIZE) {
            let values = std::iter::repeat_n("(?)", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let mut insert = self.query(sqlx::AssertSqlSafe(format!(
                "WITH requested(library_root_id) AS (VALUES {values})
                 INSERT INTO scan_manifest_roots (
                     manifest_id, library_root_id, state, directory_count,
                     postprocessing_target_stage
                 )
                 SELECT ?, requested.library_root_id, 'PENDING', 1, 'NEW'
                 FROM requested
                 JOIN library_roots lr ON lr.id = requested.library_root_id
                 WHERE lr.library_id = ?"
            )));
            for root in chunk {
                insert = insert.bind(root.library_root_id);
            }
            let inserted = insert
                .bind(manifest.id)
                .bind(manifest.library_id)
                .execute(&mut **transaction)
                .await
                .map_err(self.sqlx_err())?;
            if usize::try_from(inserted.rows_affected()).unwrap_or(usize::MAX) != chunk.len() {
                let placeholders = std::iter::repeat_n("?", chunk.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut valid_query = self.query(sqlx::AssertSqlSafe(format!(
                    "SELECT id FROM library_roots
                     WHERE library_id = ? AND id IN ({placeholders})"
                )));
                valid_query = valid_query.bind(manifest.library_id);
                for root in chunk {
                    valid_query = valid_query.bind(root.library_root_id);
                }
                let valid = valid_query
                    .fetch_all(&mut **transaction)
                    .await
                    .map_err(self.sqlx_err())?
                    .into_iter()
                    .map(|row| row.get::<String, _>("id"))
                    .collect::<HashSet<_>>();
                let missing = chunk
                    .iter()
                    .find(|root| !valid.contains(root.library_root_id))
                    .map(|root| root.library_root_id)
                    .unwrap_or("unknown");
                return Err(StorageError::Conflict(format!(
                    "library root {missing} does not belong to the manifest library"
                )));
            }
        }
        for chunk in manifest.roots.chunks(SCAN_MANIFEST_ROOT_BATCH_SIZE) {
            let values = std::iter::repeat_n("(?, ?, '', 'PENDING')", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let mut insert = self.query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO scan_manifest_directories (
                     manifest_id, library_root_id, relative_path, state
                 ) VALUES {values}"
            )));
            for root in chunk {
                insert = insert.bind(manifest.id).bind(root.library_root_id);
            }
            insert
                .execute(&mut **transaction)
                .await
                .map_err(self.sqlx_err())?;
        }
        self.query(
            "UPDATE scan_manifests
             SET discovered_directory_count = ?, updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(root_count)
        .bind(manifest.id)
        .execute(&mut **transaction)
        .await
        .map_err(self.sqlx_err())?;
        Ok(())
    }

    async fn copy_legacy_scan_manifest_targets(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        id: &str,
        legacy_retry_job_id: Option<&str>,
    ) -> Result<(), StorageError> {
        if let Some(legacy_job_id) = legacy_retry_job_id {
            self.query(
                "INSERT INTO scan_job_targets (
                     job_id, target_type, target_id, source_id, item_id, change_kind,
                     probe_state, metadata_state, thumbnail_state
                 )
                 SELECT ?, target_type, target_id, source_id, item_id, change_kind,
                        CASE WHEN probe_state IN ('PENDING', 'FAILED')
                             THEN 'PENDING' ELSE probe_state END,
                        CASE WHEN metadata_state IN ('PENDING', 'FAILED')
                             THEN 'PENDING' ELSE metadata_state END,
                        CASE WHEN thumbnail_state IN ('PENDING', 'FAILED')
                             THEN 'PENDING' ELSE thumbnail_state END
                 FROM scan_job_targets
                 WHERE job_id = ?
                   AND (probe_state IN ('PENDING', 'FAILED')
                        OR metadata_state IN ('PENDING', 'FAILED')
                        OR thumbnail_state IN ('PENDING', 'FAILED'))
                 ON CONFLICT(job_id, target_type, target_id) DO NOTHING",
            )
            .bind(id)
            .bind(legacy_job_id)
            .execute(&mut **transaction)
            .await
            .map_err(self.sqlx_err())?;
            self.query(
                "INSERT INTO scan_job_targets (
                     job_id, target_type, target_id, source_id, item_id, change_kind,
                     probe_state, metadata_state, thumbnail_state
                 )
                 SELECT ?, 'SOURCE', source.id, source.id, source.item_id, 'CHANGED',
                        'PENDING', 'SKIPPED', 'SKIPPED'
                 FROM reconciliation_scan_entries legacy
                 JOIN filesystem_entries entry
                   ON entry.library_root_id = legacy.library_root_id
                  AND entry.relative_path = legacy.relative_path
                 JOIN media_sources source ON source.filesystem_entry_id = entry.id
                 WHERE legacy.job_id = ? AND legacy.entry_type = 'FILE'
                   AND legacy.status = 'PENDING' AND entry.is_missing = 0
                 ON CONFLICT(job_id, target_type, target_id) DO NOTHING",
            )
            .bind(id)
            .bind(legacy_job_id)
            .execute(&mut **transaction)
            .await
            .map_err(self.sqlx_err())?;
            self.query(
                "INSERT INTO scan_job_targets (
                     job_id, target_type, target_id, item_id, change_kind,
                     probe_state, metadata_state, thumbnail_state
                 )
                 SELECT ?, 'ITEM', source.item_id, source.item_id, 'CHANGED',
                        'SKIPPED', 'PENDING', 'PENDING'
                 FROM reconciliation_scan_entries legacy
                 JOIN filesystem_entries entry
                   ON entry.library_root_id = legacy.library_root_id
                  AND entry.relative_path = legacy.relative_path
                 JOIN media_sources source ON source.filesystem_entry_id = entry.id
                 WHERE legacy.job_id = ? AND legacy.entry_type = 'FILE'
                   AND legacy.status = 'PENDING' AND entry.is_missing = 0
                 GROUP BY source.item_id
                 ON CONFLICT(job_id, target_type, target_id) DO NOTHING",
            )
            .bind(id)
            .bind(legacy_job_id)
            .execute(&mut **transaction)
            .await
            .map_err(self.sqlx_err())?;
            self.query("DELETE FROM scan_job_targets WHERE job_id = ?")
                .bind(legacy_job_id)
                .execute(&mut **transaction)
                .await
                .map_err(self.sqlx_err())?;
            self.query("DELETE FROM reconciliation_scan_entries WHERE job_id = ?")
                .bind(legacy_job_id)
                .execute(&mut **transaction)
                .await
                .map_err(self.sqlx_err())?;
        }
        Ok(())
    }
    pub(crate) async fn prepare_scan_manifest_retry(
        &self,
        job_id: &str,
    ) -> Result<Option<String>, StorageError> {
        let mut transaction = self.begin_scan_write_transaction().await?;
        let manifest: Option<(String, Option<String>, i64, i64, String)> = self
            .query_as(
                "SELECT state, resume_state, workflow_version,
                        discovery_format_version, discovery_mode
                 FROM scan_manifests WHERE job_id = ?",
            )
            .bind(job_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        let Some((state, resume_state, workflow_version, discovery_format_version, discovery_mode)) =
            manifest
        else {
            transaction.commit().await.map_err(self.sqlx_err())?;
            return Ok(None);
        };

        let stage = match state.as_str() {
            "DISCOVERING" | "READY_TO_DIFF" | "APPLYING" | "INDEXED" | "POSTPROCESSING" => {
                state.clone()
            }
            "FAILED" | "CANCELLED" => match resume_state.as_deref() {
                Some(
                    stage @ ("DISCOVERING" | "READY_TO_DIFF" | "APPLYING" | "INDEXED"
                    | "POSTPROCESSING"),
                ) => {
                    let restored = self
                        .query(
                            "UPDATE scan_manifests
                             SET state = ?, resume_state = NULL, error = NULL,
                                 updated_at = unixepoch()
                             WHERE job_id = ? AND state = ? AND resume_state = ?",
                        )
                        .bind(stage)
                        .bind(job_id)
                        .bind(&state)
                        .bind(stage)
                        .execute(&mut *transaction)
                        .await
                        .map_err(self.sqlx_err())?;
                    if restored.rows_affected() != 1 {
                        return Err(StorageError::Conflict(
                            "scan manifest checkpoint changed while preparing retry".to_owned(),
                        ));
                    }
                    stage.to_owned()
                }
                _ => "RESET_REQUIRED".to_owned(),
            },
            "COMPLETED" => "COMPLETED".to_owned(),
            _ => {
                return Err(StorageError::Conflict(
                    "scan manifest has an unsupported retry state".to_owned(),
                ));
            }
        };

        self.reset_manifest_discovery_for_retry(
            &mut transaction,
            job_id,
            &stage,
            workflow_version,
            discovery_format_version,
            &discovery_mode,
        )
        .await?;
        transaction.commit().await.map_err(self.sqlx_err())?;
        Ok(Some(stage))
    }

    async fn reset_manifest_discovery_for_retry(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        job_id: &str,
        stage: &str,
        workflow_version: i64,
        discovery_format_version: i64,
        discovery_mode: &str,
    ) -> Result<(), StorageError> {
        if stage == "DISCOVERING" {
            if is_lite_manifest_discovery(
                workflow_version,
                discovery_format_version,
                discovery_mode,
            ) {
                self.query(
                    "UPDATE scan_manifest_directories
                     SET state = 'PENDING', error = NULL, updated_at = unixepoch()
                     WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)
                       AND relative_path = ''",
                )
                .bind(job_id)
                .execute(&mut **transaction)
                .await
                .map_err(self.sqlx_err())?;
                self.query(
                    "UPDATE scan_manifest_roots
                     SET state = 'PENDING', error = NULL, finished_at = NULL,
                         updated_at = unixepoch()
                     WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)
                       AND state IN ('PENDING', 'SCANNING', 'INCOMPLETE')",
                )
                .bind(job_id)
                .execute(&mut **transaction)
                .await
                .map_err(self.sqlx_err())?;
            } else {
                self.query(
                    "UPDATE scan_manifest_directories
                 SET state = 'PENDING', error = NULL, updated_at = unixepoch()
                 WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)
                   AND state IN ('SCANNING', 'FAILED')",
                )
                .bind(job_id)
                .execute(&mut **transaction)
                .await
                .map_err(self.sqlx_err())?;
                self.query(
                    "UPDATE scan_manifest_roots
                 SET state = CASE WHEN EXISTS (
                         SELECT 1 FROM scan_manifest_directories directory
                         WHERE directory.manifest_id = scan_manifest_roots.manifest_id
                           AND directory.library_root_id = scan_manifest_roots.library_root_id
                           AND directory.state = 'PENDING'
                     ) THEN 'SCANNING' ELSE 'COMPLETE' END,
                     error = NULL, finished_at = NULL, updated_at = unixepoch()
                 WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)
                   AND state IN ('PENDING', 'SCANNING', 'INCOMPLETE')",
                )
                .bind(job_id)
                .execute(&mut **transaction)
                .await
                .map_err(self.sqlx_err())?;
            }
        }

        Ok(())
    }
}
