use super::*;

#[path = "state/transactions.rs"]
mod transactions;

fn valid_scan_manifest_transition(expected: &str, next: &str) -> bool {
    matches!(
        (expected, next),
        ("DISCOVERING", "READY_TO_DIFF" | "FAILED" | "CANCELLED")
            | ("READY_TO_DIFF", "APPLYING" | "FAILED" | "CANCELLED")
            | ("APPLYING", "INDEXED" | "FAILED" | "CANCELLED")
            | ("INDEXED", "POSTPROCESSING" | "COMPLETED" | "FAILED")
            | ("POSTPROCESSING", "COMPLETED" | "FAILED" | "CANCELLED")
    )
}

fn validate_scan_manifest(manifest: &NewScanManifest<'_>) -> Result<i64, StorageError> {
    let root_count = i64::try_from(manifest.roots.len())
        .map_err(|_| StorageError::Conflict("manifest root count overflow".to_owned()))?;
    let mut unique_roots = std::collections::HashSet::with_capacity(manifest.roots.len());
    if manifest
        .roots
        .iter()
        .any(|root| !unique_roots.insert(root.library_root_id))
    {
        return Err(StorageError::Conflict(
            "manifest root list contains duplicates".to_owned(),
        ));
    }
    Ok(root_count)
}

impl Database {
    #[cfg(test)]
    pub(crate) async fn create_scan_manifest(
        &self,
        manifest: &NewScanManifest<'_>,
    ) -> Result<(), StorageError> {
        let root_count = validate_scan_manifest(manifest)?;

        let mut transaction = self.begin_scan_write_transaction().await?;
        let existing: Option<(String, String)> = self
            .query_as("SELECT id, library_id FROM scan_manifests WHERE job_id = ?")
            .bind(manifest.job_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if let Some((existing_id, existing_library_id)) = existing {
            if existing_id != manifest.id || existing_library_id != manifest.library_id {
                return Err(StorageError::Conflict(
                    "scan job is already associated with a different manifest".to_owned(),
                ));
            }
            transaction
                .commit()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(());
        }

        let created = self
            .query(
                "INSERT INTO scan_manifests (id, job_id, library_id, state, root_count)
                 SELECT ?, sj.id, sj.library_id, 'DISCOVERING', ?
                 FROM scan_jobs sj
                 WHERE sj.id = ? AND sj.library_id = ?
                   AND sj.job_type = 'RECONCILE_LIBRARY'
                   AND sj.status IN ('PENDING', 'RUNNING')",
            )
            .bind(manifest.id)
            .bind(root_count)
            .bind(manifest.job_id)
            .bind(manifest.library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if created.rows_affected() != 1 {
            return Err(StorageError::Conflict(
                "manifest must reference an existing full-scan job in the same library".to_owned(),
            ));
        }

        for root in manifest.roots {
            let created_root = self
                .query(
                    "INSERT INTO scan_manifest_roots (
                         manifest_id, library_root_id, state, directory_count
                     )
                     SELECT ?, lr.id, 'PENDING', 1
                     FROM library_roots lr
                     WHERE lr.id = ? AND lr.library_id = ?",
                )
                .bind(manifest.id)
                .bind(root.library_root_id)
                .bind(manifest.library_id)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if created_root.rows_affected() != 1 {
                return Err(StorageError::Conflict(format!(
                    "library root {} does not belong to the manifest library",
                    root.library_root_id
                )));
            }
            self.query(
                "INSERT INTO scan_manifest_directories (
                     manifest_id, library_root_id, relative_path, state
                 ) VALUES (?, ?, '', 'PENDING')",
            )
            .bind(manifest.id)
            .bind(root.library_root_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }

        self.query(
            "UPDATE scan_manifests
             SET discovered_directory_count = ?, updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(root_count)
        .bind(manifest.id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    #[allow(dead_code)] // LUX-266 uses this to resume and report manifest progress.
    pub(crate) async fn get_scan_manifest(
        &self,
        id: &str,
    ) -> Result<Option<StoredScanManifest>, StorageError> {
        self.query(
            "SELECT id, job_id, library_id, state, workflow_version, discovery_format_version,
                    discovery_mode,
                    root_count,
                    discovered_directory_count, completed_directory_count,
                    observed_file_count, unchanged_count, add_count, change_count, remove_count,
                    reappeared_count, applied_delta_count, postprocessing_targets_ready
             FROM scan_manifests WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredScanManifest {
                id: row.get("id"),
                job_id: row.get("job_id"),
                library_id: row.get("library_id"),
                state: row.get("state"),
                workflow_version: row.get("workflow_version"),
                discovery_format_version: row.get("discovery_format_version"),
                discovery_mode: row.get("discovery_mode"),
                root_count: row.get("root_count"),
                discovered_directory_count: row.get("discovered_directory_count"),
                completed_directory_count: row.get("completed_directory_count"),
                observed_file_count: row.get("observed_file_count"),
                unchanged_count: row.get("unchanged_count"),
                add_count: row.get("add_count"),
                change_count: row.get("change_count"),
                remove_count: row.get("remove_count"),
                reappeared_count: row.get("reappeared_count"),
                applied_delta_count: row.get("applied_delta_count"),
                postprocessing_targets_ready: row.get::<i64, _>("postprocessing_targets_ready")
                    != 0,
            })
        })
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn get_scan_manifest_by_job(
        &self,
        job_id: &str,
    ) -> Result<Option<StoredScanManifest>, StorageError> {
        self.query(
            "SELECT id, job_id, library_id, state, workflow_version, discovery_format_version,
                    discovery_mode,
                    root_count,
                    discovered_directory_count, completed_directory_count,
                    observed_file_count, unchanged_count, add_count, change_count, remove_count,
                    reappeared_count, applied_delta_count, postprocessing_targets_ready
             FROM scan_manifests WHERE job_id = ?",
        )
        .bind(job_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredScanManifest {
                id: row.get("id"),
                job_id: row.get("job_id"),
                library_id: row.get("library_id"),
                state: row.get("state"),
                workflow_version: row.get("workflow_version"),
                discovery_format_version: row.get("discovery_format_version"),
                discovery_mode: row.get("discovery_mode"),
                root_count: row.get("root_count"),
                discovered_directory_count: row.get("discovered_directory_count"),
                completed_directory_count: row.get("completed_directory_count"),
                observed_file_count: row.get("observed_file_count"),
                unchanged_count: row.get("unchanged_count"),
                add_count: row.get("add_count"),
                change_count: row.get("change_count"),
                remove_count: row.get("remove_count"),
                reappeared_count: row.get("reappeared_count"),
                applied_delta_count: row.get("applied_delta_count"),
                postprocessing_targets_ready: row.get::<i64, _>("postprocessing_targets_ready")
                    != 0,
            })
        })
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn get_scan_manifest_postprocessing_state_by_job(
        &self,
        job_id: &str,
    ) -> Result<Option<StoredScanManifestPostprocessingState>, StorageError> {
        let rows = self
            .query(
                "SELECT manifest.id AS manifest_id,
                        manifest.workflow_version,
                        manifest.discovery_format_version,
                        root.library_root_id,
                        library_root.canonical_path,
                        root.postprocessing_target_stage,
                        root.postprocessing_target_cursor,
                        observed.device,
                        observed.inode,
                        CASE WHEN EXISTS (
                            SELECT 1 FROM filesystem_entries entry
                            WHERE entry.library_root_id = root.library_root_id
                              AND entry.last_seen_generation = job.generation
                              AND entry.entry_kind = 'FILE'
                              AND entry.last_seen_change_kind = root.postprocessing_target_stage
                              AND entry.relative_path > COALESCE(root.postprocessing_target_cursor, '')
                        ) THEN 1 ELSE 0 END AS has_stage_rows,
                        CASE WHEN EXISTS (
                            SELECT 1 FROM filesystem_entries entry
                            WHERE entry.library_root_id = root.library_root_id
                              AND entry.last_seen_generation = job.generation
                              AND entry.entry_kind = 'FILE'
                              AND entry.last_seen_change_kind IN ('NEW', 'CHANGED', 'SIDECAR')
                        ) THEN 1 ELSE 0 END AS has_positive_rows
                 FROM scan_manifests manifest
                 JOIN scan_jobs job ON job.id = manifest.job_id
                 LEFT JOIN scan_manifest_roots root ON root.manifest_id = manifest.id
                 LEFT JOIN library_roots library_root ON library_root.id = root.library_root_id
                 LEFT JOIN scan_manifest_entries observed
                   ON observed.manifest_id = root.manifest_id
                  AND observed.library_root_id = root.library_root_id
                  AND observed.relative_path = ''
                  AND observed.observation_sequence = (
                      SELECT MAX(latest.observation_sequence)
                      FROM scan_manifest_entries latest
                      WHERE latest.manifest_id = root.manifest_id
                        AND latest.library_root_id = root.library_root_id
                        AND latest.relative_path = ''
                  )
                 WHERE manifest.job_id = ?
                 ORDER BY root.library_root_id",
            )
            .bind(job_id)
            .fetch_all(&self.pool)
            .await
            .map_err(self.sqlx_err())?;

        let mut state: Option<StoredScanManifestPostprocessingState> = None;
        for row in rows {
            let Some(manifest_id) = row.get::<Option<String>, _>("manifest_id") else {
                continue;
            };
            let current_state =
                state.get_or_insert_with(|| StoredScanManifestPostprocessingState {
                    manifest_id,
                    workflow_version: row.get("workflow_version"),
                    discovery_format_version: row.get("discovery_format_version"),
                    roots: Vec::new(),
                });
            let Some(library_root_id) = row.get::<Option<String>, _>("library_root_id") else {
                continue;
            };
            current_state
                .roots
                .push(StoredScanManifestPostprocessingRoot {
                    library_root_id,
                    canonical_path: row.get("canonical_path"),
                    expected_device: row.get("device"),
                    expected_inode: row.get("inode"),
                    target_stage: row.get("postprocessing_target_stage"),
                    target_cursor: row.get("postprocessing_target_cursor"),
                    has_stage_rows: row.get::<i64, _>("has_stage_rows") != 0,
                    has_positive_rows: row.get::<i64, _>("has_positive_rows") != 0,
                });
        }
        Ok(state)
    }

    pub(crate) async fn list_scan_manifest_postprocessing_roots(
        &self,
        manifest_id: &str,
    ) -> Result<Vec<StoredScanManifestPostprocessingRoot>, StorageError> {
        self.query(
            "SELECT root.library_root_id, library_root.canonical_path,
                    root.postprocessing_target_stage, root.postprocessing_target_cursor,
                    observed.device, observed.inode,
                    CASE WHEN EXISTS (
                        SELECT 1 FROM filesystem_entries entry
                        WHERE entry.library_root_id = root.library_root_id
                          AND entry.last_seen_generation = job.generation
                          AND entry.entry_kind = 'FILE'
                          AND entry.last_seen_change_kind = root.postprocessing_target_stage
                          AND entry.relative_path > COALESCE(root.postprocessing_target_cursor, '')
                    ) THEN 1 ELSE 0 END AS has_stage_rows,
                    CASE WHEN EXISTS (
                        SELECT 1 FROM filesystem_entries entry
                        JOIN scan_jobs job ON job.id = manifest.job_id
                        WHERE entry.library_root_id = root.library_root_id
                          AND entry.last_seen_generation = job.generation
                          AND entry.entry_kind = 'FILE'
                          AND entry.last_seen_change_kind IN ('NEW', 'CHANGED', 'SIDECAR')
                    ) THEN 1 ELSE 0 END AS has_positive_rows
             FROM scan_manifest_roots root
             JOIN scan_manifests manifest ON manifest.id = root.manifest_id
             JOIN scan_jobs job ON job.id = manifest.job_id
             JOIN library_roots library_root ON library_root.id = root.library_root_id
             LEFT JOIN scan_manifest_entries observed
               ON observed.manifest_id = root.manifest_id
              AND observed.library_root_id = root.library_root_id
              AND observed.relative_path = ''
              AND observed.observation_sequence = (
                  SELECT MAX(latest.observation_sequence)
                  FROM scan_manifest_entries latest
                  WHERE latest.manifest_id = root.manifest_id
                    AND latest.library_root_id = root.library_root_id
                    AND latest.relative_path = ''
              )
             WHERE root.manifest_id = ?
             ORDER BY root.library_root_id",
        )
        .bind(manifest_id)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredScanManifestPostprocessingRoot {
                    library_root_id: row.get("library_root_id"),
                    canonical_path: row.get("canonical_path"),
                    target_stage: row.get("postprocessing_target_stage"),
                    target_cursor: row.get("postprocessing_target_cursor"),
                    expected_device: row.get("device"),
                    expected_inode: row.get("inode"),
                    has_stage_rows: row.get::<i64, _>("has_stage_rows") != 0,
                    has_positive_rows: row.get::<i64, _>("has_positive_rows") != 0,
                })
                .collect()
        })
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn mark_scan_manifest_root_unavailable(
        &self,
        manifest_id: &str,
        library_root_id: &str,
    ) -> Result<(), StorageError> {
        let mut transaction = self.begin_scan_write_transaction().await?;
        self.query(
            "UPDATE scan_manifest_roots
             SET state = 'UNAVAILABLE', error = 'filesystem root could not be read',
                 finished_at = unixepoch(), updated_at = unixepoch()
             WHERE manifest_id = ? AND library_root_id = ?
               AND state IN ('PENDING', 'SCANNING', 'COMPLETE', 'INCOMPLETE')",
        )
        .bind(manifest_id)
        .bind(library_root_id)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;
        self.query(
            "UPDATE library_roots
             SET is_available = 0, last_checked_at = unixepoch(),
                 unavailable_since = COALESCE(unavailable_since, unixepoch())
             WHERE id = ?",
        )
        .bind(library_root_id)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;
        self.query(
            "UPDATE scan_manifest_directories
             SET state = 'FAILED', error = 'filesystem root could not be read',
                 updated_at = unixepoch()
             WHERE manifest_id = ? AND library_root_id = ? AND state = 'PENDING'",
        )
        .bind(manifest_id)
        .bind(library_root_id)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;
        transaction.commit().await.map_err(self.sqlx_err())
    }

    /// Project link provenance from this manifest's observations onto the
    /// canonical filesystem rows.
    ///
    /// Discovery runs in batches and cannot dedupe across directories, so the
    /// link marker is carried by the observation table and copied here once the
    /// apply pass has settled the rows. Rows that stopped being links are reset,
    /// otherwise a link replaced by a real file would keep a stale target.
    pub(crate) async fn project_scan_manifest_symlink_origin(
        &self,
        manifest_id: &str,
    ) -> Result<(), StorageError> {
        // COUNT keeps the probe backend-neutral: SQLite returns INTEGER for
        // EXISTS while PostgreSQL returns BOOLEAN.
        let link_observations: i64 = self
            .query_scalar(
                "SELECT COUNT(*) FROM scan_manifest_entries
                 WHERE manifest_id = ? AND entry_kind = 'FILE' AND is_symlink = 1",
            )
            .bind(manifest_id)
            .fetch_one(self.pool())
            .await
            .map_err(self.sqlx_err())?;
        if link_observations == 0 {
            return Ok(());
        }
        let mut transaction = self.begin_scan_write_transaction().await?;
        self.query(
            "UPDATE filesystem_entries
             SET is_symlink = 1,
                 resolved_path = (
                     SELECT observation.resolved_path
                     FROM scan_manifest_entries AS observation
                     WHERE observation.manifest_id = ?
                       AND observation.library_root_id = filesystem_entries.library_root_id
                       AND observation.relative_path = filesystem_entries.relative_path
                       AND observation.entry_kind = 'FILE'
                       AND observation.is_symlink = 1
                     ORDER BY observation.observation_sequence DESC
                     LIMIT 1
                 )
             WHERE EXISTS (
                 SELECT 1
                 FROM scan_manifest_entries AS observation
                 WHERE observation.manifest_id = ?
                   AND observation.library_root_id = filesystem_entries.library_root_id
                   AND observation.relative_path = filesystem_entries.relative_path
                   AND observation.entry_kind = 'FILE'
                   AND observation.is_symlink = 1
             )",
        )
        .bind(manifest_id)
        .bind(manifest_id)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;
        self.query(
            "UPDATE filesystem_entries
             SET is_symlink = 0, resolved_path = NULL
             WHERE is_symlink = 1
               AND EXISTS (
                   SELECT 1
                   FROM scan_manifest_entries AS observation
                   WHERE observation.manifest_id = ?
                     AND observation.library_root_id = filesystem_entries.library_root_id
                     AND observation.relative_path = filesystem_entries.relative_path
                     AND observation.entry_kind = 'FILE'
               )
               AND NOT EXISTS (
                   SELECT 1
                   FROM scan_manifest_entries AS observation
                   WHERE observation.manifest_id = ?
                     AND observation.library_root_id = filesystem_entries.library_root_id
                     AND observation.relative_path = filesystem_entries.relative_path
                     AND observation.entry_kind = 'FILE'
                     AND observation.is_symlink = 1
               )",
        )
        .bind(manifest_id)
        .bind(manifest_id)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;
        transaction.commit().await.map_err(self.sqlx_err())
    }

    /// Mark the settled filesystem rows that discovery observed through a link.
    ///
    /// Called once per job after indexing, with the identities collected during
    /// discovery. Matching on `(device, inode, relative_path)` keeps it precise
    /// even when the same bytes are reachable through several paths.
    pub(crate) async fn mark_filesystem_entry_symlink_origins(
        &self,
        origins: &[(i64, i64, String, String)],
    ) -> Result<u64, StorageError> {
        if origins.is_empty() {
            return Ok(0);
        }
        let mut transaction = self.begin_scan_write_transaction().await?;
        let mut updated = 0_u64;
        for (device, inode, relative_path, resolved_path) in origins {
            updated = updated.saturating_add(
                self.query(
                    "UPDATE filesystem_entries
                     SET is_symlink = 1, resolved_path = ?
                     WHERE device = ? AND inode = ? AND relative_path = ?
                       AND entry_kind = 'FILE'",
                )
                .bind(resolved_path)
                .bind(device)
                .bind(inode)
                .bind(relative_path)
                .execute(&mut *transaction)
                .await
                .map_err(self.sqlx_err())?
                .rows_affected(),
            );
            // The same bytes may also be reachable through another path that the
            // scan happened to settle first, in a different directory or batch.
            // One identity keeps one entry, and the link path is the one the user
            // organised the library around.
            updated = updated.saturating_add(
                self.query(
                    "UPDATE filesystem_entries
                     SET is_missing = 1
                     WHERE device = ? AND inode = ? AND relative_path <> ?
                       AND entry_kind = 'FILE' AND is_missing = 0",
                )
                .bind(device)
                .bind(inode)
                .bind(relative_path)
                .execute(&mut *transaction)
                .await
                .map_err(self.sqlx_err())?
                .rows_affected(),
            );
        }
        transaction.commit().await.map_err(self.sqlx_err())?;
        Ok(updated)
    }

    pub(crate) async fn get_scan_manifest_root_identity(
        &self,
        manifest_id: &str,
        library_root_id: &str,
    ) -> Result<Option<(String, Option<i64>, Option<i64>)>, StorageError> {
        self.query_as(
            "SELECT root.state, observed.device, observed.inode
             FROM scan_manifest_roots root
             LEFT JOIN scan_manifest_entries observed
               ON observed.manifest_id = root.manifest_id
              AND observed.library_root_id = root.library_root_id
              AND observed.relative_path = ''
              AND observed.observation_sequence = (
                  SELECT MAX(latest.observation_sequence)
                  FROM scan_manifest_entries latest
                  WHERE latest.manifest_id = root.manifest_id
                    AND latest.library_root_id = root.library_root_id
                    AND latest.relative_path = ''
              )
             WHERE root.manifest_id = ? AND root.library_root_id = ?",
        )
        .bind(manifest_id)
        .bind(library_root_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn list_scan_manifest_root_discovery_baselines(
        &self,
        manifest_id: &str,
    ) -> Result<Vec<(String, String, bool)>, StorageError> {
        self.query_as(
            "SELECT root.library_root_id, root.state,
                    CAST(CASE
                    WHEN root.state IN ('COMPLETE', 'UNAVAILABLE') THEN 0
                    WHEN EXISTS (
                        SELECT 1 FROM filesystem_entries entry
                        WHERE entry.library_root_id = root.library_root_id
                    ) THEN 1 ELSE 0 END AS BIGINT) AS has_filesystem_entries
             FROM scan_manifest_roots root
             WHERE root.manifest_id = ?
             ORDER BY root.library_root_id",
        )
        .bind(manifest_id)
        .fetch_all(&self.pool)
        .await
        .map(|rows: Vec<(String, String, i64)>| {
            rows.into_iter()
                .map(|(root_id, state, has_filesystem_entries)| {
                    (root_id, state, has_filesystem_entries != 0)
                })
                .collect()
        })
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn finish_lite_scan_manifest_roots(
        &self,
        manifest_id: &str,
    ) -> Result<(), StorageError> {
        let mut transaction = self.begin_scan_write_transaction().await?;
        self.query(
            "UPDATE scan_manifest_roots
             SET state = 'COMPLETE', finished_at = COALESCE(finished_at, unixepoch()),
                 updated_at = unixepoch()
             WHERE manifest_id = ? AND state = 'SCANNING'",
        )
        .bind(manifest_id)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;
        self.query(
            "UPDATE scan_manifest_directories
             SET state = 'COMPLETE', error = NULL, updated_at = unixepoch()
             WHERE manifest_id = ? AND relative_path = '' AND state <> 'COMPLETE'",
        )
        .bind(manifest_id)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;
        transaction.commit().await.map_err(self.sqlx_err())
    }

    pub(crate) async fn finish_scan_manifest_discovery(
        &self,
        manifest_id: &str,
        job_id: &str,
    ) -> Result<i64, StorageError> {
        let mut transaction = self.begin_scan_write_transaction().await?;
        let pending_directories: i64 = self
            .query_scalar(
                "SELECT COUNT(*) FROM scan_manifest_directories
                 WHERE manifest_id = ? AND state = 'PENDING'",
            )
            .bind(manifest_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        let pending_roots: i64 = self
            .query_scalar(
                "SELECT COUNT(*) FROM scan_manifest_roots
                 WHERE manifest_id = ? AND state IN ('PENDING', 'SCANNING', 'INCOMPLETE')",
            )
            .bind(manifest_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        if pending_directories != 0 || pending_roots != 0 {
            return Err(StorageError::Conflict(
                "manifest discovery cannot complete while frontier or roots remain unresolved"
                    .to_owned(),
            ));
        }
        let discovered_file_count: i64 = self
            .query_scalar("SELECT observed_file_count FROM scan_manifests WHERE id = ?")
            .bind(manifest_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        let processed_count: i64 = self
            .query_scalar("SELECT processed_count FROM scan_jobs WHERE id = ?")
            .bind(job_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        let total_count = discovered_file_count.max(processed_count);
        let job_update = self
            .query(
                "UPDATE scan_jobs
             SET discovery_completed = 1, total_count = ?, updated_at = unixepoch()
             WHERE id = ? AND status = 'RUNNING' AND cancel_requested = 0",
            )
            .bind(total_count)
            .bind(job_id)
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        if job_update.rows_affected() != 1 {
            return Err(StorageError::Conflict(
                "scan job is no longer accepting manifest discovery completion".to_owned(),
            ));
        }
        let manifest_update = self
            .query(
                "UPDATE scan_manifests
             SET state = 'READY_TO_DIFF', updated_at = unixepoch()
             WHERE id = ? AND state = 'DISCOVERING'",
            )
            .bind(manifest_id)
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        if manifest_update.rows_affected() != 1 {
            return Err(StorageError::Conflict(
                "manifest is no longer accepting discovery completion".to_owned(),
            ));
        }
        transaction.commit().await.map_err(self.sqlx_err())?;
        Ok(total_count)
    }

    pub(crate) async fn finish_scan_manifest(
        &self,
        job_id: &str,
        next_state: &str,
    ) -> Result<(), StorageError> {
        if !matches!(next_state, "FAILED" | "CANCELLED") {
            return Err(StorageError::Conflict(
                "invalid manifest terminal state".to_owned(),
            ));
        }
        let mut transaction = self.begin_scan_write_transaction().await?;
        self.query(
            "UPDATE scan_manifests
             SET resume_state = state, state = ?, updated_at = unixepoch()
             WHERE job_id = ? AND state IN (
                 'DISCOVERING', 'READY_TO_DIFF', 'APPLYING', 'INDEXED', 'POSTPROCESSING'
             )",
        )
        .bind(next_state)
        .bind(job_id)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;
        self.query(
            "UPDATE scan_manifest_roots
             SET state = 'INCOMPLETE', error = 'scan did not complete this root',
                 finished_at = unixepoch(), updated_at = unixepoch()
             WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)
               AND state IN ('PENDING', 'SCANNING')",
        )
        .bind(job_id)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;
        transaction.commit().await.map_err(self.sqlx_err())
    }

    #[allow(dead_code)] // Lifecycle owners use this compare-and-swap in LUX-266 onward.
    pub(crate) async fn transition_scan_manifest_state(
        &self,
        id: &str,
        expected_state: &str,
        next_state: &str,
    ) -> Result<bool, StorageError> {
        if expected_state == next_state {
            return Ok(false);
        }
        if !valid_scan_manifest_transition(expected_state, next_state) {
            return Err(StorageError::Conflict(
                "invalid scan manifest state transition".to_owned(),
            ));
        }
        let mut transaction = self.begin_scan_write_transaction().await?;
        let result = self
            .query(
                "UPDATE scan_manifests
                 SET state = ?, updated_at = unixepoch(),
                     indexed_at = CASE WHEN ? = 'INDEXED'
                         THEN COALESCE(indexed_at, unixepoch()) ELSE indexed_at END,
                     completed_at = CASE WHEN ? = 'COMPLETED'
                         THEN COALESCE(completed_at, unixepoch()) ELSE completed_at END
                 WHERE id = ? AND state = ?",
            )
            .bind(next_state)
            .bind(next_state)
            .bind(next_state)
            .bind(id)
            .bind(expected_state)
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        transaction.commit().await.map_err(self.sqlx_err())?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn has_unready_manifest_target_materialization_for_library(
        &self,
        library_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS (
                 SELECT 1
                 FROM scan_jobs job
                 JOIN scan_manifests manifest ON manifest.job_id = job.id
                 WHERE job.library_id = ? AND job.job_type = 'RECONCILE_LIBRARY'
                   AND job.status = 'COMPLETED' AND job.scan_phase = 'POSTPROCESSING'
                   AND manifest.workflow_version IN (2, 3)
                   AND manifest.discovery_format_version = 3
                   AND manifest.postprocessing_targets_ready = 0
             ) THEN 1 ELSE 0 END",
        )
        .bind(library_id)
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(self.sqlx_err())
    }
}
