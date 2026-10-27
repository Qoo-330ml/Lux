//! Derived next-episode pointers for continue-watching lists.

use super::*;

#[derive(Debug, Clone, Eq, PartialEq, sqlx::FromRow)]
#[allow(dead_code)] // wired by the continue-watching switch in LUX-867
pub(crate) struct StoredUserSeriesNext {
    pub(crate) user_id: String,
    pub(crate) series_id: String,
    pub(crate) next_item_id: String,
    pub(crate) position_ticks: i64,
    pub(crate) played: i64,
    pub(crate) updated_at: i64,
}

#[allow(dead_code)] // wired by the continue-watching switch in LUX-867
impl Database {
    pub(crate) async fn upsert_user_series_next(
        &self,
        user_id: &str,
        series_id: &str,
        next_item_id: &str,
        position_ticks: i64,
        played: bool,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO user_series_next (
                 user_id, series_id, next_item_id, position_ticks, played, updated_at
             ) VALUES (?, ?, ?, ?, ?, unixepoch())
             ON CONFLICT(user_id, series_id) DO UPDATE SET
                 next_item_id = excluded.next_item_id,
                 position_ticks = excluded.position_ticks,
                 played = excluded.played,
                 updated_at = unixepoch()",
        )
        .bind(user_id)
        .bind(series_id)
        .bind(next_item_id)
        .bind(position_ticks.max(0))
        .bind(database_flag(played))
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_user_series_next(
        &self,
        user_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredUserSeriesNext>, StorageError> {
        self.query_as(
            "SELECT user_id, series_id, next_item_id, position_ticks, played, updated_at
             FROM user_series_next
             WHERE user_id = ?
             ORDER BY updated_at DESC, series_id
             LIMIT ? OFFSET ?",
        )
        .bind(user_id)
        .bind(limit.clamp(1, 1_000))
        .bind(offset.max(0))
        .fetch_all(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn delete_user_series_next(
        &self,
        user_id: &str,
        series_id: &str,
    ) -> Result<bool, StorageError> {
        self.query("DELETE FROM user_series_next WHERE user_id = ? AND series_id = ?")
            .bind(user_id)
            .bind(series_id)
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected() > 0)
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }
}

impl Database {
    /// Rebuilds every user's next-episode pointer from `user_item_state` and the hierarchy.
    ///
    /// Only completed episodes drive the pointer: the next episode is the first unplayed,
    /// available episode after the latest completed one in the same series.
    #[allow(dead_code)] // wired by the startup rebuild in LUX-867
    pub(crate) async fn rebuild_user_series_next(&self) -> Result<u64, StorageError> {
        self.query("DELETE FROM user_series_next")
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let result = self
            .query(
                "WITH completed AS (
                     SELECT us.user_id, mi.series_id, mi.season_number, mi.episode_number,
                            ROW_NUMBER() OVER (
                                PARTITION BY us.user_id, mi.series_id
                                ORDER BY us.last_played_at DESC NULLS LAST,
                                         mi.season_number DESC, mi.episode_number DESC,
                                         mi.id DESC
                            ) AS played_rank
                     FROM media_items mi
                     JOIN user_item_state us ON us.item_id = mi.id
                     WHERE mi.item_type = 'EPISODE'
                       AND mi.series_id IS NOT NULL
                       AND mi.season_number IS NOT NULL
                       AND mi.episode_number IS NOT NULL
                       AND mi.removed_at IS NULL
                       AND us.is_played = 1
                 ),
                 latest AS (
                     SELECT user_id, series_id, season_number, episode_number
                     FROM completed WHERE played_rank = 1
                 ),
                 next_episode AS (
                     SELECT latest.user_id, latest.series_id, mi.id,
                            ROW_NUMBER() OVER (
                                PARTITION BY latest.user_id, latest.series_id
                                ORDER BY mi.season_number, mi.episode_number,
                                         mi.sort_title, mi.id
                            ) AS next_rank
                     FROM latest
                     JOIN media_items mi
                       ON mi.series_id = latest.series_id
                      AND mi.item_type = 'EPISODE'
                      AND (mi.season_number > latest.season_number
                        OR (mi.season_number = latest.season_number
                            AND mi.episode_number > latest.episode_number))
                     LEFT JOIN user_item_state us
                       ON us.item_id = mi.id AND us.user_id = latest.user_id
                     WHERE mi.has_available_source = 1
                       AND mi.removed_at IS NULL
                       AND COALESCE(us.is_played, 0) = 0
                       AND COALESCE(us.hide_from_resume, 0) = 0
                 )
                 INSERT INTO user_series_next (
                     user_id, series_id, next_item_id, position_ticks, played, updated_at
                 )
                 SELECT user_id, series_id, id, 0, 0, unixepoch()
                 FROM next_episode
                 WHERE next_rank = 1
                 ON CONFLICT(user_id, series_id) DO UPDATE SET
                     next_item_id = excluded.next_item_id,
                     position_ticks = 0,
                     played = 0,
                     updated_at = unixepoch()",
            )
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result.rows_affected())
    }
}
