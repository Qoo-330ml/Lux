// Four bind values per path; 100 paths stays below SQLite's conservative parameter limit.
const INCREMENTAL_SCAN_PATH_BATCH_SIZE: usize = 100;

use super::*;

impl Database {
    pub(crate) async fn enqueue_incremental_scan_path(
        &self,
        job_id: &str,
        library_root_id: &str,
        relative_path: &str,
        change_kind: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO scan_job_paths (
                job_id, library_root_id, relative_path, change_kind
             ) VALUES (?, ?, ?, ?)
             ON CONFLICT(job_id, library_root_id, relative_path) DO UPDATE SET
                change_kind = excluded.change_kind,
                processed_at = NULL,
                updated_at = unixepoch()",
        )
        .bind(job_id)
        .bind(library_root_id)
        .bind(relative_path)
        .bind(change_kind)
        .execute(&self.pool)
        .await
        .map_err(self.sqlx_err())?;
        self.refresh_incremental_scan_path_count(job_id).await
    }

    pub(crate) async fn enqueue_incremental_scan_paths(
        &self,
        job_id: &str,
        paths: &[(&str, &str, &str)],
    ) -> Result<(), StorageError> {
        if paths.is_empty() {
            return Ok(());
        }
        if paths.len() == 1 {
            let (library_root_id, relative_path, change_kind) = paths[0];
            return self
                .enqueue_incremental_scan_path(job_id, library_root_id, relative_path, change_kind)
                .await;
        }

        let mut latest_path_indexes: std::collections::HashMap<(&str, &str), usize> =
            std::collections::HashMap::with_capacity(paths.len());
        let mut unique_paths: Vec<(&str, &str, &str)> = Vec::with_capacity(paths.len());
        for &(library_root_id, relative_path, change_kind) in paths {
            let path_key = (library_root_id, relative_path);
            if let Some(index) = latest_path_indexes.get(&path_key).copied() {
                unique_paths[index].2 = change_kind;
            } else {
                latest_path_indexes.insert(path_key, unique_paths.len());
                unique_paths.push((library_root_id, relative_path, change_kind));
            }
        }
        if unique_paths.len() == 1 {
            let (library_root_id, relative_path, change_kind) = unique_paths[0];
            return self
                .enqueue_incremental_scan_path(job_id, library_root_id, relative_path, change_kind)
                .await;
        }

        for batch in unique_paths.chunks(INCREMENTAL_SCAN_PATH_BATCH_SIZE) {
            let values = (0..batch.len())
                .map(|_| "(?, ?, ?, ?)")
                .collect::<Vec<_>>()
                .join(", ");
            let statement = format!(
                "INSERT INTO scan_job_paths (
                    job_id, library_root_id, relative_path, change_kind
                 ) VALUES {values}
                 ON CONFLICT(job_id, library_root_id, relative_path) DO UPDATE SET
                    change_kind = excluded.change_kind,
                    processed_at = NULL,
                    updated_at = unixepoch()"
            );
            let mut query = self.query(sqlx::AssertSqlSafe(statement));
            for (library_root_id, relative_path, change_kind) in batch {
                query = query
                    .bind(job_id)
                    .bind(library_root_id)
                    .bind(relative_path)
                    .bind(change_kind);
            }
            query.execute(&self.pool).await.map_err(self.sqlx_err())?;
            self.refresh_incremental_scan_path_count(job_id).await?;
        }
        Ok(())
    }

    async fn refresh_incremental_scan_path_count(&self, job_id: &str) -> Result<(), StorageError> {
        self.query(
            "UPDATE scan_jobs
             SET total_count = (
                 SELECT COUNT(*) FROM scan_job_paths
                 WHERE job_id = ?
             ), updated_at = unixepoch()
             WHERE id = ? AND status IN ('PENDING', 'RUNNING')",
        )
        .bind(job_id)
        .bind(job_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn list_pending_scan_job_paths(
        &self,
        job_id: &str,
        limit: i64,
    ) -> Result<Vec<StoredScanJobPath>, StorageError> {
        self.query(
            "SELECT job_id, library_root_id, relative_path, change_kind
             FROM scan_job_paths
             WHERE job_id = ? AND processed_at IS NULL
             ORDER BY created_at, relative_path
             LIMIT ?",
        )
        .bind(job_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(self.sqlx_err())?
        .into_iter()
        .map(stored_scan_job_path)
        .collect::<Result<Vec<_>, _>>()
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn mark_scan_job_path_processed(
        &self,
        job_id: &str,
        library_root_id: &str,
        relative_path: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE scan_job_paths
             SET processed_at = unixepoch(), updated_at = unixepoch()
             WHERE job_id = ? AND library_root_id = ? AND relative_path = ?",
        )
        .bind(job_id)
        .bind(library_root_id)
        .bind(relative_path)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn list_media_item_ids_for_incremental_scan(
        &self,
        job_id: &str,
    ) -> Result<Vec<String>, StorageError> {
        self.query_scalar(
            "WITH affected_items AS (
                 SELECT DISTINCT ms.item_id
                 FROM scan_job_paths sjp
                 JOIN filesystem_entries fe
                   ON fe.library_root_id = sjp.library_root_id
                  AND (
                        sjp.relative_path = '.'
                        OR
                        fe.relative_path = sjp.relative_path
                        OR substr(fe.relative_path, 1, length(sjp.relative_path) + 1)
                           = sjp.relative_path || '/'
                      )
                 JOIN media_sources ms ON ms.filesystem_entry_id = fe.id
                 JOIN media_items mi ON mi.id = ms.item_id
                 WHERE sjp.job_id = ?
                   AND sjp.processed_at IS NOT NULL
                   AND fe.is_missing = 0
                   AND mi.removed_at IS NULL
             ),
             metadata_targets AS (
                 SELECT item_id
                 FROM affected_items
                 UNION
                 SELECT mi.parent_id
                 FROM media_items mi
                 JOIN affected_items affected ON affected.item_id = mi.id
                 WHERE mi.item_type = 'EPISODE'
                   AND mi.parent_id IS NOT NULL
                 UNION
                 SELECT mi.series_id
                 FROM media_items mi
                 JOIN affected_items affected ON affected.item_id = mi.id
                 WHERE mi.item_type = 'EPISODE'
                   AND mi.series_id IS NOT NULL
             )
             SELECT DISTINCT target.id
             FROM metadata_targets targets
             JOIN media_items target ON target.id = targets.item_id
             WHERE target.removed_at IS NULL AND target.item_type <> 'VIDEO'
             ORDER BY target.id",
        )
        .bind(job_id)
        .fetch_all(&self.pool)
        .await
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn clear_scan_job_paths(&self, job_id: &str) -> Result<(), StorageError> {
        self.query("DELETE FROM scan_job_paths WHERE job_id = ?")
            .bind(job_id)
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(self.sqlx_err())
    }
}
