use super::*;

impl Database {
    pub(super) async fn update_manifest_presence_ledger<'a>(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Any>,
        chunk: &NewScanManifestDiscoveryChunk<'_>,
        discovery_format_version: i64,
        generation: &str,
        seen_filesystem_entries: &[&NewScanManifestSeenFilesystemEntry],
        file_paths: &[&'a str],
        indexed_paths: &mut Vec<&'a str>,
    ) -> Result<usize, StorageError> {
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
                        .bind(generation)
                        .bind(chunk.library_root_id)
                        .bind(generation)
                } else {
                    statement
                        .bind(chunk.library_root_id)
                        .bind(generation)
                        .bind(generation)
                };
                let updated = statement
                    .execute(&mut **transaction)
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
                    .bind(generation)
                    .fetch_all(&mut **transaction)
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
            indexed_paths.sort_unstable();
            ledger_paths = file_paths
                .iter()
                .copied()
                .filter(|path| {
                    indexed_paths.binary_search(path).is_err()
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
                .execute(&mut **transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }

        Ok(successfully_seen_paths
            .len()
            .saturating_add(ledger_paths.len()))
    }
}
