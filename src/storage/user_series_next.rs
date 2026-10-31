//! Derived resume pointers for the continue-watching read model.
//!
//! `user_series_next` holds one row per user and series: the episode to resume, which is either the
//! in-progress episode or the next unplayed one after the latest completed episode. The row also
//! carries the `last_played_at` of the episode that produced it, so the list can be ordered without
//! re-aggregating `user_item_state`.

use super::*;

#[cfg(test)]
#[derive(Debug, Clone, Eq, PartialEq, sqlx::FromRow)]
pub(crate) struct StoredUserSeriesNext {
    pub(crate) user_id: String,
    pub(crate) series_id: String,
    pub(crate) next_item_id: String,
    pub(crate) position_ticks: i64,
    pub(crate) played: i64,
    pub(crate) last_played_at: Option<i64>,
    pub(crate) updated_at: i64,
}

/// Continue-watching page backed by the derived pointers plus the in-progress non-episode items.
pub(crate) struct ResumePointerQuery<'a> {
    pub(crate) user_id: &'a str,
    pub(crate) library_ids: &'a [String],
    pub(crate) item_types: &'a [&'a str],
    pub(crate) content_restrictions: Option<&'a UserContentRestrictions>,
    pub(crate) played_percent: i64,
    pub(crate) minimum_ticks: i64,
    pub(crate) offset: i64,
    pub(crate) limit: i64,
}

/// `resume_min_ticks` server setting with the default the authoritative query also applies.
///
/// Every path that needs the threshold resolves it through this one definition: the pointer
/// candidate query, the resume list, and the per-event crossing check in
/// [`Database::record_playback_event`].
pub(super) const RESUME_MINIMUM_TICKS_SQL: &str = "COALESCE(
    (SELECT CAST(value AS BIGINT) FROM server_settings WHERE key = 'resume_min_ticks'),
    1200000000
)";

/// Per-user played percentage, defaulted and clamped like `home_resume_settings`.
const RESUME_PLAYED_PERCENT_SQL: &str = "COALESCE(
    (SELECT CASE WHEN played_percent < 1 THEN 1 WHEN played_percent > 100 THEN 100
                 ELSE played_percent END
     FROM user_playback_settings WHERE user_id = us.user_id),
    95
)";

/// `WHERE` guard of the pointer UPSERT: only a pointer whose value really changed is written.
///
/// `IS DISTINCT FROM` compares equal NULLs as equal and is spelled identically by SQLite and
/// PostgreSQL, where `IS NOT` between two expressions is SQLite-only syntax. `updated_at` is
/// deliberately absent: it is the write marker, not part of the pointer value.
const POINTER_CHANGED_PREDICATE: &str =
    "user_series_next.next_item_id IS DISTINCT FROM excluded.next_item_id
     OR user_series_next.position_ticks IS DISTINCT FROM excluded.position_ticks
     OR user_series_next.played IS DISTINCT FROM excluded.played
     OR user_series_next.last_played_at IS DISTINCT FROM excluded.last_played_at";

fn placeholders(count: usize) -> String {
    std::iter::repeat_n("?", count)
        .collect::<Vec<_>>()
        .join(", ")
}

impl Database {
    #[cfg(test)]
    pub(crate) async fn upsert_user_series_next(
        &self,
        user_id: &str,
        series_id: &str,
        next_item_id: &str,
        position_ticks: i64,
        played: bool,
        last_played_at: Option<i64>,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO user_series_next (
                 user_id, series_id, next_item_id, position_ticks, played, last_played_at,
                 updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, unixepoch())
             ON CONFLICT(user_id, series_id) DO UPDATE SET
                 next_item_id = excluded.next_item_id,
                 position_ticks = excluded.position_ticks,
                 played = excluded.played,
                 last_played_at = excluded.last_played_at,
                 updated_at = unixepoch()",
        )
        .bind(user_id)
        .bind(series_id)
        .bind(next_item_id)
        .bind(position_ticks.max(0))
        .bind(database_flag(played))
        .bind(last_played_at)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(self.sqlx_err())
    }

    #[cfg(test)]
    pub(crate) async fn list_user_series_next(
        &self,
        user_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredUserSeriesNext>, StorageError> {
        self.query_as(
            "SELECT user_id, series_id, next_item_id, position_ticks, played, last_played_at,
                    updated_at
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
        .map_err(self.sqlx_err())
    }

    #[cfg(test)]
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
            .map_err(self.sqlx_err())
    }

    /// Pages the continue-watching list from the derived pointers.
    ///
    /// Episodes come from `user_series_next`; in-progress movies and videos still come from
    /// `user_item_state`, which is a narrow index scan for one user. Both branches apply the
    /// configured resume thresholds and the catalog visibility rules, and the page is ordered by the
    /// driver timestamp exactly like the authoritative query.
    pub(crate) async fn list_resume_pointer_ids<'a>(
        &self,
        query: &'a ResumePointerQuery<'a>,
    ) -> Result<(Vec<String>, i64), StorageError> {
        if query.library_ids.is_empty() || query.item_types.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let pointer_types = query
            .item_types
            .iter()
            .copied()
            .filter(|item_type| *item_type == "EPISODE")
            .collect::<Vec<_>>();
        let progress_types = query
            .item_types
            .iter()
            .copied()
            .filter(|item_type| *item_type != "EPISODE")
            .collect::<Vec<_>>();
        let runtime_ticks = resume_runtime_ticks_sql();
        let library_placeholders = placeholders(query.library_ids.len());
        let mut branches = Vec::new();
        let mut binds: Vec<CatalogBind<'a>> = Vec::new();
        if !pointer_types.is_empty() {
            let item_type_placeholders = placeholders(pointer_types.len());
            let mut restriction = String::new();
            let mut restriction_binds = Vec::new();
            append_content_restriction_predicate(
                &mut restriction,
                &mut restriction_binds,
                query.content_restrictions,
            );
            branches.push(format!(
                "SELECT usn.next_item_id AS item_id, mi.sort_title,
                        COALESCE(us.last_played_at, usn.last_played_at, usn.updated_at) AS resume_at
                 FROM user_series_next usn
                 JOIN media_items mi ON mi.id = usn.next_item_id
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 LEFT JOIN user_item_state us
                   ON us.item_id = mi.id AND us.user_id = usn.user_id
                 WHERE usn.user_id = ?
                   AND mi.item_type IN ({item_type_placeholders})
                   AND mi.removed_at IS NULL
                   AND mi.library_id IN ({library_placeholders}){restriction}{CATALOG_VISIBLE_PREDICATE}
                   AND COALESCE(us.is_played, 0) = 0
                   AND COALESCE(us.hide_from_resume, 0) = 0"
            ));
            binds.push(CatalogBind::Text(query.user_id));
            binds.extend(
                pointer_types
                    .iter()
                    .map(|item_type| CatalogBind::Text(item_type)),
            );
            binds.extend(
                query
                    .library_ids
                    .iter()
                    .map(|library_id| CatalogBind::Text(library_id)),
            );
            binds.extend(restriction_binds.iter().copied());
        }
        if !progress_types.is_empty() {
            let item_type_placeholders = placeholders(progress_types.len());
            let mut restriction = String::new();
            let mut restriction_binds = Vec::new();
            append_content_restriction_predicate(
                &mut restriction,
                &mut restriction_binds,
                query.content_restrictions,
            );
            branches.push(format!(
                "SELECT mi.id AS item_id, mi.sort_title, us.last_played_at AS resume_at
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 JOIN user_item_state us ON us.item_id = mi.id AND us.user_id = ?
                 WHERE mi.item_type IN ({item_type_placeholders})
                   AND mi.removed_at IS NULL
                   AND us.is_played = 0 AND us.hide_from_resume = 0
                   AND us.position_ticks >= ?
                   AND mi.library_id IN ({library_placeholders}){restriction}{CATALOG_VISIBLE_PREDICATE}
                   AND {runtime_ticks} > 0
                   AND us.position_ticks * 100 < {runtime_ticks} * ?"
            ));
            binds.push(CatalogBind::Text(query.user_id));
            binds.extend(
                progress_types
                    .iter()
                    .map(|item_type| CatalogBind::Text(item_type)),
            );
            binds.push(CatalogBind::Integer(query.minimum_ticks));
            binds.extend(
                query
                    .library_ids
                    .iter()
                    .map(|library_id| CatalogBind::Text(library_id)),
            );
            binds.extend(restriction_binds.iter().copied());
            binds.push(CatalogBind::Integer(query.played_percent));
        }
        let union = branches.join(" UNION ALL ");
        let list_sql = format!(
            "WITH resume AS ({union})
             SELECT item_id FROM resume
             ORDER BY resume_at DESC NULLS LAST, sort_title, item_id
             LIMIT ? OFFSET ?"
        );
        let mut statement = self.query_as::<(String,)>(sqlx::AssertSqlSafe(list_sql));
        for bind in &binds {
            statement = match bind {
                CatalogBind::Text(value) => statement.bind(*value),
                CatalogBind::Integer(value) => statement.bind(*value),
                CatalogBind::Real(value) => statement.bind(*value),
            };
        }
        let ids = statement
            .bind(query.limit)
            .bind(query.offset.max(0))
            .fetch_all(&self.pool)
            .await
            .map_err(self.sqlx_err())?
            .into_iter()
            .map(|(item_id,)| item_id)
            .collect::<Vec<_>>();
        let count_sql = format!("WITH resume AS ({union}) SELECT COUNT(*) FROM resume");
        let mut statement = self.query_scalar::<i64>(sqlx::AssertSqlSafe(count_sql));
        for bind in &binds {
            statement = match bind {
                CatalogBind::Text(value) => statement.bind(*value),
                CatalogBind::Integer(value) => statement.bind(*value),
                CatalogBind::Real(value) => statement.bind(*value),
            };
        }
        let total = statement
            .fetch_one(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        Ok((ids, total))
    }
}

impl Database {
    /// Rebuilds every user's pointer from `user_item_state` and the hierarchy.
    ///
    /// The candidate set mirrors the authoritative continue-watching query: the user's unfinished
    /// episodes plus the first unplayed episode after the most recently completed one. The pointer
    /// is the highest episode number in that set, so the derived list returns the same item as the
    /// legacy aggregation.
    ///
    /// Both writes share one transaction and one candidate set: the UPSERT keeps a pointer row in
    /// place unless its value changed, and only the series without a candidate row lose theirs, so
    /// a rebuild never leaves the table empty while a next episode exists.
    pub(crate) async fn rebuild_user_series_next(&self) -> Result<u64, StorageError> {
        let mut transaction = self.pool.begin().await.map_err(self.sqlx_err())?;
        let candidates = format!(
            "WITH in_progress AS (
                     SELECT us.user_id, mi.series_id, mi.id, mi.season_number,
                            mi.episode_number, us.position_ticks, us.last_played_at
                     FROM media_items mi
                     JOIN user_item_state us ON us.item_id = mi.id
                     WHERE mi.item_type = 'EPISODE'
                       AND mi.series_id IS NOT NULL
                       AND mi.season_number IS NOT NULL
                       AND mi.episode_number IS NOT NULL
                       AND mi.removed_at IS NULL
                       AND us.is_played = 0
                       AND us.position_ticks > 0
                       AND COALESCE(us.hide_from_resume, 0) = 0
                       AND {runtime_ticks} > 0
                       AND us.position_ticks >= {minimum_ticks}
                       AND us.position_ticks * 100 < {runtime_ticks} * {played_percent}
                 ),
                 completed AS (
                     SELECT us.user_id, mi.series_id, mi.season_number, mi.episode_number,
                            mi.id, us.last_played_at,
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
                 latest_completed AS (
                     SELECT user_id, series_id, season_number, episode_number, last_played_at
                     FROM completed WHERE played_rank = 1
                 ),
                 next_episodes_ranked AS (
                     SELECT latest.user_id, latest.series_id, mi.id, mi.season_number,
                            mi.episode_number,
                            COALESCE(us.last_played_at, latest.last_played_at) AS last_played_at,
                            ROW_NUMBER() OVER (
                                PARTITION BY latest.user_id, latest.series_id
                                ORDER BY mi.season_number, mi.episode_number,
                                         mi.sort_title, mi.id
                            ) AS next_rank
                     FROM latest_completed latest
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
                 ),
                 candidates AS (
                     SELECT user_id, series_id, id, season_number, episode_number,
                            position_ticks, last_played_at, 0 AS candidate_priority
                     FROM in_progress
                     UNION ALL
                     SELECT user_id, series_id, id, season_number, episode_number,
                            0 AS position_ticks, last_played_at, 1 AS candidate_priority
                     FROM next_episodes_ranked WHERE next_rank = 1
                 ),
                 resume AS (
                     SELECT user_id, series_id, id, position_ticks, last_played_at,
                            ROW_NUMBER() OVER (
                                PARTITION BY user_id, series_id
                                ORDER BY season_number DESC, episode_number DESC,
                                         candidate_priority,
                                         last_played_at DESC NULLS LAST, id DESC
                            ) AS resume_rank
                     FROM candidates
                 )",
            runtime_ticks = resume_runtime_ticks_sql(),
            minimum_ticks = RESUME_MINIMUM_TICKS_SQL,
            played_percent = RESUME_PLAYED_PERCENT_SQL,
        );
        let upsert_sql = format!(
            "{candidates}
             INSERT INTO user_series_next (
                 user_id, series_id, next_item_id, position_ticks, played, last_played_at,
                 updated_at
             )
             SELECT user_id, series_id, id, position_ticks, 0, last_played_at, unixepoch()
             FROM resume
             WHERE resume_rank = 1
             ON CONFLICT(user_id, series_id) DO UPDATE SET
                 next_item_id = excluded.next_item_id,
                 position_ticks = excluded.position_ticks,
                 played = excluded.played,
                 last_played_at = excluded.last_played_at,
                 updated_at = unixepoch()
             WHERE {POINTER_CHANGED_PREDICATE}"
        );
        let written = self
            .query(sqlx::AssertSqlSafe(upsert_sql))
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?
            .rows_affected();
        // Only the series that the candidate set no longer covers lose their pointer.
        let delete_sql = format!(
            "{candidates}
             DELETE FROM user_series_next
             WHERE NOT EXISTS (
                 SELECT 1 FROM resume
                 WHERE resume.resume_rank = 1
                   AND resume.user_id = user_series_next.user_id
                   AND resume.series_id = user_series_next.series_id
             )"
        );
        self.query(sqlx::AssertSqlSafe(delete_sql))
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        transaction.commit().await.map_err(self.sqlx_err())?;
        Ok(written)
    }
}

impl Database {
    /// Refreshes one user/series pointer after a playback state change.
    ///
    /// The read and the write share one transaction and the write is a single UPSERT, so a
    /// concurrent reader sees either the previous pointer or the refreshed one and never a series
    /// without a pointer.
    pub(crate) async fn refresh_user_series_next_for_item(
        &self,
        user_id: &str,
        item_id: &str,
    ) -> Result<(), StorageError> {
        let mut transaction = self.pool.begin().await.map_err(self.sqlx_err())?;
        self.refresh_user_series_next_for_item_in(&mut transaction, user_id, item_id)
            .await?;
        transaction.commit().await.map_err(self.sqlx_err())
    }

    /// Refresh variant that joins the caller's transaction and reports the episode scope it read.
    ///
    /// Non-episode items have no resume pointer, so they return `None`. Episodes always return
    /// their scope; an episode without a series has nothing to point at, so only the pointer write
    /// is skipped.
    pub(crate) async fn refresh_user_series_next_for_item_in(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        user_id: &str,
        item_id: &str,
    ) -> Result<Option<PlaybackContainerScope>, StorageError> {
        let line: Option<(Option<String>, Option<String>)> = self
            .query_as(
                "SELECT parent_id, series_id FROM media_items
                 WHERE id = ? AND item_type = 'EPISODE'",
            )
            .bind(item_id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(self.sqlx_err())?;
        let Some((parent_id, series_id)) = line else {
            return Ok(None);
        };
        let scope = PlaybackContainerScope {
            parent_id,
            series_id,
        };
        if let Some(series_id) = scope.series_id.as_deref() {
            self.write_user_series_next_pointer(transaction, user_id, series_id)
                .await?;
        }
        Ok(Some(scope))
    }

    /// Resolves the resume threshold that decides whether an episode is in progress.
    ///
    /// `None` means the item is not an episode and has no resume pointer. The value comes from the
    /// same SQL the pointer candidate query applies, so there is no second copy of the setting or
    /// of its default.
    pub(crate) async fn resume_threshold_for_item_in(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        item_id: &str,
    ) -> Result<Option<i64>, StorageError> {
        let sql = format!(
            "SELECT CASE WHEN EXISTS (
                 SELECT 1 FROM media_items WHERE id = ? AND item_type = 'EPISODE'
             ) THEN ({RESUME_MINIMUM_TICKS_SQL}) END"
        );
        self.query_scalar::<Option<i64>>(sqlx::AssertSqlSafe(sql))
            .bind(item_id)
            .fetch_one(&mut **transaction)
            .await
            .map_err(self.sqlx_err())
    }

    /// Rewrites one pointer in place.
    ///
    /// The candidate query resolves the next episode first and the pointer is then written by one
    /// UPSERT guarded by [`POINTER_CHANGED_PREDICATE`], so a refresh that recomputes the same
    /// pointer writes nothing and a changed one keeps its row. Only when the series has no
    /// in-progress or unplayed episode left is the row deleted, so a refresh never leaves the
    /// series without a pointer while a next episode still exists.
    async fn write_user_series_next_pointer(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        user_id: &str,
        series_id: &str,
    ) -> Result<(), StorageError> {
        let candidate_sql = format!(
            "WITH in_progress AS (
                 SELECT mi.id, mi.season_number, mi.episode_number, us.position_ticks,
                        us.last_played_at
                 FROM media_items mi
                 JOIN user_item_state us ON us.item_id = mi.id AND us.user_id = ?
                 WHERE mi.item_type = 'EPISODE'
                   AND mi.series_id = ?
                   AND mi.season_number IS NOT NULL
                   AND mi.episode_number IS NOT NULL
                   AND mi.removed_at IS NULL
                   AND us.is_played = 0
                   AND us.position_ticks > 0
                   AND COALESCE(us.hide_from_resume, 0) = 0
                   AND {runtime_ticks} > 0
                   AND us.position_ticks >= {minimum_ticks}
                   AND us.position_ticks * 100 < {runtime_ticks} * {played_percent}
             ),
             completed AS (
                 SELECT mi.season_number, mi.episode_number, mi.id, us.last_played_at,
                        ROW_NUMBER() OVER (
                            ORDER BY us.last_played_at DESC NULLS LAST,
                                     mi.season_number DESC, mi.episode_number DESC, mi.id DESC
                        ) AS played_rank
                 FROM media_items mi
                 JOIN user_item_state us ON us.item_id = mi.id AND us.user_id = ?
                 WHERE mi.item_type = 'EPISODE'
                   AND mi.series_id = ?
                   AND mi.season_number IS NOT NULL
                   AND mi.episode_number IS NOT NULL
                   AND mi.removed_at IS NULL
                   AND us.is_played = 1
             ),
             latest_completed AS (
                 SELECT season_number, episode_number, last_played_at
                 FROM completed WHERE played_rank = 1
             ),
             next_episodes_ranked AS (
                 SELECT mi.id, mi.season_number, mi.episode_number,
                        COALESCE(us.last_played_at, latest.last_played_at) AS last_played_at,
                        ROW_NUMBER() OVER (
                            ORDER BY mi.season_number, mi.episode_number, mi.sort_title, mi.id
                        ) AS next_rank
                 FROM latest_completed latest
                 JOIN media_items mi
                   ON mi.series_id = ?
                  AND mi.item_type = 'EPISODE'
                  AND (mi.season_number > latest.season_number
                    OR (mi.season_number = latest.season_number
                        AND mi.episode_number > latest.episode_number))
                 LEFT JOIN user_item_state us ON us.item_id = mi.id AND us.user_id = ?
                 WHERE mi.has_available_source = 1
                   AND mi.removed_at IS NULL
                   AND COALESCE(us.is_played, 0) = 0
                   AND COALESCE(us.hide_from_resume, 0) = 0
             ),
             candidates AS (
                 SELECT id, season_number, episode_number, position_ticks, last_played_at,
                        0 AS candidate_priority
                 FROM in_progress
                 UNION ALL
                 SELECT id, season_number, episode_number, 0 AS position_ticks,
                        last_played_at, 1 AS candidate_priority
                 FROM next_episodes_ranked WHERE next_rank = 1
             ),
             resume AS (
                 SELECT id, position_ticks, last_played_at,
                        ROW_NUMBER() OVER (
                            ORDER BY season_number DESC, episode_number DESC,
                                     candidate_priority,
                                     last_played_at DESC NULLS LAST, id DESC
                        ) AS resume_rank
                 FROM candidates
             )
             SELECT id, position_ticks, last_played_at
             FROM resume
             WHERE resume_rank = 1",
            runtime_ticks = resume_runtime_ticks_sql(),
            minimum_ticks = RESUME_MINIMUM_TICKS_SQL,
            played_percent = RESUME_PLAYED_PERCENT_SQL,
        );
        let candidate: Option<(String, i64, Option<i64>)> = self
            .query_as(sqlx::AssertSqlSafe(candidate_sql))
            .bind(user_id)
            .bind(series_id)
            .bind(user_id)
            .bind(series_id)
            .bind(series_id)
            .bind(user_id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(self.sqlx_err())?;
        let Some((next_item_id, position_ticks, last_played_at)) = candidate else {
            // No in-progress or next episode is left for this series, so the pointer goes away.
            return self
                .query("DELETE FROM user_series_next WHERE user_id = ? AND series_id = ?")
                .bind(user_id)
                .bind(series_id)
                .execute(&mut **transaction)
                .await
                .map(|_| ())
                .map_err(self.sqlx_err());
        };
        let upsert_sql = format!(
            "INSERT INTO user_series_next (
                 user_id, series_id, next_item_id, position_ticks, played, last_played_at,
                 updated_at
             ) VALUES (?, ?, ?, ?, 0, ?, unixepoch())
             ON CONFLICT(user_id, series_id) DO UPDATE SET
                 next_item_id = excluded.next_item_id,
                 position_ticks = excluded.position_ticks,
                 played = excluded.played,
                 last_played_at = excluded.last_played_at,
                 updated_at = unixepoch()
             WHERE {POINTER_CHANGED_PREDICATE}"
        );
        self.query(sqlx::AssertSqlSafe(upsert_sql))
            .bind(user_id)
            .bind(series_id)
            .bind(next_item_id)
            .bind(position_ticks.max(0))
            .bind(last_played_at)
            .execute(&mut **transaction)
            .await
            .map(|_| ())
            .map_err(self.sqlx_err())
    }
}
