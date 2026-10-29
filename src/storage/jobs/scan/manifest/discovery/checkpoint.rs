use super::*;

pub(super) struct ManifestDiscoveryCheckpointInput<'a> {
    pub(super) chunk: &'a NewScanManifestDiscoveryChunk<'a>,
    pub(super) workflow_version: i64,
    pub(super) discovery_mode: &'a str,
    pub(super) lite_mode: bool,
    pub(super) observation_entry_count: usize,
    pub(super) positive_index_count: usize,
    pub(super) inserted_directory_count_i64: i64,
    pub(super) inserted_file_count: i64,
    pub(super) child_directory_count: usize,
    pub(super) completed_directory_count: i64,
    pub(super) completed_directory_input_count: usize,
}

pub(super) struct ManifestDiscoverySequenceReservations {
    pub(super) observation_sequence_start: Option<i64>,
    pub(super) local_metadata_sequence_start: Option<i64>,
}

impl Database {
    pub(super) async fn checkpoint_manifest_discovery_root(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Any>,
        input: ManifestDiscoveryCheckpointInput<'_>,
    ) -> Result<ManifestDiscoverySequenceReservations, StorageError> {
        let observation_count = if matches!(input.workflow_version, 2 | 3) {
            i64::try_from(input.observation_entry_count).map_err(|_| {
                StorageError::Conflict("manifest observation count overflow".to_owned())
            })?
        } else {
            0
        };
        let reserved_local_batch_sequence_count = if input.workflow_version == 3 {
            i64::try_from(
                input
                    .positive_index_count
                    .div_ceil(MAX_SCAN_LOCAL_METADATA_BATCH_SOURCES),
            )
            .map_err(|_| StorageError::Conflict("local metadata batch count overflow".to_owned()))?
        } else {
            0
        };
        let root_sequence_increment = observation_count
            .checked_add(reserved_local_batch_sequence_count)
            .ok_or_else(|| StorageError::Conflict("manifest root sequence overflow".into()))?;
        let inserted_directory_count = if input.lite_mode {
            i64::try_from(input.child_directory_count).map_err(|_| {
                StorageError::Conflict("manifest directory count overflow".to_owned())
            })?
        } else {
            input.inserted_directory_count_i64
        };
        let completed_directory_count = if input.lite_mode {
            i64::try_from(input.completed_directory_input_count).map_err(|_| {
                StorageError::Conflict("manifest directory count overflow".to_owned())
            })?
        } else {
            input.completed_directory_count
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
            .bind(input.discovery_mode)
            .bind(input.chunk.manifest_id)
            .bind(input.chunk.library_root_id)
            .bind(input.discovery_mode)
            .bind(input.chunk.manifest_id)
            .bind(input.chunk.library_root_id)
            .bind(inserted_directory_count)
            .bind(completed_directory_count)
            .bind(input.inserted_file_count)
            .bind(root_sequence_increment)
            .bind(input.chunk.manifest_id)
            .bind(input.chunk.library_root_id)
            .bind(input.chunk.manifest_id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .ok_or_else(|| {
                StorageError::Conflict("manifest root is not available for discovery".to_owned())
            })?;

        let observation_sequence_start = if observation_count > 0 {
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

        Ok(ManifestDiscoverySequenceReservations {
            observation_sequence_start,
            local_metadata_sequence_start,
        })
    }

    pub(super) async fn insert_manifest_discovery_observations(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Any>,
        chunk: &NewScanManifestDiscoveryChunk<'_>,
        observation_entries: &[&NewScanManifestEntry],
        observation_batch_size: usize,
        observation_sequence_start: Option<i64>,
    ) -> Result<(), StorageError> {
        // The sequence-less shape binds 12 values per row (the sequence shape binds 8 plus two
        // statement-level values), so a chunk sized for path lookups can exceed SQLite's variable
        // limit on large directories. Cap the row count by the backend's bind budget.
        let observation_batch_size = observation_batch_size.min(observation_batch_row_cap(
            self.backend(),
            observation_sequence_start.is_some(),
        ));
        let mut observation_offset = 0_i64;
        for entry_chunk in observation_entries.chunks(observation_batch_size) {
            if entry_chunk.is_empty() {
                continue;
            }
            if let Some(sequence_start) = observation_sequence_start {
                let values =
                    std::iter::repeat_n("(?, ?, ?, ?, ?, ?, ?, ?, ?, ?)", entry_chunk.len())
                        .collect::<Vec<_>>()
                        .join(", ");
                let query = format!(
                    "WITH incoming (
                         relative_path, observation_sequence, entry_kind, size,
                         modified_at, device, inode, fingerprint, is_symlink, resolved_path
                     ) AS (VALUES {values})
                     INSERT INTO scan_manifest_entries (
                         manifest_id, library_root_id, relative_path, observation_sequence,
                         entry_kind, size, modified_at, device, inode, fingerprint,
                         is_symlink, resolved_path, observed_at
                     )
                     SELECT ?, ?, relative_path, observation_sequence, entry_kind, size,
                            modified_at, device, inode, fingerprint, is_symlink,
                            resolved_path, unixepoch()
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
                        .bind(entry.fingerprint.as_slice())
                        .bind(i64::from(entry.is_symlink))
                        .bind(entry.resolved_path.as_deref());
                }
                statement = statement
                    .bind(chunk.manifest_id)
                    .bind(chunk.library_root_id);
                statement
                    .execute(&mut **transaction)
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
                            ?, ?, ?, ?, ?, ?, ?, ?, unixepoch()",
                    entry_chunk.len(),
                )
                .collect::<Vec<_>>()
                .join(" UNION ALL ");
                let query = format!(
                    "INSERT INTO scan_manifest_entries (
                         manifest_id, library_root_id, relative_path, observation_sequence,
                         entry_kind, size, modified_at, device, inode, fingerprint,
                         is_symlink, resolved_path, observed_at
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
                        .bind(entry.fingerprint.as_slice())
                        .bind(i64::from(entry.is_symlink))
                        .bind(entry.resolved_path.as_deref());
                }
                statement
                    .execute(&mut **transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
            }
        }

        Ok(())
    }
}

/// Rows one observation insert may bind without exceeding the backend's variable limit.
pub(super) fn observation_batch_row_cap(
    backend: crate::config::DatabaseBackend,
    has_sequence: bool,
) -> usize {
    let binds_per_row = if has_sequence { 10 } else { 14 };
    super::super::max_sql_bind_parameters(backend)
        .saturating_sub(2)
        .checked_div(binds_per_row)
        .unwrap_or(1)
        .max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DatabaseBackend;

    #[test]
    fn observation_rows_are_capped_by_the_statement_bind_budget() {
        // SQLite accepts 32,766 variables: 14 binds per row (link provenance adds two)
        // leaves room for 2,340 rows; the sequence shape uses 10 binds per row.
        assert_eq!(
            observation_batch_row_cap(DatabaseBackend::Sqlite, false),
            2_340
        );
        assert_eq!(
            observation_batch_row_cap(DatabaseBackend::Sqlite, true),
            3_276
        );
        // PostgreSQL's 65,535 budget: 4,680 rows without a sequence, 6,553 with one.
        assert_eq!(
            observation_batch_row_cap(DatabaseBackend::Postgres, false),
            4_680
        );
        assert_eq!(
            observation_batch_row_cap(DatabaseBackend::Postgres, true),
            6_553
        );
    }
}
