use super::*;
use crate::storage::sql_templates::{
    filesystem_entries_by_paths_capacity, filesystem_entry_records_by_paths_template,
};

impl Database {
    pub(crate) async fn mark_filesystem_entry_missing_by_path(
        &self,
        library_root_id: &str,
        relative_path: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE filesystem_entries
             SET is_missing = 1, updated_at = unixepoch()
             WHERE library_root_id = ? AND relative_path = ?",
        )
        .bind(library_root_id)
        .bind(relative_path)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn find_filesystem_entry(
        &self,
        library_root_id: &str,
        relative_path: &str,
    ) -> Result<Option<StoredFilesystemEntry>, StorageError> {
        self.query(
            "SELECT fe.id, fe.relative_path, fe.entry_kind, fe.size, fe.modified_at,
                    fe.device, fe.inode, fe.fingerprint, fe.last_seen_generation,
                    fe.is_missing, ms.item_id,
                    CASE WHEN parent.removed_at IS NULL THEN parent.identity_key END
                        AS parent_identity_key,
                    item.item_type AS item_type,
                    CASE WHEN item.removed_at IS NULL THEN item.identity_key END
                        AS item_identity_key,
                    series.provider_ids_json AS series_provider_ids_json
             FROM filesystem_entries fe
             LEFT JOIN media_sources ms ON ms.filesystem_entry_id = fe.id
             LEFT JOIN media_items item ON item.id = ms.item_id
             LEFT JOIN media_items parent ON parent.id = item.parent_id
             LEFT JOIN media_items series ON series.id = item.series_id
             WHERE fe.library_root_id = ? AND fe.relative_path = ?",
        )
        .bind(library_root_id)
        .bind(relative_path)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_filesystem_entry))
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn list_filesystem_entries_for_paths(
        &self,
        library_root_id: &str,
        relative_paths: &[String],
    ) -> Result<HashMap<String, StoredFilesystemEntry>, StorageError> {
        let mut entries = HashMap::new();
        for chunk in relative_paths.chunks(500) {
            if chunk.is_empty() {
                continue;
            }
            let template = filesystem_entries_by_paths_capacity(self.backend, chunk.len())
                .and_then(|capacity| {
                    filesystem_entry_records_by_paths_template(
                        &self.filesystem_path_templates,
                        self.backend,
                        capacity,
                    )
                    .map(|sql| (capacity, sql))
                });
            let (mut statement, padding_count) = match template {
                Ok((capacity, sql)) => (
                    self.query(sqlx::AssertSqlSafe(sql)).bind(library_root_id),
                    usize::from(capacity).saturating_sub(chunk.len()),
                ),
                Err(_) => {
                    let placeholders = std::iter::repeat_n("?", chunk.len())
                        .collect::<Vec<_>>()
                        .join(", ");
                    let query = format!(
                        "SELECT fe.id, fe.relative_path, fe.entry_kind, fe.size, fe.modified_at,
                                fe.device, fe.inode, fe.fingerprint, fe.last_seen_generation,
                                fe.is_missing, ms.item_id,
                                CASE WHEN parent.removed_at IS NULL THEN parent.identity_key END
                                    AS parent_identity_key,
                                item.item_type AS item_type,
                                CASE WHEN item.removed_at IS NULL THEN item.identity_key END
                                    AS item_identity_key,
                                series.provider_ids_json AS series_provider_ids_json
                         FROM filesystem_entries fe
                         LEFT JOIN media_sources ms ON ms.filesystem_entry_id = fe.id
                         LEFT JOIN media_items item ON item.id = ms.item_id
                         LEFT JOIN media_items parent ON parent.id = item.parent_id
                         LEFT JOIN media_items series ON series.id = item.series_id
                         WHERE fe.library_root_id = ? AND fe.relative_path IN ({placeholders})"
                    );
                    (
                        self.query(sqlx::AssertSqlSafe(query)).bind(library_root_id),
                        0,
                    )
                }
            };
            for relative_path in chunk {
                statement = statement.bind(relative_path);
            }
            for _ in 0..padding_count {
                statement = statement.bind(None::<String>);
            }
            let rows = statement
                .fetch_all(&self.pool)
                .await
                .map_err(self.sqlx_err())?;
            for row in rows {
                let entry = stored_filesystem_entry(row);
                entries.insert(entry.relative_path.clone(), entry);
            }
        }
        Ok(entries)
    }

    pub(crate) async fn has_filesystem_entries_for_root(
        &self,
        library_root_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar::<i64>(
            "SELECT CASE WHEN EXISTS (
                 SELECT 1 FROM filesystem_entries WHERE library_root_id = ?
             ) THEN 1 ELSE 0 END",
        )
        .bind(library_root_id)
        .fetch_one(&self.pool)
        .await
        .map(|value| value != 0)
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn find_filesystem_entry_by_inode(
        &self,
        library_id: &str,
        target_root_id: &str,
        device: i64,
        inode: i64,
        relative_path: &str,
    ) -> Result<Option<StoredFilesystemEntry>, StorageError> {
        let rows = self
            .query(
                "SELECT fe.id, fe.relative_path, fe.entry_kind, fe.size, fe.modified_at,
                        fe.device, fe.inode, fe.fingerprint, fe.last_seen_generation,
                        fe.is_missing, ms.item_id,
                        CASE WHEN parent.removed_at IS NULL THEN parent.identity_key END
                            AS parent_identity_key,
                        item.item_type AS item_type,
                        CASE WHEN item.removed_at IS NULL THEN item.identity_key END
                            AS item_identity_key,
                        series.provider_ids_json AS series_provider_ids_json
                 FROM filesystem_entries fe
                 JOIN library_roots lr ON lr.id = fe.library_root_id
                 LEFT JOIN media_sources ms ON ms.filesystem_entry_id = fe.id
                 LEFT JOIN media_items item ON item.id = ms.item_id
                 LEFT JOIN media_items parent ON parent.id = item.parent_id
                 LEFT JOIN media_items series ON series.id = item.series_id
                 WHERE lr.library_id = ? AND fe.device = ? AND fe.inode = ?
                   AND NOT (fe.library_root_id = ? AND fe.relative_path = ?)
                 LIMIT 2",
            )
            .bind(library_id)
            .bind(device)
            .bind(inode)
            .bind(target_root_id)
            .bind(relative_path)
            .fetch_all(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        if rows.len() != 1 {
            return Ok(None);
        }
        Ok(rows.into_iter().next().map(stored_filesystem_entry))
    }

    pub(crate) async fn move_filesystem_entry(
        &self,
        entry: FilesystemEntryMove<'_>,
    ) -> Result<(), StorageError> {
        let mut transaction = self.pool.begin().await.map_err(self.sqlx_err())?;
        self.query(
            "UPDATE filesystem_entries
             SET library_root_id = ?, relative_path = ?, size = ?, modified_at = ?,
                 device = ?, inode = ?, fingerprint = ?, last_seen_generation = ?, is_missing = 0,
                 updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(entry.library_root_id)
        .bind(entry.relative_path)
        .bind(entry.size)
        .bind(entry.modified_at)
        .bind(entry.device)
        .bind(entry.inode)
        .bind(entry.fingerprint)
        .bind(entry.generation)
        .bind(entry.entry_id)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;
        self.restore_media_items_for_filesystem_entries(
            &mut transaction,
            &[entry.entry_id.to_owned()],
        )
        .await?;
        transaction.commit().await.map_err(self.sqlx_err())
    }

    pub(crate) async fn update_filesystem_entry_inode(
        &self,
        entry_id: &str,
        inode: Option<i64>,
    ) -> Result<(), StorageError> {
        self.query("UPDATE filesystem_entries SET inode = ?, updated_at = unixepoch() WHERE id = ?")
            .bind(inode)
            .bind(entry_id)
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(self.sqlx_err())
    }

    pub(crate) async fn mark_filesystem_entries_seen_batch(
        &self,
        entry_ids: &[String],
        last_seen_generation: &str,
    ) -> Result<(), StorageError> {
        if entry_ids.is_empty() {
            return Ok(());
        }
        let mut transaction = self.begin_scan_write_transaction().await?;
        self.mark_filesystem_entries_seen_batch_in_transaction(
            &mut transaction,
            entry_ids,
            last_seen_generation,
        )
        .await?;
        transaction.commit().await.map_err(self.sqlx_err())
    }

    pub(super) async fn restore_filesystem_entries_batch_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        entry_ids: &[String],
    ) -> Result<(), StorageError> {
        if entry_ids.is_empty() {
            return Ok(());
        }
        for chunk in entry_ids.chunks(500) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "UPDATE filesystem_entries
                 SET is_missing = 0, updated_at = unixepoch()
                 WHERE is_missing = 1 AND id IN ({placeholders})"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for entry_id in chunk {
                statement = statement.bind(entry_id);
            }
            let restored = statement
                .execute(&mut **transaction)
                .await
                .map_err(self.sqlx_err())?
                .rows_affected();
            if restored > 0 {
                self.restore_media_items_for_filesystem_entries(transaction, chunk)
                    .await?;
            }
        }
        Ok(())
    }

    async fn mark_filesystem_entries_seen_batch_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        entry_ids: &[String],
        last_seen_generation: &str,
    ) -> Result<(), StorageError> {
        if entry_ids.is_empty() {
            return Ok(());
        }
        for chunk in entry_ids.chunks(500) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "UPDATE filesystem_entries
                 SET last_seen_generation = ?, is_missing = 0, updated_at = unixepoch()
                 WHERE id IN ({placeholders})"
            );
            let mut statement = self
                .query(sqlx::AssertSqlSafe(query))
                .bind(last_seen_generation);
            for entry_id in chunk {
                statement = statement.bind(entry_id);
            }
            statement
                .execute(&mut **transaction)
                .await
                .map_err(self.sqlx_err())?;
            self.restore_media_items_for_filesystem_entries(transaction, chunk)
                .await?;
        }
        Ok(())
    }

    pub(crate) async fn update_filesystem_entry(
        &self,
        update: FilesystemEntryUpdate<'_>,
    ) -> Result<(), StorageError> {
        let mut transaction = self.pool.begin().await.map_err(self.sqlx_err())?;
        self.query(
            "UPDATE filesystem_entries
             SET size = ?, modified_at = ?, fingerprint = ?, device = ?, inode = ?, last_seen_generation = ?,
                 is_missing = 0, updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(update.size)
        .bind(update.modified_at)
        .bind(update.fingerprint)
        .bind(update.device)
        .bind(update.inode)
        .bind(update.last_seen_generation)
        .bind(update.id)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;
        self.restore_media_items_for_filesystem_entries(&mut transaction, &[update.id.to_owned()])
            .await?;
        transaction.commit().await.map_err(self.sqlx_err())
    }

    pub(crate) async fn mark_filesystem_entry_seen(
        &self,
        id: &str,
        last_seen_generation: &str,
        device: Option<i64>,
        inode: Option<i64>,
    ) -> Result<(), StorageError> {
        let mut transaction = self.pool.begin().await.map_err(self.sqlx_err())?;
        self.query(
            "UPDATE filesystem_entries
             SET last_seen_generation = ?, device = ?, inode = ?, is_missing = 0, updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(last_seen_generation)
        .bind(device)
        .bind(inode)
        .bind(id)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;
        self.restore_media_items_for_filesystem_entries(&mut transaction, &[id.to_owned()])
            .await?;
        transaction.commit().await.map_err(self.sqlx_err())
    }

    /// Applies the changed-file writes of an incremental scan in one
    /// transaction: filesystem entry, restored availability and probe reset.
    pub(crate) async fn apply_incremental_filesystem_entry_change(
        &self,
        update: FilesystemEntryUpdate<'_>,
        strm_target_kind: Option<&str>,
        strm_target: Option<&str>,
    ) -> Result<(), StorageError> {
        let mut transaction = self.begin_scan_write_transaction().await?;
        self.query(
            "UPDATE filesystem_entries
             SET size = ?, modified_at = ?, fingerprint = ?, device = ?, inode = ?,
                 last_seen_generation = ?, is_missing = 0, updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(update.size)
        .bind(update.modified_at)
        .bind(update.fingerprint)
        .bind(update.device)
        .bind(update.inode)
        .bind(update.last_seen_generation)
        .bind(update.id)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;
        self.restore_media_items_for_filesystem_entries(&mut transaction, &[update.id.to_owned()])
            .await?;
        self.reset_media_probe_for_filesystem_entry_in_transaction(
            &mut transaction,
            update.id,
            update.size,
        )
        .await?;
        if strm_target_kind.is_some() {
            self.query(
                "UPDATE media_sources
                 SET external_url = ?, strm_target_kind = ?, updated_at = unixepoch()
                 WHERE filesystem_entry_id = ?",
            )
            .bind(strm_target)
            .bind(strm_target_kind)
            .bind(update.id)
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        }
        transaction.commit().await.map_err(self.sqlx_err())
    }

    /// Marks an unchanged incremental file as seen and updates its STRM target
    /// in the same transaction.
    pub(crate) async fn mark_filesystem_entry_seen_with_strm_target(
        &self,
        id: &str,
        last_seen_generation: &str,
        device: Option<i64>,
        inode: Option<i64>,
        strm_target_kind: Option<&str>,
        strm_target: Option<&str>,
    ) -> Result<(), StorageError> {
        let mut transaction = self.begin_scan_write_transaction().await?;
        self.query(
            "UPDATE filesystem_entries
             SET last_seen_generation = ?, device = ?, inode = ?, is_missing = 0,
                 updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(last_seen_generation)
        .bind(device)
        .bind(inode)
        .bind(id)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;
        self.restore_media_items_for_filesystem_entries(&mut transaction, &[id.to_owned()])
            .await?;
        if strm_target_kind.is_some() {
            self.query(
                "UPDATE media_sources
                 SET external_url = ?, strm_target_kind = ?, updated_at = unixepoch()
                 WHERE filesystem_entry_id = ?",
            )
            .bind(strm_target)
            .bind(strm_target_kind)
            .bind(id)
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        }
        transaction.commit().await.map_err(self.sqlx_err())
    }

    pub(crate) async fn mark_missing_filesystem_entries(
        &self,
        library_root_id: &str,
        generation: &str,
    ) -> Result<u64, StorageError> {
        let mut transaction = self.begin_scan_write_transaction().await?;
        let missing_entries = self
            .mark_missing_filesystem_entries_in_transaction(
                &mut transaction,
                library_root_id,
                generation,
            )
            .await?;
        transaction.commit().await.map_err(self.sqlx_err())?;
        Ok(missing_entries)
    }

    async fn mark_missing_filesystem_entries_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        library_root_id: &str,
        generation: &str,
    ) -> Result<u64, StorageError> {
        let missing_entries = self
            .query(
                "UPDATE filesystem_entries
             SET is_missing = 1, updated_at = unixepoch()
             WHERE library_root_id = ? AND last_seen_generation != ? AND is_missing = 0",
            )
            .bind(library_root_id)
            .bind(generation)
            .execute(&mut **transaction)
            .await
            .map_err(self.sqlx_err())?
            .rows_affected();
        self.refresh_removed_media_items_in_transaction(transaction, library_root_id)
            .await?;
        Ok(missing_entries)
    }

    pub(super) async fn mark_missing_filesystem_entry_paths_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        library_root_id: &str,
        generation: &str,
        relative_paths: &[String],
    ) -> Result<u64, StorageError> {
        let mut missing_entries = 0_u64;
        for paths in relative_paths.chunks(SCAN_DML_CHUNK_SIZE) {
            if paths.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", paths.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "UPDATE filesystem_entries
                 SET is_missing = 1, updated_at = unixepoch()
                 WHERE library_root_id = ? AND last_seen_generation != ? AND is_missing = 0
                   AND relative_path IN ({placeholders})"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            statement = statement.bind(library_root_id).bind(generation);
            for path in paths {
                statement = statement.bind(path);
            }
            missing_entries = missing_entries.saturating_add(
                statement
                    .execute(&mut **transaction)
                    .await
                    .map_err(self.sqlx_err())?
                    .rows_affected(),
            );
        }
        Ok(missing_entries)
    }
}

#[cfg(all(test, feature = "performance-benchmarks"))]
fn legacy_filesystem_entry_records_query(path_count: usize) -> String {
    let placeholders = std::iter::repeat_n("?", path_count)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "SELECT fe.id, fe.relative_path, fe.entry_kind, fe.size, fe.modified_at,
                fe.device, fe.inode, fe.fingerprint, fe.last_seen_generation,
                fe.is_missing, ms.item_id,
                CASE WHEN parent.removed_at IS NULL THEN parent.identity_key END
                    AS parent_identity_key,
                item.item_type AS item_type,
                CASE WHEN item.removed_at IS NULL THEN item.identity_key END
                    AS item_identity_key,
                series.provider_ids_json AS series_provider_ids_json
         FROM filesystem_entries fe
         LEFT JOIN media_sources ms ON ms.filesystem_entry_id = fe.id
         LEFT JOIN media_items item ON item.id = ms.item_id
         LEFT JOIN media_items parent ON parent.id = item.parent_id
         LEFT JOIN media_items series ON series.id = item.series_id
         WHERE fe.library_root_id = ? AND fe.relative_path IN ({placeholders})"
    )
}

#[cfg(all(test, feature = "performance-benchmarks"))]
#[test]
#[ignore = "run explicitly to compare exact filesystem SQL construction with cached buckets"]
fn filesystem_path_template_construction_benchmark() {
    use std::{hint::black_box, time::Instant};

    let cache = SqlTemplateCache::default();
    let path_count = 127;
    let capacity = filesystem_entries_by_paths_capacity(DatabaseBackend::Sqlite, path_count)
        .expect("valid filesystem path count");
    assert_eq!(capacity, 128);
    let cached =
        filesystem_entry_records_by_paths_template(&cache, DatabaseBackend::Sqlite, capacity)
            .expect("cached filesystem path template");
    assert!(cached.contains("WHERE fe.library_root_id = ? AND fe.relative_path IN ("));
    assert!(legacy_filesystem_entry_records_query(path_count).contains("IN ("));

    let iterations = 50_000;
    let mut exact_rounds = Vec::with_capacity(5);
    let mut cached_rounds = Vec::with_capacity(5);
    for _ in 0..5 {
        let started = Instant::now();
        for _ in 0..iterations {
            black_box(legacy_filesystem_entry_records_query(path_count));
        }
        exact_rounds.push(started.elapsed().as_nanos() / iterations);

        let started = Instant::now();
        for _ in 0..iterations {
            let capacity =
                filesystem_entries_by_paths_capacity(DatabaseBackend::Sqlite, path_count)
                    .expect("valid filesystem path count");
            black_box(
                filesystem_entry_records_by_paths_template(
                    &cache,
                    DatabaseBackend::Sqlite,
                    capacity,
                )
                .expect("cached filesystem path template"),
            );
        }
        cached_rounds.push(started.elapsed().as_nanos() / iterations);
    }
    println!("filesystem_exact_sql_ns_per_op={exact_rounds:?}");
    println!("filesystem_cached_bucket_sql_ns_per_op={cached_rounds:?}");
}
