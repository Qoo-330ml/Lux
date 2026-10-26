//! Read-model fan-out primitives: append-only outbox plus per-consumer watermarks.

use super::*;

#[allow(dead_code)] // LUX-864 wires the consumer.
#[derive(Debug, Clone, Eq, PartialEq, sqlx::FromRow)]
pub(crate) struct CatalogOutboxEntry {
    pub(crate) id: i64,
    pub(crate) kind: String,
    pub(crate) item_id: String,
    pub(crate) library_id: Option<String>,
}

#[allow(dead_code)] // LUX-864 wires the consumer.
#[derive(Debug, Clone, Eq, PartialEq, sqlx::FromRow)]
pub(crate) struct CatalogConsumerState {
    pub(crate) name: String,
    pub(crate) applied_seq: i64,
    pub(crate) updated_at: i64,
    pub(crate) last_error: Option<String>,
}

#[allow(dead_code)] // LUX-864 wires the consumer.
impl Database {
    /// Appends one derived-data event and returns its sequence number.
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
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    /// Reads at most `limit` events after `after_seq`, oldest first.
    pub(crate) async fn read_catalog_outbox_after(
        &self,
        after_seq: i64,
        limit: i64,
    ) -> Result<Vec<CatalogOutboxEntry>, StorageError> {
        self.query_as(
            "SELECT id, kind, item_id, library_id
             FROM catalog_outbox
             WHERE id > ?
             ORDER BY id
             LIMIT ?",
        )
        .bind(after_seq)
        .bind(limit.clamp(1, 5_000))
        .fetch_all(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
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
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
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
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
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
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    /// Detects that the queue was trimmed past a consumer's watermark.
    ///
    /// Returns `(applied_seq, oldest_remaining_seq)` when the next expected sequence is gone, so the
    /// caller can schedule a full rebuild instead of silently skipping derived rows.
    pub(crate) async fn catalog_outbox_gap(
        &self,
        name: &str,
    ) -> Result<Option<(i64, i64)>, StorageError> {
        let applied_seq = self
            .catalog_consumer(name)
            .await?
            .map(|state| state.applied_seq)
            .unwrap_or(0);
        let oldest = self
            .query_scalar::<Option<i64>>("SELECT MIN(id) FROM catalog_outbox")
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .flatten();
        Ok(oldest.and_then(|oldest| {
            (oldest > applied_seq.saturating_add(1)).then_some((applied_seq, oldest))
        }))
    }
}
