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

/// Outbox kind owned by this consumer; the catalog read model uses `catalog`.
pub(crate) const SEARCH_REINDEX_KIND: &str = "search";

impl Database {
    /// Applies at most `batch` queued events, oldest first, and returns how many items were
    /// reindexed. Callers drain until it returns zero.
    ///
    /// Public so integration tests can exercise the consumer on both backends; application code
    /// reaches it through the search request path and the background worker.
    pub async fn drain_search_reindex(&self, batch: i64) -> Result<usize, StorageError> {
        // A trimmed or lost queue means events were skipped; re-enqueue everything rather than
        // leaving the index permanently missing rows.
        if let Some((applied_seq, oldest_remaining)) = self
            .catalog_outbox_gap(SEARCH_REINDEX_CONSUMER, SEARCH_REINDEX_KIND)
            .await?
        {
            tracing::warn!(
                applied_seq,
                oldest_remaining,
                "search reindex queue gap; scheduling a full reindex"
            );
            self.enqueue_full_search_reindex().await?;
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

    /// Queues every item once; used after a detected queue gap so the index cannot stay incomplete.
    pub(crate) async fn enqueue_full_search_reindex(&self) -> Result<i64, StorageError> {
        self.query(
            "INSERT INTO catalog_outbox (kind, item_id)
             SELECT 'search', id FROM media_items",
        )
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() as i64)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    /// Rewrites the search rows for `item_ids` from the authoritative tables in one transaction.
    async fn reindex_search_items(&self, item_ids: &[String]) -> Result<(), StorageError> {
        if self.backend == DatabaseBackend::Postgres {
            return self.reindex_search_items_postgres(item_ids).await;
        }
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

    /// PostgreSQL variant: `media_search` is a plain table with a unique item_id, so the consumer
    /// upserts from the authoritative tables and drops rows for items that no longer exist.
    async fn reindex_search_items_postgres(&self, item_ids: &[String]) -> Result<(), StorageError> {
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
        let upsert = format!(
            "INSERT INTO media_search (item_id, title, sort_title, original_title, aliases)
             SELECT item.id, item.title, item.sort_title, COALESCE(item.original_title, ''),
                    COALESCE((SELECT string_agg(alias.alias, ' ')
                              FROM item_aliases alias
                              WHERE alias.item_id = item.id), '')
             FROM media_items item
             WHERE item.id IN ({placeholders})
             ON CONFLICT (item_id) DO UPDATE SET
                 title = EXCLUDED.title,
                 sort_title = EXCLUDED.sort_title,
                 original_title = EXCLUDED.original_title,
                 aliases = EXCLUDED.aliases"
        );
        let drop_missing = format!(
            "DELETE FROM media_search
             WHERE item_id IN ({placeholders})
               AND NOT EXISTS (
                   SELECT 1 FROM media_items WHERE media_items.id = media_search.item_id
               )"
        );
        for sql in [upsert, drop_missing] {
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
