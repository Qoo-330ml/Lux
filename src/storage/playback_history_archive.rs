//! Playback history that outlives the item it was recorded against.
//!
//! `playback_history_events` cascades from `media_items`, so deleting a library -- or purging an
//! expired tombstone -- erased the imported record of what the user actually watched. Watch state
//! is already archived by provider identity (see [`super::watch_state_archive`]); events follow the
//! same rule, with one difference worth remembering:
//!
//! * the archive copy only happens when the item is really deleted, because an event row keeps the
//!   importer's `source` + `source_event_key` uniqueness. Copying events onto a live twin while the
//!   original row still exists would collide with that key, so the "file moved, scan made a new
//!   item" case restores events once the old item is purged, not before;
//! * reattaching uses `ON CONFLICT DO NOTHING`, so history that survived somewhere else is never
//!   duplicated.

use super::*;

/// Events examined per reconciler pass. Imported history is a handful of rows per playback, so the
/// pass stays small; whatever does not fit is picked up by the next one.
pub(super) const PLAYBACK_HISTORY_RESTORE_BATCH: i64 = 200;
/// Events merged per write transaction, so the write lock is held for a group, not a pass.
const PLAYBACK_HISTORY_RESTORE_CHUNK: usize = 50;

const ARCHIVE_INSERT_PREFIX: &str = "
INSERT INTO playback_history_event_archive (
    id, item_type, provider, provider_id, user_id, event_type, position_ticks,
    duration_ticks, occurred_at, source, source_event_key, detached_at, source_item_id
)
SELECT
    e.id,
    mi.item_type,
    p.provider,
    p.provider_id,
    e.user_id,
    e.event_type,
    e.position_ticks,
    e.duration_ticks,
    e.occurred_at,
    e.source,
    e.source_event_key,
    unixepoch(),
    e.item_id
FROM playback_history_events e
JOIN media_items mi ON mi.id = e.item_id
JOIN media_item_provider_ids p ON p.media_item_id = e.item_id
WHERE ";

/// A second archive pass over the same event must not create a competing row or move the
/// detachment time backwards; the event itself is immutable.
const ARCHIVE_UPSERT_CLAUSE: &str = "
ON CONFLICT (id, provider, provider_id) DO UPDATE SET
    detached_at = CASE
        WHEN excluded.detached_at > playback_history_event_archive.detached_at
        THEN excluded.detached_at
        ELSE playback_history_event_archive.detached_at
    END,
    source_item_id = excluded.source_item_id";

/// Events with somewhere to go. Batching by event id keeps a multi-identity event in one group, so
/// it is reattached once and consumed once.
const RESTORE_CANDIDATE_IDS: &str = "
SELECT DISTINCT a.id
FROM playback_history_event_archive a
WHERE EXISTS (
    SELECT 1 FROM media_item_provider_ids p
    JOIN media_items m ON m.id = p.media_item_id
    WHERE p.item_type = a.item_type
      AND p.provider = a.provider
      AND p.provider_id = a.provider_id
      AND m.removed_at IS NULL
)
ORDER BY a.id
LIMIT ?";

impl Database {
    /// Copies the playback history of exactly these items into the archive.
    pub(crate) async fn archive_playback_history_for_items(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        item_ids: &[String],
    ) -> Result<u64, StorageError> {
        if item_ids.is_empty() {
            return Ok(0);
        }
        let placeholders = std::iter::repeat_n("?", item_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let scope = format!("e.item_id IN ({placeholders})");
        let binds = item_ids.iter().map(String::as_str).collect::<Vec<_>>();
        self.archive_playback_history_in_scope(transaction, &scope, &binds)
            .await
    }

    /// Copies the playback history of every identified item in a library into the archive.
    pub(crate) async fn archive_playback_history_for_library(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        library_id: &str,
    ) -> Result<u64, StorageError> {
        let scope = "e.item_id IN (SELECT id FROM media_items WHERE library_id = ?)";
        self.archive_playback_history_in_scope(transaction, scope, &[library_id])
            .await
    }

    async fn archive_playback_history_in_scope(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        scope: &str,
        binds: &[&str],
    ) -> Result<u64, StorageError> {
        let sql = format!("{ARCHIVE_INSERT_PREFIX}{scope}{ARCHIVE_UPSERT_CLAUSE}");
        let mut statement = self.query(sqlx::AssertSqlSafe(sql));
        for value in binds {
            statement = statement.bind(*value);
        }
        statement
            .execute(&mut **transaction)
            .await
            .map(|result| result.rows_affected())
            .map_err(|source| self.storage_error(source))
    }

    /// Reattaches archived playback history to the items that carry the same identity again.
    ///
    /// Returns how many events were put back. Archived rows are consumed once their event exists
    /// again, so a title that never comes back keeps its history without being retried forever.
    pub async fn restore_detached_playback_history(
        &self,
        batch_size: i64,
    ) -> Result<u64, StorageError> {
        if batch_size <= 0 {
            return Ok(0);
        }
        let event_ids = self
            .query_scalar::<String>(RESTORE_CANDIDATE_IDS)
            .bind(batch_size)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| self.storage_error(source))?;
        if event_ids.is_empty() {
            return Ok(0);
        }
        let mut restored = 0_u64;
        for chunk in event_ids.chunks(PLAYBACK_HISTORY_RESTORE_CHUNK) {
            let mut transaction = self.begin_metadata_write_transaction().await?;
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let insert = format!(
                "INSERT INTO playback_history_events (
                     id, user_id, item_id, event_type, position_ticks, duration_ticks,
                     occurred_at, source, source_event_key
                 )
                 SELECT
                     a.id,
                     a.user_id,
                     (
                         SELECT MIN(p.media_item_id)
                         FROM media_item_provider_ids p
                         JOIN media_items m ON m.id = p.media_item_id
                         WHERE p.item_type = a.item_type
                           AND p.provider = a.provider
                           AND p.provider_id = a.provider_id
                           AND m.removed_at IS NULL
                     ),
                     a.event_type,
                     a.position_ticks,
                     a.duration_ticks,
                     a.occurred_at,
                     a.source,
                     a.source_event_key
                 FROM playback_history_event_archive a
                 WHERE a.id IN ({placeholders})
                 ORDER BY a.provider, a.provider_id
                 ON CONFLICT DO NOTHING"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(insert));
            for id in chunk {
                statement = statement.bind(id.as_str());
            }
            let inserted = statement
                .execute(&mut *transaction)
                .await
                .map_err(|source| self.storage_error(source))?
                .rows_affected();
            let delete =
                format!("DELETE FROM playback_history_event_archive WHERE id IN ({placeholders})");
            let mut statement = self.query(sqlx::AssertSqlSafe(delete));
            for id in chunk {
                statement = statement.bind(id.as_str());
            }
            statement
                .execute(&mut *transaction)
                .await
                .map_err(|source| self.storage_error(source))?;
            transaction
                .commit()
                .await
                .map_err(|source| self.storage_error(source))?;
            restored = restored.saturating_add(inserted);
        }
        Ok(restored)
    }
}

#[derive(Debug, sqlx::FromRow)]
pub(crate) struct StoredPlaybackHistoryEvent {
    pub(crate) id: String,
    pub(crate) user_id: String,
    pub(crate) item_id: String,
    pub(crate) event_type: String,
    pub(crate) position_ticks: i64,
    pub(crate) duration_ticks: Option<i64>,
    pub(crate) occurred_at: i64,
    pub(crate) source: String,
}

impl Database {
    /// Old history events that the JSONL archive is about to move out of the database.
    pub(crate) async fn list_playback_history_events_for_archive(
        &self,
        before: i64,
        limit: i64,
    ) -> Result<Vec<StoredPlaybackHistoryEvent>, StorageError> {
        self.query_as(
            "SELECT id, user_id, item_id, event_type, position_ticks, duration_ticks,
                    occurred_at, source
             FROM playback_history_events
             WHERE occurred_at < ?
             ORDER BY occurred_at, id
             LIMIT ?",
        )
        .bind(before)
        .bind(limit.clamp(1, 5_000))
        .fetch_all(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    /// Deletes exactly the archived event ids. Callers must only pass ids whose lines reached disk.
    pub(crate) async fn delete_playback_history_events(
        &self,
        ids: &[String],
    ) -> Result<u64, StorageError> {
        let mut deleted = 0_u64;
        for chunk in ids.chunks(500) {
            if chunk.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!("DELETE FROM playback_history_events WHERE id IN ({placeholders})");
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for id in chunk {
                statement = statement.bind(id);
            }
            deleted += statement
                .execute(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
                .rows_affected();
        }
        Ok(deleted)
    }
}
