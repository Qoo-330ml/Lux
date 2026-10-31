//! Read-model fan-out primitives: append-only outbox plus per-consumer watermarks.

use super::catalog::{catalog_stream_columns, catalog_stream_join, catalog_stream_order};
use super::*;

/// One independent event stream: its own table (and therefore its own id sequence) plus the kind it
/// carries. Streams never share a sequence, so a consumer's watermark and the "is there a hole"
/// check are always expressed in the coordinates of the one stream it reads.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) struct OutboxStream {
    pub(crate) table: &'static str,
    pub(crate) kind: &'static str,
}

/// `catalog_items` maintenance events.
pub(crate) const CATALOG_OUTBOX: OutboxStream = OutboxStream {
    table: "catalog_outbox",
    kind: "catalog",
};

/// Search reindex events. Before migration `0243` these shared `catalog_outbox`.
pub(crate) const SEARCH_OUTBOX: OutboxStream = OutboxStream {
    table: "search_outbox",
    kind: "search",
};

/// A full re-queue copies every item into the stream, so it is rate limited per consumer.
const FULL_REQUEUE_COOLDOWN_SECONDS: i64 = 300;

/// A stream already holding more pending rows than this is not multiplied by another full re-queue:
/// the consumer is far behind and adding one row per item only deepens the backlog.
pub(crate) const OUTBOX_MAX_PENDING_ROWS: i64 = 2_000_000;

/// Versioned consumer name for incremental `catalog_items` maintenance. A change to the derived row
/// shape bumps `READ_MODEL_SCHEMA_VERSION` instead, which forces a full rebuild.
pub(crate) const CATALOG_ITEMS_INCREMENTAL_CONSUMER: &str = "catalog_items_incremental@1";

#[derive(Debug, Clone, Eq, PartialEq, sqlx::FromRow)]
pub(crate) struct CatalogOutboxEntry {
    pub(crate) id: i64,
    pub(crate) kind: String,
    pub(crate) item_id: String,
    pub(crate) library_id: Option<String>,
}

#[derive(Debug, Clone, Eq, PartialEq, sqlx::FromRow)]
pub(crate) struct CatalogConsumerState {
    pub(crate) name: String,
    pub(crate) applied_seq: i64,
    pub(crate) updated_at: i64,
    pub(crate) last_error: Option<String>,
}

impl Database {
    /// Appends one derived-data event and returns its sequence number.
    #[cfg(test)]
    pub(crate) async fn append_catalog_outbox(
        &self,
        kind: &str,
        item_id: &str,
        library_id: Option<&str>,
    ) -> Result<i64, StorageError> {
        self.query_scalar(
            "INSERT INTO catalog_outbox (kind, item_id, library_id)
             VALUES (?, ?, ?)
             RETURNING id",
        )
        .bind(kind)
        .bind(item_id)
        .bind(library_id)
        .fetch_one(&self.pool)
        .await
        .map_err(self.sqlx_err())
    }

    /// Reads at most `limit` events of the stream's kind after `after_seq`, oldest first.
    ///
    /// The kind is part of the predicate (served by the `(kind, id)` index), so a batch never
    /// contains rows the consumer would discard.
    pub(crate) async fn read_outbox_after(
        &self,
        stream: OutboxStream,
        after_seq: i64,
        limit: i64,
    ) -> Result<Vec<CatalogOutboxEntry>, StorageError> {
        self.query_as(sqlx::AssertSqlSafe(format!(
            "SELECT id, kind, item_id, library_id
             FROM {table}
             WHERE kind = ? AND id > ?
             ORDER BY id
             LIMIT ?",
            table = stream.table
        )))
        .bind(stream.kind)
        .bind(after_seq)
        .bind(limit.clamp(1, 5_000))
        .fetch_all(&self.pool)
        .await
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn catalog_consumer(
        &self,
        name: &str,
    ) -> Result<Option<CatalogConsumerState>, StorageError> {
        self.query_as(
            "SELECT name, applied_seq, updated_at, last_error
             FROM catalog_consumers WHERE name = ?",
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(self.sqlx_err())
    }

    /// Advances a consumer watermark. The watermark never moves backwards, so a retried batch is
    /// idempotent.
    pub(crate) async fn advance_catalog_consumer(
        &self,
        name: &str,
        applied_seq: i64,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO catalog_consumers (name, applied_seq)
             VALUES (?, ?)
             ON CONFLICT(name) DO UPDATE SET
                 applied_seq = excluded.applied_seq,
                 updated_at = unixepoch(),
                 last_error = NULL
             WHERE catalog_consumers.applied_seq < excluded.applied_seq",
        )
        .bind(name)
        .bind(applied_seq)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn record_catalog_consumer_error(
        &self,
        name: &str,
        error: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO catalog_consumers (name, last_error)
             VALUES (?, ?)
             ON CONFLICT(name) DO UPDATE SET
                 updated_at = unixepoch(),
                 last_error = excluded.last_error",
        )
        .bind(name)
        .bind(error)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(self.sqlx_err())
    }

    /// Drops the already-applied events of one stream, oldest first and bounded so a long backlog
    /// never turns into one huge delete.
    ///
    /// Callers run this after every applied batch, not only once the queue is empty: a consumer that
    /// is permanently behind would otherwise never reclaim anything.
    pub(crate) async fn trim_outbox(
        &self,
        consumer: &str,
        stream: OutboxStream,
        limit: i64,
    ) -> Result<i64, StorageError> {
        let applied_seq = self
            .catalog_consumer(consumer)
            .await?
            .map(|state| state.applied_seq)
            .unwrap_or(0);
        if applied_seq <= 0 {
            return Ok(0);
        }
        self.query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM {table}
             WHERE kind = ?
               AND id IN (
                   SELECT id FROM {table} WHERE kind = ? AND id <= ? ORDER BY id LIMIT ?
               )",
            table = stream.table
        )))
        .bind(stream.kind)
        .bind(stream.kind)
        .bind(applied_seq)
        .bind(limit.clamp(1, 10_000))
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() as i64)
        .map_err(self.sqlx_err())
    }

    /// Removes `search` rows that older builds appended to `catalog_outbox` (the stream moved to
    /// `search_outbox`). Nothing reads them any more; deleting in bounded batches keeps an upgrade
    /// over a large backlog from turning into one huge statement.
    pub(crate) async fn purge_legacy_search_outbox(&self, limit: i64) -> Result<i64, StorageError> {
        self.query(
            "DELETE FROM catalog_outbox
             WHERE id IN (
                 SELECT id FROM catalog_outbox WHERE kind = 'search' ORDER BY id LIMIT ?
             )",
        )
        .bind(limit.clamp(1, 10_000))
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() as i64)
        .map_err(self.sqlx_err())
    }

    /// Detects that the stream was trimmed past a consumer's watermark.
    ///
    /// Returns `(applied_seq, oldest_remaining_seq)` when the next expected sequence is gone, so the
    /// caller can schedule a full rebuild instead of silently skipping derived rows. Both numbers are
    /// ids of the same stream, which is what makes the comparison meaningful.
    pub(crate) async fn outbox_gap(
        &self,
        consumer: &str,
        stream: OutboxStream,
    ) -> Result<Option<(i64, i64)>, StorageError> {
        let applied_seq = self
            .catalog_consumer(consumer)
            .await?
            .map(|state| state.applied_seq)
            .unwrap_or(0);
        let oldest = self
            .query_scalar::<Option<i64>>(sqlx::AssertSqlSafe(format!(
                "SELECT MIN(id) FROM {table} WHERE kind = ?",
                table = stream.table
            )))
            .bind(stream.kind)
            .fetch_optional(&self.pool)
            .await
            .map_err(self.sqlx_err())?
            .flatten();
        Ok(oldest.and_then(|oldest| {
            (oldest > applied_seq.saturating_add(1)).then_some((applied_seq, oldest))
        }))
    }

    /// Decides whether a detected gap may be answered with a full re-queue.
    pub(crate) async fn full_requeue_decision(
        &self,
        consumer: &str,
        stream: OutboxStream,
        max_pending: i64,
    ) -> Result<FullRequeue, StorageError> {
        let cooling_down: i64 = self
            .query_scalar(
                "SELECT COUNT(*) FROM catalog_consumers
                 WHERE name = ? AND updated_at > unixepoch() - ?",
            )
            .bind(full_requeue_marker(consumer))
            .bind(FULL_REQUEUE_COOLDOWN_SECONDS)
            .fetch_one(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        if cooling_down > 0 {
            return Ok(FullRequeue::CoolingDown);
        }
        // Bounded count: stop looking after `max_pending + 1` rows so a runaway table is cheap to judge.
        let pending: i64 = self
            .query_scalar(sqlx::AssertSqlSafe(format!(
                "SELECT COUNT(*) FROM (
                     SELECT 1 FROM {table} WHERE kind = ? LIMIT ?
                 ) AS bounded",
                table = stream.table
            )))
            .bind(stream.kind)
            .bind(max_pending.saturating_add(1))
            .fetch_one(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        Ok(if pending > max_pending {
            FullRequeue::OverCapacity
        } else {
            FullRequeue::Allowed
        })
    }

    /// Stamps the time of a full re-queue; the stamp lives in `catalog_consumers` under a derived
    /// name so no schema change is needed and the cooldown survives a restart.
    pub(crate) async fn record_full_requeue(&self, consumer: &str) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO catalog_consumers (name, applied_seq)
             VALUES (?, 1)
             ON CONFLICT(name) DO UPDATE SET
                 applied_seq = catalog_consumers.applied_seq + 1,
                 updated_at = unixepoch()",
        )
        .bind(full_requeue_marker(consumer))
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(self.sqlx_err())
    }

    /// Answers a detected gap exactly once.
    ///
    /// A full re-queue covers any events that were lost, but it is only issued when the consumer is
    /// not in its cooldown and the stream is not already over capacity. Either way the watermark
    /// is moved to just below the oldest remaining event, so the same hole is never detected again:
    /// re-detecting it every tick is what turned one repair into an unbounded write loop.
    pub(crate) async fn repair_outbox_gap(
        &self,
        consumer: &str,
        stream: OutboxStream,
    ) -> Result<(), StorageError> {
        let Some((applied_seq, oldest_remaining)) = self.outbox_gap(consumer, stream).await? else {
            return Ok(());
        };
        match self
            .full_requeue_decision(consumer, stream, OUTBOX_MAX_PENDING_ROWS)
            .await?
        {
            FullRequeue::Allowed => {
                let requeued = self.enqueue_full_outbox(stream).await?;
                self.record_full_requeue(consumer).await?;
                tracing::warn!(
                    consumer,
                    applied_seq,
                    oldest_remaining,
                    requeued,
                    "outbox gap; re-queued every item once"
                );
            }
            FullRequeue::CoolingDown => tracing::warn!(
                consumer,
                applied_seq,
                oldest_remaining,
                "outbox gap inside the re-queue cooldown; closing it without another full re-queue"
            ),
            FullRequeue::OverCapacity => tracing::error!(
                consumer,
                applied_seq,
                oldest_remaining,
                max_pending = OUTBOX_MAX_PENDING_ROWS,
                "outbox gap on an over-capacity queue; refusing a full re-queue"
            ),
        }
        self.advance_catalog_consumer(consumer, oldest_remaining.saturating_sub(1))
            .await
    }

    /// Queues every authoritative item once into the stream.
    pub(crate) async fn enqueue_full_outbox(
        &self,
        stream: OutboxStream,
    ) -> Result<i64, StorageError> {
        self.query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO {table} (kind, item_id)
             SELECT '{kind}', id FROM media_items",
            table = stream.table,
            kind = stream.kind
        )))
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() as i64)
        .map_err(self.sqlx_err())
    }
}

/// Outcome of [`Database::full_requeue_decision`].
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum FullRequeue {
    Allowed,
    CoolingDown,
    OverCapacity,
}

fn full_requeue_marker(consumer: &str) -> String {
    format!("{consumer}#requeue")
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct CatalogReadModelSummary {
    pub(crate) item_count: i64,
    pub(crate) content_sha256: String,
}

impl Database {
    #[cfg(test)]
    pub(crate) async fn catalog_items_count(&self) -> Result<i64, StorageError> {
        self.query_scalar("SELECT COUNT(*) FROM catalog_items")
            .fetch_one(&self.pool)
            .await
            .map_err(self.sqlx_err())
    }

    /// Highest item id already materialized, used to resume an interrupted rebuild.
    pub(crate) async fn catalog_items_max_item_id(&self) -> Result<Option<String>, StorageError> {
        self.query_scalar("SELECT MAX(item_id) FROM catalog_items")
            .fetch_optional(&self.pool)
            .await
            .map(|row| row.flatten())
            .map_err(self.sqlx_err())
    }

    /// Rebuilds one keyset page of `catalog_items` from the authoritative tables.
    ///
    /// Returns `(rows_processed, last_item_id)`; the caller resumes with `after = last_item_id`.
    pub(crate) async fn rebuild_catalog_items_batch(
        &self,
        after_item_id: Option<&str>,
        limit: i64,
    ) -> Result<(u64, Option<String>), StorageError> {
        let limit = limit.clamp(1, 5_000);
        let after = after_item_id.unwrap_or("");
        let rows: Vec<String> = self
            .query_scalar(
                "SELECT id FROM media_items
                 WHERE id > ?
                 ORDER BY id
                 LIMIT ?",
            )
            .bind(after)
            .bind(limit)
            .fetch_all(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        if rows.is_empty() {
            return Ok((0, None));
        }
        let last = rows.last().cloned();
        let upsert_sql = format!(
            "{CATALOG_ITEM_UPSERT_SELECT}mi.id > ? AND mi.id <= ?{CATALOG_ITEM_UPSERT_CONFLICT}"
        );
        let upsert = self
            .query(sqlx::AssertSqlSafe(upsert_sql))
            .bind(after)
            .bind(last.as_deref())
            .execute(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        Ok((upsert.rows_affected(), last))
    }

    /// Re-derives the `catalog_items` row of every given item from the authoritative tables.
    ///
    /// Returns the number of affected rows; an item that no longer exists is handled by
    /// [`Self::delete_orphaned_catalog_items`].
    pub(crate) async fn upsert_catalog_items_by_ids(
        &self,
        item_ids: &[String],
    ) -> Result<u64, StorageError> {
        if item_ids.is_empty() {
            return Ok(0);
        }
        let placeholders = vec!["?"; item_ids.len()].join(", ");
        let sql = format!(
            "{CATALOG_ITEM_UPSERT_SELECT}mi.id IN ({placeholders}){CATALOG_ITEM_UPSERT_CONFLICT}"
        );
        let mut statement = self.query(sqlx::AssertSqlSafe(sql));
        for item_id in item_ids {
            statement = statement.bind(item_id);
        }
        statement
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected())
            .map_err(self.sqlx_err())
    }

    /// Removes derived rows whose authoritative item is gone, so a deletion disappears from the
    /// read-model paths on the next consumer pass instead of waiting for a rebuild.
    pub(crate) async fn delete_orphaned_catalog_items(
        &self,
        item_ids: &[String],
    ) -> Result<u64, StorageError> {
        if item_ids.is_empty() {
            return Ok(0);
        }
        let placeholders = vec!["?"; item_ids.len()].join(", ");
        let sql = format!(
            "DELETE FROM catalog_items
             WHERE item_id IN ({placeholders})
               AND NOT EXISTS (
                   SELECT 1 FROM media_items WHERE media_items.id = catalog_items.item_id
               )"
        );
        let mut statement = self.query(sqlx::AssertSqlSafe(sql));
        for item_id in item_ids {
            statement = statement.bind(item_id);
        }
        statement
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected())
            .map_err(self.sqlx_err())
    }

    /// Applies at most `batch` queued `kind = 'catalog'` events, oldest first, and returns how many
    /// distinct items were refreshed. Callers drain until it returns zero.
    pub(crate) async fn drain_catalog_items_incremental(
        &self,
        batch: i64,
    ) -> Result<usize, StorageError> {
        self.repair_outbox_gap(CATALOG_ITEMS_INCREMENTAL_CONSUMER, CATALOG_OUTBOX)
            .await?;
        let applied_seq = self
            .catalog_consumer(CATALOG_ITEMS_INCREMENTAL_CONSUMER)
            .await?
            .map(|state| state.applied_seq)
            .unwrap_or(0);
        let entries = self
            .read_outbox_after(CATALOG_OUTBOX, applied_seq, batch)
            .await?;
        let Some(last_seq) = entries.last().map(|entry| entry.id) else {
            return Ok(0);
        };
        let mut item_ids = entries
            .into_iter()
            .map(|entry| entry.item_id)
            .collect::<Vec<_>>();
        item_ids.sort_unstable();
        item_ids.dedup();
        if !item_ids.is_empty() {
            self.upsert_catalog_items_by_ids(&item_ids).await?;
            self.delete_orphaned_catalog_items(&item_ids).await?;
        }
        self.advance_catalog_consumer(CATALOG_ITEMS_INCREMENTAL_CONSUMER, last_seq)
            .await?;
        Ok(item_ids.len())
    }

    /// Recomputes the library/global counters in one transaction, so a concurrent reader never
    /// sees the table emptied between the delete and the inserts.
    pub(crate) async fn rebuild_catalog_item_stats(&self) -> Result<(), StorageError> {
        let sqlx_error = |source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        };
        let mut transaction = self.pool.begin().await.map_err(sqlx_error)?;
        self.query("DELETE FROM catalog_item_stats")
            .execute(&mut *transaction)
            .await
            .map_err(sqlx_error)?;
        self.query(
            "INSERT INTO catalog_item_stats (scope, scope_id, item_type, total)
             SELECT 'library', library_id, item_type, COUNT(*)
             FROM catalog_items WHERE removed_at IS NULL
             GROUP BY library_id, item_type",
        )
        .execute(&mut *transaction)
        .await
        .map_err(sqlx_error)?;
        self.query(
            "INSERT INTO catalog_item_stats (scope, scope_id, item_type, total)
             SELECT 'global', '', item_type, COUNT(*)
             FROM catalog_items WHERE removed_at IS NULL
             GROUP BY item_type",
        )
        .execute(&mut *transaction)
        .await
        .map_err(sqlx_error)?;
        transaction.commit().await.map_err(sqlx_error)
    }

    #[cfg(test)]
    pub(crate) async fn truncate_catalog_read_model(&self) -> Result<(), StorageError> {
        self.query("DELETE FROM catalog_item_stats")
            .execute(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        self.query("DELETE FROM catalog_items")
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(self.sqlx_err())
    }

    /// Stable hash of the read model's user-visible columns, used by rebuild validation.
    pub(crate) async fn catalog_items_checksum(
        &self,
    ) -> Result<CatalogReadModelSummary, StorageError> {
        use sha2::{Digest, Sha256};

        let rows: Vec<(String, String, i64, i64)> = self
            .query_as(
                "SELECT item_id, sort_title, effective_added_at, has_available_source
                 FROM catalog_items ORDER BY item_id",
            )
            .fetch_all(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        let mut hasher = Sha256::new();
        for (item_id, sort_title, added, available) in &rows {
            hasher.update(item_id.as_bytes());
            hasher.update(b"\0");
            hasher.update(sort_title.as_bytes());
            hasher.update(b"\0");
            hasher.update(added.to_string().as_bytes());
            hasher.update(b"\0");
            hasher.update(available.to_string().as_bytes());
            hasher.update(b"\n");
        }
        Ok(CatalogReadModelSummary {
            item_count: i64::try_from(rows.len()).unwrap_or(i64::MAX),
            content_sha256: format!("{:x}", hasher.finalize()),
        })
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(crate) enum ReadModelSort {
    SortTitle,
    EffectiveAddedAt,
    Rating,
    ProductionYear,
}

pub(crate) struct ReadModelPageQuery<'a> {
    pub(crate) library_id: &'a str,
    pub(crate) item_types: &'a [String],
    pub(crate) excluded_item_types: &'a [String],
    pub(crate) sort: ReadModelSort,
    pub(crate) descending: bool,
    pub(crate) offset: i64,
    pub(crate) limit: i64,
    pub(crate) include_total: bool,
}

impl Database {
    /// Shadow query for the default library list: ids and total only, so it can be compared with
    /// the authoritative path without changing the response.
    pub(crate) async fn shadow_catalog_library_page(
        &self,
        page: &ReadModelPageQuery<'_>,
    ) -> Result<(Vec<String>, i64), StorageError> {
        let ReadModelPageQuery {
            library_id,
            item_types,
            excluded_item_types,
            sort,
            descending,
            offset,
            limit,
            include_total,
        } = *page;
        let order = match sort {
            ReadModelSort::SortTitle => "sort_title",
            ReadModelSort::EffectiveAddedAt => "effective_added_at",
            ReadModelSort::Rating => "rating",
            ReadModelSort::ProductionYear => "production_year",
        };
        let direction = if descending { "DESC" } else { "ASC" };
        let mut conditions = vec![
            "library_id = ?".to_owned(),
            "removed_at IS NULL".to_owned(),
            "merged_into IS NULL".to_owned(),
            "has_available_source = 1".to_owned(),
        ];
        if !item_types.is_empty() {
            let placeholders = std::iter::repeat_n("?", item_types.len())
                .collect::<Vec<_>>()
                .join(", ");
            conditions.push(format!("item_type IN ({placeholders})"));
        }
        if !excluded_item_types.is_empty() {
            let placeholders = std::iter::repeat_n("?", excluded_item_types.len())
                .collect::<Vec<_>>()
                .join(", ");
            conditions.push(format!("item_type NOT IN ({placeholders})"));
        }
        let where_clause = conditions.join(" AND ");
        let list_query = format!(
            "SELECT item_id FROM catalog_items
             WHERE {where_clause}
             ORDER BY {order} {direction}, item_id
             LIMIT ? OFFSET ?"
        );
        let mut statement = self
            .query_as::<(String,)>(sqlx::AssertSqlSafe(list_query))
            .bind(library_id);
        for item_type in item_types {
            statement = statement.bind(item_type);
        }
        for item_type in excluded_item_types {
            statement = statement.bind(item_type);
        }
        let ids = statement
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
            .map_err(self.sqlx_err())?
            .into_iter()
            .map(|(id,)| id)
            .collect::<Vec<_>>();
        let total = if include_total {
            let count_query = format!("SELECT COUNT(*) FROM catalog_items WHERE {where_clause}");
            let mut count_statement = self
                .query_scalar::<i64>(sqlx::AssertSqlSafe(count_query))
                .bind(library_id);
            for item_type in item_types {
                count_statement = count_statement.bind(item_type);
            }
            for item_type in excluded_item_types {
                count_statement = count_statement.bind(item_type);
            }
            count_statement
                .fetch_one(&self.pool)
                .await
                .map_err(self.sqlx_err())?
        } else {
            0
        };
        Ok((ids, total))
    }
}

/// Recently-added page derived from `catalog_items`, mirroring the authoritative query's shape:
/// available items of the requested types plus containers without a source that still expose a
/// visible child.
pub(crate) struct RecentReadModelQuery<'a> {
    pub(crate) library_ids: &'a [String],
    pub(crate) media_only: bool,
    pub(crate) offset: i64,
    pub(crate) limit: i64,
    pub(crate) include_total: bool,
}

impl Database {
    pub(crate) async fn list_recent_read_model_ids(
        &self,
        query: &RecentReadModelQuery<'_>,
    ) -> Result<(Vec<String>, i64), StorageError> {
        if query.library_ids.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let (available_types, container_types) = if query.media_only {
            (
                "ci.item_type IN ('MOVIE', 'SERIES')",
                "ci.item_type = 'SERIES'",
            )
        } else {
            (
                "ci.item_type <> 'FOLDER'",
                "ci.item_type IN ('SERIES', 'SEASON', 'BOX_SET')",
            )
        };
        let placeholders = std::iter::repeat_n("?", query.library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let visible = format!(
            "ci.removed_at IS NULL AND ci.merged_into IS NULL AND (
                 ({available_types} AND ci.has_available_source = 1)
                 OR ({container_types} AND ci.has_available_source = 0 AND (
                     EXISTS (
                         SELECT 1 FROM catalog_items visible_child
                         WHERE visible_child.removed_at IS NULL
                           AND visible_child.merged_into IS NULL
                           AND visible_child.has_available_source = 1
                           AND (visible_child.parent_id = ci.item_id
                                OR visible_child.series_id = ci.item_id)
                     )
                     OR EXISTS (
                         SELECT 1 FROM collection_items visible_collection_item
                         JOIN collections visible_collection
                           ON visible_collection.id = visible_collection_item.collection_id
                         JOIN catalog_items visible_child
                           ON visible_child.item_id = visible_collection_item.item_id
                         WHERE visible_collection.item_id = ci.item_id
                           AND visible_child.removed_at IS NULL
                           AND visible_child.merged_into IS NULL
                           AND visible_child.has_available_source = 1
                     )
                 ))
             )"
        );
        // One bounded subquery per library keeps the `(library_id, effective_added_at)` index
        // ordered and short-circuits at the page size; a single IN list would force SQLite to sort
        // the whole candidate set across libraries.
        let per_library_limit = query.limit.saturating_add(query.offset.max(0));
        let per_library = query
            .library_ids
            .iter()
            .map(|_| {
                format!(
                    "SELECT item_id, effective_added_at, sort_title FROM (
                         SELECT ci.item_id, ci.effective_added_at, ci.sort_title
                         FROM catalog_items ci
                         JOIN libraries l ON l.id = ci.library_id AND l.is_enabled = 1
                         WHERE ci.library_id = ? AND {visible}
                         ORDER BY ci.effective_added_at DESC, ci.sort_title, ci.item_id
                         LIMIT ?
                     )"
                )
            })
            .collect::<Vec<_>>()
            .join(" UNION ALL ");
        let list_sql = format!(
            "SELECT item_id FROM ({per_library})
             ORDER BY effective_added_at DESC, sort_title, item_id
             LIMIT ? OFFSET ?"
        );
        let list_future = async {
            let mut statement = self.query_as::<(String,)>(sqlx::AssertSqlSafe(list_sql));
            for library_id in query.library_ids {
                statement = statement.bind(library_id).bind(per_library_limit);
            }
            statement
                .bind(query.limit)
                .bind(query.offset.max(0))
                .fetch_all(&self.pool)
                .await
                .map(|rows| {
                    rows.into_iter()
                        .map(|(item_id,)| item_id)
                        .collect::<Vec<_>>()
                })
                .map_err(self.sqlx_err())
        };
        if !query.include_total {
            return Ok((list_future.await?, 0));
        }
        // Two disjoint, index-backed counts instead of one `OR` predicate: the available branch is
        // covered by `idx_catalog_items_visibility`, while the container branch only visits
        // containers without a source. The single-predicate form made SQLite walk (and fetch) every
        // row of the library just to evaluate the `OR`.
        let available_count_sql = format!(
            "SELECT COUNT(*)
             FROM catalog_items ci
             JOIN libraries l ON l.id = ci.library_id AND l.is_enabled = 1
             WHERE ci.library_id IN ({placeholders})
               AND ci.removed_at IS NULL AND ci.merged_into IS NULL
               AND {available_types} AND ci.has_available_source = 1"
        );
        let container_count_sql = format!(
            "SELECT COUNT(*)
             FROM catalog_items ci
             JOIN libraries l ON l.id = ci.library_id AND l.is_enabled = 1
             WHERE ci.library_id IN ({placeholders})
               AND ci.removed_at IS NULL AND ci.merged_into IS NULL
               AND {container_types} AND ci.has_available_source = 0
               AND (EXISTS (
                        SELECT 1 FROM catalog_items visible_child
                        WHERE visible_child.removed_at IS NULL
                          AND visible_child.merged_into IS NULL
                          AND visible_child.has_available_source = 1
                          AND (visible_child.parent_id = ci.item_id
                               OR visible_child.series_id = ci.item_id)
                    )
                    OR EXISTS (
                        SELECT 1 FROM collection_items visible_collection_item
                        JOIN collections visible_collection
                          ON visible_collection.id = visible_collection_item.collection_id
                        JOIN catalog_items visible_child
                          ON visible_child.item_id = visible_collection_item.item_id
                        WHERE visible_collection.item_id = ci.item_id
                          AND visible_child.removed_at IS NULL
                          AND visible_child.merged_into IS NULL
                          AND visible_child.has_available_source = 1
                    ))"
        );
        let count_scope = |sql: &str| {
            let mut statement = self.query_scalar::<i64>(sqlx::AssertSqlSafe(sql.to_owned()));
            for library_id in query.library_ids {
                statement = statement.bind(library_id);
            }
            statement
        };
        let (ids, available_total, container_total) = tokio::try_join!(
            list_future,
            async {
                count_scope(&available_count_sql)
                    .fetch_one(&self.pool)
                    .await
                    .map_err(self.sqlx_err())
            },
            async {
                count_scope(&container_count_sql)
                    .fetch_one(&self.pool)
                    .await
                    .map_err(self.sqlx_err())
            },
        )?;
        Ok((ids, available_total.saturating_add(container_total)))
    }
}

/// Per-library "latest" rail: at most `limit` ids per library, ordered like the authoritative
/// query (available MOVIE/SERIES plus source-less series that still expose a visible child).
pub(crate) struct RecentByLibraryReadModelQuery<'a> {
    pub(crate) library_ids: &'a [String],
    pub(crate) limit_per_library: i64,
}

impl Database {
    pub(crate) async fn list_recent_read_model_ids_by_library(
        &self,
        query: &RecentByLibraryReadModelQuery<'_>,
    ) -> Result<Vec<(String, Vec<String>)>, StorageError> {
        if query.library_ids.is_empty() {
            return Ok(Vec::new());
        }
        let branch = "SELECT id, library_id FROM (
                SELECT ci.item_id AS id, ci.library_id,
                       ci.latest_added_at,
                       ci.sort_title
                FROM catalog_items ci
                WHERE ci.library_id = ?
                  AND ci.removed_at IS NULL AND ci.merged_into IS NULL
                  AND ci.item_type IN ('MOVIE', 'SERIES')
                  AND (
                      ci.has_available_source = 1
                      OR (ci.item_type = 'SERIES' AND ci.has_available_source = 0 AND (
                          EXISTS (
                              SELECT 1 FROM catalog_items visible_child
                              WHERE visible_child.removed_at IS NULL
                                AND visible_child.merged_into IS NULL
                                AND visible_child.has_available_source = 1
                                AND (visible_child.parent_id = ci.item_id
                                     OR visible_child.series_id = ci.item_id)
                          )
                          OR EXISTS (
                              SELECT 1 FROM collection_items visible_collection_item
                              JOIN collections visible_collection
                                ON visible_collection.id = visible_collection_item.collection_id
                              JOIN catalog_items visible_child
                                ON visible_child.item_id = visible_collection_item.item_id
                              WHERE visible_collection.item_id = ci.item_id
                                AND visible_child.removed_at IS NULL
                                AND visible_child.merged_into IS NULL
                                AND visible_child.has_available_source = 1
                          )
                      ))
                  )
                ORDER BY latest_added_at DESC, ci.sort_title, ci.item_id
                LIMIT ?
            )";
        let sql = query
            .library_ids
            .iter()
            .map(|_| branch)
            .collect::<Vec<_>>()
            .join(" UNION ALL ");
        let mut statement = self.query_as::<(String, String)>(sqlx::AssertSqlSafe(sql));
        for library_id in query.library_ids {
            statement = statement
                .bind(library_id)
                .bind(query.limit_per_library.max(1));
        }
        let rows = statement
            .fetch_all(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        let mut grouped: Vec<(String, Vec<String>)> = Vec::new();
        for (id, library_id) in rows {
            match grouped.last_mut() {
                Some((current, ids)) if *current == library_id => ids.push(id),
                _ => grouped.push((library_id, vec![id])),
            }
        }
        Ok(grouped)
    }
}

#[derive(Debug, sqlx::FromRow)]
struct ReadModelCoreRow {
    item_id: String,
    library_id: String,
    item_type: String,
    parent_id: Option<String>,
    series_id: Option<String>,
    season_number: Option<i64>,
    episode_number: Option<i64>,
    title: String,
    sort_title: String,
    original_title: Option<String>,
    overview: Option<String>,
    rating_source: Option<String>,
    production_year: Option<i64>,
    rating: Option<f64>,
    runtime_ticks: Option<i64>,
    added_at: i64,
    updated_at: i64,
    poster_tag: Option<String>,
    backdrop_tag: Option<String>,
    thumb_tag: Option<String>,
    logo_tag: Option<String>,
    series_name: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct ReadModelSourceRow {
    item_id: String,
    source_relative_path: Option<String>,
    source_id: Option<String>,
    source_kind: Option<String>,
    container: Option<String>,
    size: Option<i64>,
    external_url: Option<String>,
    edition_name: Option<String>,
    quality_label: Option<String>,
    bitrate: Option<i64>,
    duration_ticks: Option<i64>,
    is_default: Option<i64>,
    probe_status: Option<String>,
    stream_id: Option<String>,
    stream_index: Option<i64>,
    stream_type: Option<String>,
    codec: Option<String>,
    language: Option<String>,
    stream_title: Option<String>,
    stream_details_json: Option<String>,
    stream_is_external: Option<i64>,
    stream_is_default: Option<i64>,
    stream_is_forced: Option<i64>,
}

impl Database {
    /// Builds the legacy row shape from `catalog_items` plus one batched source/stream query.
    ///
    /// The projection decides whether that second query joins `media_streams`: a grid/card page
    /// asks for [`CatalogRowProjection::Summary`] and never reads stream rows or `details_json`.
    pub(crate) async fn list_catalog_rows_from_read_model(
        &self,
        page: &ReadModelPageQuery<'_>,
        projection: CatalogRowProjection,
    ) -> Result<(Vec<StoredCatalogRow>, i64), StorageError> {
        let (ids, total) = self.shadow_catalog_library_page(page).await?;
        let rows = self.read_model_rows_for_ids(&ids, projection).await?;
        Ok((rows, total))
    }

    /// Single-item variant used by the detail route.
    pub(crate) async fn find_catalog_rows_from_read_model(
        &self,
        item_id: &str,
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        self.read_model_rows_for_ids(&[item_id.to_owned()], CatalogRowProjection::Full)
            .await
    }

    /// Same as [`Self::read_model_rows_for_ids`] but keeps the requested id order, which derived
    /// lists (continue watching) need to preserve their authoritative ordering.
    pub(crate) async fn read_model_rows_in_id_order(
        &self,
        ids: &[String],
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        let mut rows = self
            .read_model_rows_for_ids(ids, CatalogRowProjection::Full)
            .await?;
        let position = ids
            .iter()
            .enumerate()
            .map(|(index, id)| (id.as_str(), index))
            .collect::<std::collections::HashMap<_, _>>();
        rows.sort_by_key(|row| {
            position
                .get(row.item_id.as_str())
                .copied()
                .unwrap_or(usize::MAX)
        });
        Ok(rows)
    }

    async fn read_model_rows_for_ids(
        &self,
        ids: &[String],
        projection: CatalogRowProjection,
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat_n("?", ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let core_query = format!(
            "SELECT ci.item_id, ci.library_id, ci.item_type, ci.parent_id, ci.series_id,
                    ci.season_number, ci.episode_number, ci.title, ci.sort_title,
                    ci.original_title, mi.overview, mi.rating_source,
                    ci.production_year, ci.rating, ci.runtime_ticks,
                    ci.added_at, ci.updated_at, ci.poster_tag, ci.backdrop_tag,
                    ci.thumb_tag, ci.logo_tag,
                    (SELECT s.title FROM media_items s WHERE s.id = ci.series_id) AS series_name
             FROM catalog_items ci
             JOIN media_items mi ON mi.id = ci.item_id
             WHERE ci.item_id IN ({placeholders})"
        );
        let mut statement = self.query_as::<ReadModelCoreRow>(sqlx::AssertSqlSafe(core_query));
        for id in ids {
            statement = statement.bind(id);
        }
        let cores = statement
            .fetch_all(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        // Same stream columns, join and tie-breaker as the authoritative list queries: the summary
        // projection binds them to NULL, so this statement never touches `media_streams` and never
        // reads `details_json`.
        let stream_columns = catalog_stream_columns(projection);
        let stream_join = catalog_stream_join(projection);
        let stream_order = catalog_stream_order(projection);
        let source_query = format!(
            "SELECT ms.item_id,
                    (SELECT fe.relative_path FROM filesystem_entries fe
                     WHERE fe.id = ms.filesystem_entry_id) AS source_relative_path,
                    ms.id AS source_id, ms.source_kind, ms.container, ms.size,
                    ms.external_url, ms.edition_name, ms.quality_label, ms.bitrate,
                    ms.duration_ticks, ms.is_default, ms.probe_status,
                    {stream_columns}
             FROM media_sources ms
             {stream_join}
             WHERE ms.item_id IN ({placeholders})
               AND EXISTS (
                   SELECT 1 FROM filesystem_entries fe
                   WHERE fe.id = ms.filesystem_entry_id AND fe.is_missing = 0
               )
             ORDER BY ms.item_id, ms.id{stream_order}"
        );
        let mut statement = self.query_as::<ReadModelSourceRow>(sqlx::AssertSqlSafe(source_query));
        for id in ids {
            statement = statement.bind(id);
        }
        let sources = statement
            .fetch_all(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        let mut by_item: std::collections::HashMap<String, Vec<ReadModelSourceRow>> =
            std::collections::HashMap::new();
        for source in sources {
            by_item
                .entry(source.item_id.clone())
                .or_default()
                .push(source);
        }
        let mut rows = Vec::new();
        for core in cores {
            let item_sources = by_item.remove(&core.item_id).unwrap_or_default();
            if item_sources.is_empty() {
                rows.push(core_row_without_source(&core));
                continue;
            }
            for source in item_sources {
                rows.push(core_row_with_source(&core, &source));
            }
        }
        Ok(rows)
    }
}

fn core_row_without_source(core: &ReadModelCoreRow) -> StoredCatalogRow {
    StoredCatalogRow {
        item_id: core.item_id.clone(),
        library_id: core.library_id.clone(),
        item_type: core.item_type.clone(),
        parent_id: core.parent_id.clone(),
        series_id: core.series_id.clone(),
        series_name: core.series_name.clone(),
        season_number: core.season_number,
        episode_number: core.episode_number,
        title: core.title.clone(),
        sort_title: core.sort_title.clone(),
        original_title: core.original_title.clone(),
        overview: core.overview.clone(),
        production_year: core.production_year,
        rating: core.rating,
        rating_source: core.rating_source.clone(),
        runtime_ticks: core.runtime_ticks,
        added_at: core.added_at,
        updated_at: core.updated_at,
        poster_image_tag: core.poster_tag.clone(),
        fanart_image_tag: core.backdrop_tag.clone(),
        thumb_image_tag: core.thumb_tag.clone(),
        logo_image_tag: core.logo_tag.clone(),
        source_id: None,
        source_relative_path: None,
        source_kind: None,
        container: None,
        size: None,
        external_url: None,
        edition_name: None,
        quality_label: None,
        bitrate: None,
        duration_ticks: None,
        is_default: None,
        probe_status: None,
        stream_id: None,
        stream_index: None,
        stream_type: None,
        codec: None,
        language: None,
        stream_title: None,
        stream_details_json: None,
        stream_is_external: None,
        stream_is_default: None,
        stream_is_forced: None,
    }
}

fn core_row_with_source(core: &ReadModelCoreRow, source: &ReadModelSourceRow) -> StoredCatalogRow {
    let mut row = core_row_without_source(core);
    row.source_id = source.source_id.clone();
    row.source_relative_path = source.source_relative_path.clone();
    row.source_kind = source.source_kind.clone();
    row.container = source.container.clone();
    row.size = source.size;
    row.external_url = source.external_url.clone();
    row.edition_name = source.edition_name.clone();
    row.quality_label = source.quality_label.clone();
    row.bitrate = source.bitrate;
    row.duration_ticks = source.duration_ticks;
    row.is_default = source.is_default.map(|value| value != 0);
    row.probe_status = source.probe_status.clone();
    row.stream_id = source.stream_id.clone();
    row.stream_index = source.stream_index;
    row.stream_type = source.stream_type.clone();
    row.codec = source.codec.clone();
    row.language = source.language.clone();
    row.stream_title = source.stream_title.clone();
    row.stream_details_json = source.stream_details_json.clone();
    row.stream_is_external = source.stream_is_external.map(|value| value != 0);
    row.stream_is_default = source.stream_is_default.map(|value| value != 0);
    row.stream_is_forced = source.stream_is_forced.map(|value| value != 0);
    row
}

/// Shared `catalog_items` upsert body; callers append their own `WHERE` clause so the full rebuild
/// (keyset) and the incremental consumer (explicit ids) derive rows with exactly the same columns.
const CATALOG_ITEM_UPSERT_SELECT: &str = "INSERT INTO catalog_items (
                     item_id, library_id, item_type, parent_id, series_id, season_number,
                     episode_number, title, sort_title, original_title, production_year,
                     rating, runtime_ticks, added_at, effective_added_at, updated_at,
                     removed_at, merged_into, has_available_source, default_source_id,
                     poster_tag, backdrop_tag, thumb_tag, logo_tag, season_count,
                     episode_count, latest_child_added_at, latest_episode_added_at,
                     latest_added_at, restriction_code, metadata_state, nfo_fingerprint,
                     applied_seq
                 )
                 SELECT
                     mi.id, mi.library_id, mi.item_type, mi.parent_id, mi.series_id,
                     mi.season_number, mi.episode_number, mi.title, mi.sort_title,
                     mi.original_title, mi.production_year, mi.rating, mi.runtime_ticks,
                     mi.added_at, mi.added_at, mi.updated_at, mi.removed_at,
                     mi.merged_into_item_id, mi.has_available_source,
                     (SELECT ms.id FROM media_sources ms WHERE ms.item_id = mi.id
                       ORDER BY ms.is_default DESC, ms.id LIMIT 1),
                     (SELECT ii.id FROM item_images ii
                       WHERE ii.item_id = mi.id AND ii.image_type = 'POSTER'
                       ORDER BY ii.image_index, ii.id LIMIT 1),
                     (SELECT ii.id FROM item_images ii
                       WHERE ii.item_id = mi.id AND ii.image_type = 'FANART'
                       ORDER BY ii.image_index, ii.id LIMIT 1),
                     (SELECT ii.id FROM item_images ii
                       WHERE ii.item_id = mi.id AND ii.image_type = 'THUMB'
                       ORDER BY ii.image_index, ii.id LIMIT 1),
                     (SELECT ii.id FROM item_images ii
                       WHERE ii.item_id = mi.id AND ii.image_type = 'LOGO'
                       ORDER BY ii.image_index, ii.id LIMIT 1),
                     (SELECT COUNT(*) FROM media_items child
                       WHERE child.series_id = mi.id AND child.item_type = 'SEASON'),
                     (SELECT COUNT(*) FROM media_items child
                       WHERE child.item_type = 'EPISODE'
                         AND (child.series_id = mi.id OR child.parent_id = mi.id)),
                     (SELECT MAX(child.added_at) FROM media_items child
                       WHERE child.parent_id = mi.id OR child.series_id = mi.id),
                     (SELECT MAX(child.added_at) FROM media_items child
                       WHERE child.item_type = 'EPISODE'
                         AND child.removed_at IS NULL
                         AND child.has_available_source = 1
                         AND (child.series_id = mi.id OR child.parent_id = mi.id)),
                     CASE WHEN mi.item_type = 'SERIES' THEN COALESCE(
                         (SELECT MAX(child.added_at) FROM media_items child
                          WHERE child.item_type = 'EPISODE'
                            AND child.removed_at IS NULL
                            AND child.has_available_source = 1
                            AND (child.series_id = mi.id OR child.parent_id = mi.id)),
                         mi.added_at)
                     ELSE mi.added_at END,
                     (SELECT cri.certification_code FROM media_content_restriction_index cri
                       WHERE cri.item_id = mi.id LIMIT 1),
                     mi.identification_status, mi.nfo_metadata_fingerprint, 0
                 FROM media_items mi
                 WHERE ";

const CATALOG_ITEM_UPSERT_CONFLICT: &str = " ON CONFLICT(item_id) DO UPDATE SET
                     library_id = excluded.library_id,
                     item_type = excluded.item_type,
                     parent_id = excluded.parent_id,
                     series_id = excluded.series_id,
                     season_number = excluded.season_number,
                     episode_number = excluded.episode_number,
                     title = excluded.title,
                     sort_title = excluded.sort_title,
                     original_title = excluded.original_title,
                     production_year = excluded.production_year,
                     rating = excluded.rating,
                     runtime_ticks = excluded.runtime_ticks,
                     added_at = excluded.added_at,
                     effective_added_at = excluded.effective_added_at,
                     updated_at = excluded.updated_at,
                     removed_at = excluded.removed_at,
                     merged_into = excluded.merged_into,
                     has_available_source = excluded.has_available_source,
                     default_source_id = excluded.default_source_id,
                     poster_tag = excluded.poster_tag,
                     backdrop_tag = excluded.backdrop_tag,
                     thumb_tag = excluded.thumb_tag,
                     logo_tag = excluded.logo_tag,
                     season_count = excluded.season_count,
                     episode_count = excluded.episode_count,
                     latest_child_added_at = excluded.latest_child_added_at,
                     latest_episode_added_at = excluded.latest_episode_added_at,
                     latest_added_at = excluded.latest_added_at,
                     restriction_code = excluded.restriction_code,
                     metadata_state = excluded.metadata_state,
                     nfo_fingerprint = excluded.nfo_fingerprint,
                     applied_seq = excluded.applied_seq";
