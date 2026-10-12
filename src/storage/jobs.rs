use super::*;

#[path = "jobs/local_metadata.rs"]
mod local_metadata;
use local_metadata::MAX_SCAN_LOCAL_METADATA_BATCH_SOURCES;
pub(crate) use local_metadata::StoredScanLocalMetadataBackfillPage;
#[path = "jobs/metadata_reidentify.rs"]
mod metadata_reidentify;
use local_metadata::escape_sql_like_pattern;
#[path = "jobs/scan.rs"]
mod scan;
use scan::prune_sidecar_directories;
#[cfg(test)]
use scan::sidecar_target_query;
#[path = "jobs/shutdown.rs"]
mod shutdown;

#[path = "jobs/strm_probe.rs"]
mod strm_probe;

#[cfg(test)]
pub(crate) use super::media_sources::MAX_MEDIA_SOURCE_DELETE_BATCH_SIZE;
use std::{collections::HashMap, time::Instant};

const SCAN_MANIFEST_DIFF_TRANSACTION_BATCH_SIZE: usize = 500;
const MAX_SCAN_MANIFEST_APPLY_BATCH_SIZE: i64 = 500;

impl Database {
    pub(crate) const LEGACY_SCAN_REQUIRES_NEW_MANIFEST: &'static str =
        "LEGACY_SCAN_REQUIRES_NEW_MANIFEST";
}

impl Database {
    pub(crate) async fn list_scan_manifest_directories(
        &self,
        manifest_id: &str,
        limit: i64,
    ) -> Result<Vec<StoredScanManifestDirectory>, StorageError> {
        self.query(
            "SELECT library_root_id, relative_path
             FROM scan_manifest_directories
             WHERE manifest_id = ? AND state = 'PENDING'
             ORDER BY library_root_id, relative_path
             LIMIT ?",
        )
        .bind(manifest_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredScanManifestDirectory {
                    library_root_id: row.get("library_root_id"),
                    relative_path: row.get("relative_path"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_scan_manifest_complete_directories(
        &self,
        manifest_id: &str,
        after_library_root_id: Option<&str>,
        after_relative_path: Option<&str>,
        limit: i64,
    ) -> Result<Vec<StoredScanManifestDirectory>, StorageError> {
        self.query(
            "SELECT directory.library_root_id, directory.relative_path
             FROM scan_manifest_directories directory
             JOIN scan_manifest_roots root
               ON root.manifest_id = directory.manifest_id
              AND root.library_root_id = directory.library_root_id
              AND root.state = 'COMPLETE'
             WHERE directory.manifest_id = ?
               AND directory.state = 'COMPLETE'
               AND (directory.library_root_id, directory.relative_path) > (?, ?)
             ORDER BY directory.library_root_id, directory.relative_path
             LIMIT ?",
        )
        .bind(manifest_id)
        .bind(after_library_root_id.unwrap_or_default())
        .bind(after_relative_path.unwrap_or_default())
        .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredScanManifestDirectory {
                    library_root_id: row.get("library_root_id"),
                    relative_path: row.get("relative_path"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn scan_manifest_has_uncovered_files(
        &self,
        manifest_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS (
                 SELECT 1
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
                   )
                   AND NOT EXISTS (
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
             ) THEN 1 ELSE 0 END",
        )
        .bind(manifest_id)
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_scan_manifest_removal_candidates_for_directories(
        &self,
        manifest_id: &str,
        directories: &[StoredScanManifestDirectory],
        limit: i64,
    ) -> Result<Vec<StoredScanManifestRemovalCandidate>, StorageError> {
        if directories.is_empty() {
            return Ok(Vec::new());
        }
        let predicates = directories
            .iter()
            .map(|directory| {
                if directory.relative_path.is_empty() {
                    "(fe.library_root_id = ? AND fe.relative_path NOT LIKE '%/%' ESCAPE '\\')"
                        .to_owned()
                } else {
                    "(fe.library_root_id = ?
                      AND fe.relative_path LIKE ? ESCAPE '\\'
                      AND fe.relative_path NOT LIKE ? ESCAPE '\\')"
                        .to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join(" OR ");
        let query = format!(
            "SELECT fe.library_root_id, fe.relative_path,
                    fe.id AS base_filesystem_entry_id,
                    fe.fingerprint AS base_fingerprint
             FROM filesystem_entries fe
             JOIN scan_manifest_roots root
              ON root.manifest_id = ?
              AND root.library_root_id = fe.library_root_id
              AND root.state = 'COMPLETE'
             JOIN scan_manifests manifest ON manifest.id = root.manifest_id
             JOIN scan_jobs job ON job.id = manifest.job_id
             WHERE fe.entry_kind = 'FILE' AND fe.is_missing = 0
               AND ({predicates})
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
               )
             ORDER BY fe.library_root_id, fe.relative_path
             LIMIT ?"
        );
        let mut statement = self.query(sqlx::AssertSqlSafe(query)).bind(manifest_id);
        for directory in directories {
            statement = statement.bind(&directory.library_root_id);
            if !directory.relative_path.is_empty() {
                let escaped = escape_sql_like_pattern(&directory.relative_path);
                statement = statement
                    .bind(format!("{escaped}/%"))
                    .bind(format!("{escaped}/%/%"));
            }
        }
        statement = statement
            .bind(manifest_id)
            .bind(manifest_id)
            .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE));
        statement
            .fetch_all(&self.pool)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(|row| StoredScanManifestRemovalCandidate {
                        library_root_id: row.get("library_root_id"),
                        relative_path: row.get("relative_path"),
                        base_filesystem_entry_id: row.get("base_filesystem_entry_id"),
                        base_fingerprint: row.get("base_fingerprint"),
                    })
                    .collect()
            })
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_scan_manifest_diff_candidates(
        &self,
        manifest_id: &str,
        after_library_root_id: Option<&str>,
        after_relative_path: Option<&str>,
        limit: i64,
    ) -> Result<Vec<StoredScanManifestDiffCandidate>, StorageError> {
        self.query(
            "SELECT observed.library_root_id, observed.relative_path,
                    observed.observation_sequence,
                    CASE WHEN fe.id IS NULL THEN 'ADD'
                         WHEN fe.is_missing = 1 THEN 'REAPPEARED'
                         ELSE 'CHANGE' END AS delta_kind,
                    fe.id AS base_filesystem_entry_id,
                    fe.fingerprint AS base_fingerprint,
                    fe.entry_kind AS base_entry_kind,
                    fe.is_missing AS base_is_missing
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
               AND (observed.library_root_id, observed.relative_path) > (?, ?)
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
               )
             ORDER BY observed.library_root_id, observed.relative_path
             LIMIT ?",
        )
        .bind(manifest_id)
        .bind(after_library_root_id.unwrap_or_default())
        .bind(after_relative_path.unwrap_or_default())
        .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredScanManifestDiffCandidate {
                    library_root_id: row.get("library_root_id"),
                    relative_path: row.get("relative_path"),
                    observation_sequence: row.get("observation_sequence"),
                    delta_kind: row.get("delta_kind"),
                    base_filesystem_entry_id: row.get("base_filesystem_entry_id"),
                    base_fingerprint: row.get("base_fingerprint"),
                    base_entry_kind: row.get("base_entry_kind"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_scan_manifest_filesystem_baselines(
        &self,
        library_root_id: &str,
        relative_paths: &[String],
    ) -> Result<HashMap<String, StoredScanManifestFilesystemBaseline>, StorageError> {
        let mut baselines = HashMap::with_capacity(relative_paths.len());
        for paths in relative_paths.chunks(super::manifest_path_query_chunk_size(self.backend())) {
            if paths.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", paths.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT id, relative_path, entry_kind, fingerprint, is_missing
                 FROM filesystem_entries
                 WHERE library_root_id = ? AND relative_path IN ({placeholders})"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query)).bind(library_root_id);
            for path in paths {
                statement = statement.bind(path);
            }
            for row in
                statement
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?
            {
                let baseline = StoredScanManifestFilesystemBaseline {
                    id: row.get("id"),
                    entry_kind: row.get("entry_kind"),
                    fingerprint: row.get("fingerprint"),
                    is_missing: row.get::<i64, _>("is_missing") != 0,
                };
                baselines.insert(row.get("relative_path"), baseline);
            }
        }
        Ok(baselines)
    }

    pub(crate) async fn scan_manifest_movie_variant_identity_is_current(
        &self,
        library_root_id: &str,
        relative_path: &str,
        base_item_id: &str,
        edition_name: Option<&str>,
    ) -> Result<bool, StorageError> {
        self.query_scalar::<i64>(
            "SELECT 1
             FROM filesystem_entries entry
             JOIN media_sources source ON source.filesystem_entry_id = entry.id
             JOIN media_items item ON item.id = source.item_id
             WHERE entry.library_root_id = ? AND entry.relative_path = ?
               AND entry.entry_kind = 'FILE' AND entry.is_missing = 0
               AND item.removed_at IS NULL
               AND COALESCE(item.merged_into_item_id, item.id) = ?
               AND COALESCE(source.edition_name, '') = COALESCE(?, '')
             LIMIT 1",
        )
        .bind(library_root_id)
        .bind(relative_path)
        .bind(base_item_id)
        .bind(edition_name)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.is_some())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_scan_manifest_removal_candidates(
        &self,
        manifest_id: &str,
        discovery_format_version: i64,
        uncovered_only: bool,
        after_library_root_id: Option<&str>,
        after_relative_path: Option<&str>,
        limit: i64,
    ) -> Result<Vec<StoredScanManifestRemovalCandidate>, StorageError> {
        let unseen_path_predicate = if discovery_format_version == 3 {
            "COALESCE(fe.last_seen_generation, '') <> job.generation
             AND NOT EXISTS (
                 SELECT 1 FROM scan_manifest_seen_paths seen
                 WHERE seen.manifest_id = ?
                   AND seen.library_root_id = fe.library_root_id
                   AND seen.relative_path = fe.relative_path
             )"
        } else {
            "COALESCE((
                   SELECT observed.entry_kind
                   FROM scan_manifest_entries observed
                   WHERE observed.manifest_id = ?
                     AND observed.library_root_id = fe.library_root_id
                     AND observed.relative_path = fe.relative_path
                   ORDER BY observed.observation_sequence DESC
                   LIMIT 1
               ), '') <> 'FILE'"
        };
        let uncovered_predicate = if discovery_format_version == 3 && uncovered_only {
            "AND NOT EXISTS (
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
                )"
        } else {
            ""
        };
        let query = format!(
            "SELECT fe.library_root_id, fe.relative_path, fe.id AS base_filesystem_entry_id,
                    fe.fingerprint AS base_fingerprint
             FROM filesystem_entries fe
             JOIN scan_manifest_roots root
              ON root.manifest_id = ?
              AND root.library_root_id = fe.library_root_id
              AND root.state = 'COMPLETE'
             JOIN scan_manifests manifest ON manifest.id = root.manifest_id
             JOIN scan_jobs job ON job.id = manifest.job_id
             WHERE fe.entry_kind = 'FILE' AND fe.is_missing = 0
               AND (fe.library_root_id, fe.relative_path) > (?, ?)
               AND {unseen_path_predicate}
               {uncovered_predicate}
               AND NOT EXISTS (
                   SELECT 1 FROM scan_manifest_deltas delta
                   WHERE delta.manifest_id = ?
                     AND delta.library_root_id = fe.library_root_id
                     AND delta.relative_path = fe.relative_path
               )
            ORDER BY fe.library_root_id, fe.relative_path
            LIMIT ?"
        );
        let mut statement = self
            .query(sqlx::AssertSqlSafe(query))
            .bind(manifest_id)
            .bind(after_library_root_id.unwrap_or_default())
            .bind(after_relative_path.unwrap_or_default());
        statement = statement.bind(manifest_id).bind(manifest_id);
        statement
            .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
            .fetch_all(&self.pool)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(|row| StoredScanManifestRemovalCandidate {
                        library_root_id: row.get("library_root_id"),
                        relative_path: row.get("relative_path"),
                        base_filesystem_entry_id: row.get("base_filesystem_entry_id"),
                        base_fingerprint: row.get("base_fingerprint"),
                    })
                    .collect()
            })
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn finish_scan_manifest_diff(
        &self,
        manifest_id: &str,
        job_id: &str,
    ) -> Result<bool, StorageError> {
        let mut transaction = self.begin_scan_write_transaction().await?;
        let (workflow_version, discovery_format_version, discovery_mode): (i64, i64, String) = self
            .query_as(
                "SELECT workflow_version, discovery_format_version, discovery_mode
                 FROM scan_manifests WHERE id = ?",
            )
            .bind(manifest_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let remaining_changes: i64 = if is_lite_manifest_discovery(
            workflow_version,
            discovery_format_version,
            &discovery_mode,
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
            .fetch_one(&mut *transaction)
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
            .fetch_one(&mut *transaction)
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
            .fetch_one(&mut *transaction)
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
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
        };
        if remaining_changes > 0 {
            return Ok(false);
        }
        let unchanged_count: i64 = if matches!(workflow_version, 2 | 3) {
            self.query_scalar("SELECT unchanged_count FROM scan_manifests WHERE id = ?")
                .bind(manifest_id)
                .fetch_one(&mut *transaction)
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
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
        };
        let manifest_update = self
            .query(
                "UPDATE scan_manifests
                 SET state = 'APPLYING', unchanged_count = ?, updated_at = unixepoch()
                 WHERE id = ? AND state = 'READY_TO_DIFF'",
            )
            .bind(unchanged_count)
            .bind(manifest_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if manifest_update.rows_affected() != 1 {
            return Ok(false);
        }
        let total_count: i64 = self
            .query_scalar(if matches!(workflow_version, 2 | 3) {
                "SELECT observed_file_count + remove_count FROM scan_manifests WHERE id = ?"
            } else {
                "SELECT unchanged_count + add_count + change_count + remove_count + reappeared_count
                 FROM scan_manifests WHERE id = ?"
            })
            .bind(manifest_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let processed_count = if matches!(workflow_version, 2 | 3) {
            self.query_scalar("SELECT processed_count FROM scan_jobs WHERE id = ?")
                .bind(job_id)
                .fetch_one(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
        } else {
            unchanged_count
        };
        let job_update = self
            .query(
                "UPDATE scan_jobs
                 SET processed_count = ?, total_count = ?, updated_at = unixepoch()
                 WHERE id = ? AND status = 'RUNNING' AND cancel_requested = 0",
            )
            .bind(processed_count)
            .bind(total_count)
            .bind(job_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if job_update.rows_affected() != 1 {
            return Err(StorageError::Conflict(
                "scan job stopped before manifest diff completion".to_owned(),
            ));
        }
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(true)
    }

    pub(crate) async fn list_pending_scan_manifest_deltas(
        &self,
        manifest_id: &str,
        limit: i64,
    ) -> Result<Vec<StoredScanManifestDelta>, StorageError> {
        self.query(
            "SELECT delta.id, delta.library_root_id, delta.relative_path,
                    delta.observation_sequence, delta.delta_kind,
                    delta.base_filesystem_entry_id, delta.base_fingerprint,
                    observed.entry_kind, observed.size, observed.modified_at,
                    observed.device, observed.inode, observed.fingerprint
             FROM scan_manifest_deltas delta
             LEFT JOIN scan_manifest_entries observed
               ON observed.manifest_id = delta.manifest_id
              AND observed.library_root_id = delta.library_root_id
              AND observed.relative_path = delta.relative_path
              AND observed.observation_sequence = delta.observation_sequence
             WHERE delta.manifest_id = ? AND delta.state = 'PENDING'
             ORDER BY delta.library_root_id, delta.relative_path
             LIMIT ?",
        )
        .bind(manifest_id)
        .bind(limit.clamp(1, MAX_SCAN_MANIFEST_APPLY_BATCH_SIZE))
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredScanManifestDelta {
                    id: row.get("id"),
                    library_root_id: row.get("library_root_id"),
                    relative_path: row.get("relative_path"),
                    observation_sequence: row.get("observation_sequence"),
                    delta_kind: row.get("delta_kind"),
                    base_filesystem_entry_id: row.get("base_filesystem_entry_id"),
                    base_fingerprint: row.get("base_fingerprint"),
                    entry_kind: row.get("entry_kind"),
                    size: row.get("size"),
                    modified_at: row.get("modified_at"),
                    device: row.get("device"),
                    inode: row.get("inode"),
                    fingerprint: row.get("fingerprint"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn count_applied_scan_manifest_removals(
        &self,
        manifest_id: &str,
    ) -> Result<i64, StorageError> {
        self.query_scalar(
            "SELECT COUNT(*) FROM scan_manifest_deltas
             WHERE manifest_id = ? AND delta_kind = 'REMOVE' AND state = 'APPLIED'",
        )
        .bind(manifest_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn commit_scan_manifest_delta_batch(
        &self,
        batch: &ManifestDeltaBatchCommit<'_>,
    ) -> Result<ManifestDeltaBatchCommitResult, StorageError> {
        if batch.deltas.is_empty() {
            return Ok(ManifestDeltaBatchCommitResult::default());
        }
        if batch
            .deltas
            .iter()
            .any(|delta| delta.library_root_id != batch.library_root_id)
        {
            return Err(StorageError::Conflict(
                "manifest apply batch cannot span library roots".to_owned(),
            ));
        }
        let delta_ids = batch
            .deltas
            .iter()
            .map(|delta| delta.id.as_str())
            .collect::<std::collections::HashSet<_>>();
        if batch
            .unstable_delta_ids
            .iter()
            .any(|id| !delta_ids.contains(id.as_str()))
        {
            return Err(StorageError::Conflict(
                "manifest unstable set contains an entry outside the batch".to_owned(),
            ));
        }

        let mut transaction = self.begin_scan_write_transaction().await?;
        let active_job: i64 = self
            .query_scalar(
                "SELECT COUNT(*) FROM scan_jobs
                 WHERE id = ? AND status = 'RUNNING' AND cancel_requested = 0",
            )
            .bind(batch.job_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let manifest_state: Option<String> = self
            .query_scalar("SELECT state FROM scan_manifests WHERE id = ? AND job_id = ?")
            .bind(batch.manifest_id)
            .bind(batch.job_id)
            .fetch_optional(&mut *transaction)
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
            .fetch_optional(&mut *transaction)
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
                    inode: delta.inode,
                    fingerprint,
                    last_seen_change_kind: None,
                });
            }
        }
        let claimed_add_paths = self
            .claim_manifest_add_filesystem_entries_in_transaction(
                &mut transaction,
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
                &mut transaction,
                batch.library_id,
                batch.library_root_id,
                batch.generation,
                &claimed_movie_files,
            )
            .await?;
        result.created_items = result.created_items.saturating_add(
            self.insert_episode_files_without_filesystem_entries_in_transaction(
                &mut transaction,
                batch.library_id,
                batch.library_root_id,
                batch.generation,
                &claimed_episode_files,
            )
            .await?,
        );
        let mut new_paths = Vec::new();
        let mut changed_paths = Vec::new();
        let mut removed_media_paths = Vec::new();
        let mut changed_sidecar_paths = Vec::new();
        let mut removed_sidecar_paths = Vec::new();
        let mut removed_media_entry_ids = Vec::new();
        let mut delta_updates = Vec::with_capacity(batch.deltas.len());
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
                        let Some((entry_kind, _, _, _, _)) = observation else {
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
                        {
                            new_paths.push(delta.relative_path.clone());
                            true
                        } else if let Some(file) =
                            unresolved_files.get(delta.relative_path.as_str())
                        {
                            self.materialize_manifest_unresolved_file_after_filesystem_insert_in_transaction(
                                &mut transaction,
                                batch.library_id,
                                batch.library_root_id,
                                file,
                            )
                            .await?;
                            result.created_items = result.created_items.saturating_add(1);
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
                        let Some((entry_kind, size, modified_at, inode, fingerprint)) = observation
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
                                &mut transaction,
                                ManifestExistingFileUpdate {
                                    filesystem_entry_id,
                                    library_root_id: batch.library_root_id,
                                    relative_path: &delta.relative_path,
                                    base_fingerprint: expected_fingerprint,
                                    expected_missing,
                                    size,
                                    modified_at,
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
                                &mut transaction,
                                ManifestExistingFileUpdate {
                                    filesystem_entry_id,
                                    library_root_id: batch.library_root_id,
                                    relative_path: &delta.relative_path,
                                    base_fingerprint: expected_fingerprint,
                                    expected_missing,
                                    size,
                                    modified_at,
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
                                &mut transaction,
                                ManifestExistingFileUpdate {
                                    filesystem_entry_id,
                                    library_root_id: batch.library_root_id,
                                    relative_path: &delta.relative_path,
                                    base_fingerprint: expected_fingerprint,
                                    expected_missing,
                                    size,
                                    modified_at,
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
                                 SET size = ?, modified_at = ?, inode = ?, fingerprint = ?,
                                     last_seen_generation = ?, is_missing = 0,
                                     updated_at = unixepoch()
                                 WHERE id = ? AND library_root_id = ? AND relative_path = ?
                                   AND entry_kind = 'FILE' AND is_missing = ?
                                   AND (fingerprint = ? OR (fingerprint IS NULL AND ? IS NULL))",
                            )
                            .bind(size)
                            .bind(modified_at)
                            .bind(inode)
                            .bind(fingerprint)
                            .bind(batch.generation)
                            .bind(filesystem_entry_id)
                            .bind(batch.library_root_id)
                            .bind(&delta.relative_path)
                            .bind(database_flag(expected_missing))
                            .bind(expected_fingerprint)
                            .bind(expected_fingerprint)
                            .execute(&mut *transaction)
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
                            .execute(&mut *transaction)
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

        for chunk in delta_updates.chunks(SCAN_MANIFEST_DELTA_BATCH_SIZE) {
            let state_cases = std::iter::repeat_n("WHEN ? THEN ?", chunk.len())
                .collect::<Vec<_>>()
                .join(" ");
            let error_cases = state_cases.clone();
            let ids = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "UPDATE scan_manifest_deltas
                 SET state = CASE id {state_cases} ELSE state END,
                     error = CASE id {error_cases} ELSE error END,
                     attempt_count = attempt_count + 1,
                     updated_at = unixepoch()
                 WHERE manifest_id = ? AND state = 'PENDING' AND id IN ({ids})"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for (id, state, _) in chunk {
                statement = statement.bind(id).bind(state);
            }
            for (id, _, error) in chunk {
                statement = statement.bind(id).bind(error);
            }
            statement = statement.bind(batch.manifest_id);
            for (id, _, _) in chunk {
                statement = statement.bind(id);
            }
            let updated = statement
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if usize::try_from(updated.rows_affected()).ok() != Some(chunk.len()) {
                return Err(StorageError::Conflict(
                    "manifest delta changed before its batch commit".to_owned(),
                ));
            }
        }

        let manifest_path_chunk_size = super::manifest_path_query_chunk_size(self.backend());
        for paths in new_paths.chunks(manifest_path_chunk_size) {
            result.metadata_targets_changed |= self
                .record_scan_job_targets_in_transaction(
                    &mut transaction,
                    batch.job_id,
                    batch.library_root_id,
                    paths,
                    "NEW",
                )
                .await?;
        }
        for paths in changed_paths.chunks(manifest_path_chunk_size) {
            result.metadata_targets_changed |= self
                .record_scan_job_targets_in_transaction(
                    &mut transaction,
                    batch.job_id,
                    batch.library_root_id,
                    paths,
                    "CHANGED",
                )
                .await?;
        }
        for paths in [&changed_sidecar_paths, &removed_sidecar_paths] {
            let directories = prune_sidecar_directories(
                paths
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
            result.metadata_targets_changed |= self
                .record_scan_job_sidecar_targets_in_transaction(
                    &mut transaction,
                    batch.job_id,
                    batch.library_root_id,
                    &directories,
                )
                .await?;
        }
        self.record_scan_job_removed_targets_for_entry_ids_in_transaction(
            &mut transaction,
            batch.job_id,
            &removed_media_entry_ids,
        )
        .await?;
        if !removed_media_paths.is_empty() {
            self.refresh_removed_media_items_in_transaction(
                &mut transaction,
                batch.library_root_id,
            )
            .await?;
        }
        let processed_count = result
            .applied_count
            .saturating_add(result.conflict_count)
            .saturating_add(result.unstable_count);
        if processed_count > 0 {
            let processed_count_i64 = i64::try_from(processed_count).map_err(|_| {
                StorageError::Conflict("manifest progress count overflow".to_owned())
            })?;
            self.query(
                "UPDATE scan_manifests
                 SET applied_delta_count = applied_delta_count + ?, updated_at = unixepoch()
                 WHERE id = ? AND state = 'APPLYING'",
            )
            .bind(
                i64::try_from(result.applied_count).map_err(|_| {
                    StorageError::Conflict("manifest apply count overflow".to_owned())
                })?,
            )
            .bind(batch.manifest_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
            let update = self
                .query(
                    "UPDATE scan_jobs
                     SET processed_count = processed_count + ?, updated_at = unixepoch()
                     WHERE id = ? AND status = 'RUNNING' AND cancel_requested = 0",
                )
                .bind(processed_count_i64)
                .bind(batch.job_id)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if update.rows_affected() != 1 {
                return Err(StorageError::Conflict(
                    "scan job stopped during manifest delta apply".to_owned(),
                ));
            }
        }
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result)
    }

    #[allow(dead_code)] // LUX-267 writes the computed manifest delta set through this boundary.
    pub(crate) async fn insert_scan_manifest_deltas(
        &self,
        manifest_id: &str,
        deltas: &[NewScanManifestDelta<'_>],
    ) -> Result<usize, StorageError> {
        if deltas.is_empty() {
            return Ok(0);
        }

        let mut unique_paths = std::collections::HashSet::with_capacity(deltas.len());
        for delta in deltas {
            if !unique_paths.insert((delta.library_root_id, delta.relative_path)) {
                return Err(StorageError::Conflict(
                    "manifest delta batch contains duplicate root paths".to_owned(),
                ));
            }
            let valid_delta = match delta.delta_kind {
                "ADD" => {
                    delta.observation_sequence.is_some()
                        && delta.base_filesystem_entry_id.is_none()
                        && delta.base_fingerprint.is_none()
                }
                "CHANGE" | "REAPPEARED" => {
                    delta.observation_sequence.is_some() && delta.base_filesystem_entry_id.is_some()
                }
                "REMOVE" => {
                    delta.observation_sequence.is_none() && delta.base_filesystem_entry_id.is_some()
                }
                _ => false,
            };
            if !valid_delta {
                return Err(StorageError::Conflict(
                    "manifest delta payload does not match its kind".to_owned(),
                ));
            }
        }

        let mut inserted_count = 0_usize;
        for transaction_batch in deltas.chunks(SCAN_MANIFEST_DIFF_TRANSACTION_BATCH_SIZE) {
            let mut transaction = self.begin_scan_write_transaction().await?;
            let locked: Option<String> = self
                .query_scalar(
                    "UPDATE scan_manifests SET updated_at = updated_at
                     WHERE id = ? AND state = 'READY_TO_DIFF'
                     RETURNING state",
                )
                .bind(manifest_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if locked.as_deref() != Some("READY_TO_DIFF") {
                return Err(StorageError::Conflict(
                    "manifest is not ready to accept difference rows".to_owned(),
                ));
            }

            let mut batch_counts = [0_i64; 4];
            let mut batch_inserted_count = 0_usize;
            for chunk in transaction_batch.chunks(SCAN_MANIFEST_DELTA_BATCH_SIZE) {
                let tuple = "(?, ?, ?, ?, ?, ?, ?)";
                let values = std::iter::repeat_n(tuple, chunk.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                let verify_query = format!(
                    "WITH incoming (
                     id, library_root_id, relative_path, observation_sequence,
                     delta_kind, base_filesystem_entry_id, base_fingerprint
                 ) AS (VALUES {values})
                 SELECT stored.id
                 FROM incoming
                 JOIN scan_manifest_deltas stored
                   ON stored.manifest_id = ?
                  AND stored.library_root_id = incoming.library_root_id
                  AND stored.relative_path = incoming.relative_path
                 WHERE stored.id <> incoming.id
                    OR NOT (
                        stored.observation_sequence = incoming.observation_sequence
                        OR (stored.observation_sequence IS NULL
                            AND incoming.observation_sequence IS NULL)
                    )
                    OR stored.delta_kind <> incoming.delta_kind
                    OR NOT (
                        stored.base_filesystem_entry_id = incoming.base_filesystem_entry_id
                        OR (stored.base_filesystem_entry_id IS NULL
                            AND incoming.base_filesystem_entry_id IS NULL)
                    )
                    OR NOT (
                        stored.base_fingerprint = incoming.base_fingerprint
                        OR (stored.base_fingerprint IS NULL
                            AND incoming.base_fingerprint IS NULL)
                    )
                 LIMIT 1"
                );
                let mut verify = self.query_scalar(sqlx::AssertSqlSafe(verify_query));
                for delta in chunk {
                    verify = verify
                        .bind(delta.id)
                        .bind(delta.library_root_id)
                        .bind(delta.relative_path)
                        .bind(delta.observation_sequence)
                        .bind(delta.delta_kind)
                        .bind(delta.base_filesystem_entry_id)
                        .bind(delta.base_fingerprint);
                }
                let conflicting_row: Option<String> = verify
                    .bind(manifest_id)
                    .fetch_optional(&mut *transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                if conflicting_row.is_some() {
                    return Err(StorageError::Conflict(
                        "manifest delta retry disagrees with its persisted baseline".to_owned(),
                    ));
                }

                let insert_values = std::iter::repeat_n("(?, ?, ?, ?, ?, ?, ?, ?)", chunk.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                let insert_query = format!(
                    "INSERT INTO scan_manifest_deltas (
                     id, manifest_id, library_root_id, relative_path,
                     observation_sequence, delta_kind, base_filesystem_entry_id, base_fingerprint
                 ) VALUES {insert_values}
                 ON CONFLICT(manifest_id, library_root_id, relative_path) DO NOTHING
                 RETURNING delta_kind"
                );
                let mut insert = self.query(sqlx::AssertSqlSafe(insert_query));
                for delta in chunk {
                    insert = insert
                        .bind(delta.id)
                        .bind(manifest_id)
                        .bind(delta.library_root_id)
                        .bind(delta.relative_path)
                        .bind(delta.observation_sequence)
                        .bind(delta.delta_kind)
                        .bind(delta.base_filesystem_entry_id)
                        .bind(delta.base_fingerprint);
                }
                let inserted_kinds: Vec<String> = insert
                    .fetch_all(&mut *transaction)
                    .await
                    .map(|rows| rows.into_iter().map(|row| row.get("delta_kind")).collect())
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                for kind in &inserted_kinds {
                    let index = match kind.as_str() {
                        "ADD" => 0,
                        "CHANGE" => 1,
                        "REMOVE" => 2,
                        "REAPPEARED" => 3,
                        _ => {
                            return Err(StorageError::Conflict(
                                "database returned an unknown manifest delta kind".to_owned(),
                            ));
                        }
                    };
                    batch_counts[index] += 1;
                }
                batch_inserted_count = batch_inserted_count
                    .checked_add(inserted_kinds.len())
                    .ok_or_else(|| {
                        StorageError::Conflict("manifest delta count overflow".to_owned())
                    })?;
            }
            if batch_inserted_count > 0 {
                self.query(
                    "UPDATE scan_manifests
                 SET add_count = add_count + ?, change_count = change_count + ?,
                     remove_count = remove_count + ?, reappeared_count = reappeared_count + ?,
                     updated_at = unixepoch()
                 WHERE id = ? AND state = 'READY_TO_DIFF'",
                )
                .bind(batch_counts[0])
                .bind(batch_counts[1])
                .bind(batch_counts[2])
                .bind(batch_counts[3])
                .bind(manifest_id)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            }
            transaction
                .commit()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            inserted_count = inserted_count
                .checked_add(batch_inserted_count)
                .ok_or_else(|| {
                    StorageError::Conflict("manifest delta count overflow".to_owned())
                })?;
        }
        Ok(inserted_count)
    }

    pub(crate) async fn restore_media_item(&self, item_id: &str) -> Result<(), StorageError> {
        self.query(
            "UPDATE media_items
             SET removed_at = NULL, updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(item_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }
}

#[cfg(test)]
#[path = "jobs/tests.rs"]
mod tests;
