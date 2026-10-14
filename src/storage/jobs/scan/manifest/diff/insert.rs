use super::*;

impl Database {
    pub(super) async fn insert_manifest_delta_rows_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        manifest_id: &str,
        deltas: &[NewScanManifestDelta<'_>],
    ) -> Result<(usize, [i64; 4]), StorageError> {
        let mut batch_counts = [0_i64; 4];
        let mut batch_inserted_count = 0_usize;
        for chunk in deltas.chunks(SCAN_MANIFEST_DELTA_BATCH_SIZE) {
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
                .fetch_optional(&mut **transaction)
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
                .fetch_all(&mut **transaction)
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
        Ok((batch_inserted_count, batch_counts))
    }
}
