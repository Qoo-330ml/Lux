use super::*;

impl Database {
    pub(super) async fn count_manifest_diff_remaining_changes(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Any>,
        manifest_id: &str,
        workflow_version: i64,
        discovery_format_version: i64,
        discovery_mode: &str,
    ) -> Result<i64, StorageError> {
        let remaining_changes: i64 = if is_lite_manifest_discovery(
            workflow_version,
            discovery_format_version,
            discovery_mode,
        ) {
            self.query_scalar(
                "SELECT COUNT(*)
                 FROM filesystem_entries fe
                 JOIN scan_manifest_roots root
                   ON root.manifest_id = ?
                  AND root.library_root_id = fe.library_root_id
                  AND root.state = 'COMPLETE'
                 JOIN scan_manifests manifest ON manifest.id = root.manifest_id
                 JOIN scan_jobs job ON job.id = manifest.job_id
                 WHERE fe.entry_kind = 'FILE' AND fe.is_missing = 0
                   AND COALESCE(fe.last_seen_generation, '') <> job.generation
                   AND NOT EXISTS (
                       SELECT 1 FROM scan_manifest_seen_paths seen
                       WHERE seen.manifest_id = root.manifest_id
                         AND seen.library_root_id = fe.library_root_id
                         AND seen.relative_path = fe.relative_path
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM scan_manifest_deltas delta
                       WHERE delta.manifest_id = root.manifest_id
                         AND delta.library_root_id = fe.library_root_id
                         AND delta.relative_path = fe.relative_path
                   )",
            )
            .bind(manifest_id)
            .fetch_one(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
        } else if matches!(workflow_version, 2 | 3) && discovery_format_version == 3 {
            self.query_scalar(
                "SELECT COUNT(*)
                 FROM filesystem_entries fe
                 JOIN scan_manifest_roots root
                   ON root.manifest_id = ?
                  AND root.library_root_id = fe.library_root_id
                  AND root.state = 'COMPLETE'
                 JOIN scan_manifests manifest ON manifest.id = root.manifest_id
                 JOIN scan_jobs job ON job.id = manifest.job_id
                 WHERE fe.entry_kind = 'FILE' AND fe.is_missing = 0
                   AND EXISTS (
                       SELECT 1
                       FROM scan_manifest_directories directory
                       WHERE directory.manifest_id = root.manifest_id
                         AND directory.library_root_id = fe.library_root_id
                         AND directory.state = 'COMPLETE'
                         AND (
                             (directory.relative_path = ''
                              AND fe.relative_path NOT LIKE '%/%' ESCAPE '\\')
                             OR (directory.relative_path <> ''
                                 AND fe.relative_path LIKE (
                                     REPLACE(
                                         REPLACE(
                                             REPLACE(directory.relative_path, '\\', '\\\\'),
                                             '%',
                                             '\\%'
                                         ),
                                         '_',
                                         '\\_'
                                     ) || '/%'
                                 ) ESCAPE '\\'
                                 AND fe.relative_path NOT LIKE (
                                     REPLACE(
                                         REPLACE(
                                             REPLACE(directory.relative_path, '\\', '\\\\'),
                                             '%',
                                             '\\%'
                                         ),
                                         '_',
                                         '\\_'
                                     ) || '/%/%'
                                 ) ESCAPE '\\')
                         )
                   )
                   AND COALESCE(fe.last_seen_generation, '') <> job.generation
                   AND NOT EXISTS (
                       SELECT 1 FROM scan_manifest_seen_paths seen
                       WHERE seen.manifest_id = ?
                         AND seen.library_root_id = fe.library_root_id
                         AND seen.relative_path = fe.relative_path
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM scan_manifest_deltas delta
                       WHERE delta.manifest_id = ?
                         AND delta.library_root_id = fe.library_root_id
                         AND delta.relative_path = fe.relative_path
                   )",
            )
            .bind(manifest_id)
            .bind(manifest_id)
            .bind(manifest_id)
            .fetch_one(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
        } else if matches!(workflow_version, 2 | 3) {
            self.query_scalar(
                "SELECT COUNT(*)
                 FROM filesystem_entries fe
                 JOIN scan_manifest_roots root
                   ON root.manifest_id = ?
                  AND root.library_root_id = fe.library_root_id
                  AND root.state = 'COMPLETE'
                 WHERE fe.entry_kind = 'FILE' AND fe.is_missing = 0
                   AND COALESCE((
                       SELECT observed.entry_kind
                       FROM scan_manifest_entries observed
                       WHERE observed.manifest_id = ?
                         AND observed.library_root_id = fe.library_root_id
                         AND observed.relative_path = fe.relative_path
                       ORDER BY observed.observation_sequence DESC LIMIT 1
                   ), '') <> 'FILE'
                   AND NOT EXISTS (
                       SELECT 1 FROM scan_manifest_deltas delta
                       WHERE delta.manifest_id = ?
                         AND delta.library_root_id = fe.library_root_id
                         AND delta.relative_path = fe.relative_path
                   )",
            )
            .bind(manifest_id)
            .bind(manifest_id)
            .bind(manifest_id)
            .fetch_one(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
        } else {
            self.query_scalar(
                "SELECT
                    (SELECT COUNT(*)
                     FROM scan_manifest_entries observed
                     JOIN scan_manifest_roots root
                       ON root.manifest_id = observed.manifest_id
                      AND root.library_root_id = observed.library_root_id
                      AND root.state = 'COMPLETE'
                     LEFT JOIN filesystem_entries fe
                       ON fe.library_root_id = observed.library_root_id
                      AND fe.relative_path = observed.relative_path
                     WHERE observed.manifest_id = ?
                       AND observed.entry_kind = 'FILE'
                       AND observed.observation_sequence = (
                           SELECT latest.observation_sequence
                           FROM scan_manifest_entries latest
                           WHERE latest.manifest_id = observed.manifest_id
                             AND latest.library_root_id = observed.library_root_id
                             AND latest.relative_path = observed.relative_path
                           ORDER BY latest.observation_sequence DESC
                           LIMIT 1
                       )
                       AND (fe.id IS NULL OR fe.is_missing = 1 OR fe.fingerprint IS NULL
                            OR observed.fingerprint IS NULL OR fe.fingerprint <> observed.fingerprint)
                       AND NOT EXISTS (
                           SELECT 1 FROM scan_manifest_deltas delta
                           WHERE delta.manifest_id = observed.manifest_id
                             AND delta.library_root_id = observed.library_root_id
                             AND delta.relative_path = observed.relative_path
                       ))
                    +
                    (SELECT COUNT(*)
                     FROM filesystem_entries fe
                     JOIN scan_manifest_roots root
                       ON root.manifest_id = ?
                      AND root.library_root_id = fe.library_root_id
                      AND root.state = 'COMPLETE'
                     WHERE fe.entry_kind = 'FILE' AND fe.is_missing = 0
                       AND COALESCE((
                           SELECT observed.entry_kind
                           FROM scan_manifest_entries observed
                           WHERE observed.manifest_id = ?
                             AND observed.library_root_id = fe.library_root_id
                             AND observed.relative_path = fe.relative_path
                           ORDER BY observed.observation_sequence DESC
                           LIMIT 1
                       ), '') <> 'FILE'
                       AND NOT EXISTS (
                           SELECT 1 FROM scan_manifest_deltas delta
                           WHERE delta.manifest_id = ?
                             AND delta.library_root_id = fe.library_root_id
                             AND delta.relative_path = fe.relative_path
                       ))",
            )
            .bind(manifest_id)
            .bind(manifest_id)
            .bind(manifest_id)
            .bind(manifest_id)
            .fetch_one(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
        };
        Ok(remaining_changes)
    }

    pub(super) async fn count_manifest_diff_unchanged_entries(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Any>,
        manifest_id: &str,
        workflow_version: i64,
    ) -> Result<i64, StorageError> {
        let unchanged_count: i64 = if matches!(workflow_version, 2 | 3) {
            self.query_scalar("SELECT unchanged_count FROM scan_manifests WHERE id = ?")
                .bind(manifest_id)
                .fetch_one(&mut **transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
        } else {
            self.query_scalar(
                "SELECT COUNT(*)
                 FROM scan_manifest_entries observed
                 JOIN scan_manifest_roots root
                   ON root.manifest_id = observed.manifest_id
                  AND root.library_root_id = observed.library_root_id
                  AND root.state = 'COMPLETE'
                 JOIN filesystem_entries fe
                   ON fe.library_root_id = observed.library_root_id
                  AND fe.relative_path = observed.relative_path
                  AND fe.entry_kind = 'FILE'
                  AND fe.is_missing = 0
                  AND fe.fingerprint = observed.fingerprint
                 WHERE observed.manifest_id = ?
                   AND observed.entry_kind = 'FILE'
                   AND observed.observation_sequence = (
                       SELECT latest.observation_sequence
                       FROM scan_manifest_entries latest
                       WHERE latest.manifest_id = observed.manifest_id
                         AND latest.library_root_id = observed.library_root_id
                         AND latest.relative_path = observed.relative_path
                       ORDER BY latest.observation_sequence DESC
                       LIMIT 1
                   )",
            )
            .bind(manifest_id)
            .fetch_one(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
        };
        Ok(unchanged_count)
    }
}
