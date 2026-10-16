const SCAN_MANIFEST_DIFF_TRANSACTION_BATCH_SIZE: usize = 500;

use super::*;

#[path = "diff/finish.rs"]
mod finish;

#[path = "diff/insert.rs"]
mod insert;

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

    pub(crate) async fn list_scan_manifest_complete_roots(
        &self,
        manifest_id: &str,
        after_library_root_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<String>, StorageError> {
        self.query(
            "SELECT library_root_id
             FROM scan_manifest_roots
             WHERE manifest_id = ? AND state = 'COMPLETE' AND library_root_id > ?
             ORDER BY library_root_id
             LIMIT ?",
        )
        .bind(manifest_id)
        .bind(after_library_root_id.unwrap_or_default())
        .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| row.get("library_root_id"))
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_scan_manifest_complete_directories_for_root(
        &self,
        manifest_id: &str,
        library_root_id: &str,
        after_relative_path: Option<&str>,
        limit: i64,
    ) -> Result<Vec<StoredScanManifestDirectory>, StorageError> {
        let path_cursor_predicate = if after_relative_path.is_some() {
            "relative_path > ?"
        } else {
            "relative_path >= ?"
        };
        self.query(sqlx::AssertSqlSafe(format!(
            "SELECT library_root_id, relative_path
             FROM scan_manifest_directories
             WHERE manifest_id = ? AND library_root_id = ? AND state = 'COMPLETE'
               AND {path_cursor_predicate}
             ORDER BY relative_path
             LIMIT ?"
        )))
        .bind(manifest_id)
        .bind(library_root_id)
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

    pub(crate) async fn list_scan_manifest_removal_candidates_for_root(
        &self,
        query: ScanManifestRemovalCandidateQuery<'_>,
    ) -> Result<Vec<StoredScanManifestRemovalCandidate>, StorageError> {
        let ScanManifestRemovalCandidateQuery {
            manifest_id,
            library_root_id,
            discovery_format_version,
            uncovered_only,
            directories,
            after_relative_path,
            limit,
        } = query;
        if let Some(directories) = directories {
            if directories.is_empty() {
                return Ok(Vec::new());
            }
            if directories
                .iter()
                .any(|directory| directory.library_root_id != library_root_id)
            {
                return Err(StorageError::Conflict(
                    "manifest removal directory page spans multiple roots".to_owned(),
                ));
            }
        }

        let directory_predicate = directories.map(|directories| {
            directories
                .iter()
                .map(|directory| {
                    if directory.relative_path.is_empty() {
                        "(fe.relative_path NOT LIKE '%/%' ESCAPE '\\')".to_owned()
                    } else {
                        "(fe.relative_path LIKE ? ESCAPE '\\'
                          AND fe.relative_path NOT LIKE ? ESCAPE '\\')"
                            .to_owned()
                    }
                })
                .collect::<Vec<_>>()
                .join(" OR ")
        });
        let unseen_path_predicate = if discovery_format_version == 3 {
            "AND COALESCE(fe.last_seen_generation, '') <> (
                     SELECT job.generation
                     FROM scan_manifests manifest
                     JOIN scan_jobs job ON job.id = manifest.job_id
                     WHERE manifest.id = ?
                 )
                 AND NOT EXISTS (
                     SELECT 1 FROM scan_manifest_seen_paths seen
                     WHERE seen.manifest_id = ?
                       AND seen.library_root_id = fe.library_root_id
                       AND seen.relative_path = fe.relative_path
                 )"
        } else {
            "AND COALESCE((
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
                    WHERE directory.manifest_id = ?
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
        let directory_filter = directory_predicate
            .as_deref()
            .map(|predicate| format!("AND ({predicate})"))
            .unwrap_or_default();
        let query = format!(
            "SELECT fe.library_root_id, fe.relative_path,
                    fe.id AS base_filesystem_entry_id,
                    fe.fingerprint AS base_fingerprint
             FROM filesystem_entries fe
             WHERE fe.library_root_id = ?
               AND fe.relative_path > ?
               AND fe.entry_kind = 'FILE' AND fe.is_missing = 0
               AND EXISTS (
                   SELECT 1 FROM scan_manifest_roots root
                   WHERE root.manifest_id = ?
                     AND root.library_root_id = fe.library_root_id
                     AND root.state = 'COMPLETE'
               )
               {unseen_path_predicate}
               {directory_filter}
               {uncovered_predicate}
               AND NOT EXISTS (
                   SELECT 1 FROM scan_manifest_deltas delta
                   WHERE delta.manifest_id = ?
                     AND delta.library_root_id = fe.library_root_id
                     AND delta.relative_path = fe.relative_path
               )
             ORDER BY fe.relative_path
             LIMIT ?"
        );
        let mut statement = self
            .query(sqlx::AssertSqlSafe(query))
            .bind(library_root_id)
            .bind(after_relative_path.unwrap_or_default())
            .bind(manifest_id);
        if discovery_format_version == 3 {
            statement = statement.bind(manifest_id).bind(manifest_id);
        } else {
            statement = statement.bind(manifest_id);
        }
        if let Some(directories) = directories {
            for directory in directories {
                if !directory.relative_path.is_empty() {
                    let escaped = escape_sql_like_pattern(&directory.relative_path);
                    statement = statement
                        .bind(format!("{escaped}/%"))
                        .bind(format!("{escaped}/%/%"));
                }
            }
        }
        if discovery_format_version == 3 && uncovered_only {
            statement = statement.bind(manifest_id);
        }
        statement = statement
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
        let has_remaining_changes = self
            .has_manifest_diff_remaining_changes(
                &mut transaction,
                manifest_id,
                workflow_version,
                discovery_format_version,
                &discovery_mode,
            )
            .await?;
        if has_remaining_changes {
            return Ok(false);
        }
        let unchanged_count = self
            .count_manifest_diff_unchanged_entries(&mut transaction, manifest_id, workflow_version)
            .await?;
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

            let (batch_inserted_count, batch_counts) = self
                .insert_manifest_delta_rows_in_transaction(
                    &mut transaction,
                    manifest_id,
                    transaction_batch,
                )
                .await?;
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
}
