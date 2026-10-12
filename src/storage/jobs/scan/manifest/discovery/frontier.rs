use super::*;

impl Database {
    pub(super) async fn insert_manifest_directory_frontier(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Any>,
        chunk: &NewScanManifestDiscoveryChunk<'_>,
        child_directories: &[&str],
        lite_mode: bool,
    ) -> Result<u64, StorageError> {
        let mut inserted_directory_count = 0_u64;
        if lite_mode {
            return Ok(inserted_directory_count);
        }

        for paths in child_directories.chunks(SCAN_DML_CHUNK_SIZE) {
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
                .execute(&mut **transaction)
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

        Ok(inserted_directory_count)
    }

    pub(super) async fn complete_manifest_directory_frontier(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Any>,
        chunk: &NewScanManifestDiscoveryChunk<'_>,
        completed_directories: &[&str],
        lite_mode: bool,
    ) -> Result<i64, StorageError> {
        let mut completed_directory_count = 0_i64;
        if lite_mode {
            return Ok(completed_directory_count);
        }

        for directories in completed_directories.chunks(SCAN_DML_CHUNK_SIZE) {
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
                .execute(&mut **transaction)
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

        Ok(completed_directory_count)
    }
}
