//! Search reindex consumer (W4).
//!
//! Authoritative writes only append `kind = 'search'` rows to `catalog_outbox`; this module turns
//! them into FTS updates in batches. The write path therefore costs one cheap insert per item
//! instead of maintaining `media_search` (including a `group_concat` alias subquery) inside the
//! caller's transaction.
//!
//! The consumer is idempotent: a batch that fails leaves the watermark untouched, and the next run
//! re-derives the same rows from the authoritative tables.

use super::*;

/// Versioned consumer name so a rewrite of the reindex logic can start from a fresh watermark.
pub(crate) const SEARCH_REINDEX_CONSUMER: &str = "search_reindex@1";

impl Database {
    /// Applies at most `batch` queued events, oldest first, and returns how many items were
    /// reindexed. Callers drain until it returns zero.
    pub(crate) async fn drain_search_reindex(&self, batch: i64) -> Result<usize, StorageError> {
        if self.backend == DatabaseBackend::Postgres {
            // PostgreSQL still maintains `media_search` inline; nothing is queued there yet.
            return Ok(0);
        }
        let applied_seq = self
            .catalog_consumer(SEARCH_REINDEX_CONSUMER)
            .await?
            .map(|state| state.applied_seq)
            .unwrap_or(0);
        let entries = self.read_catalog_outbox_after(applied_seq, batch).await?;
        let Some(last_seq) = entries.last().map(|entry| entry.id) else {
            return Ok(0);
        };
        let mut item_ids = entries
            .into_iter()
            .filter(|entry| entry.kind == "search")
            .map(|entry| entry.item_id)
            .collect::<Vec<_>>();
        item_ids.sort_unstable();
        item_ids.dedup();
        if !item_ids.is_empty() {
            self.reindex_search_items(&item_ids).await?;
        }
        self.advance_catalog_consumer(SEARCH_REINDEX_CONSUMER, last_seq)
            .await?;
        Ok(item_ids.len())
    }

    /// Rewrites the FTS rows for `item_ids` from the authoritative tables in one transaction.
    async fn reindex_search_items(&self, item_ids: &[String]) -> Result<(), StorageError> {
        let placeholders = std::iter::repeat_n("?", item_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        // Claim rowids first: FTS5 cannot delete by item_id, and inserts need the mapped rowid.
        let statements = [
            format!(
                "INSERT INTO media_search_map (item_id)
                 SELECT id FROM media_items WHERE id IN ({placeholders})
                 ON CONFLICT(item_id) DO NOTHING"
            ),
            // Drop the current rows (also drops the rows of items that were deleted since).
            format!(
                "DELETE FROM media_search
                 WHERE rowid IN (
                     SELECT fts_rowid FROM media_search_map WHERE item_id IN ({placeholders})
                 )"
            ),
            // Rebuild from the authoritative columns.
            format!(
                "INSERT INTO media_search (rowid, item_id, title, sort_title, original_title, aliases)
                 SELECT map.fts_rowid, item.id, item.title,
                        CASE
                            WHEN item.sort_title = item.title COLLATE NOCASE THEN ''
                            ELSE item.sort_title
                        END,
                        COALESCE(item.original_title, ''),
                        COALESCE(
                            (SELECT group_concat(alias, ' ')
                             FROM item_aliases
                             WHERE item_id = item.id),
                            ''
                        )
                 FROM media_items item
                 JOIN media_search_map map ON map.item_id = item.id
                 WHERE item.id IN ({placeholders})"
            ),
            // Forget the rowid of items that no longer exist.
            format!(
                "DELETE FROM media_search_map
                 WHERE item_id IN ({placeholders})
                   AND NOT EXISTS (
                       SELECT 1 FROM media_items WHERE media_items.id = media_search_map.item_id
                   )"
            ),
        ];
        for sql in statements {
            let mut statement = self.query(sqlx::AssertSqlSafe(sql));
            for item_id in item_ids {
                statement = statement.bind(item_id);
            }
            statement
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
            })
    }
}
