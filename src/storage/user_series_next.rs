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
