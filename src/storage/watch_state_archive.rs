//! Watch state that outlives the item it was recorded against.
//!
//! `user_item_state` cascades from `media_items`, so hard-deleting an item used to erase the
//! user's position, played flag, favourite flag and play count with it. Deleting a library and
//! re-adding the same files elsewhere therefore started from zero, even though the user had
//! already watched the same film.
//!
//! Two things live here:
//!
//! 1. Archiving, executed by the hard-delete paths (`database_cleanup`'s retention purge and
//!    `library_deletion`) inside their own transaction, before anything removes the rows. Only
//!    items with a provider identity are archived, because only those can be matched again later;
//!    the purge keeps pinning the state of unidentified items instead of dropping it.
//! 2. Reconciliation, a background pass that copies archived state onto whichever live item now
//!    carries the same identity, and additionally copies state that still hangs off a soft-deleted
//!    item onto a live twin. That second half is what makes "the file moved to another library and
//!    the scan created a new item id" work immediately, not only after the retention purge.
//!
//! Reconciliation only ever raises a value the user already earned (played stays played, the play
//! count keeps the higher number, the newer position wins) and never deletes state from a live
//! item, so running it repeatedly converges instead of drifting.

use std::time::Duration;

use super::*;

/// Identities examined per reconciler pass. The pass is cheap and idempotent, so a title that
/// reappears late is picked up by a later run rather than by a longer pass.
const WATCH_STATE_RECONCILE_BATCH: i64 = 500;
/// Merges per write transaction. A pass can touch hundreds of identities; committing them in
/// small groups keeps the write lock held for the length of one group, not of the whole pass.
const WATCH_STATE_RESTORE_CHUNK: usize = 100;
/// Short enough that a re-added title comes back played within a browse, long enough that an idle
/// server does no meaningful work.
const WATCH_STATE_RECONCILE_INTERVAL: Duration = Duration::from_secs(300);
const WATCH_STATE_RECONCILE_INITIAL_DELAY: Duration = Duration::from_secs(60);

/// The `ON CONFLICT` half of the archive write, kept apart because both scopes use it.
///
/// SQLite and PostgreSQL share the syntax but not `max(a, b)`/`greatest(a, b)`, so every fold is
/// written as a portable `CASE`. A later recording never lowers a value the user already earned.
const ARCHIVE_UPSERT_CLAUSE: &str = "
ON CONFLICT (user_id, item_type, provider, provider_id) DO UPDATE SET
    position_ticks = CASE
        WHEN excluded.is_played = 0
         AND user_item_state_archive.is_played = 0
         AND COALESCE(excluded.last_played_at, 0)
             > COALESCE(user_item_state_archive.last_played_at, 0)
        THEN excluded.position_ticks
        ELSE user_item_state_archive.position_ticks
    END,
    is_played = CASE
        WHEN excluded.is_played > user_item_state_archive.is_played
        THEN excluded.is_played
        ELSE user_item_state_archive.is_played
    END,
    is_favorite = CASE
        WHEN excluded.is_favorite > user_item_state_archive.is_favorite
        THEN excluded.is_favorite
        ELSE user_item_state_archive.is_favorite
    END,
    play_count = CASE
        WHEN excluded.play_count > user_item_state_archive.play_count
        THEN excluded.play_count
        ELSE user_item_state_archive.play_count
    END,
    last_played_at = CASE
        WHEN COALESCE(excluded.last_played_at, 0)
             > COALESCE(user_item_state_archive.last_played_at, 0)
        THEN excluded.last_played_at
        ELSE user_item_state_archive.last_played_at
    END,
    detached_at = CASE
        WHEN excluded.detached_at > user_item_state_archive.detached_at
        THEN excluded.detached_at
        ELSE user_item_state_archive.detached_at
    END,
    source_item_id = CASE
        WHEN excluded.detached_at >= user_item_state_archive.detached_at
        THEN excluded.source_item_id
        ELSE user_item_state_archive.source_item_id
    END";

const ARCHIVE_INSERT_PREFIX: &str = "
INSERT INTO user_item_state_archive (
    user_id, item_type, provider, provider_id,
    position_ticks, is_played, is_favorite, play_count, last_played_at,
    detached_at, source_item_id
)
SELECT
    s.user_id,
    mi.item_type,
    p.provider,
    p.provider_id,
    MAX(s.position_ticks),
    MAX(s.is_played),
    MAX(s.is_favorite),
    MAX(s.play_count),
    MAX(s.last_played_at),
    unixepoch(),
    MIN(s.item_id)
FROM user_item_state s
JOIN media_items mi ON mi.id = s.item_id
JOIN media_item_provider_ids p ON p.media_item_id = s.item_id
WHERE ";

const ARCHIVE_GROUP_BY: &str = "
GROUP BY s.user_id, mi.item_type, p.provider, p.provider_id";

/// Archived rows that already have somewhere to go, joined to that target item.
const ARCHIVED_RESTORE_SELECT: &str = "
SELECT
    a.user_id AS user_id,
    a.item_type AS item_type,
    a.provider AS provider,
    a.provider_id AS provider_id,
    target.item_id AS target_item_id,
    a.position_ticks AS position_ticks,
    a.is_played AS is_played,
    a.is_favorite AS is_favorite,
    a.play_count AS play_count,
    a.last_played_at AS last_played_at
FROM user_item_state_archive a
JOIN (
    SELECT
        p.item_type AS item_type,
        p.provider AS provider,
        p.provider_id AS provider_id,
        MIN(p.media_item_id) AS item_id
    FROM media_item_provider_ids p
    JOIN media_items m ON m.id = p.media_item_id
    WHERE m.removed_at IS NULL
    GROUP BY p.item_type, p.provider, p.provider_id
) target
    ON target.item_type = a.item_type
   AND target.provider = a.provider
   AND target.provider_id = a.provider_id
ORDER BY a.detached_at, a.user_id
LIMIT ?";

/// State still hanging off a soft-deleted item, joined to a live item carrying the same identity.
///
/// This covers "the same file showed up under a new path in another library while the old item was
/// only soft-deleted": the new item has a new id, so nothing points at the old state, but the
/// identity matches and the state can be copied across. The `NOT EXISTS` filter keeps rows that are
/// already up to date out of the batch, so the pass always makes progress instead of retrying the
/// same finished rows.
const REMOVED_ITEM_RESTORE_SELECT: &str = "
SELECT
    s.user_id AS user_id,
    live.id AS target_item_id,
    s.position_ticks AS position_ticks,
    s.is_played AS is_played,
    s.is_favorite AS is_favorite,
    s.play_count AS play_count,
    s.last_played_at AS last_played_at
FROM user_item_state s
JOIN media_items src ON src.id = s.item_id
JOIN media_item_provider_ids sp ON sp.media_item_id = s.item_id
JOIN media_item_provider_ids tp
    ON tp.item_type = sp.item_type
   AND tp.provider = sp.provider
   AND tp.provider_id = sp.provider_id
JOIN media_items live ON live.id = tp.media_item_id
WHERE src.removed_at IS NOT NULL
  AND live.removed_at IS NULL
  AND NOT EXISTS (
      SELECT 1 FROM user_item_state t
      WHERE t.user_id = s.user_id
        AND t.item_id = live.id
        AND t.is_played >= s.is_played
        AND t.is_favorite >= s.is_favorite
        AND t.play_count >= s.play_count
        AND COALESCE(t.last_played_at, 0) >= COALESCE(s.last_played_at, 0)
  )
ORDER BY s.last_played_at, s.user_id, s.item_id
LIMIT ?";

/// The watch state of one user on one item, detached from the item it was recorded against.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct WatchState {
    position_ticks: i64,
    is_played: bool,
    is_favorite: bool,
    play_count: i64,
    last_played_at: Option<i64>,
}

impl WatchState {
    fn from_row(row: &sqlx::any::AnyRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            position_ticks: row.try_get("position_ticks")?,
            is_played: row.try_get::<i64, _>("is_played")? != 0,
            is_favorite: row.try_get::<i64, _>("is_favorite")? != 0,
            play_count: row.try_get("play_count")?,
            last_played_at: row.try_get("last_played_at")?,
        })
    }

    /// Folds two recordings of the same title into the one the user expects to see.
    ///
    /// Nothing the user earned is dropped: played and favourite are sticky, the play count keeps
    /// the higher number, and the position comes from the most recent playback. Only when both
    /// sides agree on the played flag can the newer recording win the position.
    fn fold(self, other: Self) -> Self {
        let other_is_newer = other.last_played_at.unwrap_or(0) > self.last_played_at.unwrap_or(0);
        let position_ticks = if self.is_played != other.is_played {
            // A finished watch wins over an unfinished one even when it is older: its position was
            // already reset, and the unfinished copy must not drag the user back into the middle.
            if self.is_played {
                self.position_ticks
            } else {
                other.position_ticks
            }
        } else if other_is_newer {
            other.position_ticks
        } else {
            self.position_ticks
        };
        Self {
            position_ticks,
            is_played: self.is_played || other.is_played,
            is_favorite: self.is_favorite || other.is_favorite,
            play_count: self.play_count.max(other.play_count),
            last_played_at: match (self.last_played_at, other.last_played_at) {
                (Some(left), Some(right)) => Some(left.max(right)),
                (left, right) => left.or(right),
            },
        }
    }

    fn is_empty(self) -> bool {
        self == Self::default()
    }
}

/// Which archive row a restore candidate came from, so it can be consumed once reattached.
struct ArchiveKey {
    user_id: String,
    item_type: String,
    provider: String,
    provider_id: String,
}

struct RestoreCandidate {
    user_id: String,
    target_item_id: String,
    state: WatchState,
    archive: Option<ArchiveKey>,
}

impl Database {
    /// Copies the watch state of exactly these items into the archive.
    ///
    /// Every identified item contributes one row per provider identity, so one archive row can be
    /// matched by TMDb while another is matched by IMDb. Items without an identity contribute
    /// nothing, which is what lets the purge tell "safe to drop" from "would lose history".
    pub(crate) async fn archive_watch_state_for_items(
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
        let scope = format!("s.item_id IN ({placeholders})");
        let binds = item_ids.iter().map(String::as_str).collect::<Vec<_>>();
        self.archive_watch_state_in_scope(transaction, &scope, &binds)
            .await
    }

    /// Copies the watch state of every identified item in a library into the archive.
    ///
    /// Called before the library-deletion batches remove anything, and repeated per batch on
    /// purpose: the archive write is an upsert, so a resumed deletion job folds the same values in
    /// again instead of needing to know which items an earlier run already covered.
    pub(crate) async fn archive_watch_state_for_library(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        library_id: &str,
    ) -> Result<u64, StorageError> {
        let scope = "s.item_id IN (SELECT id FROM media_items WHERE library_id = ?)";
        self.archive_watch_state_in_scope(transaction, scope, &[library_id])
            .await
    }

    async fn archive_watch_state_in_scope(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        scope: &str,
        binds: &[&str],
    ) -> Result<u64, StorageError> {
        let sql =
            format!("{ARCHIVE_INSERT_PREFIX}{scope}{ARCHIVE_GROUP_BY}{ARCHIVE_UPSERT_CLAUSE}");
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

    /// Reattaches detached watch state to the items that carry the same identity again.
    ///
    /// Returns how many items received state. Each group of merges commits on its own so the write
    /// lock is never held for the whole pass, and an archived row is only consumed once its target
    /// holds what the row carried.
    pub async fn restore_detached_watch_state(&self, batch_size: i64) -> Result<u64, StorageError> {
        if batch_size <= 0 {
            return Ok(0);
        }
        let candidates = self.watch_state_restore_candidates(batch_size).await?;
        let mut restored = 0_u64;
        for chunk in candidates.chunks(WATCH_STATE_RESTORE_CHUNK) {
            let mut transaction = self.begin_metadata_write_transaction().await?;
            for candidate in chunk {
                let merged = if candidate.state.is_empty() {
                    false
                } else {
                    self.merge_watch_state(
                        &mut transaction,
                        &candidate.user_id,
                        &candidate.target_item_id,
                        candidate.state,
                    )
                    .await?
                };
                if merged {
                    restored = restored.saturating_add(1);
                }
                if let Some(archive) = candidate.archive.as_ref() {
                    // Consumed even when there was nothing to apply: an all-default row would
                    // otherwise come back as a candidate on every pass and crowd out real work.
                    self.delete_archived_watch_state(&mut transaction, archive)
                        .await?;
                }
            }
            transaction
                .commit()
                .await
                .map_err(|source| self.storage_error(source))?;
        }
        Ok(restored)
    }

    async fn watch_state_restore_candidates(
        &self,
        batch_size: i64,
    ) -> Result<Vec<RestoreCandidate>, StorageError> {
        let mut candidates = Vec::new();
        let archived = self
            .query(ARCHIVED_RESTORE_SELECT)
            .bind(batch_size)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| self.storage_error(source))?;
        for row in &archived {
            candidates.push(RestoreCandidate {
                user_id: row
                    .try_get("user_id")
                    .map_err(|source| self.storage_error(source))?,
                target_item_id: row
                    .try_get("target_item_id")
                    .map_err(|source| self.storage_error(source))?,
                state: WatchState::from_row(row).map_err(|source| self.storage_error(source))?,
                archive: Some(ArchiveKey {
                    user_id: row
                        .try_get("user_id")
                        .map_err(|source| self.storage_error(source))?,
                    item_type: row
                        .try_get("item_type")
                        .map_err(|source| self.storage_error(source))?,
                    provider: row
                        .try_get("provider")
                        .map_err(|source| self.storage_error(source))?,
                    provider_id: row
                        .try_get("provider_id")
                        .map_err(|source| self.storage_error(source))?,
                }),
            });
        }
        let removed = self
            .query(REMOVED_ITEM_RESTORE_SELECT)
            .bind(batch_size)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| self.storage_error(source))?;
        for row in &removed {
            candidates.push(RestoreCandidate {
                user_id: row
                    .try_get("user_id")
                    .map_err(|source| self.storage_error(source))?,
                target_item_id: row
                    .try_get("target_item_id")
                    .map_err(|source| self.storage_error(source))?,
                state: WatchState::from_row(row).map_err(|source| self.storage_error(source))?,
                archive: None,
            });
        }
        Ok(candidates)
    }

    /// Raises a live item's state to at least `incoming`, never lowering a value.
    ///
    /// Returns whether the stored row changed, which is also what decides whether any cache keyed
    /// on `SUM(version)` has to notice.
    async fn merge_watch_state(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        user_id: &str,
        item_id: &str,
        incoming: WatchState,
    ) -> Result<bool, StorageError> {
        let existing = self
            .query(
                "SELECT position_ticks, is_played, is_favorite, play_count, last_played_at
                 FROM user_item_state WHERE user_id = ? AND item_id = ?",
            )
            .bind(user_id)
            .bind(item_id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(|source| self.storage_error(source))?;
        let merged = match existing.as_ref() {
            Some(row) => {
                let current =
                    WatchState::from_row(row).map_err(|source| self.storage_error(source))?;
                let merged = current.fold(incoming);
                if merged == current {
                    return Ok(false);
                }
                merged
            }
            None => incoming,
        };
        if merged.is_empty() {
            return Ok(false);
        }
        self.query(
            "INSERT INTO user_item_state (
                 user_id, item_id, position_ticks, is_played, is_favorite, play_count,
                 last_played_at, version
             ) VALUES (?, ?, ?, ?, ?, ?, ?, 1)
             ON CONFLICT (user_id, item_id) DO UPDATE SET
                 position_ticks = excluded.position_ticks,
                 is_played = excluded.is_played,
                 is_favorite = excluded.is_favorite,
                 play_count = excluded.play_count,
                 last_played_at = excluded.last_played_at,
                 version = user_item_state.version + 1",
        )
        .bind(user_id)
        .bind(item_id)
        .bind(merged.position_ticks)
        .bind(i64::from(merged.is_played))
        .bind(i64::from(merged.is_favorite))
        .bind(merged.play_count)
        .bind(merged.last_played_at)
        .execute(&mut **transaction)
        .await
        .map_err(|source| self.storage_error(source))?;
        Ok(true)
    }

    async fn delete_archived_watch_state(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        key: &ArchiveKey,
    ) -> Result<(), StorageError> {
        self.query(
            "DELETE FROM user_item_state_archive
             WHERE user_id = ? AND item_type = ? AND provider = ? AND provider_id = ?",
        )
        .bind(&key.user_id)
        .bind(&key.item_type)
        .bind(&key.provider)
        .bind(&key.provider_id)
        .execute(&mut **transaction)
        .await
        .map(|_| ())
        .map_err(|source| self.storage_error(source))
    }

    /// Reattaches detached watch state and playback history on a schedule.
    ///
    /// Runs after the database is ready rather than inside a request, and tolerates a failed pass:
    /// the next one sees the same rows, because a row is only consumed once it was applied.
    pub fn spawn_detached_state_reconcile(&self) {
        let database = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(WATCH_STATE_RECONCILE_INITIAL_DELAY).await;
            let mut ticker = tokio::time::interval(WATCH_STATE_RECONCILE_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                ticker.tick().await;
                match database
                    .restore_detached_watch_state(WATCH_STATE_RECONCILE_BATCH)
                    .await
                {
                    Ok(0) => {}
                    Ok(restored) => {
                        tracing::info!(restored, "reattached detached watch state");
                    }
                    Err(error) => {
                        tracing::warn!(%error, "reattaching detached watch state failed");
                    }
                }
                match database
                    .restore_detached_playback_history(
                        super::playback_history_archive::PLAYBACK_HISTORY_RESTORE_BATCH,
                    )
                    .await
                {
                    Ok(0) => {}
                    Ok(restored) => {
                        tracing::info!(restored, "reattached detached playback history");
                    }
                    Err(error) => {
                        tracing::warn!(%error, "reattaching detached playback history failed");
                    }
                }
            }
        });
    }
}
