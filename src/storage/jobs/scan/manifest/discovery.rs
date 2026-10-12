use super::*;

#[path = "discovery/input.rs"]
mod input;
use input::ManifestDiscoveryInput;

fn manifest_positive_file_path(file: &NewScanManifestIndexedFile) -> &str {
    match file {
        NewScanManifestIndexedFile::Movie(file) => &file.relative_path,
        NewScanManifestIndexedFile::Episode(file) => &file.relative_path,
        NewScanManifestIndexedFile::Unresolved(file) => &file.relative_path,
        NewScanManifestIndexedFile::Sidecar(file) => &file.relative_path,
    }
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
        let ManifestDiscoveryInput {
            chunk,
            child_directories,
            entries,
            completed_directories,
            positive_indexes,
            unchanged_paths,
            seen_filesystem_entries,
            ..
        } = input;
        if child_directories.is_empty() && entries.is_empty() && completed_directories.is_empty() {
            return Ok(ManifestDiscoveryCommitResult::default());
        }

        record_manifest_storage_stage(
            "transaction_input_validation",
            input_validation_started,
            entries.len().saturating_add(child_directories.len()),
            entries
                .iter()
                .filter(|entry| entry.entry_kind == "FILE")
                .count(),
            child_directories.len(),
        );
        let transaction_total_started = Instant::now();
        let transaction_begin_started = Instant::now();
        let mut transaction = self.begin_scan_write_transaction().await?;
        record_manifest_storage_stage(
            "transaction_begin",
            transaction_begin_started,
            entries.len().saturating_add(child_directories.len()),
            entries
                .iter()
                .filter(|entry| entry.entry_kind == "FILE")
                .count(),
            child_directories.len(),
        );
        let manifest_state_started = Instant::now();
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
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let lite_mode =
            is_lite_manifest_discovery(workflow_version, discovery_format_version, &discovery_mode);

        record_manifest_storage_stage("manifest_state_check", manifest_state_started, 1, 0, 0);
        let directory_frontier_started = Instant::now();
        let mut inserted_directory_count = 0_u64;
        let directory_chunks = if lite_mode {
            Vec::new()
        } else {
            child_directories
                .chunks(SCAN_DML_CHUNK_SIZE)
                .collect::<Vec<_>>()
        };
        for paths in directory_chunks {
            if paths.is_empty() {
                continue;
            }
            let values = std::iter::repeat_n("(?, ?, ?, 'PENDING')", paths.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "INSERT INTO scan_manifest_directories (
                     manifest_id, library_root_id, relative_path, state
                 ) VALUES {values}
                 ON CONFLICT(manifest_id, library_root_id, relative_path) DO NOTHING"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for path in paths {
                statement = statement
                    .bind(chunk.manifest_id)
                    .bind(chunk.library_root_id)
                    .bind(*path);
            }
            let result = statement
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            inserted_directory_count = inserted_directory_count
                .checked_add(result.rows_affected())
                .ok_or_else(|| {
                    StorageError::Conflict("manifest directory count overflow".to_owned())
                })?;
        }
        record_manifest_storage_stage(
            "directory_frontier_insert",
            directory_frontier_started,
            child_directories.len(),
            0,
            child_directories.len(),
        );

        let file_paths = entries
            .iter()
            .filter(|entry| entry.entry_kind == "FILE")
            .map(|entry| entry.relative_path.as_str())
            .collect::<Vec<_>>();
        let known_path_started = Instant::now();
        let mut known_file_paths = std::collections::HashSet::new();
        for paths in file_paths.chunks(super::manifest_path_query_chunk_size(self.backend())) {
            if paths.is_empty() {
                continue;
            }
            let query = if discovery_format_version == 3 {
                let values = std::iter::repeat_n("(?)", paths.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "WITH incoming(relative_path) AS (VALUES {values})
                     SELECT incoming.relative_path FROM incoming
                     WHERE EXISTS (
                         SELECT 1 FROM filesystem_entries entry
                           WHERE entry.library_root_id = ?
                           AND entry.relative_path = incoming.relative_path
                           AND entry.last_seen_generation = ?
                     )"
                )
            } else {
                let placeholders = std::iter::repeat_n("?", paths.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "SELECT DISTINCT relative_path FROM scan_manifest_entries
                     WHERE manifest_id = ? AND library_root_id = ? AND entry_kind = 'FILE'
                       AND relative_path IN ({placeholders})"
                )
            };
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            if discovery_format_version == 3 {
                for path in paths {
                    statement = statement.bind(path);
                }
                statement = statement.bind(chunk.library_root_id).bind(&generation);
            } else {
                statement = statement
                    .bind(chunk.manifest_id)
                    .bind(chunk.library_root_id);
                for path in paths {
                    statement = statement.bind(path);
                }
            }
            known_file_paths.extend(
                statement
                    .fetch_all(&mut *transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?
                    .into_iter()
                    .map(|row| row.get::<String, _>("relative_path")),
            );
        }
        let inserted_file_count = file_paths
            .iter()
            .filter(|path| !known_file_paths.contains(**path))
            .count();
        let unchanged_file_count = unchanged_paths
            .iter()
            .filter(|path| !known_file_paths.contains(**path))
            .count();
        record_manifest_storage_stage(
            "known_path_query",
            known_path_started,
            file_paths.len().saturating_add(unchanged_paths.len()),
            file_paths.len(),
            0,
        );

        let inserted_file_count_i64 = i64::try_from(inserted_file_count)
            .map_err(|_| StorageError::Conflict("manifest file count overflow".to_owned()))?;
        let inserted_directory_count_i64 = i64::try_from(inserted_directory_count)
            .map_err(|_| StorageError::Conflict("manifest directory count overflow".to_owned()))?;
        let directory_frontier_completion_started = Instant::now();
        let mut completed_directory_count = 0_i64;
        let completed_directory_chunks = if lite_mode {
            Vec::new()
        } else {
            completed_directories
                .chunks(SCAN_DML_CHUNK_SIZE)
                .collect::<Vec<_>>()
        };
        for directories in completed_directory_chunks {
            let placeholders = std::iter::repeat_n("?", directories.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "UPDATE scan_manifest_directories
                 SET state = 'COMPLETE', error = NULL, updated_at = unixepoch()
                 WHERE manifest_id = ? AND library_root_id = ? AND state <> 'COMPLETE'
                   AND relative_path IN ({placeholders})"
            );
            let mut statement = self
                .query(sqlx::AssertSqlSafe(query))
                .bind(chunk.manifest_id)
                .bind(chunk.library_root_id);
            for directory in directories {
                statement = statement.bind(*directory);
            }
            let completed = statement
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            completed_directory_count = completed_directory_count
                .checked_add(i64::try_from(completed.rows_affected()).map_err(|_| {
                    StorageError::Conflict("manifest directory count overflow".to_owned())
                })?)
                .ok_or_else(|| {
                    StorageError::Conflict("manifest directory count overflow".to_owned())
                })?;
        }
        record_manifest_storage_stage(
            "directory_frontier_completion",
            directory_frontier_completion_started,
            completed_directories.len(),
            0,
            completed_directories.len(),
        );

        let root_checkpoint_started = Instant::now();
        let observation_entries = if discovery_format_version == 3 {
            entries
                .iter()
                .copied()
                .filter(|entry| {
                    entry.entry_kind == "DIRECTORY"
                        && (!lite_mode || entry.relative_path.is_empty())
                })
                .collect::<Vec<_>>()
        } else {
            entries.clone()
        };
        let observation_count_i64 = if matches!(workflow_version, 2 | 3) {
            i64::try_from(observation_entries.len()).map_err(|_| {
                StorageError::Conflict("manifest observation count overflow".to_owned())
            })?
        } else {
            0
        };
        let reserved_local_batch_sequence_count = if workflow_version == 3 {
            i64::try_from(
                positive_indexes
                    .len()
                    .div_ceil(MAX_SCAN_LOCAL_METADATA_BATCH_SOURCES),
            )
            .map_err(|_| StorageError::Conflict("local metadata batch count overflow".to_owned()))?
        } else {
            0
        };
        let root_sequence_increment = observation_count_i64
            .checked_add(reserved_local_batch_sequence_count)
            .ok_or_else(|| StorageError::Conflict("manifest root sequence overflow".into()))?;
        let discovered_directory_count_i64 = if lite_mode {
            i64::try_from(child_directories.len()).map_err(|_| {
                StorageError::Conflict("manifest directory count overflow".to_owned())
            })?
        } else {
            inserted_directory_count_i64
        };
        let completed_directory_count_for_root = if lite_mode {
            i64::try_from(completed_directories.len()).map_err(|_| {
                StorageError::Conflict("manifest directory count overflow".to_owned())
            })?
        } else {
            completed_directory_count
        };
        let last_sequence = self
            .query_scalar::<i64>(
                "UPDATE scan_manifest_roots
                 SET state = CASE WHEN ? = 'LITE' THEN 'SCANNING'
                     WHEN NOT EXISTS (
                         SELECT 1 FROM scan_manifest_directories
                         WHERE manifest_id = ? AND library_root_id = ? AND state = 'PENDING'
                     ) THEN 'COMPLETE' ELSE 'SCANNING' END,
                     started_at = COALESCE(started_at, unixepoch()),
                     finished_at = CASE WHEN ? = 'LITE' THEN finished_at
                         WHEN NOT EXISTS (
                         SELECT 1 FROM scan_manifest_directories
                         WHERE manifest_id = ? AND library_root_id = ? AND state = 'PENDING'
                     ) THEN COALESCE(finished_at, unixepoch()) ELSE finished_at END,
                     directory_count = directory_count + ?,
                     completed_directory_count = completed_directory_count + ?,
                     observed_file_count = observed_file_count + ?,
                     next_observation_sequence = next_observation_sequence + ?,
                     updated_at = unixepoch()
                 WHERE manifest_id = ? AND library_root_id = ?
                   AND state IN ('PENDING', 'SCANNING')
                   AND EXISTS (
                       SELECT 1 FROM scan_manifests
                       WHERE id = ? AND state = 'DISCOVERING'
                   )
                 RETURNING next_observation_sequence",
            )
            .bind(&discovery_mode)
            .bind(chunk.manifest_id)
            .bind(chunk.library_root_id)
            .bind(&discovery_mode)
            .bind(chunk.manifest_id)
            .bind(chunk.library_root_id)
            .bind(discovered_directory_count_i64)
            .bind(completed_directory_count_for_root)
            .bind(inserted_file_count_i64)
            .bind(root_sequence_increment)
            .bind(chunk.manifest_id)
            .bind(chunk.library_root_id)
            .bind(chunk.manifest_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .ok_or_else(|| {
                StorageError::Conflict("manifest root is not available for discovery".to_owned())
            })?;
        record_manifest_storage_stage(
            "root_checkpoint",
            root_checkpoint_started,
            inserted_directory_count
                .try_into()
                .unwrap_or(usize::MAX)
                .saturating_add(completed_directories.len())
                .saturating_add(inserted_file_count),
            inserted_file_count,
            completed_directories.len(),
        );
        let observation_sequence_start = if observation_count_i64 > 0 {
            Some(
                last_sequence
                    .checked_sub(root_sequence_increment)
                    .and_then(|sequence| sequence.checked_add(1))
                    .ok_or_else(|| {
                        StorageError::Conflict("manifest observation sequence overflow".to_owned())
                    })?,
            )
        } else {
            None
        };
        let local_metadata_sequence_start = if reserved_local_batch_sequence_count > 0 {
            Some(
                last_sequence
                    .checked_sub(reserved_local_batch_sequence_count)
                    .and_then(|sequence| sequence.checked_add(1))
                    .ok_or_else(|| {
                        StorageError::Conflict("local metadata batch sequence overflow".to_owned())
                    })?,
            )
        } else {
            None
        };

        let observation_batch_size = if matches!(workflow_version, 2 | 3) {
            super::manifest_path_query_chunk_size(self.backend())
        } else {
            80
        };
        let observation_started = Instant::now();
        let mut observation_offset = 0_i64;
        for entry_chunk in observation_entries.chunks(observation_batch_size) {
            if entry_chunk.is_empty() {
                continue;
            }
            if let Some(sequence_start) = observation_sequence_start {
                let values = std::iter::repeat_n("(?, ?, ?, ?, ?, ?, ?, ?)", entry_chunk.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                let query = format!(
                    "WITH incoming (
                         relative_path, observation_sequence, entry_kind, size,
                         modified_at, device, inode, fingerprint
                     ) AS (VALUES {values})
                     INSERT INTO scan_manifest_entries (
                         manifest_id, library_root_id, relative_path, observation_sequence,
                         entry_kind, size, modified_at, device, inode, fingerprint, observed_at
                     )
                     SELECT ?, ?, relative_path, observation_sequence, entry_kind, size,
                            modified_at, device, inode, fingerprint, unixepoch()
                     FROM incoming"
                );
                let mut statement = self.query(sqlx::AssertSqlSafe(query));
                for entry in entry_chunk {
                    let sequence =
                        sequence_start
                            .checked_add(observation_offset)
                            .ok_or_else(|| {
                                StorageError::Conflict(
                                    "manifest observation sequence overflow".to_owned(),
                                )
                            })?;
                    observation_offset = observation_offset.saturating_add(1);
                    statement = statement
                        .bind(entry.relative_path.as_str())
                        .bind(sequence)
                        .bind(entry.entry_kind.as_str())
                        .bind(entry.size)
                        .bind(entry.modified_at)
                        .bind(entry.device)
                        .bind(entry.inode)
                        .bind(entry.fingerprint.as_slice());
                }
                statement = statement
                    .bind(chunk.manifest_id)
                    .bind(chunk.library_root_id);
                statement
                    .execute(&mut *transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
            } else {
                let selects = std::iter::repeat_n(
                    "SELECT ?, ?, ?,
                            (SELECT COALESCE(MAX(observation_sequence), 0) + 1
                             FROM scan_manifest_entries
                             WHERE manifest_id = ? AND library_root_id = ? AND relative_path = ?),
                            ?, ?, ?, ?, ?, ?, unixepoch()",
                    entry_chunk.len(),
                )
                .collect::<Vec<_>>()
                .join(" UNION ALL ");
                let query = format!(
                    "INSERT INTO scan_manifest_entries (
                         manifest_id, library_root_id, relative_path, observation_sequence,
                         entry_kind, size, modified_at, device, inode, fingerprint, observed_at
                     ) {selects}"
                );
                let mut statement = self.query(sqlx::AssertSqlSafe(query));
                for entry in entry_chunk {
                    statement = statement
                        .bind(chunk.manifest_id)
                        .bind(chunk.library_root_id)
                        .bind(entry.relative_path.as_str())
                        .bind(chunk.manifest_id)
                        .bind(chunk.library_root_id)
                        .bind(entry.relative_path.as_str())
                        .bind(entry.entry_kind.as_str())
                        .bind(entry.size)
                        .bind(entry.modified_at)
                        .bind(entry.device)
                        .bind(entry.inode)
                        .bind(entry.fingerprint.as_slice());
                }
                statement
                    .execute(&mut *transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
            }
        }
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
                    &mut transaction,
                    ManifestDiscoveryPositiveIndexCommit {
                        job_id: chunk.job_id,
                        library_id: &library_id,
                        library_root_id: chunk.library_root_id,
                        generation: &generation,
                        positives: &positive_indexes,
                        observations: &entries,
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
                &mut transaction,
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

        let presence_ledger_started = Instant::now();
        let mut ledger_paths = Vec::new();
        let mut successfully_seen_paths = std::collections::HashSet::new();
        if discovery_format_version == 3 && !seen_filesystem_entries.is_empty() {
            for entries in seen_filesystem_entries
                .chunks(super::manifest_path_query_chunk_size(self.backend()))
            {
                if entries.is_empty() {
                    continue;
                }
                let values = std::iter::repeat_n("(?, ?, ?)", entries.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                let postgres_update_from = self.backend() == DatabaseBackend::Postgres;
                let query = if postgres_update_from {
                    format!(
                        "WITH incoming(filesystem_entry_id, relative_path, fingerprint) AS (VALUES {values})
                         UPDATE filesystem_entries AS current
                         SET last_seen_generation = ?,
                             last_seen_change_kind = NULL
                         FROM incoming
                         WHERE current.id = incoming.filesystem_entry_id
                           AND current.library_root_id = ?
                           AND current.relative_path = incoming.relative_path
                           AND current.fingerprint = incoming.fingerprint
                           AND current.entry_kind = 'FILE'
                           AND current.is_missing = 0
                           AND (current.last_seen_generation IS NULL
                                OR current.last_seen_generation <> ?
                                OR current.last_seen_change_kind IS NOT NULL)"
                    )
                } else {
                    format!(
                        "WITH incoming(filesystem_entry_id, relative_path, fingerprint) AS (VALUES {values}),
                              matching(filesystem_entry_id) AS (
                                  SELECT incoming.filesystem_entry_id
                                  FROM incoming
                                  JOIN filesystem_entries current
                                    ON current.id = incoming.filesystem_entry_id
                                   AND current.relative_path = incoming.relative_path
                                   AND current.fingerprint = incoming.fingerprint
                                  WHERE current.library_root_id = ?
                                    AND current.entry_kind = 'FILE'
                                    AND current.is_missing = 0
                              )
                         UPDATE filesystem_entries
                         SET last_seen_generation = ?,
                             last_seen_change_kind = NULL
                         WHERE id IN (SELECT filesystem_entry_id FROM matching)
                           AND (last_seen_generation IS NULL
                                OR last_seen_generation <> ?
                                OR last_seen_change_kind IS NOT NULL)"
                    )
                };
                let mut statement = self.query(sqlx::AssertSqlSafe(query));
                for entry in entries {
                    statement = statement
                        .bind(&entry.filesystem_entry_id)
                        .bind(&entry.relative_path)
                        .bind(&entry.fingerprint);
                }
                statement = if postgres_update_from {
                    statement
                        .bind(&generation)
                        .bind(chunk.library_root_id)
                        .bind(&generation)
                } else {
                    statement
                        .bind(chunk.library_root_id)
                        .bind(&generation)
                        .bind(&generation)
                };
                let updated = statement
                    .execute(&mut *transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                let updated_count = usize::try_from(updated.rows_affected()).unwrap_or(usize::MAX);
                if updated_count == entries.len() {
                    successfully_seen_paths
                        .extend(entries.iter().map(|entry| entry.relative_path.clone()));
                    continue;
                }

                let values = std::iter::repeat_n("(?, ?, ?)", entries.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                let query = format!(
                    "WITH incoming(filesystem_entry_id, relative_path, fingerprint) AS (VALUES {values})
                     SELECT incoming.relative_path
                     FROM incoming
                     JOIN filesystem_entries entry
                       ON entry.id = incoming.filesystem_entry_id
                      AND entry.relative_path = incoming.relative_path
                     WHERE entry.library_root_id = ?
                       AND entry.entry_kind = 'FILE'
                       AND entry.is_missing = 0
                       AND entry.last_seen_generation = ?
                       AND entry.fingerprint = incoming.fingerprint"
                );
                let mut statement = self.query(sqlx::AssertSqlSafe(query));
                for entry in entries {
                    statement = statement
                        .bind(&entry.filesystem_entry_id)
                        .bind(&entry.relative_path)
                        .bind(&entry.fingerprint);
                }
                let successful_rows = statement
                    .bind(chunk.library_root_id)
                    .bind(&generation)
                    .fetch_all(&mut *transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                successfully_seen_paths.extend(
                    successful_rows
                        .into_iter()
                        .map(|row| row.get::<String, _>("relative_path")),
                );
            }
        }
        if discovery_format_version == 3 {
            positive_result.indexed_paths.sort_unstable();
            ledger_paths = file_paths
                .iter()
                .copied()
                .filter(|path| {
                    positive_result.indexed_paths.binary_search(path).is_err()
                        && !successfully_seen_paths.contains(*path)
                })
                .collect::<Vec<_>>();
        }
        for paths in ledger_paths.chunks(super::manifest_path_query_chunk_size(self.backend())) {
            if paths.is_empty() {
                continue;
            }
            let values = std::iter::repeat_n("(?)", paths.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "WITH incoming_seen_paths(relative_path) AS (VALUES {values})
                 INSERT INTO scan_manifest_seen_paths (
                     manifest_id, library_root_id, relative_path
                 )
                 SELECT ?, ?, incoming_seen_paths.relative_path
                 FROM incoming_seen_paths WHERE TRUE
                 ON CONFLICT(manifest_id, library_root_id, relative_path) DO NOTHING"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for path in paths {
                statement = statement.bind(path);
            }
            statement = statement
                .bind(chunk.manifest_id)
                .bind(chunk.library_root_id);
            statement
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }
        record_manifest_storage_stage(
            "presence_ledger",
            presence_ledger_started,
            successfully_seen_paths
                .len()
                .saturating_add(ledger_paths.len()),
            successfully_seen_paths
                .len()
                .saturating_add(ledger_paths.len()),
            0,
        );

        let manifest_counters_started = Instant::now();
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
        .bind(inserted_directory_count_i64)
        .bind(inserted_file_count_i64)
        .bind(completed_directory_count)
        .bind(
            i64::try_from(unchanged_file_count).map_err(|_| {
                StorageError::Conflict("manifest unchanged count overflow".to_owned())
            })?,
        )
        .bind(positive_result.add_count)
        .bind(positive_result.change_count)
        .bind(positive_result.reappeared_count)
        .bind(positive_result.applied_count)
        .bind(chunk.manifest_id)
        .execute(&mut *transaction)
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
            .bind(inserted_file_count_i64)
            .bind(if matches!(workflow_version, 2 | 3) {
                inserted_file_count_i64
            } else {
                0
            })
            .bind(chunk.job_id)
            .execute(&mut *transaction)
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

        record_manifest_storage_stage(
            "manifest_counter_checkpoint",
            manifest_counters_started,
            inserted_file_count
                .saturating_add(completed_directories.len())
                .saturating_add(positive_indexes.len()),
            inserted_file_count,
            completed_directories.len(),
        );
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
            entries.len().saturating_add(child_directories.len()),
            file_paths.len(),
            child_directories.len(),
        );
        record_manifest_storage_stage(
            "transaction_total",
            transaction_total_started,
            entries.len().saturating_add(child_directories.len()),
            file_paths.len(),
            child_directories.len(),
        );
        Ok(ManifestDiscoveryCommitResult {
            observed_file_count: i64::try_from(inserted_file_count)
                .map_err(|_| StorageError::Conflict("manifest file count overflow".to_owned()))?,
            created_items: positive_result.created_items,
            metadata_targets_changed: positive_result.metadata_targets_changed,
            local_metadata_batches_changed,
        })
    }
}
