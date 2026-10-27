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

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct CatalogReadModelSummary {
    pub(crate) item_count: i64,
    pub(crate) content_sha256: String,
}

impl Database {
    #[allow(dead_code)] // used by rebuild tests and diagnostics
    pub(crate) async fn catalog_items_count(&self) -> Result<i64, StorageError> {
        self.query_scalar("SELECT COUNT(*) FROM catalog_items")
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    /// Highest item id already materialized, used to resume an interrupted rebuild.
    pub(crate) async fn catalog_items_max_item_id(&self) -> Result<Option<String>, StorageError> {
        self.query_scalar("SELECT MAX(item_id) FROM catalog_items")
            .fetch_optional(&self.pool)
            .await
            .map(|row| row.flatten())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
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
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if rows.is_empty() {
            return Ok((0, None));
        }
        let last = rows.last().cloned();
        let upsert = self
            .query(
                "INSERT INTO catalog_items (
                     item_id, library_id, item_type, parent_id, series_id, season_number,
                     episode_number, title, sort_title, original_title, production_year,
                     rating, runtime_ticks, added_at, effective_added_at, updated_at,
                     removed_at, merged_into, has_available_source, default_source_id,
                     poster_tag, backdrop_tag, thumb_tag, logo_tag, season_count,
                     episode_count, latest_child_added_at, restriction_code, metadata_state,
                     nfo_fingerprint, applied_seq
                 )
                 SELECT
                     mi.id, mi.library_id, mi.item_type, mi.parent_id, mi.series_id,
                     mi.season_number, mi.episode_number, mi.title, mi.sort_title,
                     mi.original_title, mi.production_year, mi.rating, mi.runtime_ticks,
                     mi.added_at, mi.added_at, mi.updated_at, mi.removed_at,
                     mi.merged_into_item_id, mi.has_available_source,
                     (SELECT ms.id FROM media_sources ms WHERE ms.item_id = mi.id
                       ORDER BY ms.is_default DESC, ms.id LIMIT 1),
                     (SELECT ii.content_tag FROM item_images ii
                       WHERE ii.item_id = mi.id AND ii.image_type = 'POSTER'
                       ORDER BY ii.image_index, ii.id LIMIT 1),
                     (SELECT ii.content_tag FROM item_images ii
                       WHERE ii.item_id = mi.id AND ii.image_type = 'BACKDROP'
                       ORDER BY ii.image_index, ii.id LIMIT 1),
                     (SELECT ii.content_tag FROM item_images ii
                       WHERE ii.item_id = mi.id AND ii.image_type = 'THUMB'
                       ORDER BY ii.image_index, ii.id LIMIT 1),
                     (SELECT ii.content_tag FROM item_images ii
                       WHERE ii.item_id = mi.id AND ii.image_type = 'LOGO'
                       ORDER BY ii.image_index, ii.id LIMIT 1),
                     (SELECT COUNT(*) FROM media_items child
                       WHERE child.series_id = mi.id AND child.item_type = 'SEASON'),
                     (SELECT COUNT(*) FROM media_items child
                       WHERE child.item_type = 'EPISODE'
                         AND (child.series_id = mi.id OR child.parent_id = mi.id)),
                     (SELECT MAX(child.added_at) FROM media_items child
                       WHERE child.parent_id = mi.id OR child.series_id = mi.id),
                     (SELECT cri.certification_code FROM media_content_restriction_index cri
                       WHERE cri.item_id = mi.id LIMIT 1),
                     mi.identification_status, mi.nfo_metadata_fingerprint, 0
                 FROM media_items mi
                 WHERE mi.id > ? AND mi.id <= ?
                 ON CONFLICT(item_id) DO UPDATE SET
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
                     restriction_code = excluded.restriction_code,
                     metadata_state = excluded.metadata_state,
                     nfo_fingerprint = excluded.nfo_fingerprint,
                     applied_seq = excluded.applied_seq",
            )
            .bind(after)
            .bind(last.as_deref())
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok((upsert.rows_affected(), last))
    }

    pub(crate) async fn rebuild_catalog_item_stats(&self) -> Result<(), StorageError> {
        self.query("DELETE FROM catalog_item_stats")
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.query(
            "INSERT INTO catalog_item_stats (scope, scope_id, item_type, total)
             SELECT 'library', library_id, item_type, COUNT(*)
             FROM catalog_items WHERE removed_at IS NULL
             GROUP BY library_id, item_type",
        )
        .execute(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query(
            "INSERT INTO catalog_item_stats (scope, scope_id, item_type, total)
             SELECT 'global', '', item_type, COUNT(*)
             FROM catalog_items WHERE removed_at IS NULL
             GROUP BY item_type",
        )
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    #[allow(dead_code)] // used by the read-model reset path in LUX-865
    pub(crate) async fn truncate_catalog_read_model(&self) -> Result<(), StorageError> {
        self.query("DELETE FROM catalog_item_stats")
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.query("DELETE FROM catalog_items")
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
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
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
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
