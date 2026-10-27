//! Derived resume pointers for the continue-watching read model.
//!
//! `user_series_next` holds one row per user and series: the episode to resume, which is either the
//! in-progress episode or the next unplayed one after the latest completed episode. The row also
//! carries the `last_played_at` of the episode that produced it, so the list can be ordered without
//! re-aggregating `user_item_state`.

use super::*;

#[derive(Debug, Clone, Eq, PartialEq, sqlx::FromRow)]
#[allow(dead_code)] // read back by the storage regression tests
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

fn placeholders(count: usize) -> String {
    std::iter::repeat_n("?", count)
        .collect::<Vec<_>>()
        .join(", ")
}

#[allow(dead_code)] // exercised by the storage regression tests
impl Database {
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
                   AND COALESCE(us.hide_from_resume, 0) = 0
                   AND (usn.position_ticks = 0
                        OR ({runtime_ticks} > 0
                            AND usn.position_ticks >= ?
                            AND usn.position_ticks * 100 < {runtime_ticks} * ?))"
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
            binds.push(CatalogBind::Integer(query.minimum_ticks));
            binds.push(CatalogBind::Integer(query.played_percent));
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
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
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
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok((ids, total))
    }
}

impl Database {
    /// Rebuilds every user's pointer from `user_item_state` and the hierarchy.
    ///
    /// The pointer is the in-progress episode when the latest activity left one unfinished;
    /// otherwise it is the first unplayed, available episode after the latest completed one.
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
                "WITH candidates AS (
                     SELECT us.user_id, mi.series_id, mi.season_number, mi.episode_number,
                            mi.id, us.is_played, us.position_ticks, us.last_played_at,
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
                       AND COALESCE(us.hide_from_resume, 0) = 0
                       AND (us.is_played = 1 OR us.position_ticks > 0)
                 ),
                 latest AS (
                     SELECT user_id, series_id, season_number, episode_number, id,
                            is_played, position_ticks, last_played_at
                     FROM candidates WHERE played_rank = 1
                 ),
                 next_episode AS (
                     SELECT latest.user_id, latest.series_id, mi.id,
                            COALESCE(us.last_played_at, latest.last_played_at) AS last_played_at,
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
                     WHERE latest.is_played = 1
                       AND mi.has_available_source = 1
                       AND mi.removed_at IS NULL
                       AND COALESCE(us.is_played, 0) = 0
                       AND COALESCE(us.hide_from_resume, 0) = 0
                 ),
                 resume AS (
                     SELECT user_id, series_id, id, position_ticks, last_played_at
                     FROM latest WHERE is_played = 0 AND position_ticks > 0
                     UNION ALL
                     SELECT user_id, series_id, id, 0, last_played_at
                     FROM next_episode WHERE next_rank = 1
                 )
                 INSERT INTO user_series_next (
                     user_id, series_id, next_item_id, position_ticks, played, last_played_at,
                     updated_at
                 )
                 SELECT user_id, series_id, id, position_ticks, 0, last_played_at, unixepoch()
                 FROM resume
                 WHERE 1 = 1
                 ON CONFLICT(user_id, series_id) DO UPDATE SET
                     next_item_id = excluded.next_item_id,
                     position_ticks = excluded.position_ticks,
                     played = 0,
                     last_played_at = excluded.last_played_at,
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

impl Database {
    /// Refreshes one user/series pointer after a playback state change.
    pub(crate) async fn refresh_user_series_next_for_item(
        &self,
        user_id: &str,
        item_id: &str,
    ) -> Result<(), StorageError> {
        let series_id: Option<Option<String>> = self
            .query_scalar(
                "SELECT series_id FROM media_items WHERE id = ? AND item_type = 'EPISODE'",
            )
            .bind(item_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let Some(Some(series_id)) = series_id else {
            return Ok(());
        };
        self.refresh_user_series_next_for_series(user_id, &series_id)
            .await
    }

    pub(crate) async fn refresh_user_series_next_for_series(
        &self,
        user_id: &str,
        series_id: &str,
    ) -> Result<(), StorageError> {
        self.query("DELETE FROM user_series_next WHERE user_id = ? AND series_id = ?")
            .bind(user_id)
            .bind(series_id)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.query(
            "WITH candidates AS (
                 SELECT mi.season_number, mi.episode_number, mi.id,
                        us.is_played, us.position_ticks, us.last_played_at,
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
                   AND COALESCE(us.hide_from_resume, 0) = 0
                   AND (us.is_played = 1 OR us.position_ticks > 0)
             ),
             latest AS (
                 SELECT season_number, episode_number, id, is_played, position_ticks,
                        last_played_at
                 FROM candidates WHERE played_rank = 1
             ),
             next_episode AS (
                 SELECT mi.id,
                        COALESCE(us.last_played_at, latest.last_played_at) AS last_played_at,
                        ROW_NUMBER() OVER (
                            ORDER BY mi.season_number, mi.episode_number, mi.sort_title, mi.id
                        ) AS next_rank
                 FROM latest
                 JOIN media_items mi
                   ON mi.series_id = ?
                  AND mi.item_type = 'EPISODE'
                  AND (mi.season_number > latest.season_number
                    OR (mi.season_number = latest.season_number
                        AND mi.episode_number > latest.episode_number))
                 LEFT JOIN user_item_state us ON us.item_id = mi.id AND us.user_id = ?
                 WHERE latest.is_played = 1
                   AND mi.has_available_source = 1
                   AND mi.removed_at IS NULL
                   AND COALESCE(us.is_played, 0) = 0
                   AND COALESCE(us.hide_from_resume, 0) = 0
             ),
             resume AS (
                 SELECT id, position_ticks, last_played_at
                 FROM latest WHERE is_played = 0 AND position_ticks > 0
                 UNION ALL
                 SELECT id, 0, last_played_at FROM next_episode WHERE next_rank = 1
             )
             INSERT INTO user_series_next (
                 user_id, series_id, next_item_id, position_ticks, played, last_played_at,
                 updated_at
             )
             SELECT ?, ?, id, position_ticks, 0, last_played_at, unixepoch()
             FROM resume
             WHERE 1 = 1
             ON CONFLICT(user_id, series_id) DO UPDATE SET
                 next_item_id = excluded.next_item_id,
                 position_ticks = excluded.position_ticks,
                 played = 0,
                 last_played_at = excluded.last_played_at,
                 updated_at = unixepoch()",
        )
        .bind(user_id)
        .bind(series_id)
        .bind(series_id)
        .bind(user_id)
        .bind(user_id)
        .bind(series_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }
}
