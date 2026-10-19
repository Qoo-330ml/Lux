use super::*;
use crate::storage::sql_templates::{
    manifest_presence_readback_template, manifest_presence_update_template,
    manifest_seen_paths_insert_template,
};
use std::borrow::Cow;

pub(super) struct ManifestPresenceLedgerInput<'input, 'data>
where
    'data: 'input,
{
    pub(super) chunk: &'input NewScanManifestDiscoveryChunk<'data>,
    pub(super) discovery_format_version: i64,
    pub(super) generation: &'input str,
    pub(super) seen_filesystem_entries: &'input [&'data NewScanManifestSeenFilesystemEntry],
    pub(super) file_paths: &'input [&'data str],
    pub(super) indexed_paths: &'input mut Vec<&'data str>,
}

impl Database {
    pub(super) async fn update_manifest_presence_ledger<'input, 'data>(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Any>,
        input: ManifestPresenceLedgerInput<'input, 'data>,
    ) -> Result<usize, StorageError>
    where
        'data: 'input,
    {
        let ManifestPresenceLedgerInput {
            chunk,
            discovery_format_version,
            generation,
            seen_filesystem_entries,
            file_paths,
            indexed_paths,
        } = input;
        let mut ledger_paths = Vec::new();
        let mut successfully_seen_paths: std::collections::HashSet<Cow<'data, str>> =
            std::collections::HashSet::new();
        if discovery_format_version == 3 && !seen_filesystem_entries.is_empty() {
            for entries in seen_filesystem_entries.chunks(
                super::manifest_seen_filesystem_update_chunk_size(self.backend()),
            ) {
                if entries.is_empty() {
                    continue;
                }
                let postgres_update_from = self.backend() == DatabaseBackend::Postgres;
                let template = manifest_presence_update_template(
                    &self.filesystem_path_templates,
                    self.backend(),
                    entries.len(),
                );
                let mut statement = match template {
                    Ok(sql) => self.query(sqlx::AssertSqlSafe(sql)),
                    Err(_) => {
                        let values = std::iter::repeat_n("(?, ?, ?, ?, ?, ?, ?)", entries.len())
                            .collect::<Vec<_>>()
                            .join(", ");
                        let query = if postgres_update_from {
                            format!(
                                "WITH incoming(
                                     filesystem_entry_id, relative_path, fingerprint, size,
                                     modified_at, device, inode
                                 ) AS (VALUES {values})
                                 UPDATE filesystem_entries AS current
                                 SET size = incoming.size,
                                     modified_at = incoming.modified_at,
                                     device = incoming.device,
                                     inode = incoming.inode,
                                     last_seen_generation = ?,
                                     last_seen_change_kind = NULL
                                 FROM incoming
                                 WHERE current.id = incoming.filesystem_entry_id
                                   AND current.library_root_id = ?
                                   AND current.relative_path = incoming.relative_path
                                   AND current.fingerprint = incoming.fingerprint
                                   AND current.entry_kind = 'FILE'
                                   AND current.is_missing = 0
                                   AND current.entry_kind = 'FILE'
                                   AND current.is_missing = 0"
                            )
                        } else {
                            format!(
                                "WITH incoming(
                                          filesystem_entry_id, relative_path, fingerprint, size,
                                          modified_at, device, inode
                                      ) AS (VALUES {values}),
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
                                 SET size = (SELECT size FROM incoming WHERE filesystem_entry_id = filesystem_entries.id),
                                     modified_at = (SELECT modified_at FROM incoming WHERE filesystem_entry_id = filesystem_entries.id),
                                     device = (SELECT device FROM incoming WHERE filesystem_entry_id = filesystem_entries.id),
                                     inode = (SELECT inode FROM incoming WHERE filesystem_entry_id = filesystem_entries.id),
                                     last_seen_generation = ?,
                                     last_seen_change_kind = NULL
                                 WHERE id IN (SELECT filesystem_entry_id FROM matching)"
                            )
                        };
                        self.query(sqlx::AssertSqlSafe(query))
                    }
                };
                for entry in entries {
                    statement = statement
                        .bind(&entry.filesystem_entry_id)
                        .bind(&entry.relative_path)
                        .bind(&entry.fingerprint)
                        .bind(entry.size)
                        .bind(entry.modified_at)
                        .bind(entry.device)
                        .bind(entry.inode);
                }
                statement = if postgres_update_from {
                    statement.bind(generation).bind(chunk.library_root_id)
                } else {
                    statement.bind(chunk.library_root_id).bind(generation)
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
                    successfully_seen_paths.extend(
                        entries
                            .iter()
                            .map(|entry| Cow::Borrowed(entry.relative_path.as_str())),
                    );
                    continue;
                }

                let template = manifest_presence_readback_template(
                    &self.filesystem_path_templates,
                    self.backend(),
                    entries.len(),
                );
                let mut statement = match template {
                    Ok(sql) => self.query(sqlx::AssertSqlSafe(sql)),
                    Err(_) => {
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
                        self.query(sqlx::AssertSqlSafe(query))
                    }
                };
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
                        .map(|row| Cow::Owned(row.get::<String, _>("relative_path"))),
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
            let template = manifest_seen_paths_insert_template(
                &self.filesystem_path_templates,
                self.backend(),
                paths.len(),
            );
            let mut statement = match template {
                Ok(sql) => self.query(sqlx::AssertSqlSafe(sql)),
                Err(_) => {
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
                    self.query(sqlx::AssertSqlSafe(query))
                }
            };
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

        self.record_scan_job_processed_media_items_in_transaction(
            transaction,
            chunk,
            generation,
            file_paths,
        )
        .await?;

        Ok(successfully_seen_paths
            .len()
            .saturating_add(ledger_paths.len()))
    }

    async fn record_scan_job_processed_media_items_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Any>,
        chunk: &NewScanManifestDiscoveryChunk<'_>,
        generation: &str,
        file_paths: &[&str],
    ) -> Result<(), StorageError> {
        if file_paths.is_empty() {
            return Ok(());
        }

        let chunk_size = (super::manifest_path_query_chunk_size(self.backend()) / 4).max(1);
        let mut processed_media_count = 0_u64;
        for paths in file_paths.chunks(chunk_size) {
            let values = std::iter::repeat_n("(?)", paths.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "WITH requested_paths(relative_path) AS (VALUES {values})
                 INSERT INTO scan_job_processed_media_items (job_id, media_item_id)
                 SELECT job.id,
                        CASE WHEN item.item_type = 'MOVIE' THEN item.id
                             ELSE item.series_id END
                 FROM requested_paths
                 JOIN scan_jobs job
                   ON job.id = ? AND job.job_type = 'RECONCILE_LIBRARY'
                  AND job.status = 'RUNNING' AND job.cancel_requested = 0
                  AND job.processed_media_count IS NOT NULL
                 JOIN filesystem_entries entry
                   ON entry.library_root_id = ?
                  AND entry.relative_path = requested_paths.relative_path
                  AND entry.last_seen_generation = ? AND entry.is_missing = 0
                 JOIN media_sources source ON source.filesystem_entry_id = entry.id
                 JOIN media_items item ON item.id = source.item_id
                 WHERE item.removed_at IS NULL
                   AND item.item_type IN ('MOVIE', 'EPISODE')
                   AND (item.item_type = 'MOVIE' OR item.series_id IS NOT NULL)
                   AND 1 = 1
                 GROUP BY job.id,
                          CASE WHEN item.item_type = 'MOVIE' THEN item.id
                               ELSE item.series_id END
                 ON CONFLICT(job_id, media_item_id) DO NOTHING"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for path in paths {
                statement = statement.bind(path);
            }
            let inserted = statement
                .bind(chunk.job_id)
                .bind(chunk.library_root_id)
                .bind(generation)
                .execute(&mut **transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            processed_media_count = processed_media_count
                .checked_add(inserted.rows_affected())
                .ok_or_else(|| {
                    StorageError::Conflict("processed media count overflow".to_owned())
                })?;
        }

        if processed_media_count == 0 {
            return Ok(());
        }
        let processed_media_count = i64::try_from(processed_media_count)
            .map_err(|_| StorageError::Conflict("processed media count overflow".to_owned()))?;
        let updated = self
            .query(
                "UPDATE scan_jobs
                 SET processed_media_count = processed_media_count + ?,
                     updated_at = unixepoch()
                 WHERE id = ? AND job_type = 'RECONCILE_LIBRARY'
                   AND status = 'RUNNING' AND cancel_requested = 0
                   AND processed_media_count IS NOT NULL",
            )
            .bind(processed_media_count)
            .bind(chunk.job_id)
            .execute(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if updated.rows_affected() != 1 {
            return Err(StorageError::Conflict(
                "scan job stopped accepting processed media progress".to_owned(),
            ));
        }
        Ok(())
    }
}
