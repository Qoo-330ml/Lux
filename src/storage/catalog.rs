use super::*;

const NFO_METADATA_BATCH_SIZE: usize = 400;
use crate::storage::{
    CatalogSearchQuery, GlobalCollectionSourcePreference, GlobalMovieCollectionReference,
    GlobalMovieCollectionReferenceUpdate, StoredMovieNfoCollectionReferenceReconciliation,
    global_collection_title_fallback, global_collection_title_is_placeholder,
};
use crate::{
    domain::content_restrictions::ContentRestrictionIndexProjection,
    storage::{
        CONTENT_RESTRICTION_INDEX_BATCH_LIMIT, ContentRestrictionIndexBatchOutcome,
        ContentRestrictionIndexWrite, MediaContentRestrictionIndexCandidate,
    },
};

const RECOMMENDATION_PLAYBACK_WINDOW_SECONDS: i64 = 180 * 86_400;
const RECOMMENDATION_STATS_MAX_IDLE_REFRESH_SECONDS: i64 = 7 * 86_400;
const CHAPTER_DETECTION_JOB_ITEM_INSERT_BATCH_SIZE: usize = 100;
const MEDIA_METADATA_UPDATE_BATCH_SIZE: usize = 16;
const MEDIA_STREAM_INSERT_BATCH_SIZE: usize = 75;
const MEDIA_CHAPTER_INSERT_BATCH_SIZE: usize = 100;

fn user_library_access_filter(library_id_expression: &str, include_admin: bool) -> String {
    let active_library = format!(
        "EXISTS (
            SELECT 1 FROM libraries active_library
            WHERE active_library.id = {library_id_expression}
              AND active_library.deletion_state = 'ACTIVE'
        )"
    );
    let selected_access = format!(
        "EXISTS (
            SELECT 1 FROM users access_mode_user
            WHERE access_mode_user.id = ?
              AND (
                  access_mode_user.library_access_mode = 'ALL'
                  OR (access_mode_user.library_access_mode = 'SELECTED' AND EXISTS (
                      SELECT 1 FROM user_library_access selected_library
                      WHERE selected_library.user_id = access_mode_user.id
                        AND selected_library.library_id = {library_id_expression}
                        AND selected_library.can_view = 1
                  ))
              )
        )"
    );
    if include_admin {
        format!("({active_library} AND (? = 1 OR {selected_access}))")
    } else {
        format!("({active_library} AND {selected_access})")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PendingGlobalCollectionSync {
    pub(crate) collection_provider: String,
    pub(crate) collection_provider_id: String,
    pub(crate) generation: i64,
}

struct GlobalCollectionSyncMember {
    movie_provider: String,
    movie_provider_id: String,
    title: String,
    production_year: Option<i64>,
    sort_order: Option<i64>,
    collection_title: String,
}

struct GlobalGeneratedMediaItem<'a> {
    item_type: &'a str,
    identity_key: &'a str,
    title: &'a str,
    sort_title: &'a str,
    overview: Option<&'a str>,
    has_available_source: bool,
}

const RECOMMENDATION_STATS_CLEANUP_QUERY: &str = "DELETE FROM recommendation_item_stats
             WHERE NOT EXISTS (
                 SELECT 1
                 FROM media_items
                 WHERE media_items.id = recommendation_item_stats.item_id
                   AND media_items.removed_at IS NULL
                   AND media_items.item_type IN ('MOVIE', 'SERIES')
             )";

#[derive(Clone, Copy)]
struct CatalogSearchPageBindings<'a> {
    fts_query: Option<&'a str>,
    like_query: Option<&'a str>,
    item_types: &'a [String],
    library_ids: Option<&'a [String]>,
    content_restriction_binds: &'a [CatalogBind<'a>],
    offset: i64,
    limit: i64,
}

fn postgres_recent_catalog_rows_by_library_query(
    library_count: usize,
    content_restriction_predicate: &str,
) -> String {
    let values = std::iter::repeat_n("(?)", library_count)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "WITH episode_latest_by_root AS MATERIALIZED (
             SELECT roots.root_id, MAX(roots.latest_added_at) AS latest_added_at
             FROM (
                 SELECT episode.series_id AS root_id, MAX(episode.added_at) AS latest_added_at
                 FROM media_items episode
                 WHERE episode.item_type = 'EPISODE'
                   AND episode.removed_at IS NULL
                   AND episode.has_available_source = 1
                   AND episode.series_id IS NOT NULL
                 GROUP BY episode.series_id
                 UNION ALL
                 SELECT episode.parent_id AS root_id, MAX(episode.added_at) AS latest_added_at
                 FROM media_items episode
                 WHERE episode.item_type = 'EPISODE'
                   AND episode.removed_at IS NULL
                   AND episode.has_available_source = 1
                   AND episode.parent_id IS NOT NULL
                 GROUP BY episode.parent_id
             ) roots
             GROUP BY roots.root_id
         ), requested_libraries(library_id) AS (
             SELECT DISTINCT library_id FROM (VALUES {values}) requested(library_id)
         ), home_candidates AS MATERIALIZED (
             SELECT mi.id, mi.library_id, mi.sort_title,
                    CASE WHEN mi.item_type = 'SERIES' THEN COALESCE(
                        episode_latest_by_root.latest_added_at, mi.added_at
                    ) ELSE mi.added_at END AS latest_added_at
             FROM media_items mi
             JOIN requested_libraries requested ON requested.library_id = mi.library_id
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             LEFT JOIN episode_latest_by_root ON episode_latest_by_root.root_id = mi.id
             WHERE mi.removed_at IS NULL{content_restriction_predicate}
               AND (
                   (mi.item_type IN ('MOVIE', 'SERIES') AND mi.has_available_source = 1)
                   OR (
                       mi.item_type = 'SERIES'
                       AND mi.has_available_source = 0
                       AND (
                           EXISTS (
                               SELECT 1 FROM media_items visible_child
                               WHERE visible_child.removed_at IS NULL
                                 AND visible_child.has_available_source = 1
                                 AND (visible_child.parent_id = mi.id OR visible_child.series_id = mi.id)
                           )
                           OR EXISTS (
                               SELECT 1 FROM collection_items visible_collection_item
                               JOIN collections visible_collection
                                 ON visible_collection.id = visible_collection_item.collection_id
                               JOIN media_items visible_child
                                 ON visible_child.id = visible_collection_item.item_id
                               WHERE visible_collection.item_id = mi.id
                                 AND visible_child.removed_at IS NULL
                                 AND visible_child.has_available_source = 1
                           )
                       )
                   )
               )
         ), selected AS (
             SELECT requested.library_id, recent.id, recent.latest_added_at
             FROM requested_libraries requested
             CROSS JOIN LATERAL (
                 SELECT candidate.id, candidate.latest_added_at, candidate.sort_title
                 FROM home_candidates candidate
                 WHERE candidate.library_id = requested.library_id
                 ORDER BY candidate.latest_added_at DESC, candidate.sort_title, candidate.id
                 LIMIT ?
             ) recent
         )"
    )
}

fn sqlite_recent_catalog_rows_by_library_query(
    library_count: usize,
    content_restriction_predicate: &str,
) -> String {
    // The explicit `item_type <> 'FOLDER'` predicate lets SQLite match the
    // existing partial index; the IN predicate alone is not inferred.
    let libraries = (0..library_count)
        .map(|_| {
            format!(
                "SELECT recent.id, recent.library_id, recent.latest_added_at
             FROM (
                 SELECT candidates.id, candidates.library_id, candidates.latest_added_at
                 FROM (
                     SELECT available.id, available.library_id,
                            available.added_at, available.sort_title,
                            available.latest_added_at
                     FROM (
                         SELECT mi.id, mi.library_id, mi.added_at, mi.sort_title,
                                CASE WHEN mi.item_type = 'SERIES' THEN COALESCE(
                                    (SELECT MAX(child.added_at)
                                     FROM media_items child
                                     WHERE child.item_type = 'EPISODE'
                                       AND child.removed_at IS NULL
                                       AND child.has_available_source = 1
                                       AND (child.series_id = mi.id OR child.parent_id = mi.id)),
                                    mi.added_at
                                ) ELSE mi.added_at END AS latest_added_at
                         FROM media_items mi
                         JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                         WHERE mi.library_id = ?
                           AND mi.item_type <> 'FOLDER'
                           AND mi.item_type IN ('MOVIE', 'SERIES')
                           AND mi.removed_at IS NULL
                           AND mi.has_available_source = 1{content_restriction_predicate}
                         ORDER BY latest_added_at DESC, mi.sort_title, mi.id
                         LIMIT ?
                     ) AS available
                     UNION ALL
                     SELECT unavailable_series.id, unavailable_series.library_id,
                            unavailable_series.added_at, unavailable_series.sort_title,
                            unavailable_series.latest_added_at
                     FROM (
                         SELECT mi.id, mi.library_id, mi.added_at, mi.sort_title,
                                COALESCE(
                                    (SELECT MAX(child.added_at)
                                     FROM media_items child
                                     WHERE child.item_type = 'EPISODE'
                                       AND child.removed_at IS NULL
                                       AND child.has_available_source = 1
                                       AND (child.series_id = mi.id OR child.parent_id = mi.id)),
                                    mi.added_at
                                ) AS latest_added_at
                         FROM media_items mi
                         JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                         WHERE mi.library_id = ?
                           AND mi.item_type = 'SERIES'
                           AND mi.removed_at IS NULL
                           AND mi.has_available_source = 0{content_restriction_predicate}
                           AND (
                               EXISTS (
                                   SELECT 1
                                   FROM media_items visible_child
                                   WHERE visible_child.removed_at IS NULL
                                     AND visible_child.has_available_source = 1
                                     AND (visible_child.parent_id = mi.id
                                          OR visible_child.series_id = mi.id)
                               )
                               OR EXISTS (
                                   SELECT 1
                                   FROM collection_items visible_collection_item
                                   JOIN collections visible_collection
                                     ON visible_collection.id = visible_collection_item.collection_id
                                   JOIN media_items visible_child
                                     ON visible_child.id = visible_collection_item.item_id
                                   WHERE visible_collection.item_id = mi.id
                                     AND visible_child.removed_at IS NULL
                                     AND visible_child.has_available_source = 1
                               )
                           )
                         ORDER BY latest_added_at DESC, mi.sort_title, mi.id
                         LIMIT ?
                     ) AS unavailable_series
                 ) AS candidates
                 ORDER BY candidates.latest_added_at DESC,
                          candidates.sort_title, candidates.id
                 LIMIT ?
             ) AS recent"
            )
        })
        .collect::<Vec<_>>()
        .join("\n             UNION ALL\n             ");
    format!("WITH selected AS (\n             {libraries}\n             )")
}

fn catalog_item_order(sort_by: CatalogSort, descending: bool) -> &'static str {
    match (sort_by, descending) {
        (CatalogSort::DateCreated, true) => {
            "mi.added_at DESC, LOWER(mi.title) ASC, mi.id ASC"
        }
        (CatalogSort::DateCreated, false) => {
            "mi.added_at ASC, LOWER(mi.title) ASC, mi.id ASC"
        }
        (CatalogSort::PremiereDate, true) => {
            "CASE WHEN NULLIF(mi.premiere_date, '') IS NULL AND mi.production_year IS NULL THEN 1 ELSE 0 END ASC,
             COALESCE(NULLIF(mi.premiere_date, ''), CAST(mi.production_year AS TEXT) || '-01-01') DESC,
             LOWER(mi.title) ASC, mi.id ASC"
        }
        (CatalogSort::PremiereDate, false) => {
            "CASE WHEN NULLIF(mi.premiere_date, '') IS NULL AND mi.production_year IS NULL THEN 1 ELSE 0 END ASC,
             COALESCE(NULLIF(mi.premiere_date, ''), CAST(mi.production_year AS TEXT) || '-01-01') ASC,
             LOWER(mi.title) ASC, mi.id ASC"
        }
        (CatalogSort::ProductionYear, true) => {
            "CASE WHEN mi.production_year IS NULL THEN 1 ELSE 0 END ASC,
             mi.production_year DESC, LOWER(mi.title) ASC, mi.id ASC"
        }
        (CatalogSort::ProductionYear, false) => {
            "CASE WHEN mi.production_year IS NULL THEN 1 ELSE 0 END ASC,
             mi.production_year ASC, LOWER(mi.title) ASC, mi.id ASC"
        }
        (CatalogSort::Rating, true) => {
            "CASE WHEN mi.rating IS NULL THEN 1 ELSE 0 END ASC,
             mi.rating DESC, LOWER(mi.title) ASC, mi.id ASC"
        }
        (CatalogSort::Rating, false) => {
            "CASE WHEN mi.rating IS NULL THEN 1 ELSE 0 END ASC,
             mi.rating ASC, LOWER(mi.title) ASC, mi.id ASC"
        }
        (CatalogSort::SortName, true) => "LOWER(mi.sort_title) DESC, mi.id DESC",
        (CatalogSort::SortName, false) => "LOWER(mi.sort_title) ASC, mi.id ASC",
        (CatalogSort::Name, true) => "LOWER(mi.title) DESC, mi.id DESC",
        (CatalogSort::Name, false) => "LOWER(mi.title) ASC, mi.id ASC",
    }
}

impl Database {
    pub(crate) async fn ensure_thumbnail_scraper_retry(
        &self,
        item_id: &str,
        first_attempt_at: i64,
        next_retry_at: i64,
    ) -> Result<bool, StorageError> {
        let result = self
            .query(
                "INSERT INTO thumbnail_scraper_retries (
                    item_id, status, attempt_count, first_attempt_at, next_retry_at,
                    claimed_until, created_at, updated_at
                 ) VALUES (?, 'PENDING', 1, ?, ?, NULL, ?, ?)
                 ON CONFLICT(item_id) DO NOTHING",
            )
            .bind(item_id)
            .bind(first_attempt_at)
            .bind(next_retry_at)
            .bind(first_attempt_at)
            .bind(first_attempt_at)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn find_thumbnail_scraper_retry(
        &self,
        item_id: &str,
    ) -> Result<Option<StoredThumbnailScraperRetry>, StorageError> {
        self.query(
            "SELECT item_id, status, attempt_count, first_attempt_at, next_retry_at
             FROM thumbnail_scraper_retries WHERE item_id = ?",
        )
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredThumbnailScraperRetry {
                item_id: row.get("item_id"),
                status: row.get("status"),
                attempt_count: u32::try_from(row.get::<i64, _>("attempt_count").max(0))
                    .unwrap_or(u32::MAX),
                first_attempt_at: row.get("first_attempt_at"),
                next_retry_at: row.get("next_retry_at"),
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn claim_due_thumbnail_scraper_retries(
        &self,
        now: i64,
        claimed_until: i64,
        limit: i64,
    ) -> Result<Vec<StoredThumbnailScraperRetry>, StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let rows = self
            .query(
                "SELECT item_id, status, attempt_count, first_attempt_at, next_retry_at
                 FROM thumbnail_scraper_retries
                 WHERE (status = 'PENDING' AND next_retry_at <= ?)
                    OR (status = 'RUNNING' AND claimed_until <= ?)
                 ORDER BY next_retry_at, item_id
                 LIMIT ?",
            )
            .bind(now)
            .bind(now)
            .bind(limit.clamp(1, 100))
            .fetch_all(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let mut claimed = Vec::with_capacity(rows.len());
        if !rows.is_empty() {
            let placeholders = std::iter::repeat_n("?", rows.len())
                .collect::<Vec<_>>()
                .join(", ");
            let mut update = self.query(sqlx::AssertSqlSafe(format!(
                "UPDATE thumbnail_scraper_retries
                 SET status = 'RUNNING', claimed_until = ?, updated_at = ?
                 WHERE item_id IN ({placeholders}) AND (
                     (status = 'PENDING' AND next_retry_at <= ?)
                     OR (status = 'RUNNING' AND claimed_until <= ?)
                 )
                 RETURNING item_id"
            )));
            update = update.bind(claimed_until).bind(now);
            for row in &rows {
                update = update.bind(row.get::<String, _>("item_id"));
            }
            let claimed_ids = update
                .bind(now)
                .bind(now)
                .fetch_all(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
                .into_iter()
                .map(|row| row.get::<String, _>("item_id"))
                .collect::<std::collections::HashSet<_>>();
            // RETURNING has no ordering guarantee; retain the original due-time order.
            for row in rows {
                let item_id: String = row.get("item_id");
                if claimed_ids.contains(&item_id) {
                    claimed.push(StoredThumbnailScraperRetry {
                        item_id,
                        status: "RUNNING".to_owned(),
                        attempt_count: u32::try_from(row.get::<i64, _>("attempt_count").max(0))
                            .unwrap_or(u32::MAX),
                        first_attempt_at: row.get("first_attempt_at"),
                        next_retry_at: row.get("next_retry_at"),
                    });
                }
            }
        }
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(claimed)
    }

    pub(crate) async fn finish_thumbnail_scraper_retry(
        &self,
        item_id: &str,
        attempt_count: u32,
        next_retry_at: Option<i64>,
        now: i64,
    ) -> Result<bool, StorageError> {
        let status = if next_retry_at.is_some() {
            "PENDING"
        } else {
            "COMPLETE"
        };
        let result = self
            .query(
                "UPDATE thumbnail_scraper_retries
                 SET status = ?, attempt_count = ?, next_retry_at = ?,
                     claimed_until = NULL, updated_at = ?
                 WHERE item_id = ? AND status = 'RUNNING'",
            )
            .bind(status)
            .bind(i64::from(attempt_count.clamp(1, 3)))
            .bind(next_retry_at)
            .bind(now)
            .bind(item_id)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn clear_metadata_image_attempt_history_for_thumbnail_retry(
        &self,
        item_id: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "DELETE FROM metadata_image_attempts
             WHERE item_id = ? AND image_type IN ('POSTER', 'THUMB')
               AND status <> 'RUNNING'",
        )
        .bind(item_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_local_thumbnail_source(
        &self,
        item_id: &str,
    ) -> Result<Option<StoredThumbnailSource>, StorageError> {
        self.query(
            "SELECT ms.item_id, lr.canonical_path AS root_path, fe.relative_path,
                    l.media_strategy_json AS library_media_strategy_json, l.scraper_id
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN libraries l ON l.id = mi.library_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE ms.item_id = ? AND ms.source_kind = 'LOCAL_FILE'
               AND fe.is_missing = 0
             ORDER BY ms.is_default DESC, ms.id
             LIMIT 1",
        )
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredThumbnailSource {
                item_id: row.get("item_id"),
                root_path: row.get("root_path"),
                relative_path: row.get("relative_path"),
                library_media_strategy_json: row.get("library_media_strategy_json"),
                scraper_id: row.get("scraper_id"),
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn refresh_recommendation_stats_if_needed(
        &self,
    ) -> Result<bool, StorageError> {
        let _refresh_guard = self.recommendation_stats_refresh_lock.lock().await;
        let now = current_unix_timestamp();
        let batch_key = recommendation_batch_key_at(now);
        let state = self
            .query_as::<(i64, Option<String>, i64)>(
                "SELECT batch_key, source_fingerprint, refreshed_at
                 FROM recommendation_stats_state
                 WHERE id = 1",
            )
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let Some((stored_batch_key, stored_fingerprint, refreshed_at)) = state else {
            // A missing state row is handled by the full refresh below.
            return self
                .refresh_recommendation_stats(now, batch_key, None)
                .await;
        };
        if stored_batch_key == batch_key {
            return Ok(false);
        }
        let fingerprint = self.recommendation_stats_source_fingerprint(now).await?;
        if stored_fingerprint.as_deref() == Some(fingerprint.as_str())
            && now.saturating_sub(refreshed_at) < RECOMMENDATION_STATS_MAX_IDLE_REFRESH_SECONDS
        {
            // The daily batch key changed but nothing that affects scores did.
            // Advance the batch key so the 15-second scheduler check stays cheap;
            // refreshed_at remains the last real recomputation time.
            self.query("UPDATE recommendation_stats_state SET batch_key = ? WHERE id = 1")
                .bind(batch_key)
                .execute(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(false);
        }
        self.refresh_recommendation_stats(now, batch_key, Some(&fingerprint))
            .await
    }

    async fn recommendation_stats_source_fingerprint(
        &self,
        now: i64,
    ) -> Result<String, StorageError> {
        let window_start = now - RECOMMENDATION_PLAYBACK_WINDOW_SECONDS;
        let (playback_count, playback_last_event_at, favorite_count, version_sum, visible_item_count, item_updated_at): (
            i64,
            i64,
            i64,
            i64,
            i64,
            i64,
        ) = self
            .query_as(
                "SELECT
                    (SELECT COUNT(*) FROM playback_sessions WHERE last_event_at > ?),
                    (SELECT COALESCE(MAX(last_event_at), 0) FROM playback_sessions WHERE last_event_at > ?),
                    (SELECT COUNT(*) FROM user_item_state WHERE is_favorite = 1),
                    (SELECT COALESCE(SUM(version), 0) FROM user_item_state),
                    (SELECT COUNT(*) FROM media_items WHERE removed_at IS NULL),
                    (SELECT COALESCE(MAX(updated_at), 0) FROM media_items)",
            )
            .bind(window_start)
            .bind(window_start)
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(format!(
            "{playback_count}:{playback_last_event_at}:{favorite_count}:{version_sum}:{visible_item_count}:{item_updated_at}"
        ))
    }

    async fn refresh_recommendation_stats(
        &self,
        now: i64,
        batch_key: i64,
        source_fingerprint: Option<&str>,
    ) -> Result<bool, StorageError> {
        let min_function = self.scalar_min_function();
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let refresh_query = format!(
            "WITH recent_playback_users AS (
                 SELECT ps.item_id, ps.user_id
                 FROM playback_sessions ps
                 WHERE ps.last_event_at > ?
                 UNION
                 SELECT us.item_id, us.user_id
                 FROM user_item_state us
                 WHERE us.last_played_at > ?
             ),
             playback_counts AS (
                 SELECT item_id, COUNT(*) AS recent_playback_user_count
                 FROM recent_playback_users
                 GROUP BY item_id
             ),
             favorite_counts AS (
                 SELECT item_id, COUNT(*) AS favorite_user_count
                 FROM user_item_state
                 WHERE is_favorite = 1
                 GROUP BY item_id
             )
             INSERT INTO recommendation_item_stats (
                 item_id, recent_playback_score, favorite_score, refreshed_batch_key
             )
             SELECT mi.id,
                    {min_function}(50, COALESCE(pc.recent_playback_user_count, 0)),
                    {min_function}(50, 5 * COALESCE(fc.favorite_user_count, 0)),
                    ?
             FROM media_items mi
             LEFT JOIN playback_counts pc ON pc.item_id = mi.id
             LEFT JOIN favorite_counts fc ON fc.item_id = mi.id
             WHERE mi.removed_at IS NULL
               AND mi.item_type IN ('MOVIE', 'SERIES')
             ON CONFLICT(item_id) DO UPDATE SET
                 recent_playback_score = excluded.recent_playback_score,
                 favorite_score = excluded.favorite_score,
                 refreshed_batch_key = excluded.refreshed_batch_key",
        );
        self.query(sqlx::AssertSqlSafe(refresh_query))
            .bind(now - RECOMMENDATION_PLAYBACK_WINDOW_SECONDS)
            .bind(now - RECOMMENDATION_PLAYBACK_WINDOW_SECONDS)
            .bind(batch_key)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.query(RECOMMENDATION_STATS_CLEANUP_QUERY)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.query(
            "INSERT INTO recommendation_stats_state (
                 id, batch_key, refreshed_at, source_fingerprint
             )
             VALUES (1, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                 batch_key = excluded.batch_key,
                 refreshed_at = excluded.refreshed_at,
                 source_fingerprint = excluded.source_fingerprint",
        )
        .bind(batch_key)
        .bind(now)
        .bind(source_fingerprint)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query(
            "DELETE FROM recommendation_daily_batches
             WHERE batch_key < ?",
        )
        .bind(batch_key)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(true)
    }

    pub(crate) async fn find_recommendation_daily_batch(
        &self,
        user_id: &str,
        library_scope_key: &str,
        batch_key: i64,
    ) -> Result<Option<Vec<String>>, StorageError> {
        let Some(item_ids_json) = self
            .query_scalar::<String>(
                "SELECT item_ids_json
                 FROM recommendation_daily_batches
                 WHERE user_id = ? AND library_scope_key = ? AND batch_key = ?",
            )
            .bind(user_id)
            .bind(library_scope_key)
            .bind(batch_key)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
        else {
            return Ok(None);
        };
        serde_json::from_str(&item_ids_json)
            .map(Some)
            .map_err(|error| StorageError::Serialization(error.to_string()))
    }

    pub(crate) async fn save_recommendation_daily_batch(
        &self,
        user_id: &str,
        library_scope_key: &str,
        batch_key: i64,
        item_ids: &[String],
    ) -> Result<bool, StorageError> {
        let item_ids_json = serde_json::to_string(item_ids)
            .map_err(|error| StorageError::Serialization(error.to_string()))?;
        self.query(
            "INSERT INTO recommendation_daily_batches (
                 user_id, library_scope_key, batch_key, item_ids_json
             ) VALUES (?, ?, ?, ?)
             ON CONFLICT(user_id, library_scope_key, batch_key) DO NOTHING",
        )
        .bind(user_id)
        .bind(library_scope_key)
        .bind(batch_key)
        .bind(item_ids_json)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn count_catalog_items(
        &self,
        library_id: Option<&str>,
    ) -> Result<i64, StorageError> {
        let query = match library_id {
            Some(_) => format!(
                "SELECT COUNT(*) FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 WHERE mi.library_id = ? AND mi.item_type <> 'FOLDER'
                   AND mi.removed_at IS NULL{CATALOG_VISIBLE_PREDICATE}"
            ),
            None => format!(
                "SELECT COUNT(*) FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 WHERE mi.item_type <> 'FOLDER'
                   AND mi.removed_at IS NULL{CATALOG_VISIBLE_PREDICATE}"
            ),
        };
        let mut statement = self.query_scalar::<i64>(sqlx::AssertSqlSafe(query));
        if let Some(library_id) = library_id {
            statement = statement.bind(library_id);
        }
        statement
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn count_catalog_root_items_by_library(
        &self,
        library_ids: &[String],
    ) -> Result<HashMap<String, StoredCatalogItemCounts>, StorageError> {
        if library_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = std::iter::repeat_n("?", library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "SELECT mi.library_id,
                    COUNT(CASE WHEN mi.item_type = 'MOVIE' THEN 1 END) AS movie_count,
                    COUNT(CASE WHEN mi.item_type = 'SERIES' THEN 1 END) AS series_count,
                    COUNT(*) AS item_count
             FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             WHERE mi.removed_at IS NULL
               AND mi.library_id IN ({placeholders})
               AND mi.item_type IN ('MOVIE', 'SERIES')
               {CATALOG_VISIBLE_PREDICATE}
             GROUP BY mi.library_id"
        );
        let mut statement = self.query(sqlx::AssertSqlSafe(query));
        for library_id in library_ids {
            statement = statement.bind(library_id);
        }
        statement
            .fetch_all(&self.pool)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(|row| {
                        (
                            row.get::<String, _>("library_id"),
                            StoredCatalogItemCounts {
                                movie_count: row.get("movie_count"),
                                series_count: row.get("series_count"),
                                item_count: row.get("item_count"),
                                ..StoredCatalogItemCounts::default()
                            },
                        )
                    })
                    .collect()
            })
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn count_catalog_item_types(
        &self,
        library_ids: &[String],
        user_id: &str,
        is_favorite: Option<bool>,
        content_restrictions: Option<&UserContentRestrictions>,
    ) -> Result<StoredCatalogItemCounts, StorageError> {
        if library_ids.is_empty() {
            return Ok(StoredCatalogItemCounts::default());
        }

        let library_placeholders = std::iter::repeat_n("?", library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let favorite_filter = is_favorite.map_or(String::new(), |_| {
            " AND COALESCE(
                (SELECT state_filter.is_favorite
                 FROM user_item_state state_filter
                 WHERE state_filter.user_id = ? AND state_filter.item_id = mi.id),
                0
            ) = ?"
                .to_owned()
        });
        let mut content_restriction_predicate = String::new();
        let mut content_restriction_binds = Vec::new();
        append_content_restriction_predicate(
            &mut content_restriction_predicate,
            &mut content_restriction_binds,
            content_restrictions,
        );
        let mut visible_collection_predicate = String::new();
        let mut visible_collection_library_ids = Vec::new();
        let mut visible_collection_content_binds = Vec::new();
        if !content_restriction_predicate.is_empty() {
            let collection_library_placeholders = std::iter::repeat_n("?", library_ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            let mut visible_member_content_predicate = String::new();
            append_content_restriction_predicate_for_item_alias(
                &mut visible_member_content_predicate,
                &mut visible_collection_content_binds,
                content_restrictions,
                "visible_member",
            );
            let visible_member_catalog_predicate =
                CATALOG_VISIBLE_PREDICATE.replace("mi.", "visible_member.");
            visible_collection_predicate = format!(
                " AND (mi.item_type <> 'BOX_SET' OR EXISTS (
                    SELECT 1
                    FROM collections visible_collection
                    JOIN collection_items visible_collection_item
                      ON visible_collection_item.collection_id = visible_collection.id
                    JOIN media_items visible_member
                      ON visible_member.id = visible_collection_item.item_id
                    JOIN libraries visible_member_library
                      ON visible_member_library.id = visible_member.library_id
                     AND visible_member_library.is_enabled = 1
                    WHERE visible_collection.item_id = mi.id
                      AND visible_member.library_id IN ({collection_library_placeholders})
                      {visible_member_catalog_predicate}
                      {visible_member_content_predicate}
                ))"
            );
            visible_collection_library_ids.extend_from_slice(library_ids);
        }
        let query = format!(
            "SELECT
                COUNT(CASE WHEN mi.item_type = 'MOVIE' THEN 1 END) AS movie_count,
                COUNT(CASE WHEN mi.item_type = 'SERIES' THEN 1 END) AS series_count,
                COUNT(CASE WHEN mi.item_type = 'EPISODE' THEN 1 END) AS episode_count,
                COUNT(CASE WHEN mi.item_type = 'BOX_SET' THEN 1 END) AS box_set_count,
                COUNT(*) AS item_count
             FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             WHERE mi.removed_at IS NULL
               AND mi.library_id IN ({library_placeholders})
               AND mi.item_type <> 'FOLDER'
               {CATALOG_VISIBLE_PREDICATE}
               {content_restriction_predicate}
               {visible_collection_predicate}
               {favorite_filter}"
        );
        let mut statement = self.query(sqlx::AssertSqlSafe(query));
        for library_id in library_ids {
            statement = statement.bind(library_id);
        }
        for bind in content_restriction_binds {
            statement = match bind {
                CatalogBind::Text(value) => statement.bind(value),
                CatalogBind::Integer(value) => statement.bind(value),
                CatalogBind::Real(value) => statement.bind(value),
            };
        }
        for library_id in visible_collection_library_ids {
            statement = statement.bind(library_id);
        }
        for bind in visible_collection_content_binds {
            statement = match bind {
                CatalogBind::Text(value) => statement.bind(value),
                CatalogBind::Integer(value) => statement.bind(value),
                CatalogBind::Real(value) => statement.bind(value),
            };
        }
        if let Some(is_favorite) = is_favorite {
            statement = statement.bind(user_id).bind(database_flag(is_favorite));
        }
        statement
            .fetch_one(&self.pool)
            .await
            .map(|row| StoredCatalogItemCounts {
                movie_count: row.get("movie_count"),
                series_count: row.get("series_count"),
                episode_count: row.get("episode_count"),
                box_set_count: row.get("box_set_count"),
                item_count: row.get("item_count"),
            })
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn count_visible_global_collections_with_restrictions(
        &self,
        user_id: &str,
        is_admin: bool,
        is_favorite: Option<bool>,
        content_restrictions: Option<&UserContentRestrictions>,
    ) -> Result<i64, StorageError> {
        let favorite_filter = is_favorite.map_or(String::new(), |_| {
            " AND COALESCE(
                (SELECT state_filter.is_favorite
                 FROM user_item_state state_filter
                 WHERE state_filter.user_id = ? AND state_filter.item_id = collection_item.id),
                0
            ) = ?"
                .to_owned()
        });
        let library_access_filter = user_library_access_filter("source_item.library_id", true);
        let mut content_restriction_predicate = String::new();
        let mut content_restriction_binds = Vec::new();
        append_content_restriction_predicate_for_item_alias(
            &mut content_restriction_predicate,
            &mut content_restriction_binds,
            content_restrictions,
            "source_item",
        );
        let query = format!(
            "SELECT COUNT(DISTINCT collection.id)
             FROM collections collection
             JOIN media_items collection_item ON collection_item.id = collection.item_id
             JOIN collection_items member ON member.collection_id = collection.id
             JOIN collection_work_items work ON work.work_item_id = member.item_id
             JOIN media_items work_item ON work_item.id = member.item_id
             WHERE collection.library_id = ?
               AND collection_item.item_type = 'BOX_SET'
               AND collection_item.removed_at IS NULL
               AND collection_item.has_available_source = 1
               AND EXISTS (
                   SELECT 1 FROM collection_work_sources cws
                   JOIN media_items source_item ON source_item.id = cws.source_item_id
                   JOIN libraries source_library
                     ON source_library.id = source_item.library_id AND source_library.is_enabled = 1
                   WHERE cws.work_item_id = member.item_id
                     AND source_item.item_type = 'MOVIE'
                     AND source_item.removed_at IS NULL
                     AND source_item.has_available_source = 1
                     AND {library_access_filter}
                     {content_restriction_predicate}
               )
               {favorite_filter}"
        );
        let mut statement = self
            .query_scalar::<i64>(sqlx::AssertSqlSafe(query))
            .bind(GLOBAL_COLLECTION_LIBRARY_ID)
            .bind(database_flag(is_admin))
            .bind(user_id);
        for bind in &content_restriction_binds {
            statement = match bind {
                CatalogBind::Text(value) => statement.bind(*value),
                CatalogBind::Integer(value) => statement.bind(*value),
                CatalogBind::Real(value) => statement.bind(*value),
            };
        }
        if let Some(is_favorite) = is_favorite {
            statement = statement.bind(user_id).bind(database_flag(is_favorite));
        }
        statement
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn dashboard_stats(&self) -> Result<DashboardStats, StorageError> {
        const CACHE_TTL_SECONDS: i64 = 60;
        let now = current_unix_timestamp();
        let mut cache = self.dashboard_stats_cache.lock().await;
        if let Some(cached) = cache.as_ref()
            && now.saturating_sub(cached.computed_at) < CACHE_TTL_SECONDS
        {
            return Ok(cached.stats.clone());
        }
        let stats = self
            .query(
                "SELECT
                    COUNT(CASE WHEN mi.item_type = 'MOVIE' THEN 1 END) AS movie_count,
                    COUNT(CASE WHEN mi.item_type = 'SERIES' THEN 1 END) AS series_count,
                    (SELECT COUNT(*) FROM users WHERE is_disabled = 0) AS user_count
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 WHERE mi.removed_at IS NULL",
            )
            .fetch_one(&self.pool)
            .await
            .map(|row| DashboardStats {
                movie_count: row.get("movie_count"),
                series_count: row.get("series_count"),
                user_count: row.get("user_count"),
            })
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        *cache = Some(CachedDashboardStats {
            computed_at: now,
            stats: stats.clone(),
        });
        Ok(stats)
    }

    pub(crate) async fn search_catalog_item_ids(
        &self,
        search: &CatalogSearchQuery<'_>,
    ) -> Result<(Vec<String>, i64), StorageError> {
        let query = search.query;
        let like_query = search.like_query;
        let item_types = search.item_types;
        let library_ids = search.library_ids;
        let offset = search.offset;
        let limit = search.limit;
        if item_types.is_empty() {
            return Ok((Vec::new(), 0));
        }
        if self.backend == DatabaseBackend::Postgres {
            return self.search_catalog_item_ids_postgres(search).await;
        }

        if let Some(library_ids) = library_ids
            && library_ids.is_empty()
        {
            return Ok((Vec::new(), 0));
        }
        let library_filter = library_ids.map(|ids| {
            format!(
                " AND mi.library_id IN ({})",
                std::iter::repeat_n("?", ids.len())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        });
        let item_type_filter = format!(
            " AND mi.item_type IN ({})",
            std::iter::repeat_n("?", item_types.len())
                .collect::<Vec<_>>()
                .join(", ")
        );
        let mut content_restriction_predicate = String::new();
        let mut content_restriction_binds = Vec::new();
        append_content_restriction_predicate(
            &mut content_restriction_predicate,
            &mut content_restriction_binds,
            search.content_restrictions,
        );
        let fts_query = format!(
            "SELECT mi.id FROM media_search
             JOIN media_items mi ON mi.id = media_search.item_id
             JOIN libraries l ON l.id = mi.library_id
                               AND l.is_enabled = 1 AND l.system_kind IS NULL
             WHERE media_search MATCH ? AND mi.removed_at IS NULL
               {item_type_filter}{CATALOG_VISIBLE_PREDICATE}{}{content_restriction_predicate}",
            library_filter.as_deref().unwrap_or_default(),
        );
        // Complete-token searches are served by FTS alone. The LIKE branch remains
        // available for partial searches, but is avoided when FTS already has a page.
        let fts_page_query = format!(
            "SELECT matches.id, COUNT(*) OVER() AS total FROM ({fts_query}) matches
             JOIN media_items mi ON mi.id = matches.id
             ORDER BY mi.sort_title, mi.id LIMIT ? OFFSET ?"
        );
        let fts_page = self
            .fetch_catalog_search_page(
                &fts_page_query,
                CatalogSearchPageBindings {
                    fts_query: Some(query),
                    like_query: None,
                    item_types,
                    library_ids,
                    content_restriction_binds: &content_restriction_binds,
                    offset,
                    limit,
                },
            )
            .await?;
        if !fts_page.0.is_empty() {
            return Ok(fts_page);
        }

        let like_query_sql = format!(
            "SELECT mi.id FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id
                               AND l.is_enabled = 1 AND l.system_kind IS NULL
             WHERE (mi.title LIKE ? ESCAPE '!' OR COALESCE(mi.original_title, '') LIKE ? ESCAPE '!'
                    OR EXISTS (SELECT 1 FROM item_aliases ia
                               WHERE ia.item_id = mi.id AND ia.alias LIKE ? ESCAPE '!'))
             AND mi.removed_at IS NULL
               {item_type_filter}{CATALOG_VISIBLE_PREDICATE}{}{content_restriction_predicate}",
            library_filter.as_deref().unwrap_or_default(),
        );
        let like_page_query = format!(
            "SELECT matches.id, COUNT(*) OVER() AS total FROM ({like_query_sql}) matches
             JOIN media_items mi ON mi.id = matches.id
             ORDER BY mi.sort_title, mi.id LIMIT ? OFFSET ?"
        );
        if offset == 0 {
            return self
                .fetch_catalog_search_page(
                    &like_page_query,
                    CatalogSearchPageBindings {
                        fts_query: None,
                        like_query: Some(like_query),
                        item_types,
                        library_ids,
                        content_restriction_binds: &content_restriction_binds,
                        offset,
                        limit,
                    },
                )
                .await;
        }

        let union_query = format!("{fts_query} UNION {like_query_sql}");
        let union_page_query = format!(
            "SELECT matches.id, COUNT(*) OVER() AS total FROM ({union_query}) matches
             JOIN media_items mi ON mi.id = matches.id
             ORDER BY mi.sort_title, mi.id LIMIT ? OFFSET ?"
        );
        self.fetch_catalog_search_page(
            &union_page_query,
            CatalogSearchPageBindings {
                fts_query: Some(query),
                like_query: Some(like_query),
                item_types,
                library_ids,
                content_restriction_binds: &content_restriction_binds,
                offset,
                limit,
            },
        )
        .await
    }

    async fn search_catalog_item_ids_postgres(
        &self,
        search: &CatalogSearchQuery<'_>,
    ) -> Result<(Vec<String>, i64), StorageError> {
        let like_query = search.like_query;
        let item_types = search.item_types;
        let library_ids = search.library_ids;
        let offset = search.offset;
        let limit = search.limit;
        if let Some(library_ids) = library_ids
            && library_ids.is_empty()
        {
            return Ok((Vec::new(), 0));
        }
        let library_filter = library_ids.map(|ids| {
            format!(
                " AND mi.library_id IN ({})",
                std::iter::repeat_n("?", ids.len())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        });
        let item_type_filter = format!(
            " AND mi.item_type IN ({})",
            std::iter::repeat_n("?", item_types.len())
                .collect::<Vec<_>>()
                .join(", ")
        );
        let mut content_restriction_predicate = String::new();
        let mut content_restriction_binds = Vec::new();
        append_content_restriction_predicate(
            &mut content_restriction_predicate,
            &mut content_restriction_binds,
            search.content_restrictions,
        );
        let like_query_sql = format!(
            "SELECT mi.id FROM media_search ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN libraries l ON l.id = mi.library_id
                               AND l.is_enabled = 1 AND l.system_kind IS NULL
             WHERE (ms.title ILIKE ? OR ms.original_title ILIKE ?
                    OR ms.aliases ILIKE ?)
               AND mi.removed_at IS NULL
               {item_type_filter}{CATALOG_VISIBLE_PREDICATE}{}{content_restriction_predicate}",
            library_filter.as_deref().unwrap_or_default(),
        );
        let page_query = format!(
            "SELECT matches.id, COUNT(*) OVER() AS total FROM ({like_query_sql}) matches
             JOIN media_items mi ON mi.id = matches.id
             ORDER BY mi.sort_title, mi.id LIMIT ? OFFSET ?"
        );
        self.fetch_catalog_search_page(
            &page_query,
            CatalogSearchPageBindings {
                fts_query: None,
                like_query: Some(like_query),
                item_types,
                library_ids,
                content_restriction_binds: &content_restriction_binds,
                offset,
                limit,
            },
        )
        .await
    }

    async fn fetch_catalog_search_page(
        &self,
        query: &str,
        bindings: CatalogSearchPageBindings<'_>,
    ) -> Result<(Vec<String>, i64), StorageError> {
        let mut statement = self.query(sqlx::AssertSqlSafe(query));
        if let Some(fts_query) = bindings.fts_query {
            statement = statement.bind(fts_query);
            for item_type in bindings.item_types {
                statement = statement.bind(item_type);
            }
            if let Some(library_ids) = bindings.library_ids {
                for library_id in library_ids {
                    statement = statement.bind(library_id);
                }
            }
            for bind in bindings.content_restriction_binds {
                statement = match bind {
                    CatalogBind::Text(value) => statement.bind(*value),
                    CatalogBind::Integer(value) => statement.bind(*value),
                    CatalogBind::Real(value) => statement.bind(*value),
                };
            }
        }
        if let Some(like_query) = bindings.like_query {
            statement = statement.bind(like_query).bind(like_query).bind(like_query);
            for item_type in bindings.item_types {
                statement = statement.bind(item_type);
            }
            if let Some(library_ids) = bindings.library_ids {
                for library_id in library_ids {
                    statement = statement.bind(library_id);
                }
            }
            for bind in bindings.content_restriction_binds {
                statement = match bind {
                    CatalogBind::Text(value) => statement.bind(*value),
                    CatalogBind::Integer(value) => statement.bind(*value),
                    CatalogBind::Real(value) => statement.bind(*value),
                };
            }
        }
        let rows = statement
            .bind(bindings.limit)
            .bind(bindings.offset)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if rows.is_empty() && bindings.offset > 0 {
            // `COUNT(*) OVER()` only appears on returned rows, so a page past the end would
            // report a total of zero. Ask for the first page to learn the real total.
            let mut first_page = bindings;
            first_page.offset = 0;
            let (_, total) = Box::pin(self.fetch_catalog_search_page(query, first_page)).await?;
            return Ok((Vec::new(), total));
        }
        let total = rows.first().map(|row| row.get("total")).unwrap_or(0);
        Ok((rows.into_iter().map(|row| row.get("id")).collect(), total))
    }

    pub(crate) async fn list_recent_catalog_item_ids(
        &self,
        library_ids: &[String],
        offset: i64,
        limit: i64,
        media_only: bool,
        content_restrictions: Option<&UserContentRestrictions>,
    ) -> Result<(Vec<String>, i64), StorageError> {
        if library_ids.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let available_item_types = if media_only {
            "mi.item_type IN ('MOVIE', 'SERIES')"
        } else {
            "mi.item_type <> 'FOLDER'"
        };
        let unavailable_item_types = if media_only {
            "mi.item_type = 'SERIES'"
        } else {
            "mi.item_type IN ('SERIES', 'SEASON', 'BOX_SET')"
        };
        let placeholders = std::iter::repeat_n("?", library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let mut content_restriction_predicate = String::new();
        let mut content_restriction_binds = Vec::new();
        append_content_restriction_predicate(
            &mut content_restriction_predicate,
            &mut content_restriction_binds,
            content_restrictions,
        );
        let count_query = format!(
            "SELECT CAST(COALESCE(SUM(item_count), 0) AS BIGINT)
             FROM (
                 SELECT COUNT(*) AS item_count
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 WHERE mi.removed_at IS NULL
                   AND {available_item_types}
                   AND mi.has_available_source = 1
                   AND mi.library_id IN ({placeholders}){content_restriction_predicate}
                 UNION ALL
                 SELECT COUNT(*) AS item_count
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 WHERE mi.removed_at IS NULL
                   AND {unavailable_item_types}
                   AND mi.has_available_source = 0
                   AND mi.library_id IN ({placeholders}){content_restriction_predicate}
                   AND (
                       EXISTS (
                           SELECT 1
                           FROM media_items visible_child
                           WHERE visible_child.removed_at IS NULL
                             AND visible_child.has_available_source = 1
                             AND (visible_child.parent_id = mi.id OR visible_child.series_id = mi.id)
                       )
                       OR EXISTS (
                           SELECT 1
                           FROM collection_items visible_collection_item
                           JOIN collections visible_collection
                             ON visible_collection.id = visible_collection_item.collection_id
                           JOIN media_items visible_child
                             ON visible_child.id = visible_collection_item.item_id
                           WHERE visible_collection.item_id = mi.id
                             AND visible_child.removed_at IS NULL
                             AND visible_child.has_available_source = 1
                       )
                   )
             ) visible_catalog"
        );
        let mut count_statement = self.query_scalar::<i64>(sqlx::AssertSqlSafe(count_query));
        for library_id in library_ids {
            count_statement = count_statement.bind(library_id);
        }
        for bind in &content_restriction_binds {
            count_statement = match bind {
                CatalogBind::Text(value) => count_statement.bind(*value),
                CatalogBind::Integer(value) => count_statement.bind(*value),
                CatalogBind::Real(value) => count_statement.bind(*value),
            };
        }
        for library_id in library_ids {
            count_statement = count_statement.bind(library_id);
        }
        for bind in &content_restriction_binds {
            count_statement = match bind {
                CatalogBind::Text(value) => count_statement.bind(*value),
                CatalogBind::Integer(value) => count_statement.bind(*value),
                CatalogBind::Real(value) => count_statement.bind(*value),
            };
        }
        let list_query = format!(
            "WITH visible_catalog AS (
                 SELECT mi.id, mi.library_id, mi.added_at, mi.sort_title
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 WHERE mi.removed_at IS NULL
                   AND {available_item_types}
                   AND mi.has_available_source = 1
                   AND mi.library_id IN ({placeholders}){content_restriction_predicate}
                 UNION ALL
                 SELECT mi.id, mi.library_id, mi.added_at, mi.sort_title
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 WHERE mi.removed_at IS NULL
                   AND {unavailable_item_types}
                   AND mi.has_available_source = 0
                   AND mi.library_id IN ({placeholders}){content_restriction_predicate}
                   AND (
                       EXISTS (
                           SELECT 1
                           FROM media_items visible_child
                           WHERE visible_child.removed_at IS NULL
                             AND visible_child.has_available_source = 1
                             AND (visible_child.parent_id = mi.id OR visible_child.series_id = mi.id)
                       )
                       OR EXISTS (
                           SELECT 1
                           FROM collection_items visible_collection_item
                           JOIN collections visible_collection
                             ON visible_collection.id = visible_collection_item.collection_id
                           JOIN media_items visible_child
                             ON visible_child.id = visible_collection_item.item_id
                           WHERE visible_collection.item_id = mi.id
                             AND visible_child.removed_at IS NULL
                             AND visible_child.has_available_source = 1
                       )
                   )
             )
             SELECT id
             FROM visible_catalog
             ORDER BY added_at DESC, sort_title, id
             LIMIT ? OFFSET ?"
        );
        let mut list_statement = self.query(sqlx::AssertSqlSafe(list_query));
        for library_id in library_ids {
            list_statement = list_statement.bind(library_id);
        }
        for bind in &content_restriction_binds {
            list_statement = match bind {
                CatalogBind::Text(value) => list_statement.bind(*value),
                CatalogBind::Integer(value) => list_statement.bind(*value),
                CatalogBind::Real(value) => list_statement.bind(*value),
            };
        }
        for library_id in library_ids {
            list_statement = list_statement.bind(library_id);
        }
        for bind in &content_restriction_binds {
            list_statement = match bind {
                CatalogBind::Text(value) => list_statement.bind(*value),
                CatalogBind::Integer(value) => list_statement.bind(*value),
                CatalogBind::Real(value) => list_statement.bind(*value),
            };
        }
        let count_future = async {
            count_statement
                .fetch_one(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })
        };
        let list_future = async {
            list_statement
                .bind(limit)
                .bind(offset)
                .fetch_all(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })
        };
        let (total, rows) = tokio::try_join!(count_future, list_future)?;
        Ok((rows.into_iter().map(|row| row.get("id")).collect(), total))
    }

    #[cfg(test)]
    pub(crate) async fn list_recent_catalog_rows_by_library(
        &self,
        library_ids: &[String],
        limit: i64,
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        self.list_recent_catalog_rows_by_library_with_restrictions(library_ids, limit, None)
            .await
    }

    pub(crate) async fn list_recent_catalog_rows_by_library_with_restrictions(
        &self,
        library_ids: &[String],
        limit: i64,
        content_restrictions: Option<&UserContentRestrictions>,
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        if library_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut content_restriction_predicate = String::new();
        let mut content_restriction_binds = Vec::new();
        append_content_restriction_predicate(
            &mut content_restriction_predicate,
            &mut content_restriction_binds,
            content_restrictions,
        );
        let mut rows = Vec::new();
        for library_ids in library_ids.chunks(500) {
            let (selection_query, binds) = if self.backend == DatabaseBackend::Postgres {
                let mut binds =
                    Vec::with_capacity(library_ids.len() + content_restriction_binds.len() + 1);
                binds.extend(library_ids.iter().map(|id| CatalogBind::Text(id)));
                binds.extend(content_restriction_binds.iter().copied());
                binds.push(CatalogBind::Integer(limit));
                (
                    postgres_recent_catalog_rows_by_library_query(
                        library_ids.len(),
                        &content_restriction_predicate,
                    ),
                    binds,
                )
            } else {
                let query = sqlite_recent_catalog_rows_by_library_query(
                    library_ids.len(),
                    &content_restriction_predicate,
                );
                let mut binds = Vec::with_capacity(
                    library_ids.len() * (5 + content_restriction_binds.len() * 2),
                );
                for library_id in library_ids {
                    binds.push(CatalogBind::Text(library_id));
                    binds.extend(content_restriction_binds.iter().copied());
                    binds.push(CatalogBind::Integer(limit));
                    binds.push(CatalogBind::Text(library_id));
                    binds.extend(content_restriction_binds.iter().copied());
                    binds.push(CatalogBind::Integer(limit));
                    binds.push(CatalogBind::Integer(limit));
                }
                (query, binds)
            };
            let query = format!(
                "{selection_query}
             SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                    mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                    mi.title, mi.sort_title, mi.original_title, mi.overview,
                    mi.production_year, mi.rating, mi.rating_source, mi.runtime_ticks,
                    mi.added_at, mi.updated_at,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                     ORDER BY image_index LIMIT 1) AS poster_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                     ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'THUMB'
                     ORDER BY image_index LIMIT 1) AS thumb_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'LOGO'
                     ORDER BY image_index LIMIT 1) AS logo_image_tag,
                    (SELECT fe.relative_path FROM filesystem_entries fe
                     WHERE fe.id = ms.filesystem_entry_id) AS source_relative_path,
                    ms.id AS source_id, ms.source_kind, ms.container, ms.size, ms.external_url,
                    ms.edition_name, ms.quality_label,
                    ms.bitrate, ms.duration_ticks, ms.is_default, ms.probe_status,
                    mt.id AS stream_id, mt.stream_index, mt.stream_type,
                    mt.codec, mt.language, mt.title AS stream_title,
                    mt.details_json AS stream_details_json,
                    mt.is_external AS stream_is_external,
                    mt.is_default AS stream_is_default,
                    mt.is_forced AS stream_is_forced
             FROM selected
             JOIN media_items mi ON mi.id = selected.id
             LEFT JOIN media_sources ms
               ON ms.item_id = mi.id
              AND EXISTS (
                  SELECT 1 FROM filesystem_entries fe
                  WHERE fe.id = ms.filesystem_entry_id AND fe.is_missing = 0
             )
             LEFT JOIN media_streams mt ON mt.media_source_id = ms.id
             ORDER BY selected.library_id, selected.latest_added_at DESC, mi.sort_title ASC,
                      mi.id ASC, ms.id, mt.stream_index"
            );
            rows.extend(self.fetch_catalog_rows(&query, &binds).await?);
        }
        Ok(rows)
    }

    #[cfg(test)]
    pub(crate) async fn list_recommended_catalog_rows(
        &self,
        user_id: &str,
        library_ids: &[String],
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        self.list_recommended_catalog_rows_with_restrictions(
            user_id,
            library_ids,
            offset,
            limit,
            None,
        )
        .await
    }

    pub(crate) async fn list_recommended_catalog_rows_with_restrictions(
        &self,
        user_id: &str,
        library_ids: &[String],
        offset: i64,
        limit: i64,
        content_restrictions: Option<&UserContentRestrictions>,
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        if library_ids.is_empty() {
            return Ok(Vec::new());
        }
        let max_function = self.scalar_max_function();
        let min_function = self.scalar_min_function();
        let median_rating = self.recommendation_rating_median(library_ids).await?;
        let mut content_restriction_predicate = String::new();
        let mut content_restriction_binds = Vec::new();
        append_content_restriction_predicate(
            &mut content_restriction_predicate,
            &mut content_restriction_binds,
            content_restrictions,
        );
        let catalog_placeholders = std::iter::repeat_n("?", library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "WITH scored AS (
                 SELECT mi.id,
                        COALESCE(rs.recent_playback_score, 0) AS recent_playback_score,
                        (
                            CASE WHEN us.item_id IS NULL THEN 35 ELSE 0 END
                            + CASE WHEN COALESCE(us.is_played, 0) = 1 THEN -35 ELSE 0 END
                            + COALESCE(rs.favorite_score, 0)
                            + {min_function}(50.0, {max_function}(0.0,
                                COALESCE(mi.rating, ?) * 5.0))
                            + COALESCE(rs.recent_playback_score, 0)
                            + {min_function}(7, {max_function}(0, 7 - CAST((unixepoch() - mi.added_at) / 86400 AS INTEGER)))
                            + CASE WHEN us.last_played_at IS NULL THEN 0 ELSE
                                {min_function}(30, {max_function}(0, 30 - CAST((unixepoch() - us.last_played_at) / 86400 AS INTEGER)))
                              END
                        ) AS recommendation_score,
                        mi.added_at,
                        mi.sort_title
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 LEFT JOIN recommendation_item_stats rs ON rs.item_id = mi.id
                 LEFT JOIN user_item_state us
                   ON us.item_id = mi.id AND us.user_id = ?
                 WHERE mi.removed_at IS NULL{CATALOG_VISIBLE_PREDICATE}
                   AND mi.item_type IN ('MOVIE', 'SERIES')
                   AND mi.library_id IN ({catalog_placeholders}){content_restriction_predicate}
             ),
             playback_top AS (
                 SELECT scored.*
                 FROM scored
                 WHERE recent_playback_score > 0
                 ORDER BY recommendation_score DESC, added_at DESC,
                          sort_title, id
                 LIMIT 5
             ),
             paged AS (
                 SELECT * FROM playback_top
                 UNION ALL
                 SELECT *
                 FROM scored
                 WHERE recent_playback_score = 0
             ),
             ranked AS (
                 SELECT *
                 FROM paged
                 ORDER BY recommendation_score DESC, added_at DESC,
                          sort_title, id
                 LIMIT ? OFFSET ?
             )
             SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                    mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                    mi.title, mi.sort_title, mi.original_title, mi.overview,
                    mi.production_year, mi.rating, mi.rating_source, mi.runtime_ticks,
                    mi.added_at, mi.updated_at,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                     ORDER BY image_index LIMIT 1) AS poster_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                     ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'THUMB'
                     ORDER BY image_index LIMIT 1) AS thumb_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'LOGO'
                     ORDER BY image_index LIMIT 1) AS logo_image_tag,
                    (SELECT fe.relative_path FROM filesystem_entries fe
                     WHERE fe.id = ms.filesystem_entry_id) AS source_relative_path,
                    ms.id AS source_id, ms.source_kind, ms.container, ms.size, ms.external_url,
                    ms.edition_name, ms.quality_label,
                    ms.bitrate, ms.duration_ticks, ms.is_default, ms.probe_status,
                    mt.id AS stream_id, mt.stream_index, mt.stream_type,
                    mt.codec, mt.language, mt.title AS stream_title,
                    mt.details_json AS stream_details_json,
                    mt.is_external AS stream_is_external,
                    mt.is_default AS stream_is_default,
                    mt.is_forced AS stream_is_forced
             FROM ranked
             JOIN media_items mi ON mi.id = ranked.id
             LEFT JOIN media_sources ms
               ON ms.item_id = mi.id
              AND EXISTS (
                  SELECT 1 FROM filesystem_entries fe
                  WHERE fe.id = ms.filesystem_entry_id AND fe.is_missing = 0
              )
             LEFT JOIN media_streams mt ON mt.media_source_id = ms.id
             ORDER BY ranked.recommendation_score DESC, mi.added_at DESC,
                      mi.sort_title, mi.id, ms.id, mt.stream_index"
        );
        let mut binds = Vec::with_capacity(library_ids.len() + content_restriction_binds.len() + 4);
        binds.push(CatalogBind::Real(median_rating));
        binds.push(CatalogBind::Text(user_id));
        binds.extend(library_ids.iter().map(|value| CatalogBind::Text(value)));
        binds.extend(content_restriction_binds);
        binds.push(CatalogBind::Integer(limit));
        binds.push(CatalogBind::Integer(offset));
        self.fetch_catalog_rows(&query, &binds).await
    }

    pub(crate) async fn count_catalog_children(
        &self,
        parent_id: &str,
        item_types: &str,
        content_restrictions: Option<&UserContentRestrictions>,
    ) -> Result<i64, StorageError> {
        let item_types = item_types
            .split(',')
            .map(str::trim)
            .filter(|item_type| !item_type.is_empty())
            .collect::<Vec<_>>();
        if item_types.is_empty() {
            return Ok(0);
        }
        let placeholders = std::iter::repeat_n("?", item_types.len())
            .collect::<Vec<_>>()
            .join(", ");
        let mut content_restriction_predicate = String::new();
        let mut content_restriction_binds = Vec::new();
        append_content_restriction_predicate(
            &mut content_restriction_predicate,
            &mut content_restriction_binds,
            content_restrictions,
        );
        let query = format!(
            "SELECT COUNT(*) FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             WHERE mi.parent_id = ? AND mi.item_type IN ({placeholders}) AND mi.removed_at IS NULL
               {CATALOG_VISIBLE_PREDICATE}{content_restriction_predicate}"
        );
        let mut statement = self
            .query_scalar::<i64>(sqlx::AssertSqlSafe(query))
            .bind(parent_id);
        for item_type in item_types {
            statement = statement.bind(item_type);
        }
        for bind in content_restriction_binds {
            statement = match bind {
                CatalogBind::Text(value) => statement.bind(value),
                CatalogBind::Integer(value) => statement.bind(value),
                CatalogBind::Real(value) => statement.bind(value),
            };
        }
        statement
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_episode_counts(
        &self,
        item_ids: &[String],
        content_restrictions: Option<&UserContentRestrictions>,
    ) -> Result<HashMap<String, i64>, StorageError> {
        if item_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let mut content_restriction_predicate = String::new();
        let mut content_restriction_binds = Vec::new();
        append_content_restriction_predicate_for_item_alias(
            &mut content_restriction_predicate,
            &mut content_restriction_binds,
            content_restrictions,
            "child",
        );
        let placeholders = std::iter::repeat_n("?", item_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "SELECT parent.id,
                    COUNT(DISTINCT CASE
                        WHEN parent.item_type = 'SERIES' THEN
                            COALESCE(CAST(child.season_number AS TEXT), '') || ':' ||
                            COALESCE(CAST(child.episode_number AS TEXT), child.id)
                        ELSE COALESCE(CAST(child.episode_number AS TEXT), child.id)
                    END) AS episode_count
             FROM media_items parent
             JOIN libraries l ON l.id = parent.library_id AND l.is_enabled = 1
             LEFT JOIN media_items child
               ON child.item_type = 'EPISODE' AND child.removed_at IS NULL
              AND ((parent.item_type = 'SERIES' AND child.series_id = parent.id)
                OR (parent.item_type = 'SEASON' AND child.parent_id = parent.id))
              AND EXISTS (
                  SELECT 1
                  FROM media_sources child_source
                  JOIN filesystem_entries child_entry
                    ON child_entry.id = child_source.filesystem_entry_id
                  WHERE child_source.item_id = child.id
                    AND child_entry.is_missing = 0
              ){content_restriction_predicate}
             WHERE parent.id IN ({placeholders})
               AND parent.item_type IN ('SERIES', 'SEASON')
               AND parent.removed_at IS NULL
             GROUP BY parent.id"
        );
        let mut statement = self.query(sqlx::AssertSqlSafe(query));
        for bind in content_restriction_binds {
            statement = match bind {
                CatalogBind::Text(value) => statement.bind(value),
                CatalogBind::Integer(value) => statement.bind(value),
                CatalogBind::Real(value) => statement.bind(value),
            };
        }
        for item_id in item_ids {
            statement = statement.bind(item_id);
        }
        statement
            .fetch_all(&self.pool)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(|row| (row.get("id"), row.get("episode_count")))
                    .collect()
            })
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_unplayed_episode_counts(
        &self,
        user_id: &str,
        item_ids: &[String],
    ) -> Result<HashMap<String, i64>, StorageError> {
        if item_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = std::iter::repeat_n("?", item_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "SELECT parent.id,
                    COUNT(DISTINCT CASE
                        WHEN COALESCE(user_state.is_played, 0) = 0 THEN
                            CASE
                                WHEN parent.item_type = 'SERIES' THEN
                                    COALESCE(CAST(child.season_number AS TEXT), '') || ':' ||
                                    COALESCE(CAST(child.episode_number AS TEXT), child.id)
                                ELSE COALESCE(CAST(child.episode_number AS TEXT), child.id)
                            END
                    END) AS unplayed_episode_count
             FROM media_items parent
             JOIN libraries l ON l.id = parent.library_id AND l.is_enabled = 1
             LEFT JOIN media_items child
               ON child.item_type = 'EPISODE' AND child.removed_at IS NULL
              AND ((parent.item_type = 'SERIES' AND child.series_id = parent.id)
                OR (parent.item_type = 'SEASON' AND child.parent_id = parent.id))
              AND EXISTS (
                  SELECT 1
                  FROM media_sources child_source
                  JOIN filesystem_entries child_entry
                    ON child_entry.id = child_source.filesystem_entry_id
                  WHERE child_source.item_id = child.id
                    AND child_entry.is_missing = 0
              )
             LEFT JOIN user_item_state user_state
               ON user_state.item_id = child.id AND user_state.user_id = ?
             WHERE parent.id IN ({placeholders})
               AND parent.item_type IN ('SERIES', 'SEASON')
               AND parent.removed_at IS NULL
             GROUP BY parent.id"
        );
        let mut statement = self.query(sqlx::AssertSqlSafe(query)).bind(user_id);
        for item_id in item_ids {
            statement = statement.bind(item_id);
        }
        statement
            .fetch_all(&self.pool)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(|row| (row.get("id"), row.get("unplayed_episode_count")))
                    .collect()
            })
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_catalog_children(
        &self,
        parent_id: &str,
        item_types: &str,
        sort: Option<(CatalogSort, bool)>,
        content_restrictions: Option<&UserContentRestrictions>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        let item_types = item_types
            .split(',')
            .map(str::trim)
            .filter(|item_type| !item_type.is_empty())
            .collect::<Vec<_>>();
        if item_types.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat_n("?", item_types.len())
            .collect::<Vec<_>>()
            .join(", ");
        let item_order = sort
            .map(|(sort_by, descending)| catalog_item_order(sort_by, descending))
            .unwrap_or("mi.season_number, mi.episode_number, mi.sort_title, mi.id");
        let mut content_restriction_predicate = String::new();
        let mut content_restriction_binds = Vec::new();
        append_content_restriction_predicate(
            &mut content_restriction_predicate,
            &mut content_restriction_binds,
            content_restrictions,
        );
        let query = format!(
            "SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                    mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                    mi.title, mi.sort_title, mi.original_title, mi.overview,
                    mi.production_year, mi.rating, mi.rating_source, mi.runtime_ticks,
                    mi.added_at, mi.updated_at,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                     ORDER BY image_index LIMIT 1) AS poster_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                     ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'THUMB'
                     ORDER BY image_index LIMIT 1) AS thumb_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'LOGO'
                     ORDER BY image_index LIMIT 1) AS logo_image_tag,
                    (SELECT fe.relative_path FROM filesystem_entries fe
                     WHERE fe.id = ms.filesystem_entry_id) AS source_relative_path,
                    ms.id AS source_id, ms.source_kind, ms.container, ms.size, ms.external_url,
                    ms.edition_name, ms.quality_label,
                    ms.bitrate, ms.duration_ticks, ms.is_default, ms.probe_status,
                    mt.id AS stream_id, mt.stream_index, mt.stream_type,
                    mt.codec, mt.language, mt.title AS stream_title,
                    mt.details_json AS stream_details_json,
                    mt.is_external AS stream_is_external,
                    mt.is_default AS stream_is_default,
                    mt.is_forced AS stream_is_forced
             FROM (
                 -- Page over items first: joining sources and streams before LIMIT would
                 -- paginate the joined rows, so a multi-source item could push other items
                 -- off the page and appear again on the next one.
                 SELECT mi.id AS item_id
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 WHERE mi.parent_id = ? AND mi.item_type IN ({placeholders})
                   AND mi.removed_at IS NULL
                   {CATALOG_VISIBLE_PREDICATE}{content_restriction_predicate}
                 ORDER BY {item_order}
                 LIMIT ? OFFSET ?
             ) page
             JOIN media_items mi ON mi.id = page.item_id
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             LEFT JOIN media_sources ms
               ON ms.item_id = mi.id
              AND EXISTS (
                  SELECT 1 FROM filesystem_entries fe
                  WHERE fe.id = ms.filesystem_entry_id AND fe.is_missing = 0
              )
             LEFT JOIN media_streams mt ON mt.media_source_id = ms.id
             ORDER BY {item_order}, ms.id, mt.stream_index"
        );
        let mut binds = Vec::with_capacity(item_types.len() + content_restriction_binds.len() + 3);
        binds.push(CatalogBind::Text(parent_id));
        binds.extend(item_types.into_iter().map(CatalogBind::Text));
        binds.extend(content_restriction_binds);
        binds.push(CatalogBind::Integer(limit));
        binds.push(CatalogBind::Integer(offset));
        self.fetch_catalog_rows(&query, &binds).await
    }

    pub(crate) async fn list_series_episode_ids(
        &self,
        series_id: &str,
        season_id: Option<&str>,
        content_restrictions: Option<&UserContentRestrictions>,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<String>, i64), StorageError> {
        let mut content_restriction_predicate = String::new();
        let mut content_restriction_binds = Vec::new();
        append_content_restriction_predicate(
            &mut content_restriction_predicate,
            &mut content_restriction_binds,
            content_restrictions,
        );
        let season_filter = if season_id.is_some() {
            " AND mi.parent_id = ?"
        } else {
            ""
        };
        let count_sql = format!(
            "SELECT COUNT(*)
             FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             WHERE mi.series_id = ? AND mi.item_type = 'EPISODE'
               AND mi.removed_at IS NULL{season_filter}
               {CATALOG_VISIBLE_PREDICATE}{content_restriction_predicate}"
        );
        let mut count_statement = self
            .query_scalar::<i64>(sqlx::AssertSqlSafe(count_sql))
            .bind(series_id);
        if let Some(season_id) = season_id {
            count_statement = count_statement.bind(season_id);
        }
        for bind in &content_restriction_binds {
            count_statement = match bind {
                CatalogBind::Text(value) => count_statement.bind(*value),
                CatalogBind::Integer(value) => count_statement.bind(*value),
                CatalogBind::Real(value) => count_statement.bind(*value),
            };
        }
        let total = count_statement
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let list_sql = format!(
            "SELECT mi.id
             FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             WHERE mi.series_id = ? AND mi.item_type = 'EPISODE'
               AND mi.removed_at IS NULL{season_filter}
               {CATALOG_VISIBLE_PREDICATE}{content_restriction_predicate}
             ORDER BY mi.season_number, mi.episode_number, mi.sort_title, mi.id
             LIMIT ? OFFSET ?"
        );
        let mut list_statement = self.query(sqlx::AssertSqlSafe(list_sql)).bind(series_id);
        if let Some(season_id) = season_id {
            list_statement = list_statement.bind(season_id);
        }
        for bind in &content_restriction_binds {
            list_statement = match bind {
                CatalogBind::Text(value) => list_statement.bind(*value),
                CatalogBind::Integer(value) => list_statement.bind(*value),
                CatalogBind::Real(value) => list_statement.bind(*value),
            };
        }
        let rows = list_statement
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok((rows.into_iter().map(|row| row.get("id")).collect(), total))
    }

    pub(crate) async fn list_resume_items_with_total(
        &self,
        query: &ResumeItemsQuery<'_>,
        latest_episode_per_series: bool,
    ) -> Result<(Vec<StoredCatalogRow>, i64), StorageError> {
        self.list_resume_items_with_next_episode(query, latest_episode_per_series, false)
            .await
    }

    pub(crate) async fn list_home_resume_items_with_total(
        &self,
        query: &ResumeItemsQuery<'_>,
        latest_episode_per_series: bool,
    ) -> Result<(Vec<StoredCatalogRow>, i64), StorageError> {
        self.list_resume_items_with_next_episode(query, latest_episode_per_series, true)
            .await
    }

    async fn list_resume_items_with_next_episode(
        &self,
        query: &ResumeItemsQuery<'_>,
        latest_episode_per_series: bool,
        include_next_unplayed_episode: bool,
    ) -> Result<(Vec<StoredCatalogRow>, i64), StorageError> {
        if query.library_ids.is_empty() || query.item_types.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let item_type_placeholders = std::iter::repeat_n("?", query.item_types.len())
            .collect::<Vec<_>>()
            .join(", ");
        let library_placeholders = std::iter::repeat_n("?", query.library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let mut content_restriction_predicate = String::new();
        let mut content_restriction_binds = Vec::new();
        append_content_restriction_predicate(
            &mut content_restriction_predicate,
            &mut content_restriction_binds,
            query.content_restrictions,
        );
        let runtime_ticks = resume_runtime_ticks_sql();
        let resume_rank = Self::resume_rank_expression(latest_episode_per_series);
        let (next_episode_ctes, ranking_ctes) = Self::resume_next_episode_query_parts(
            include_next_unplayed_episode,
            &library_placeholders,
            resume_rank,
            &content_restriction_predicate,
        );
        let statement_sql = format!(
            "WITH candidates AS (
                 SELECT mi.id, mi.item_type, mi.series_id, mi.season_number,
                        mi.episode_number, mi.sort_title, us.position_ticks, us.last_played_at,
                        {runtime_ticks} AS resume_runtime_ticks
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 JOIN user_item_state us ON us.item_id = mi.id AND us.user_id = ?
                 WHERE mi.item_type IN ({item_type_placeholders}) AND mi.removed_at IS NULL{CATALOG_VISIBLE_PREDICATE}
                   AND us.is_played = 0 AND us.hide_from_resume = 0 AND us.position_ticks >= ?
                   AND mi.library_id IN ({library_placeholders}){content_restriction_predicate}
             ),
             eligible AS (
                 SELECT id, item_type, series_id, season_number, episode_number,
                        sort_title, last_played_at
                 FROM candidates
                 WHERE resume_runtime_ticks > 0
                   AND position_ticks * 100 < resume_runtime_ticks * ?
             ){next_episode_ctes}{ranking_ctes},
             qualified AS (
                 SELECT id, sort_title, last_played_at,
                        COUNT(*) OVER () AS resume_total
                 FROM ranked
                 WHERE resume_rank = 1
             ),
             limited AS (
                 SELECT id, sort_title, last_played_at, resume_total
                 FROM qualified
                 ORDER BY last_played_at DESC NULLS LAST, sort_title, id
                 LIMIT ? OFFSET ?
             )
             SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                    mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                    series.title AS series_name,
                    mi.title, mi.sort_title, mi.original_title, mi.overview,
                    mi.production_year, mi.rating, mi.rating_source, mi.runtime_ticks,
                    mi.added_at, mi.updated_at,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                     ORDER BY image_index LIMIT 1) AS poster_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                     ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'THUMB'
                     ORDER BY image_index LIMIT 1) AS thumb_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'LOGO'
                     ORDER BY image_index LIMIT 1) AS logo_image_tag,
                    (SELECT fe.relative_path FROM filesystem_entries fe
                     WHERE fe.id = ms.filesystem_entry_id) AS source_relative_path,
                    ms.id AS source_id, ms.source_kind, ms.container, ms.size, ms.external_url,
                    ms.edition_name, ms.quality_label,
                    ms.bitrate, ms.duration_ticks, ms.is_default, ms.probe_status,
                    mt.id AS stream_id, mt.stream_index, mt.stream_type,
                    mt.codec, mt.language, mt.title AS stream_title,
                    mt.details_json AS stream_details_json,
                    mt.is_external AS stream_is_external,
                    mt.is_default AS stream_is_default,
                    mt.is_forced AS stream_is_forced,
                    ranked.resume_total AS resume_total
             FROM limited ranked
             JOIN media_items mi ON mi.id = ranked.id
             LEFT JOIN media_items series ON series.id = mi.series_id
             LEFT JOIN media_sources ms
               ON ms.item_id = mi.id
              AND EXISTS (
                  SELECT 1 FROM filesystem_entries fe
                  WHERE fe.id = ms.filesystem_entry_id AND fe.is_missing = 0
              )
             LEFT JOIN media_streams mt ON mt.media_source_id = ms.id
             ORDER BY ranked.last_played_at DESC NULLS LAST,
                      ranked.sort_title, ranked.id,
                      ms.id, mt.stream_index"
        );
        let mut binds = Vec::with_capacity(query.item_types.len() + query.library_ids.len() + 5);
        binds.push(CatalogBind::Text(query.user_id));
        binds.extend(query.item_types.iter().copied().map(CatalogBind::Text));
        binds.push(CatalogBind::Integer(query.minimum_ticks));
        binds.extend(
            query
                .library_ids
                .iter()
                .map(|value| CatalogBind::Text(value)),
        );
        binds.extend(content_restriction_binds.iter().copied());
        binds.push(CatalogBind::Integer(query.played_percent));
        if include_next_unplayed_episode {
            binds.push(CatalogBind::Text(query.user_id));
            binds.extend(
                query
                    .library_ids
                    .iter()
                    .map(|value| CatalogBind::Text(value)),
            );
            binds.push(CatalogBind::Text(query.user_id));
            binds.extend(
                query
                    .library_ids
                    .iter()
                    .map(|value| CatalogBind::Text(value)),
            );
            binds.extend(content_restriction_binds.iter().copied());
        }
        binds.push(CatalogBind::Integer(query.limit));
        binds.push(CatalogBind::Integer(query.offset));
        let (rows, total) = self
            .fetch_catalog_rows_with_total(&statement_sql, &binds)
            .await?;
        if rows.is_empty() && (query.offset > 0 || query.limit <= 0) {
            let total = self
                .count_resume_items(
                    query,
                    latest_episode_per_series,
                    include_next_unplayed_episode,
                )
                .await?;
            return Ok((rows, total));
        }
        Ok((rows, total))
    }

    pub(crate) async fn count_resume_items(
        &self,
        query: &ResumeItemsQuery<'_>,
        latest_episode_per_series: bool,
        include_next_unplayed_episode: bool,
    ) -> Result<i64, StorageError> {
        if query.library_ids.is_empty() || query.item_types.is_empty() {
            return Ok(0);
        }
        let item_type_placeholders = std::iter::repeat_n("?", query.item_types.len())
            .collect::<Vec<_>>()
            .join(", ");
        let library_placeholders = std::iter::repeat_n("?", query.library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let mut content_restriction_predicate = String::new();
        let mut content_restriction_binds = Vec::new();
        append_content_restriction_predicate(
            &mut content_restriction_predicate,
            &mut content_restriction_binds,
            query.content_restrictions,
        );
        let runtime_ticks = resume_runtime_ticks_sql();
        let resume_rank = Self::resume_rank_expression(latest_episode_per_series);
        let (next_episode_ctes, ranking_ctes) = Self::resume_next_episode_query_parts(
            include_next_unplayed_episode,
            &library_placeholders,
            resume_rank,
            &content_restriction_predicate,
        );
        let statement_sql = format!(
            "WITH candidates AS (
                 SELECT mi.id, mi.item_type, mi.series_id,
                        mi.season_number, mi.episode_number, mi.sort_title,
                        us.position_ticks, us.last_played_at,
                        {runtime_ticks} AS resume_runtime_ticks
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 JOIN user_item_state us ON us.item_id = mi.id AND us.user_id = ?
                 WHERE mi.item_type IN ({item_type_placeholders}) AND mi.removed_at IS NULL{CATALOG_VISIBLE_PREDICATE}
                   AND us.is_played = 0 AND us.hide_from_resume = 0 AND us.position_ticks >= ?
                   AND mi.library_id IN ({library_placeholders}){content_restriction_predicate}
             ), eligible AS (
                 SELECT id, item_type, series_id, season_number, episode_number,
                        sort_title, last_played_at
                 FROM candidates
                 WHERE resume_runtime_ticks > 0
                   AND position_ticks * 100 < resume_runtime_ticks * ?
             ){next_episode_ctes}{ranking_ctes}
             SELECT COUNT(*) FROM ranked WHERE resume_rank = 1"
        );
        let mut statement = self
            .query_scalar::<i64>(sqlx::AssertSqlSafe(statement_sql))
            .bind(query.user_id);
        for item_type in query.item_types {
            statement = statement.bind(*item_type);
        }
        statement = statement.bind(query.minimum_ticks);
        for library_id in query.library_ids {
            statement = statement.bind(library_id);
        }
        for bind in &content_restriction_binds {
            statement = match bind {
                CatalogBind::Text(value) => statement.bind(*value),
                CatalogBind::Integer(value) => statement.bind(*value),
                CatalogBind::Real(value) => statement.bind(*value),
            };
        }
        statement = statement.bind(query.played_percent);
        if include_next_unplayed_episode {
            statement = statement.bind(query.user_id);
            for library_id in query.library_ids {
                statement = statement.bind(library_id);
            }
            statement = statement.bind(query.user_id);
            for library_id in query.library_ids {
                statement = statement.bind(library_id);
            }
            for bind in &content_restriction_binds {
                statement = match bind {
                    CatalogBind::Text(value) => statement.bind(*value),
                    CatalogBind::Integer(value) => statement.bind(*value),
                    CatalogBind::Real(value) => statement.bind(*value),
                };
            }
        }
        statement
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    fn resume_next_episode_query_parts(
        include_next_unplayed_episode: bool,
        library_placeholders: &str,
        resume_rank: &str,
        content_restriction_predicate: &str,
    ) -> (String, String) {
        if !include_next_unplayed_episode {
            return (
                String::new(),
                format!(
                    ", ranked AS (
                         SELECT id, sort_title, last_played_at,
                                {resume_rank} AS resume_rank
                         FROM eligible
                     )"
                ),
            );
        }

        let next_episode_ctes = format!(
            ", completed_episodes_ranked AS (
                 SELECT mi.series_id, mi.season_number, mi.episode_number,
                        us.last_played_at,
                        ROW_NUMBER() OVER (
                            PARTITION BY mi.series_id
                            ORDER BY us.last_played_at DESC NULLS LAST,
                                     mi.season_number DESC NULLS LAST,
                                     mi.episode_number DESC NULLS LAST,
                                     mi.id DESC
                        ) AS played_rank
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 JOIN user_item_state us ON us.item_id = mi.id AND us.user_id = ?
                 WHERE mi.item_type = 'EPISODE' AND mi.series_id IS NOT NULL
                   AND mi.season_number IS NOT NULL AND mi.episode_number IS NOT NULL
                   AND mi.removed_at IS NULL AND mi.library_id IN ({library_placeholders})
                   AND us.is_played = 1{CATALOG_VISIBLE_PREDICATE}
             ),
             latest_completed_episode AS (
                 SELECT series_id, season_number, episode_number, last_played_at
                 FROM completed_episodes_ranked
                 WHERE played_rank = 1
             ),
             next_episodes_ranked AS (
                 SELECT mi.id, mi.item_type, mi.series_id, mi.season_number,
                        mi.episode_number, mi.sort_title,
                        COALESCE(us.last_played_at, completed.last_played_at) AS last_played_at,
                        ROW_NUMBER() OVER (
                            PARTITION BY mi.series_id
                            ORDER BY mi.season_number ASC, mi.episode_number ASC,
                                     mi.sort_title, mi.id
                        ) AS next_rank
                 FROM latest_completed_episode completed
                 JOIN media_items mi
                   ON mi.series_id = completed.series_id
                  AND (mi.season_number > completed.season_number
                    OR (mi.season_number = completed.season_number
                        AND mi.episode_number > completed.episode_number))
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 LEFT JOIN user_item_state us ON us.item_id = mi.id AND us.user_id = ?
                 WHERE mi.item_type = 'EPISODE'
                   AND mi.season_number IS NOT NULL AND mi.episode_number IS NOT NULL
                   AND mi.has_available_source = 1 AND mi.removed_at IS NULL
                   AND mi.library_id IN ({library_placeholders})
                   AND COALESCE(us.is_played, 0) = 0
                   AND COALESCE(us.hide_from_resume, 0) = 0{CATALOG_VISIBLE_PREDICATE}{content_restriction_predicate}
             ),
             next_episode AS (
                 SELECT id, item_type, series_id, season_number, episode_number,
                        sort_title, last_played_at
                 FROM next_episodes_ranked
                 WHERE next_rank = 1
             )"
        );
        let ranking_ctes = format!(
            ", combined AS (
                 SELECT id, item_type, series_id, season_number, episode_number,
                        sort_title, last_played_at, 0 AS candidate_priority
                 FROM eligible
                 UNION ALL
                 SELECT id, item_type, series_id, season_number, episode_number,
                        sort_title, last_played_at, 1 AS candidate_priority
                 FROM next_episode
             ),
             deduplicated AS (
                 SELECT id, item_type, series_id, season_number, episode_number,
                        sort_title, last_played_at,
                        ROW_NUMBER() OVER (
                            PARTITION BY id ORDER BY candidate_priority
                        ) AS candidate_rank
                 FROM combined
             ),
             ranked AS (
                 SELECT id, sort_title, last_played_at,
                        {resume_rank} AS resume_rank
                 FROM deduplicated
                 WHERE candidate_rank = 1
             )"
        );
        (next_episode_ctes, ranking_ctes)
    }

    fn resume_rank_expression(latest_episode_per_series: bool) -> &'static str {
        if latest_episode_per_series {
            "ROW_NUMBER() OVER (
                 PARTITION BY CASE WHEN item_type = 'EPISODE'
                                   THEN COALESCE(series_id, id) ELSE id END
                 ORDER BY season_number DESC NULLS LAST,
                          episode_number DESC NULLS LAST,
                          last_played_at DESC NULLS LAST, sort_title, id
             )"
        } else {
            "1"
        }
    }

    pub(crate) async fn count_progress_items(
        &self,
        user_id: &str,
        library_ids: &[String],
        item_types: &[&str],
        series_id: Option<&str>,
    ) -> Result<i64, StorageError> {
        if library_ids.is_empty() || item_types.is_empty() {
            return Ok(0);
        }
        let item_type_placeholders = std::iter::repeat_n("?", item_types.len())
            .collect::<Vec<_>>()
            .join(", ");
        let library_placeholders = std::iter::repeat_n("?", library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let series_predicate = series_id.map(|_| " AND mi.series_id = ?").unwrap_or("");
        let query = format!(
            "SELECT COUNT(*) FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             JOIN user_item_state us ON us.item_id = mi.id AND us.user_id = ?
             WHERE mi.item_type IN ({item_type_placeholders}) AND mi.removed_at IS NULL{CATALOG_VISIBLE_PREDICATE}
               AND us.is_played = 0 AND us.position_ticks > 0
               AND mi.library_id IN ({library_placeholders}){series_predicate}"
        );
        let mut statement = self
            .query_scalar::<i64>(sqlx::AssertSqlSafe(query))
            .bind(user_id);
        for item_type in item_types {
            statement = statement.bind(*item_type);
        }
        for library_id in library_ids {
            statement = statement.bind(library_id);
        }
        if let Some(series_id) = series_id {
            statement = statement.bind(series_id);
        }
        statement
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_progress_items(
        &self,
        user_id: &str,
        library_ids: &[String],
        item_types: &[&str],
        series_id: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        if library_ids.is_empty() || item_types.is_empty() {
            return Ok(Vec::new());
        }
        let item_type_placeholders = std::iter::repeat_n("?", item_types.len())
            .collect::<Vec<_>>()
            .join(", ");
        let library_placeholders = std::iter::repeat_n("?", library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let series_predicate = series_id.map(|_| " AND mi.series_id = ?").unwrap_or("");
        let query = format!(
            "SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                    mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                    mi.title, mi.sort_title, mi.original_title, mi.overview,
                    mi.production_year, mi.rating, mi.rating_source, mi.runtime_ticks,
                    mi.added_at, mi.updated_at,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                     ORDER BY image_index LIMIT 1) AS poster_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                     ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'THUMB'
                     ORDER BY image_index LIMIT 1) AS thumb_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'LOGO'
                     ORDER BY image_index LIMIT 1) AS logo_image_tag,
                    (SELECT fe.relative_path FROM filesystem_entries fe
                     WHERE fe.id = ms.filesystem_entry_id) AS source_relative_path,
                    ms.id AS source_id, ms.source_kind, ms.container, ms.size, ms.external_url,
                    ms.edition_name, ms.quality_label,
                    ms.bitrate, ms.duration_ticks, ms.is_default, ms.probe_status,
                    mt.id AS stream_id, mt.stream_index, mt.stream_type,
                    mt.codec, mt.language, mt.title AS stream_title,
                    mt.details_json AS stream_details_json,
                    mt.is_external AS stream_is_external,
                    mt.is_default AS stream_is_default,
                    mt.is_forced AS stream_is_forced
             FROM (
                 -- Page over items first: the source/stream join would otherwise consume page
                 -- slots for multi-source items and repeat or skip entries between pages.
                 SELECT mi.id AS item_id,
                        us.last_played_at AS progress_order
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 JOIN user_item_state us ON us.item_id = mi.id AND us.user_id = ?
                 WHERE mi.item_type IN ({item_type_placeholders}) AND mi.removed_at IS NULL{CATALOG_VISIBLE_PREDICATE}
                   AND us.is_played = 0 AND us.position_ticks > 0
                   AND mi.library_id IN ({library_placeholders}){series_predicate}
                 ORDER BY us.last_played_at DESC, mi.series_id, mi.season_number,
                          mi.episode_number, mi.id
                 LIMIT ? OFFSET ?
             ) page
             JOIN media_items mi ON mi.id = page.item_id
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             LEFT JOIN media_sources ms
               ON ms.item_id = mi.id
              AND EXISTS (
                  SELECT 1 FROM filesystem_entries fe
                  WHERE fe.id = ms.filesystem_entry_id AND fe.is_missing = 0
              )
             LEFT JOIN media_streams mt ON mt.media_source_id = ms.id
             ORDER BY page.progress_order DESC, mi.series_id, mi.season_number,
                      mi.episode_number, mi.id, ms.id, mt.stream_index"
        );
        let mut binds = Vec::with_capacity(item_types.len() + library_ids.len() + 4);
        binds.push(CatalogBind::Text(user_id));
        binds.extend(item_types.iter().copied().map(CatalogBind::Text));
        binds.extend(library_ids.iter().map(|value| CatalogBind::Text(value)));
        if let Some(series_id) = series_id {
            binds.push(CatalogBind::Text(series_id));
        }
        binds.push(CatalogBind::Integer(limit));
        binds.push(CatalogBind::Integer(offset));
        self.fetch_catalog_rows(&query, &binds).await
    }

    pub(crate) async fn list_filtered_catalog_rows(
        &self,
        filter: &CatalogFilterQuery<'_>,
    ) -> Result<(Vec<StoredCatalogRow>, i64), StorageError> {
        if filter.library_ids.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let (where_clause, filter_binds) = catalog_filter_where_clause(filter);
        let (query, list_binds) = filtered_catalog_list_query(filter);
        if !filter.include_total {
            let rows = self.fetch_catalog_rows(&query, &list_binds).await?;
            return Ok((rows, 0));
        }
        let count_query = format!(
            "SELECT COUNT(*) FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             {where_clause}"
        );
        let mut count_statement = self.query_scalar::<i64>(sqlx::AssertSqlSafe(count_query));
        for bind in &filter_binds {
            count_statement = match bind {
                CatalogBind::Text(value) => count_statement.bind(*value),
                CatalogBind::Integer(value) => count_statement.bind(*value),
                CatalogBind::Real(value) => count_statement.bind(*value),
            };
        }
        let total_future = async {
            count_statement
                .fetch_one(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })
        };
        let rows_future = self.fetch_catalog_rows(&query, &list_binds);
        let (total, rows) = tokio::try_join!(total_future, rows_future)?;
        Ok((rows, total))
    }

    #[cfg(test)]
    pub(crate) async fn explain_filtered_catalog_rows(
        &self,
        filter: &CatalogFilterQuery<'_>,
    ) -> Result<Vec<String>, StorageError> {
        let (query, binds) = filtered_catalog_list_query(filter);
        let explain = format!("EXPLAIN QUERY PLAN {query}");
        let mut statement = self.query(sqlx::AssertSqlSafe(explain));
        for bind in &binds {
            statement = match bind {
                CatalogBind::Text(value) => statement.bind(*value),
                CatalogBind::Integer(value) => statement.bind(*value),
                CatalogBind::Real(value) => statement.bind(*value),
            };
        }
        let rows = statement
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        rows.into_iter()
            .map(|row| {
                row.try_get::<String, _>("detail")
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })
            })
            .collect()
    }

    pub(crate) async fn list_catalog_rows(
        &self,
        library_id: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        let (query, binds) = match library_id {
            Some(library_id) => (
                format!(
                    "SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                        mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                        mi.title, mi.sort_title, mi.original_title, mi.overview,
                        mi.production_year, mi.rating, mi.rating_source, mi.runtime_ticks,
                        mi.added_at, mi.updated_at,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                         ORDER BY image_index LIMIT 1) AS poster_image_tag,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                         ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'THUMB'
                         ORDER BY image_index LIMIT 1) AS thumb_image_tag,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'LOGO'
                         ORDER BY image_index LIMIT 1) AS logo_image_tag,
                        (SELECT fe.relative_path FROM filesystem_entries fe
                         WHERE fe.id = ms.filesystem_entry_id) AS source_relative_path,
                        ms.id AS source_id, ms.source_kind, ms.container, ms.size, ms.external_url,
                        ms.edition_name, ms.quality_label,
                        ms.bitrate, ms.duration_ticks, ms.is_default, ms.probe_status,
                        mt.id AS stream_id, mt.stream_index, mt.stream_type,
                        mt.codec, mt.language, mt.title AS stream_title,
                        mt.details_json AS stream_details_json,
                        mt.is_external AS stream_is_external,
                        mt.is_default AS stream_is_default,
                        mt.is_forced AS stream_is_forced
                 FROM (
                     SELECT mi.id, mi.library_id, mi.item_type,
                            mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                            mi.title, mi.sort_title,
                            mi.original_title, mi.overview, mi.production_year,
                            mi.rating, mi.rating_source, mi.runtime_ticks,
                            mi.added_at, mi.updated_at
                     FROM media_items mi
                     JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 WHERE mi.library_id = ? AND mi.item_type <> 'FOLDER'
                   AND mi.removed_at IS NULL{CATALOG_VISIBLE_PREDICATE}
                     ORDER BY mi.sort_title, mi.id
                     LIMIT ? OFFSET ?
                 ) mi
                 LEFT JOIN media_sources ms
                   ON ms.item_id = mi.id
                  AND EXISTS (
                      SELECT 1 FROM filesystem_entries fe
                      WHERE fe.id = ms.filesystem_entry_id AND fe.is_missing = 0
                  )
                 LEFT JOIN media_streams mt ON mt.media_source_id = ms.id
                 ORDER BY mi.sort_title, mi.id, ms.id, mt.stream_index"
                ),
                vec![
                    CatalogBind::Text(library_id),
                    CatalogBind::Integer(limit),
                    CatalogBind::Integer(offset),
                ],
            ),
            None => (
                format!(
                    "SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                        mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                        mi.title, mi.sort_title, mi.original_title, mi.overview,
                        mi.production_year, mi.rating, mi.rating_source, mi.runtime_ticks,
                        mi.added_at, mi.updated_at,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                         ORDER BY image_index LIMIT 1) AS poster_image_tag,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                         ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'THUMB'
                         ORDER BY image_index LIMIT 1) AS thumb_image_tag,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'LOGO'
                         ORDER BY image_index LIMIT 1) AS logo_image_tag,
                        (SELECT fe.relative_path FROM filesystem_entries fe
                         WHERE fe.id = ms.filesystem_entry_id) AS source_relative_path,
                        ms.id AS source_id, ms.source_kind, ms.container, ms.size, ms.external_url,
                        ms.edition_name, ms.quality_label,
                        ms.bitrate, ms.duration_ticks, ms.is_default, ms.probe_status,
                        mt.id AS stream_id, mt.stream_index, mt.stream_type,
                        mt.codec, mt.language, mt.title AS stream_title,
                        mt.details_json AS stream_details_json,
                        mt.is_external AS stream_is_external,
                        mt.is_default AS stream_is_default,
                        mt.is_forced AS stream_is_forced
                 FROM (
                     SELECT mi.id, mi.library_id, mi.item_type,
                            mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                            mi.title, mi.sort_title,
                            mi.original_title, mi.overview, mi.production_year,
                            mi.rating, mi.rating_source, mi.runtime_ticks,
                            mi.added_at, mi.updated_at
                     FROM media_items mi
                     JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                     WHERE mi.item_type <> 'FOLDER'
                       AND mi.removed_at IS NULL{CATALOG_VISIBLE_PREDICATE}
                     ORDER BY mi.sort_title, mi.id
                     LIMIT ? OFFSET ?
                 ) mi
                 LEFT JOIN media_sources ms
                   ON ms.item_id = mi.id
                  AND EXISTS (
                      SELECT 1 FROM filesystem_entries fe
                      WHERE fe.id = ms.filesystem_entry_id AND fe.is_missing = 0
                  )
                 LEFT JOIN media_streams mt ON mt.media_source_id = ms.id
                 ORDER BY mi.sort_title, mi.id, ms.id, mt.stream_index"
                ),
                vec![CatalogBind::Integer(limit), CatalogBind::Integer(offset)],
            ),
        };
        self.fetch_catalog_rows(&query, &binds).await
    }

    pub(crate) async fn find_catalog_rows(
        &self,
        item_id: &str,
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        let query = format!(
            "SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                    mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                    mi.title, mi.sort_title, mi.original_title, mi.overview,
                    mi.production_year, mi.rating, mi.rating_source, mi.runtime_ticks,
                    mi.added_at, mi.updated_at,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                     ORDER BY image_index LIMIT 1) AS poster_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                     ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'THUMB'
                     ORDER BY image_index LIMIT 1) AS thumb_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'LOGO'
                     ORDER BY image_index LIMIT 1) AS logo_image_tag,
                    (SELECT fe.relative_path FROM filesystem_entries fe
                     WHERE fe.id = ms.filesystem_entry_id) AS source_relative_path,
                    ms.id AS source_id, ms.source_kind, ms.container, ms.size, ms.external_url,
                    ms.edition_name, ms.quality_label,
                    ms.bitrate, ms.duration_ticks, ms.is_default, ms.probe_status,
                    mt.id AS stream_id, mt.stream_index, mt.stream_type,
                    mt.codec, mt.language, mt.title AS stream_title,
                    mt.details_json AS stream_details_json,
                    mt.is_external AS stream_is_external,
                    mt.is_default AS stream_is_default,
                    mt.is_forced AS stream_is_forced
             FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             LEFT JOIN media_sources ms
               ON ms.item_id = mi.id
              AND EXISTS (
                  SELECT 1 FROM filesystem_entries fe
                  WHERE fe.id = ms.filesystem_entry_id AND fe.is_missing = 0
              )
             LEFT JOIN media_streams mt ON mt.media_source_id = ms.id
             WHERE mi.id = ? AND mi.removed_at IS NULL{CATALOG_VISIBLE_PREDICATE}
             ORDER BY mi.sort_title, mi.id, ms.id, mt.stream_index",
        );
        self.fetch_catalog_rows(&query, &[CatalogBind::Text(item_id)])
            .await
    }

    pub(crate) async fn list_catalog_rows_by_ids(
        &self,
        item_ids: &[String],
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        let mut rows = Vec::new();
        for item_ids in item_ids.chunks(500) {
            if item_ids.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", item_ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                        mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                        mi.title, mi.sort_title, mi.original_title, mi.overview,
                        mi.production_year, mi.rating, mi.rating_source, mi.runtime_ticks,
                        mi.added_at, mi.updated_at,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                         ORDER BY image_index LIMIT 1) AS poster_image_tag,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                         ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'THUMB'
                         ORDER BY image_index LIMIT 1) AS thumb_image_tag,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'LOGO'
                         ORDER BY image_index LIMIT 1) AS logo_image_tag,
                        (SELECT fe.relative_path FROM filesystem_entries fe
                         WHERE fe.id = ms.filesystem_entry_id) AS source_relative_path,
                        ms.id AS source_id, ms.source_kind, ms.container, ms.size, ms.external_url,
                        ms.edition_name, ms.quality_label,
                        ms.bitrate, ms.duration_ticks, ms.is_default, ms.probe_status,
                        mt.id AS stream_id, mt.stream_index, mt.stream_type,
                        mt.codec, mt.language, mt.title AS stream_title,
                        mt.details_json AS stream_details_json,
                        mt.is_external AS stream_is_external,
                        mt.is_default AS stream_is_default,
                        mt.is_forced AS stream_is_forced
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 LEFT JOIN media_sources ms
                   ON ms.item_id = mi.id
                  AND EXISTS (
                      SELECT 1 FROM filesystem_entries fe
                      WHERE fe.id = ms.filesystem_entry_id AND fe.is_missing = 0
                  )
                 LEFT JOIN media_streams mt ON mt.media_source_id = ms.id
                 WHERE mi.id IN ({placeholders})
                   AND mi.removed_at IS NULL{CATALOG_VISIBLE_PREDICATE}
                 ORDER BY mi.sort_title, mi.id, ms.id, mt.stream_index"
            );
            let binds = item_ids
                .iter()
                .map(|item_id| CatalogBind::Text(item_id))
                .collect::<Vec<_>>();
            rows.extend(self.fetch_catalog_rows(&query, &binds).await?);
        }
        Ok(rows)
    }

    pub(crate) async fn find_catalog_detail(
        &self,
        item_id: &str,
    ) -> Result<Option<StoredCatalogDetail>, StorageError> {
        Ok(self
            .list_catalog_details_by_ids(&[item_id.to_owned()])
            .await?
            .remove(item_id))
    }

    pub(crate) async fn list_catalog_details_by_ids(
        &self,
        item_ids: &[String],
    ) -> Result<HashMap<String, StoredCatalogDetail>, StorageError> {
        let mut details = HashMap::with_capacity(item_ids.len());
        for item_ids in item_ids.chunks(500) {
            if item_ids.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", item_ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT mi.id AS item_id, mi.premiere_date, mi.last_air_date,
                        mi.status, mi.original_language, mi.provider_ids_json,
                        (SELECT series.title
                         FROM media_items series
                         WHERE series.id = CASE
                             WHEN mi.item_type = 'SEASON' THEN mi.parent_id
                             ELSE mi.series_id
                         END
                           AND series.removed_at IS NULL) AS series_name,
                        (SELECT COUNT(*) FROM media_items child
                         WHERE child.parent_id = mi.id AND child.item_type = 'SEASON'
                           AND child.removed_at IS NULL) AS season_count,
                        (SELECT COUNT(DISTINCT CASE
                                    WHEN mi.item_type = 'SERIES' THEN
                                        COALESCE(CAST(child.season_number AS TEXT), '') || ':' ||
                                        COALESCE(CAST(child.episode_number AS TEXT), child.id)
                                    ELSE COALESCE(CAST(child.episode_number AS TEXT), child.id)
                                END)
                         FROM media_items child
                         WHERE child.item_type = 'EPISODE'
                           AND child.removed_at IS NULL
                           AND ((mi.item_type = 'SERIES' AND child.series_id = mi.id)
                             OR (mi.item_type = 'SEASON' AND child.parent_id = mi.id))) AS episode_count
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 WHERE mi.id IN ({placeholders}) AND mi.removed_at IS NULL"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for item_id in item_ids {
                statement = statement.bind(item_id);
            }
            let batch =
                statement
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
            for row in batch {
                let item_id: String = row.get("item_id");
                details.insert(
                    item_id.clone(),
                    StoredCatalogDetail {
                        premiere_date: row.get("premiere_date"),
                        last_air_date: row.get("last_air_date"),
                        status: row.get("status"),
                        original_language: row.get("original_language"),
                        provider_ids_json: row.get("provider_ids_json"),
                        series_name: row.get("series_name"),
                        season_count: row.get("season_count"),
                        episode_count: row.get("episode_count"),
                    },
                );
            }
        }
        Ok(details)
    }

    pub(crate) async fn list_media_chapters_by_source_ids(
        &self,
        source_ids: &[String],
    ) -> Result<HashMap<String, Vec<StoredMediaChapter>>, StorageError> {
        let mut chapters = HashMap::<String, Vec<StoredMediaChapter>>::new();
        for source_ids in source_ids.chunks(500) {
            if source_ids.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", source_ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT mc.media_source_id, mc.start_position_ticks, mc.name, mc.marker_type, mc.chapter_index
                 FROM media_chapters mc
                 JOIN media_sources ms ON ms.id = mc.media_source_id
                 JOIN media_items mi ON mi.id = ms.item_id
                 JOIN libraries l ON l.id = mi.library_id
                 WHERE mc.media_source_id IN ({placeholders})
                   AND mi.item_type = 'EPISODE'
                   AND l.chapter_source_id = mc.provider_id
                 ORDER BY media_source_id, start_position_ticks,
                          CASE marker_type
                              WHEN 'INTRO_START' THEN 0
                              WHEN 'INTRO_END' THEN 1
                              WHEN 'CREDITS_START' THEN 2
                              ELSE 99
                          END,
                          chapter_index, mc.id"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for source_id in source_ids {
                statement = statement.bind(source_id);
            }
            let rows =
                statement
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
            for row in rows {
                let source_id: String = row.get("media_source_id");
                chapters
                    .entry(source_id.clone())
                    .or_default()
                    .push(StoredMediaChapter {
                        source_id,
                        start_position_ticks: row.get("start_position_ticks"),
                        name: row.get("name"),
                        marker_type: row.get("marker_type"),
                        chapter_index: row.get("chapter_index"),
                    });
            }
        }
        for source_ids in source_ids.chunks(500) {
            if source_ids.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", source_ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT media_source_id, start_position_ticks, name, chapter_index
                 FROM media_info_chapters WHERE media_source_id IN ({placeholders})
                 ORDER BY media_source_id, start_position_ticks, chapter_index, id"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for source_id in source_ids {
                statement = statement.bind(source_id);
            }
            let rows =
                statement
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
            for row in rows {
                let source_id: String = row.get("media_source_id");
                chapters
                    .entry(source_id.clone())
                    .or_default()
                    .push(StoredMediaChapter {
                        source_id,
                        start_position_ticks: row.get("start_position_ticks"),
                        name: row.get("name"),
                        marker_type: "CHAPTER".to_owned(),
                        chapter_index: row.get("chapter_index"),
                    });
            }
        }
        Ok(chapters)
    }

    async fn fetch_catalog_rows(
        &self,
        query: &str,
        binds: &[CatalogBind<'_>],
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        self.fetch_catalog_rows_internal(query, binds, false)
            .await
            .map(|(rows, _)| rows)
    }

    async fn fetch_catalog_rows_with_total(
        &self,
        query: &str,
        binds: &[CatalogBind<'_>],
    ) -> Result<(Vec<StoredCatalogRow>, i64), StorageError> {
        self.fetch_catalog_rows_internal(query, binds, true).await
    }

    async fn fetch_catalog_rows_internal(
        &self,
        query: &str,
        binds: &[CatalogBind<'_>],
        read_resume_total: bool,
    ) -> Result<(Vec<StoredCatalogRow>, i64), StorageError> {
        let mut statement = self.query(sqlx::AssertSqlSafe(query));
        for bind in binds {
            statement = match bind {
                CatalogBind::Text(value) => statement.bind(*value),
                CatalogBind::Integer(value) => statement.bind(*value),
                CatalogBind::Real(value) => statement.bind(*value),
            };
        }
        statement
            .fetch_all(&self.pool)
            .await
            .map(|rows| {
                let total = if read_resume_total {
                    rows.first()
                        .and_then(|row| row.try_get("resume_total").ok())
                        .unwrap_or(0)
                } else {
                    0
                };
                let rows = rows
                    .into_iter()
                    .map(|row| StoredCatalogRow {
                        item_id: row.get("item_id"),
                        library_id: row.get("library_id"),
                        item_type: row.get("item_type"),
                        parent_id: row.get("parent_id"),
                        series_id: row.get("series_id"),
                        series_name: row.try_get("series_name").ok().flatten(),
                        season_number: row.get("season_number"),
                        episode_number: row.get("episode_number"),
                        title: row.get("title"),
                        sort_title: row.get("sort_title"),
                        original_title: row.get("original_title"),
                        overview: row.get("overview"),
                        production_year: row.get("production_year"),
                        rating: row.get("rating"),
                        rating_source: row.get("rating_source"),
                        runtime_ticks: row.get("runtime_ticks"),
                        added_at: row.get("added_at"),
                        updated_at: row.get("updated_at"),
                        poster_image_tag: row.get("poster_image_tag"),
                        fanart_image_tag: row.get("fanart_image_tag"),
                        thumb_image_tag: row.get("thumb_image_tag"),
                        logo_image_tag: row.get("logo_image_tag"),
                        source_id: row.get("source_id"),
                        source_relative_path: row.try_get("source_relative_path").ok().flatten(),
                        source_kind: row.get("source_kind"),
                        container: row.get("container"),
                        size: row.get("size"),
                        external_url: row.get("external_url"),
                        edition_name: row.get("edition_name"),
                        quality_label: row.get("quality_label"),
                        bitrate: row.get("bitrate"),
                        duration_ticks: row.get("duration_ticks"),
                        is_default: row
                            .get::<Option<i64>, _>("is_default")
                            .map(|value| value != 0),
                        probe_status: row.get("probe_status"),
                        stream_id: row.get("stream_id"),
                        stream_index: row.get("stream_index"),
                        stream_type: row.get("stream_type"),
                        codec: row.get("codec"),
                        language: row.get("language"),
                        stream_title: row.get("stream_title"),
                        stream_details_json: row.get("stream_details_json"),
                        stream_is_external: row
                            .get::<Option<i64>, _>("stream_is_external")
                            .map(|value| value != 0),
                        stream_is_default: row
                            .get::<Option<i64>, _>("stream_is_default")
                            .map(|value| value != 0),
                        stream_is_forced: row
                            .get::<Option<i64>, _>("stream_is_forced")
                            .map(|value| value != 0),
                    })
                    .collect();
                (rows, total)
            })
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn insert_media_item(
        &self,
        item: NewMediaItem<'_>,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title,
                original_title, production_year, provider_ids_json, identification_status
            ) VALUES (?, ?, 'MOVIE', ?, ?, ?, ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(item.id)
        .bind(item.library_id)
        .bind(item.title)
        .bind(item.sort_title)
        .bind(item.original_title)
        .bind(item.production_year)
        .bind(item.provider_ids_json)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn update_local_provider_ids_if_empty(
        &self,
        item_id: &str,
        provider_ids_json: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE media_items
             SET provider_ids_json = ?, updated_at = unixepoch()
             WHERE id = ? AND (provider_ids_json IS NULL OR provider_ids_json = '{}')",
        )
        .bind(provider_ids_json)
        .bind(item_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn repair_local_nfo_defaults(
        &self,
        item_id: &str,
        provider_ids: &BTreeMap<String, String>,
        premiere_date: Option<&str>,
    ) -> Result<(), StorageError> {
        if provider_ids.is_empty() && premiere_date.is_none() {
            return Ok(());
        }
        self.commit_local_nfo_state_batch(
            &[],
            &[LocalNfoDefaultsRepair {
                item_id,
                provider_ids,
                premiere_date,
            }],
        )
        .await
    }

    pub(crate) async fn update_local_provider_ids_for_identity_if_empty(
        &self,
        identity_key: &str,
        provider_ids_json: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE media_items
             SET provider_ids_json = ?, updated_at = unixepoch()
             WHERE identity_key = ? AND item_type = 'SERIES'
               AND removed_at IS NULL
               AND (provider_ids_json IS NULL OR provider_ids_json = '{}')",
        )
        .bind(provider_ids_json)
        .bind(identity_key)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn insert_media_source(
        &self,
        source: NewMediaSource<'_>,
    ) -> Result<(), StorageError> {
        let is_strm = source.source_kind == "STRM_URL";
        self.query(
            "INSERT INTO media_sources (
                id, item_id, source_kind, filesystem_entry_id,
                edition_name, quality_label, container, size,
                external_url, strm_target_kind, is_default, probe_status
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
                CASE WHEN ? = 'STRM_URL' THEN 'NOT_CONFIGURED' ELSE 'PENDING' END)",
        )
        .bind(source.id)
        .bind(source.item_id)
        .bind(source.source_kind)
        .bind(source.filesystem_entry_id)
        .bind(source.edition_name)
        .bind(source.quality_label)
        .bind(source.container)
        .bind(source.size)
        .bind(source.external_url)
        .bind(source.strm_target_kind)
        .bind(database_flag(source.is_default))
        .bind(source.source_kind)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        if is_strm {
            self.query(
                "UPDATE media_items
                 SET poster_fallback_required = 1
                 WHERE id = ?
                   AND NOT EXISTS (
                       SELECT 1 FROM item_images
                       WHERE item_id = media_items.id
                         AND image_type IN ('POSTER', 'THUMB')
                         AND image_index = 0
                   )",
            )
            .bind(source.item_id)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        Ok(())
    }

    pub(crate) async fn list_media_sources_for_library_page(
        &self,
        library_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<StoredMediaSourcePath>, StorageError> {
        self.query(
            "SELECT ms.id AS source_id, ms.item_id, ms.probe_status,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE mi.library_id = ? AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
               AND fe.is_missing = 0
             ORDER BY ms.item_id, fe.relative_path
             LIMIT ? OFFSET ?",
        )
        .bind(library_id)
        .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
        .bind(offset.max(0))
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredMediaSourcePath {
                    source_id: row.get("source_id"),
                    item_id: row.get("item_id"),
                    probe_status: row.get("probe_status"),
                    root_path: row.get("root_path"),
                    relative_path: row.get("relative_path"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_strm_source_by_absolute_path(
        &self,
        absolute_path: &str,
    ) -> Result<Option<StoredMediaSourcePath>, StorageError> {
        self.query(
            "SELECT ms.id AS source_id, ms.item_id, ms.probe_status,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_sources ms
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             JOIN media_items mi ON mi.id = ms.item_id
             WHERE ms.source_kind = 'STRM_URL'
               AND fe.is_missing = 0
               AND mi.removed_at IS NULL
               AND ? = lr.canonical_path || '/' || fe.relative_path
             LIMIT 2",
        )
        .bind(absolute_path)
        .fetch_all(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
        .and_then(|rows| {
            if rows.len() > 1 {
                return Err(StorageError::Conflict(
                    "multiple STRM sources match the absolute path".to_owned(),
                ));
            }
            Ok(rows.into_iter().next().map(|row| StoredMediaSourcePath {
                source_id: row.get("source_id"),
                item_id: row.get("item_id"),
                probe_status: row.get("probe_status"),
                root_path: row.get("root_path"),
                relative_path: row.get("relative_path"),
            }))
        })
    }

    pub(crate) async fn find_strm_source_by_path_suffix(
        &self,
        external_path: &str,
    ) -> Result<Option<StoredMediaSourcePath>, StorageError> {
        self.query(
            "SELECT ms.id AS source_id, ms.item_id, ms.probe_status,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_sources ms
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             JOIN media_items mi ON mi.id = ms.item_id
             WHERE ms.source_kind = 'STRM_URL'
               AND fe.is_missing = 0
               AND mi.removed_at IS NULL
               AND ? LIKE '%/' || fe.relative_path
             LIMIT 2",
        )
        .bind(external_path)
        .fetch_all(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
        .and_then(|rows| {
            if rows.len() > 1 {
                return Err(StorageError::Conflict(
                    "multiple STRM sources match the external path suffix".to_owned(),
                ));
            }
            Ok(rows.into_iter().next().map(|row| StoredMediaSourcePath {
                source_id: row.get("source_id"),
                item_id: row.get("item_id"),
                probe_status: row.get("probe_status"),
                root_path: row.get("root_path"),
                relative_path: row.get("relative_path"),
            }))
        })
    }

    pub(crate) async fn find_strm_source_by_emby_id(
        &self,
        id: &str,
    ) -> Result<Option<StoredMediaSourcePath>, StorageError> {
        self.query(
            "SELECT ms.id AS source_id, ms.item_id, ms.probe_status,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_sources ms
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             JOIN media_items mi ON mi.id = ms.item_id
             WHERE ms.source_kind = 'STRM_URL'
               AND fe.is_missing = 0
               AND mi.removed_at IS NULL
               AND (ms.id = ? OR ms.item_id = ?)
             LIMIT 2",
        )
        .bind(id)
        .bind(id)
        .fetch_all(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
        .and_then(|rows| {
            if rows.len() > 1 {
                return Err(StorageError::Conflict(
                    "multiple STRM sources match the Emby id".to_owned(),
                ));
            }
            Ok(rows.into_iter().next().map(|row| StoredMediaSourcePath {
                source_id: row.get("source_id"),
                item_id: row.get("item_id"),
                probe_status: row.get("probe_status"),
                root_path: row.get("root_path"),
                relative_path: row.get("relative_path"),
            }))
        })
    }

    pub(crate) async fn list_movie_metadata_sources_page(
        &self,
        library_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<StoredMediaSourcePath>, StorageError> {
        self.query(
            "SELECT ms.id AS source_id, ms.item_id, ms.probe_status,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE mi.library_id = ? AND mi.item_type = 'MOVIE'
               AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
               AND fe.is_missing = 0
             ORDER BY ms.item_id, fe.relative_path
             LIMIT ? OFFSET ?",
        )
        .bind(library_id)
        .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
        .bind(offset.max(0))
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredMediaSourcePath {
                    source_id: row.get("source_id"),
                    item_id: row.get("item_id"),
                    probe_status: row.get("probe_status"),
                    root_path: row.get("root_path"),
                    relative_path: row.get("relative_path"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_movie_metadata_sources_for_incremental_scan(
        &self,
        scan_job_id: &str,
    ) -> Result<Vec<StoredMediaSourcePath>, StorageError> {
        self.query(sqlx::AssertSqlSafe(
            "SELECT ms.id AS source_id, ms.item_id, ms.probe_status,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE mi.item_type = 'MOVIE'
               AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
               AND fe.is_missing = 0
               AND mi.removed_at IS NULL
               AND {SCOPE}
             ORDER BY ms.item_id, fe.relative_path"
                .replace("{SCOPE}", incremental_scan_scope_sql(self.backend)),
        ))
        .bind(scan_job_id)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredMediaSourcePath {
                    source_id: row.get("source_id"),
                    item_id: row.get("item_id"),
                    probe_status: row.get("probe_status"),
                    root_path: row.get("root_path"),
                    relative_path: row.get("relative_path"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_home_video_metadata_sources_for_incremental_scan(
        &self,
        scan_job_id: &str,
    ) -> Result<Vec<StoredMediaSourcePath>, StorageError> {
        self.query(sqlx::AssertSqlSafe(
            "SELECT ms.id AS source_id, ms.item_id, ms.probe_status,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE mi.item_type = 'VIDEO'
               AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
               AND fe.is_missing = 0
               AND mi.removed_at IS NULL
               AND {SCOPE}
             ORDER BY ms.item_id, fe.relative_path"
                .replace("{SCOPE}", incremental_scan_scope_sql(self.backend)),
        ))
        .bind(scan_job_id)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredMediaSourcePath {
                    source_id: row.get("source_id"),
                    item_id: row.get("item_id"),
                    probe_status: row.get("probe_status"),
                    root_path: row.get("root_path"),
                    relative_path: row.get("relative_path"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_danmaku_source_for_item(
        &self,
        item_id: &str,
        source_id: Option<&str>,
    ) -> Result<Option<StoredDanmakuSource>, StorageError> {
        let row = match source_id {
            Some(source_id) => {
                self.query(
                    "SELECT ms.id AS source_id, ms.item_id,
                            mi.item_type, mi.season_number, mi.episode_number,
                            mi.title, mi.original_title,
                            series.title AS series_title,
                            series.original_title AS series_original_title,
                            lr.canonical_path AS root_path, fe.relative_path
                     FROM media_sources ms
                     JOIN media_items mi ON mi.id = ms.item_id
                     LEFT JOIN media_items series ON series.id = mi.series_id
                     JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                     JOIN library_roots lr ON lr.id = fe.library_root_id
                     WHERE mi.id = ? AND ms.id = ?
                       AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
                       AND fe.is_missing = 0
                     LIMIT 1",
                )
                .bind(item_id)
                .bind(source_id)
                .fetch_optional(&self.pool)
                .await
            }
            None => {
                self.query(
                    "SELECT ms.id AS source_id, ms.item_id,
                            mi.item_type, mi.season_number, mi.episode_number,
                            mi.title, mi.original_title,
                            series.title AS series_title,
                            series.original_title AS series_original_title,
                            lr.canonical_path AS root_path, fe.relative_path
                     FROM media_sources ms
                     JOIN media_items mi ON mi.id = ms.item_id
                     LEFT JOIN media_items series ON series.id = mi.series_id
                     JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                     JOIN library_roots lr ON lr.id = fe.library_root_id
                     WHERE mi.id = ? AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
                       AND fe.is_missing = 0
                     ORDER BY ms.is_default DESC, fe.relative_path
                     LIMIT 1",
                )
                .bind(item_id)
                .fetch_optional(&self.pool)
                .await
            }
        }
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        Ok(row.map(|row| StoredDanmakuSource {
            source_id: row.get("source_id"),
            root_path: row.get("root_path"),
            relative_path: row.get("relative_path"),
            item_type: row.get("item_type"),
            season_number: row.get("season_number"),
            episode_number: row.get("episode_number"),
            title: row.get("title"),
            original_title: row.get("original_title"),
            series_title: row.get("series_title"),
            series_original_title: row.get("series_original_title"),
        }))
    }

    pub(crate) async fn find_registered_danmaku_source_for_item(
        &self,
        item_id: &str,
        source_id: Option<&str>,
    ) -> Result<Option<StoredDanmakuSource>, StorageError> {
        let row = match source_id {
            Some(source_id) => {
                self.query(
                    "SELECT ms.id AS source_id, ms.item_id,
                            mi.item_type, mi.season_number, mi.episode_number,
                            mi.title, mi.original_title,
                            series.title AS series_title,
                            series.original_title AS series_original_title,
                            lr.canonical_path AS root_path, fe.relative_path
                     FROM media_sources ms
                     JOIN danmaku_tracks dt ON dt.media_source_id = ms.id
                     JOIN media_items mi ON mi.id = ms.item_id
                     LEFT JOIN media_items series ON series.id = mi.series_id
                     JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                     JOIN library_roots lr ON lr.id = fe.library_root_id
                     WHERE mi.id = ? AND ms.id = ?
                       AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
                       AND fe.is_missing = 0 AND dt.status = 'READY'
                     LIMIT 1",
                )
                .bind(item_id)
                .bind(source_id)
                .fetch_optional(&self.pool)
                .await
            }
            None => {
                self.query(
                    "SELECT ms.id AS source_id, ms.item_id,
                            mi.item_type, mi.season_number, mi.episode_number,
                            mi.title, mi.original_title,
                            series.title AS series_title,
                            series.original_title AS series_original_title,
                            lr.canonical_path AS root_path, fe.relative_path
                     FROM media_sources ms
                     JOIN danmaku_tracks dt ON dt.media_source_id = ms.id
                     JOIN media_items mi ON mi.id = ms.item_id
                     LEFT JOIN media_items series ON series.id = mi.series_id
                     JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                     JOIN library_roots lr ON lr.id = fe.library_root_id
                     WHERE mi.id = ? AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
                       AND fe.is_missing = 0 AND dt.status = 'READY'
                     ORDER BY ms.is_default DESC, fe.relative_path
                     LIMIT 1",
                )
                .bind(item_id)
                .fetch_optional(&self.pool)
                .await
            }
        }
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        Ok(row.map(|row| StoredDanmakuSource {
            source_id: row.get("source_id"),
            root_path: row.get("root_path"),
            relative_path: row.get("relative_path"),
            item_type: row.get("item_type"),
            season_number: row.get("season_number"),
            episode_number: row.get("episode_number"),
            title: row.get("title"),
            original_title: row.get("original_title"),
            series_title: row.get("series_title"),
            series_original_title: row.get("series_original_title"),
        }))
    }

    pub(crate) async fn upsert_danmaku_track(
        &self,
        track: NewDanmakuTrack<'_>,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO danmaku_tracks (
                id, media_source_id, relative_path, format, provider,
                provider_anime_id, provider_episode_id, fingerprint,
                status, error_code, last_checked_at
             ) VALUES (?, ?, ?, 'XML', ?, ?, ?, ?, ?, ?, unixepoch())
             ON CONFLICT(media_source_id) DO UPDATE SET
                relative_path = excluded.relative_path,
                provider = excluded.provider,
                provider_anime_id = excluded.provider_anime_id,
                provider_episode_id = excluded.provider_episode_id,
                fingerprint = excluded.fingerprint,
                status = excluded.status,
                error_code = excluded.error_code,
                last_checked_at = unixepoch(),
                updated_at = unixepoch()",
        )
        .bind(track.id)
        .bind(track.media_source_id)
        .bind(track.relative_path)
        .bind(track.provider)
        .bind(track.provider_anime_id)
        .bind(track.provider_episode_id)
        .bind(track.fingerprint)
        .bind(track.status)
        .bind(track.error_code)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn apply_scan_danmaku_file_change(
        &self,
        job_id: &str,
        change: &StoredScanJobPath,
        tracks: &[NewDanmakuTrack<'_>],
        missing_source_ids: &[String],
    ) -> Result<bool, StorageError> {
        let Some(revision) = change.file_change_revision else {
            return Err(StorageError::Conflict(
                "scan file change is missing its revision".to_owned(),
            ));
        };
        if !matches!(change.change_kind.as_str(), "ADD" | "CHANGE" | "REMOVE")
            || tracks
                .iter()
                .any(|track| track.relative_path != change.relative_path || track.status != "READY")
            || (change.change_kind == "REMOVE" && !tracks.is_empty())
            || (change.change_kind != "REMOVE" && !missing_source_ids.is_empty())
        {
            return Err(StorageError::Conflict(
                "invalid danmaku file update".to_owned(),
            ));
        }

        let mut transaction = self.begin_scan_write_transaction().await?;
        let current = self
            .query_scalar::<i64>(
                "SELECT 1 FROM scan_job_file_changes
                 WHERE job_id = ? AND library_root_id = ? AND relative_path = ?
                   AND revision = ? AND processing_state = 'RUNNING'
                 LIMIT 1",
            )
            .bind(job_id)
            .bind(&change.library_root_id)
            .bind(&change.relative_path)
            .bind(revision)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .is_some();
        if !current {
            return Ok(false);
        }

        if change.change_kind == "REMOVE" {
            for source_id in missing_source_ids {
                self.query(
                    "UPDATE danmaku_tracks
                     SET status = 'MISSING', fingerprint = NULL, error_code = NULL,
                         last_checked_at = unixepoch(), updated_at = unixepoch()
                     WHERE media_source_id = ? AND relative_path = ?",
                )
                .bind(source_id)
                .bind(&change.relative_path)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            }
        } else {
            for track in tracks {
                self.query(
                    "INSERT INTO danmaku_tracks (
                        id, media_source_id, relative_path, format, provider,
                        provider_anime_id, provider_episode_id, fingerprint,
                        status, error_code, last_checked_at
                     ) VALUES (?, ?, ?, 'XML', ?, ?, ?, ?, ?, ?, unixepoch())
                     ON CONFLICT(media_source_id) DO UPDATE SET
                        relative_path = excluded.relative_path,
                        provider = excluded.provider,
                        provider_anime_id = excluded.provider_anime_id,
                        provider_episode_id = excluded.provider_episode_id,
                        fingerprint = excluded.fingerprint,
                        status = excluded.status,
                        error_code = excluded.error_code,
                        last_checked_at = unixepoch(),
                        updated_at = unixepoch()",
                )
                .bind(track.id)
                .bind(track.media_source_id)
                .bind(track.relative_path)
                .bind(track.provider)
                .bind(track.provider_anime_id)
                .bind(track.provider_episode_id)
                .bind(track.fingerprint)
                .bind(track.status)
                .bind(track.error_code)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            }
        }

        let completed = self
            .query(
                "UPDATE scan_job_file_changes
                 SET processing_state = 'DONE', error = NULL, updated_at = unixepoch()
                 WHERE job_id = ? AND library_root_id = ? AND relative_path = ?
                   AND revision = ? AND processing_state = 'RUNNING'",
            )
            .bind(job_id)
            .bind(&change.library_root_id)
            .bind(&change.relative_path)
            .bind(revision)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .rows_affected()
            == 1;
        if !completed {
            return Ok(false);
        }
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(true)
    }

    pub(crate) async fn create_danmaku_match_job(
        &self,
        job: NewDanmakuMatchJob<'_>,
    ) -> Result<(), StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.query(
            "INSERT INTO danmaku_match_jobs (
                id, library_id, status, overwrite, concurrency, total_count
             ) VALUES (?, ?, 'PENDING', ?, ?, ?)",
        )
        .bind(job.id)
        .bind(job.library_id)
        .bind(database_flag(job.overwrite))
        .bind(job.concurrency)
        .bind(0_i64)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        let inserted = self
            .query(
                "INSERT INTO danmaku_match_job_items (
                    id, job_id, media_source_id, status
                 )
                 SELECT ? || ':' || ms.id, ?, ms.id, 'PENDING'
                 FROM media_sources ms
                 JOIN media_items mi ON mi.id = ms.item_id
                 JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                 WHERE mi.library_id = ?
                   AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
                   AND fe.is_missing = 0",
            )
            .bind(job.id)
            .bind(job.id)
            .bind(job.library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let total_count = i64::try_from(inserted.rows_affected()).unwrap_or(i64::MAX);
        self.query(
            "UPDATE danmaku_match_jobs
             SET total_count = ?, updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(total_count)
        .bind(job.id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn has_active_danmaku_match_jobs(
        &self,
        library_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(
                SELECT 1 FROM danmaku_match_jobs
                WHERE library_id = ? AND status IN ('PENDING', 'RUNNING')
            ) THEN 1 ELSE 0 END",
        )
        .bind(library_id)
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_danmaku_match_job(
        &self,
        id: &str,
    ) -> Result<Option<StoredDanmakuMatchJob>, StorageError> {
        self.query(
            "SELECT id, library_id, status, overwrite, concurrency,
                    total_count, processed_count, success_count,
                    skipped_count, failed_count, cancel_requested, error,
                    created_at, started_at, finished_at
             FROM danmaku_match_jobs WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_danmaku_match_job))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_danmaku_match_jobs(
        &self,
        status: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredDanmakuMatchJob>, StorageError> {
        let rows = if let Some(status) = status {
            self.query(
                "SELECT id, library_id, status, overwrite, concurrency,
                        total_count, processed_count, success_count,
                        skipped_count, failed_count, cancel_requested, error,
                        created_at, started_at, finished_at
                 FROM danmaku_match_jobs
                 WHERE status = ?
                 ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
            )
            .bind(status)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        } else {
            self.query(
                "SELECT id, library_id, status, overwrite, concurrency,
                        total_count, processed_count, success_count,
                        skipped_count, failed_count, cancel_requested, error,
                        created_at, started_at, finished_at
                 FROM danmaku_match_jobs
                 ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
            )
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        };
        rows.map(|rows| rows.into_iter().map(stored_danmaku_match_job).collect())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_active_danmaku_match_job_ids(
        &self,
    ) -> Result<Vec<String>, StorageError> {
        self.query_scalar(
            "SELECT id FROM danmaku_match_jobs
             WHERE status IN ('PENDING', 'RUNNING')
             ORDER BY created_at, id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn claim_danmaku_match_job(&self, id: &str) -> Result<bool, StorageError> {
        self.query(
            "UPDATE danmaku_match_jobs
             SET status = 'RUNNING',
                 started_at = COALESCE(started_at, unixepoch()),
                 updated_at = unixepoch()
             WHERE id = ? AND status = 'PENDING'",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn reset_running_danmaku_match_items(
        &self,
        job_id: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE danmaku_match_job_items
             SET status = 'PENDING', updated_at = unixepoch()
             WHERE job_id = ? AND status = 'RUNNING'",
        )
        .bind(job_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn cancel_pending_danmaku_match_items(
        &self,
        job_id: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE danmaku_match_job_items
             SET status = 'CANCELLED', updated_at = unixepoch()
             WHERE job_id = ? AND status = 'PENDING'",
        )
        .bind(job_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_pending_danmaku_match_items(
        &self,
        job_id: &str,
        limit: i64,
    ) -> Result<Vec<StoredDanmakuMatchItem>, StorageError> {
        self.query(
            "SELECT ji.id, ji.media_source_id,
                    mi.item_type, mi.season_number, mi.episode_number,
                    mi.title, mi.original_title,
                    series.title AS series_title,
                    series.original_title AS series_original_title,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM danmaku_match_job_items ji
             LEFT JOIN media_sources ms
               ON ms.id = ji.media_source_id
              AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
             LEFT JOIN media_items mi ON mi.id = ms.item_id
             LEFT JOIN media_items series ON series.id = mi.series_id
             LEFT JOIN filesystem_entries fe
               ON fe.id = ms.filesystem_entry_id AND fe.is_missing = 0
             LEFT JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE ji.job_id = ? AND ji.status = 'PENDING'
             ORDER BY ji.id
             LIMIT ?",
        )
        .bind(job_id)
        .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredDanmakuMatchItem {
                    id: row.get("id"),
                    media_source_id: row.get("media_source_id"),
                    root_path: row.get("root_path"),
                    relative_path: row.get("relative_path"),
                    item_type: row.get("item_type"),
                    season_number: row.get("season_number"),
                    episode_number: row.get("episode_number"),
                    title: row.get("title"),
                    original_title: row.get("original_title"),
                    series_title: row.get("series_title"),
                    series_original_title: row.get("series_original_title"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn claim_danmaku_match_item(&self, id: &str) -> Result<bool, StorageError> {
        self.query(
            "UPDATE danmaku_match_job_items
             SET status = 'RUNNING', attempts = attempts + 1, updated_at = unixepoch()
             WHERE id = ? AND status = 'PENDING'",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn finish_danmaku_match_item(
        &self,
        id: &str,
        status: &str,
        provider_anime_id: Option<&str>,
        provider_episode_id: Option<&str>,
        error_code: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE danmaku_match_job_items
             SET status = ?, provider_anime_id = ?, provider_episode_id = ?,
                 error_code = ?, error_message = ?, updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(status)
        .bind(provider_anime_id)
        .bind(provider_episode_id)
        .bind(error_code)
        .bind(error_message)
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn increment_danmaku_match_progress(
        &self,
        job_id: &str,
        success: bool,
        skipped: bool,
        failed: bool,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE danmaku_match_jobs
             SET processed_count = processed_count + 1,
                 success_count = success_count + ?,
                 skipped_count = skipped_count + ?,
                 failed_count = failed_count + ?,
                 updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(i64::from(success))
        .bind(i64::from(skipped))
        .bind(i64::from(failed))
        .bind(job_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn danmaku_match_job_cancel_requested(
        &self,
        id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar("SELECT cancel_requested FROM danmaku_match_jobs WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .map(|value: i64| value != 0)
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn request_danmaku_match_job_cancel(
        &self,
        id: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE danmaku_match_jobs
             SET cancel_requested = 1, updated_at = unixepoch()
             WHERE id = ? AND status IN ('PENDING', 'RUNNING')",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn finish_danmaku_match_job(
        &self,
        id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE danmaku_match_jobs
             SET status = CASE WHEN cancel_requested = 1 THEN 'CANCELLED' ELSE ? END,
                 error = CASE WHEN cancel_requested = 1 THEN NULL ELSE ? END,
                 finished_at = unixepoch(), updated_at = unixepoch()
             WHERE id = ? AND status IN ('PENDING', 'RUNNING')",
        )
        .bind(status)
        .bind(error)
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_local_thumbnail_sources_for_library_page(
        &self,
        library_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<StoredThumbnailSource>, StorageError> {
        self.query(
            "SELECT ms.item_id, lr.canonical_path AS root_path, fe.relative_path,
                    l.media_strategy_json AS library_media_strategy_json, l.scraper_id
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN libraries l ON l.id = mi.library_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE mi.library_id = ? AND ms.source_kind = 'LOCAL_FILE'
               AND fe.is_missing = 0
             ORDER BY ms.item_id, ms.is_default DESC, ms.id
             LIMIT ? OFFSET ?",
        )
        .bind(library_id)
        .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
        .bind(offset.max(0))
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredThumbnailSource {
                    item_id: row.get("item_id"),
                    root_path: row.get("root_path"),
                    relative_path: row.get("relative_path"),
                    library_media_strategy_json: row.get("library_media_strategy_json"),
                    scraper_id: row.get("scraper_id"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_local_thumbnail_sources_for_incremental_scan_page(
        &self,
        scan_job_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<StoredThumbnailSource>, StorageError> {
        self.query(sqlx::AssertSqlSafe(
            "SELECT ms.item_id, lr.canonical_path AS root_path, fe.relative_path,
                    l.media_strategy_json AS library_media_strategy_json, l.scraper_id
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN libraries l ON l.id = mi.library_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE ms.source_kind = 'LOCAL_FILE'
               AND fe.is_missing = 0
               AND mi.removed_at IS NULL
               AND {SCOPE}
             ORDER BY ms.item_id, ms.is_default DESC, ms.id
             LIMIT ? OFFSET ?"
                .replace("{SCOPE}", incremental_scan_scope_sql(self.backend)),
        ))
        .bind(scan_job_id)
        .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
        .bind(offset.max(0))
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredThumbnailSource {
                    item_id: row.get("item_id"),
                    root_path: row.get("root_path"),
                    relative_path: row.get("relative_path"),
                    library_media_strategy_json: row.get("library_media_strategy_json"),
                    scraper_id: row.get("scraper_id"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_scan_job_thumbnail_sources_page(
        &self,
        job_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<StoredThumbnailSource>, StorageError> {
        self.query(
            "SELECT t.item_id, lr.canonical_path AS root_path, fe.relative_path,
                    l.media_strategy_json AS library_media_strategy_json, l.scraper_id
             FROM scan_job_targets t
             JOIN media_items mi ON mi.id = t.item_id
             JOIN libraries l ON l.id = mi.library_id
             JOIN media_sources ms ON ms.id = (
                 SELECT preferred.id FROM media_sources preferred
                 JOIN filesystem_entries preferred_fe
                   ON preferred_fe.id = preferred.filesystem_entry_id
                 WHERE preferred.item_id = t.item_id
                   AND preferred.source_kind = 'LOCAL_FILE'
                   AND preferred_fe.is_missing = 0
                 ORDER BY preferred.is_default DESC, preferred.id
                 LIMIT 1
             )
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE t.job_id = ? AND t.target_type = 'ITEM'
               AND t.thumbnail_state = 'PENDING'
               AND fe.is_missing = 0
             ORDER BY t.target_id
             LIMIT ? OFFSET ?",
        )
        .bind(job_id)
        .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
        .bind(offset.max(0))
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredThumbnailSource {
                    item_id: row.get("item_id"),
                    root_path: row.get("root_path"),
                    relative_path: row.get("relative_path"),
                    library_media_strategy_json: row.get("library_media_strategy_json"),
                    scraper_id: row.get("scraper_id"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_strm_media_sources_for_library_page(
        &self,
        library_id: &str,
        after_source_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<StoredStrmMediaSource>, StorageError> {
        let rows = if let Some(after_source_id) = after_source_id {
            self.query(
                "SELECT ms.id AS source_id, ms.item_id, ms.external_url,
                        mi.poster_fallback_required,
                        CASE WHEN EXISTS (
                            SELECT 1 FROM media_streams mt
                            WHERE mt.media_source_id = ms.id
                        ) OR ms.duration_ticks IS NOT NULL
                            OR ms.bitrate IS NOT NULL
                            OR (ms.container IS NOT NULL AND lower(ms.container) <> 'strm')
                        THEN 1 ELSE 0 END AS has_media_info,
                        CASE WHEN EXISTS (
                            SELECT 1 FROM item_images screenshot
                            WHERE screenshot.item_id = ms.item_id
                              AND screenshot.image_index = 0
                              AND screenshot.source = 'STRM_FFMPEG'
                              AND screenshot.image_type = 'POSTER'
                        ) AND EXISTS (
                            SELECT 1 FROM item_images screenshot
                            WHERE screenshot.item_id = ms.item_id
                              AND screenshot.image_index = 0
                              AND screenshot.source = 'STRM_FFMPEG'
                              AND screenshot.image_type = 'THUMB'
                        ) THEN 1 ELSE 0 END AS has_strm_thumbnail,
                        lr.canonical_path AS root_path, fe.relative_path,
                        ii.local_path AS thumbnail_path
                 FROM media_sources ms
                 JOIN media_items mi ON mi.id = ms.item_id
                 JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                 JOIN library_roots lr ON lr.id = fe.library_root_id
                 LEFT JOIN item_images ii
                   ON ii.item_id = ms.item_id AND ii.image_type = 'THUMB'
                  AND ii.image_index = 0
                 WHERE mi.library_id = ? AND ms.source_kind = 'STRM_URL'
                   AND fe.is_missing = 0 AND ms.id > ?
                 ORDER BY ms.id, fe.relative_path
                 LIMIT ?",
            )
            .bind(library_id)
            .bind(after_source_id)
            .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
            .fetch_all(&self.pool)
            .await
        } else {
            self.query(
                "SELECT ms.id AS source_id, ms.item_id, ms.external_url,
                        mi.poster_fallback_required,
                        CASE WHEN EXISTS (
                            SELECT 1 FROM media_streams mt
                            WHERE mt.media_source_id = ms.id
                        ) OR ms.duration_ticks IS NOT NULL
                            OR ms.bitrate IS NOT NULL
                            OR (ms.container IS NOT NULL AND lower(ms.container) <> 'strm')
                        THEN 1 ELSE 0 END AS has_media_info,
                        CASE WHEN EXISTS (
                            SELECT 1 FROM item_images screenshot
                            WHERE screenshot.item_id = ms.item_id
                              AND screenshot.image_index = 0
                              AND screenshot.source = 'STRM_FFMPEG'
                              AND screenshot.image_type = 'POSTER'
                        ) AND EXISTS (
                            SELECT 1 FROM item_images screenshot
                            WHERE screenshot.item_id = ms.item_id
                              AND screenshot.image_index = 0
                              AND screenshot.source = 'STRM_FFMPEG'
                              AND screenshot.image_type = 'THUMB'
                        ) THEN 1 ELSE 0 END AS has_strm_thumbnail,
                        lr.canonical_path AS root_path, fe.relative_path,
                        ii.local_path AS thumbnail_path
                 FROM media_sources ms
                 JOIN media_items mi ON mi.id = ms.item_id
                 JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                 JOIN library_roots lr ON lr.id = fe.library_root_id
                 LEFT JOIN item_images ii
                   ON ii.item_id = ms.item_id AND ii.image_type = 'THUMB'
                  AND ii.image_index = 0
                 WHERE mi.library_id = ? AND ms.source_kind = 'STRM_URL'
                   AND fe.is_missing = 0
                 ORDER BY ms.id, fe.relative_path
                 LIMIT ?",
            )
            .bind(library_id)
            .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
            .fetch_all(&self.pool)
            .await
        };
        rows.map(|rows| {
            rows.into_iter()
                .map(|row| StoredStrmMediaSource {
                    source_id: row.get("source_id"),
                    item_id: row.get("item_id"),
                    poster_fallback_required: row.get::<i64, _>("poster_fallback_required") != 0,
                    has_media_info: row.get::<i64, _>("has_media_info") != 0,
                    external_url: row.get("external_url"),
                    root_path: row.get("root_path"),
                    relative_path: row.get("relative_path"),
                    thumbnail_path: row.get("thumbnail_path"),
                    has_strm_thumbnail: row.get::<i64, _>("has_strm_thumbnail") != 0,
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_strm_media_source_for_library(
        &self,
        library_id: &str,
        source_id: &str,
    ) -> Result<Option<StoredStrmMediaSource>, StorageError> {
        self.query(
            "SELECT ms.id AS source_id, ms.item_id, ms.external_url,
                    mi.poster_fallback_required,
                    CASE WHEN EXISTS (
                        SELECT 1 FROM media_streams mt
                        WHERE mt.media_source_id = ms.id
                    ) OR ms.duration_ticks IS NOT NULL
                        OR ms.bitrate IS NOT NULL
                        OR (ms.container IS NOT NULL AND lower(ms.container) <> 'strm')
                    THEN 1 ELSE 0 END AS has_media_info,
                    CASE WHEN EXISTS (
                        SELECT 1 FROM item_images screenshot
                        WHERE screenshot.item_id = ms.item_id
                          AND screenshot.image_index = 0
                          AND screenshot.source = 'STRM_FFMPEG'
                          AND screenshot.image_type = 'POSTER'
                    ) AND EXISTS (
                        SELECT 1 FROM item_images screenshot
                        WHERE screenshot.item_id = ms.item_id
                          AND screenshot.image_index = 0
                          AND screenshot.source = 'STRM_FFMPEG'
                          AND screenshot.image_type = 'THUMB'
                    ) THEN 1 ELSE 0 END AS has_strm_thumbnail,
                    lr.canonical_path AS root_path, fe.relative_path,
                    ii.local_path AS thumbnail_path
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             LEFT JOIN item_images ii
               ON ii.item_id = ms.item_id AND ii.image_type = 'THUMB'
              AND ii.image_index = 0
             WHERE mi.library_id = ? AND mi.removed_at IS NULL
               AND ms.source_kind = 'STRM_URL' AND fe.is_missing = 0
               AND ms.id = ?
             LIMIT 1",
        )
        .bind(library_id)
        .bind(source_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredStrmMediaSource {
                source_id: row.get("source_id"),
                item_id: row.get("item_id"),
                poster_fallback_required: row.get::<i64, _>("poster_fallback_required") != 0,
                has_media_info: row.get::<i64, _>("has_media_info") != 0,
                external_url: row.get("external_url"),
                root_path: row.get("root_path"),
                relative_path: row.get("relative_path"),
                thumbnail_path: row.get("thumbnail_path"),
                has_strm_thumbnail: row.get::<i64, _>("has_strm_thumbnail") != 0,
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_strm_media_source_counts_for_libraries(
        &self,
        library_ids: &[String],
    ) -> Result<HashMap<String, i64>, StorageError> {
        if library_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let mut counts = HashMap::with_capacity(library_ids.len());
        for chunk in library_ids.chunks(100) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let mut query = self.query(sqlx::AssertSqlSafe(format!(
                "SELECT l.id, COUNT(fe.id) AS total_count
                 FROM libraries l
                 LEFT JOIN media_items mi
                   ON mi.library_id = l.id AND mi.removed_at IS NULL
                 LEFT JOIN media_sources ms
                   ON ms.item_id = mi.id AND ms.source_kind = 'STRM_URL'
                 LEFT JOIN filesystem_entries fe
                   ON fe.id = ms.filesystem_entry_id AND fe.is_missing = 0
                 WHERE l.id IN ({placeholders})
                 GROUP BY l.id"
            )));
            for library_id in chunk {
                query = query.bind(library_id);
            }
            for row in query
                .fetch_all(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
            {
                counts.insert(row.get("id"), row.get("total_count"));
            }
        }
        Ok(counts)
    }

    pub(crate) async fn list_strm_media_sources_for_incremental_scan_page(
        &self,
        scan_job_id: &str,
        after_source_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<StoredStrmMediaSource>, StorageError> {
        let rows = if let Some(after_source_id) = after_source_id {
            self.query(sqlx::AssertSqlSafe(
                "SELECT ms.id AS source_id, ms.item_id, ms.external_url,
                        mi.poster_fallback_required,
                        CASE WHEN EXISTS (
                            SELECT 1 FROM media_streams mt
                            WHERE mt.media_source_id = ms.id
                        ) OR ms.duration_ticks IS NOT NULL
                            OR ms.bitrate IS NOT NULL
                            OR (ms.container IS NOT NULL AND lower(ms.container) <> 'strm')
                        THEN 1 ELSE 0 END AS has_media_info,
                        CASE WHEN EXISTS (
                            SELECT 1 FROM item_images screenshot
                            WHERE screenshot.item_id = ms.item_id
                              AND screenshot.image_index = 0
                              AND screenshot.source = 'STRM_FFMPEG'
                              AND screenshot.image_type = 'POSTER'
                        ) AND EXISTS (
                            SELECT 1 FROM item_images screenshot
                            WHERE screenshot.item_id = ms.item_id
                              AND screenshot.image_index = 0
                              AND screenshot.source = 'STRM_FFMPEG'
                              AND screenshot.image_type = 'THUMB'
                        ) THEN 1 ELSE 0 END AS has_strm_thumbnail,
                        lr.canonical_path AS root_path, fe.relative_path,
                        ii.local_path AS thumbnail_path
                 FROM media_sources ms
                 JOIN media_items mi ON mi.id = ms.item_id
                 JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                 JOIN library_roots lr ON lr.id = fe.library_root_id
                 LEFT JOIN item_images ii
                   ON ii.item_id = ms.item_id AND ii.image_type = 'THUMB'
                  AND ii.image_index = 0
                 WHERE ms.source_kind = 'STRM_URL'
                   AND fe.is_missing = 0 AND mi.removed_at IS NULL
                   AND ms.id > ? AND {SCOPE}
                 ORDER BY ms.id, fe.relative_path
                 LIMIT ?"
                    .replace("{SCOPE}", incremental_scan_scope_sql(self.backend)),
            ))
            .bind(after_source_id)
            .bind(scan_job_id)
            .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
            .fetch_all(&self.pool)
            .await
        } else {
            self.query(sqlx::AssertSqlSafe(
                "SELECT ms.id AS source_id, ms.item_id, ms.external_url,
                        mi.poster_fallback_required,
                        CASE WHEN EXISTS (
                            SELECT 1 FROM media_streams mt
                            WHERE mt.media_source_id = ms.id
                        ) OR ms.duration_ticks IS NOT NULL
                            OR ms.bitrate IS NOT NULL
                            OR (ms.container IS NOT NULL AND lower(ms.container) <> 'strm')
                        THEN 1 ELSE 0 END AS has_media_info,
                        CASE WHEN EXISTS (
                            SELECT 1 FROM item_images screenshot
                            WHERE screenshot.item_id = ms.item_id
                              AND screenshot.image_index = 0
                              AND screenshot.source = 'STRM_FFMPEG'
                              AND screenshot.image_type = 'POSTER'
                        ) AND EXISTS (
                            SELECT 1 FROM item_images screenshot
                            WHERE screenshot.item_id = ms.item_id
                              AND screenshot.image_index = 0
                              AND screenshot.source = 'STRM_FFMPEG'
                              AND screenshot.image_type = 'THUMB'
                        ) THEN 1 ELSE 0 END AS has_strm_thumbnail,
                        lr.canonical_path AS root_path, fe.relative_path,
                        ii.local_path AS thumbnail_path
                 FROM media_sources ms
                 JOIN media_items mi ON mi.id = ms.item_id
                 JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                 JOIN library_roots lr ON lr.id = fe.library_root_id
                 LEFT JOIN item_images ii
                   ON ii.item_id = ms.item_id AND ii.image_type = 'THUMB'
                  AND ii.image_index = 0
                 WHERE ms.source_kind = 'STRM_URL'
                   AND fe.is_missing = 0 AND mi.removed_at IS NULL
                   AND {SCOPE}
                 ORDER BY ms.id, fe.relative_path
                 LIMIT ?"
                    .replace("{SCOPE}", incremental_scan_scope_sql(self.backend)),
            ))
            .bind(scan_job_id)
            .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
            .fetch_all(&self.pool)
            .await
        };
        rows.map(|rows| {
            rows.into_iter()
                .map(|row| StoredStrmMediaSource {
                    source_id: row.get("source_id"),
                    item_id: row.get("item_id"),
                    poster_fallback_required: row.get::<i64, _>("poster_fallback_required") != 0,
                    has_media_info: row.get::<i64, _>("has_media_info") != 0,
                    external_url: row.get("external_url"),
                    root_path: row.get("root_path"),
                    relative_path: row.get("relative_path"),
                    thumbnail_path: row.get("thumbnail_path"),
                    has_strm_thumbnail: row.get::<i64, _>("has_strm_thumbnail") != 0,
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn count_strm_media_sources_for_incremental_scan(
        &self,
        scan_job_id: &str,
    ) -> Result<i64, StorageError> {
        let has_directory_scope: i64 = self
            .query_scalar(
                // CASE keeps a portable 0/1 integer: PostgreSQL returns BOOLEAN for a bare EXISTS.
                "SELECT CAST(CASE WHEN EXISTS (
                    SELECT 1
                    FROM scan_job_paths sjp
                    LEFT JOIN filesystem_entries path_entry
                      ON path_entry.library_root_id = sjp.library_root_id
                     AND path_entry.relative_path = sjp.relative_path
                    WHERE sjp.job_id = ? AND sjp.processed_at IS NOT NULL
                      AND (sjp.relative_path = '.' OR path_entry.entry_kind = 'DIRECTORY')
                ) THEN 1 ELSE 0 END AS BIGINT)",
            )
            .bind(scan_job_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if has_directory_scope == 0 {
            return self
                .query_scalar(
                    "SELECT COUNT(*)
                     FROM scan_job_paths sjp
                     JOIN filesystem_entries fe
                       ON fe.library_root_id = sjp.library_root_id
                      AND fe.relative_path = sjp.relative_path
                     JOIN media_sources ms ON ms.filesystem_entry_id = fe.id
                     JOIN media_items mi ON mi.id = ms.item_id
                     WHERE sjp.job_id = ? AND sjp.processed_at IS NOT NULL
                       AND ms.source_kind = 'STRM_URL'
                       AND fe.is_missing = 0 AND mi.removed_at IS NULL",
                )
                .bind(scan_job_id)
                .fetch_one(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                });
        }

        self.query_scalar(sqlx::AssertSqlSafe(
            "SELECT COUNT(DISTINCT ms.id)
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             WHERE ms.source_kind = 'STRM_URL'
               AND fe.is_missing = 0 AND mi.removed_at IS NULL
               AND {SCOPE}"
                .replace("{SCOPE}", incremental_scan_scope_sql(self.backend)),
        ))
        .bind(scan_job_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_download_source(
        &self,
        item_id: &str,
    ) -> Result<Option<StoredDownloadSource>, StorageError> {
        self.query(
            "SELECT ms.source_kind,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE mi.id = ? AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
               AND fe.is_missing = 0
             ORDER BY ms.is_default DESC, ms.id LIMIT 1",
        )
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredDownloadSource {
                source_kind: row.get("source_kind"),
                root_path: row.get("root_path"),
                relative_path: row.get("relative_path"),
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_metadata_writeback_source_path(
        &self,
        item_id: &str,
    ) -> Result<Option<StoredMediaSourcePath>, StorageError> {
        self.query(
            "SELECT ms.id AS source_id, ms.item_id, ms.probe_status,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE mi.id = ? AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
               AND fe.is_missing = 0
             ORDER BY ms.is_default DESC, ms.id LIMIT 1",
        )
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredMediaSourcePath {
                source_id: row.get("source_id"),
                item_id: row.get("item_id"),
                probe_status: row.get("probe_status"),
                root_path: row.get("root_path"),
                relative_path: row.get("relative_path"),
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_media_item_writeback_contexts_by_ids(
        &self,
        item_ids: &[String],
    ) -> Result<HashMap<String, StoredMediaWritebackContext>, StorageError> {
        let mut contexts = HashMap::with_capacity(item_ids.len());
        for chunk in item_ids.chunks(500) {
            if chunk.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT mi.id AS item_id, mi.item_type,
                        ms.id AS source_id, ms.item_id AS source_item_id,
                        ms.probe_status, lr.canonical_path AS root_path,
                        fe.relative_path
                 FROM media_items mi
                 LEFT JOIN media_sources ms ON ms.id = CASE
                     WHEN mi.item_type IN ('SERIES', 'SEASON') THEN (
                         SELECT episode_source.id
                         FROM media_items episode
                         JOIN media_sources episode_source
                           ON episode_source.item_id = episode.id
                         JOIN filesystem_entries episode_fe
                           ON episode_fe.id = episode_source.filesystem_entry_id
                         WHERE episode.item_type = 'EPISODE'
                           AND (episode.series_id = mi.id OR episode.parent_id = mi.id)
                           AND episode_fe.is_missing = 0
                         ORDER BY episode.id, episode_fe.relative_path
                         LIMIT 1
                     )
                     ELSE (
                         SELECT direct_source.id
                         FROM media_sources direct_source
                         JOIN filesystem_entries direct_fe
                           ON direct_fe.id = direct_source.filesystem_entry_id
                         WHERE direct_source.item_id = mi.id
                           AND direct_source.source_kind IN ('LOCAL_FILE', 'STRM_URL')
                           AND direct_fe.is_missing = 0
                         ORDER BY direct_source.is_default DESC, direct_source.id
                         LIMIT 1
                     )
                 END
                 LEFT JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                 LEFT JOIN library_roots lr ON lr.id = fe.library_root_id
                 WHERE mi.id IN ({placeholders})"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for item_id in chunk {
                statement = statement.bind(item_id);
            }
            let rows =
                statement
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
            for row in rows {
                let source_id = row.try_get::<String, _>("source_id").ok();
                let source = source_id.map(|source_id| StoredMediaSourcePath {
                    source_id,
                    item_id: row.get("source_item_id"),
                    probe_status: row.get("probe_status"),
                    root_path: row.get("root_path"),
                    relative_path: row.get("relative_path"),
                });
                let item_id = row.get::<String, _>("item_id");
                contexts.insert(
                    item_id,
                    StoredMediaWritebackContext {
                        item_type: row.get("item_type"),
                        source,
                    },
                );
            }
        }
        Ok(contexts)
    }

    pub(crate) async fn find_movie_nfo_writeback_context(
        &self,
        item_id: &str,
    ) -> Result<
        Option<(
            String,
            Option<i64>,
            Option<String>,
            Option<i64>,
            Option<StoredMediaSourcePath>,
        )>,
        StorageError,
    > {
        let row = self
            .query(
                "SELECT mi.item_type, mi.season_number,
                        CASE WHEN mi.item_type = 'MOVIE' AND mi.removed_at IS NULL
                             THEN mi.sort_title END AS sort_title,
                        CASE WHEN mi.item_type = 'MOVIE' AND mi.removed_at IS NULL
                             THEN mi.added_at END AS added_at,
                        ms.id AS source_id, ms.item_id AS source_item_id,
                        ms.probe_status, lr.canonical_path AS root_path,
                        fe.relative_path
                 FROM media_items mi
                 LEFT JOIN media_sources ms ON ms.id = CASE
                     WHEN mi.item_type IN ('SERIES', 'SEASON') THEN (
                         SELECT episode_source.id
                         FROM media_items episode
                         JOIN media_sources episode_source
                           ON episode_source.item_id = episode.id
                         JOIN filesystem_entries episode_fe
                           ON episode_fe.id = episode_source.filesystem_entry_id
                         WHERE episode.item_type = 'EPISODE'
                           AND (episode.series_id = mi.id OR episode.parent_id = mi.id)
                           AND episode_fe.is_missing = 0
                         ORDER BY episode.id, episode_fe.relative_path
                         LIMIT 1
                     )
                     WHEN mi.item_type IN ('MOVIE', 'EPISODE', 'VIDEO') THEN (
                         SELECT direct_source.id
                         FROM media_sources direct_source
                         JOIN filesystem_entries direct_fe
                           ON direct_fe.id = direct_source.filesystem_entry_id
                         WHERE direct_source.item_id = mi.id
                           AND direct_source.source_kind IN ('LOCAL_FILE', 'STRM_URL')
                           AND direct_fe.is_missing = 0
                         ORDER BY direct_source.is_default DESC, direct_source.id
                         LIMIT 1
                     )
                     ELSE NULL
                 END
                 LEFT JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                 LEFT JOIN library_roots lr ON lr.id = fe.library_root_id
                 WHERE mi.id = ?",
            )
            .bind(item_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(row.map(|row| {
            let source =
                row.try_get::<String, _>("source_id")
                    .ok()
                    .map(|source_id| StoredMediaSourcePath {
                        source_id,
                        item_id: row.get("source_item_id"),
                        probe_status: row.get("probe_status"),
                        root_path: row.get("root_path"),
                        relative_path: row.get("relative_path"),
                    });
            (
                row.get("item_type"),
                row.get("season_number"),
                row.get("sort_title"),
                row.get("added_at"),
                source,
            )
        }))
    }

    pub(crate) async fn list_strm_source_paths_by_ids(
        &self,
        source_ids: &[String],
    ) -> Result<Vec<StoredMediaSourcePath>, StorageError> {
        let mut paths = Vec::new();
        for source_ids in source_ids.chunks(500) {
            let placeholders = std::iter::repeat_n("?", source_ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT ms.id AS source_id, ms.item_id, ms.probe_status,
                        lr.canonical_path AS root_path, fe.relative_path
                 FROM media_sources ms
                 JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                 JOIN library_roots lr ON lr.id = fe.library_root_id
                 WHERE ms.source_kind = 'STRM_URL'
                   AND fe.is_missing = 0
                   AND ms.id IN ({placeholders})
                 ORDER BY ms.item_id, ms.is_default DESC, ms.id"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for source_id in source_ids {
                statement = statement.bind(source_id);
            }
            paths.extend(
                statement
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?
                    .into_iter()
                    .map(|row| StoredMediaSourcePath {
                        source_id: row.get("source_id"),
                        item_id: row.get("item_id"),
                        probe_status: row.get("probe_status"),
                        root_path: row.get("root_path"),
                        relative_path: row.get("relative_path"),
                    }),
            );
        }
        Ok(paths)
    }

    pub(crate) async fn find_authorized_playback_source(
        &self,
        item_id: &str,
        source_id: Option<&str>,
        user_id: &str,
        is_admin: bool,
    ) -> Result<Option<StoredPlaybackSource>, StorageError> {
        // Administrators bypass per-user modes; other principals use the stored explicit mode.
        let access_filter = if is_admin {
            String::new()
        } else {
            format!("AND {}", user_library_access_filter("mi.library_id", false))
        };
        let query = format!(
            "SELECT ms.id AS source_id, ms.source_kind, ms.container, ms.external_url,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE mi.id = ? AND mi.removed_at IS NULL
               {access_filter}
               AND (? IS NULL OR ms.id = ?)
               AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
               AND fe.is_missing = 0
             ORDER BY ms.is_default DESC, ms.id
             LIMIT 1"
        );
        let mut statement = self.query(sqlx::AssertSqlSafe(query));
        statement = statement.bind(item_id);
        if !is_admin {
            statement = statement.bind(user_id);
        }
        statement = statement.bind(source_id).bind(source_id);
        statement
            .fetch_optional(&self.pool)
            .await
            .map(|row| {
                row.map(|row| StoredPlaybackSource {
                    source_id: row.get("source_id"),
                    source_kind: row.get("source_kind"),
                    container: row.get("container"),
                    external_url: row.get("external_url"),
                    root_path: row.get("root_path"),
                    relative_path: row.get("relative_path"),
                })
            })
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn find_download_source_by_id(
        &self,
        item_id: &str,
        source_id: &str,
    ) -> Result<Option<StoredDownloadSource>, StorageError> {
        self.query(
            "SELECT ms.source_kind,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE ms.id = ? AND mi.id = ? AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
               AND fe.is_missing = 0
             LIMIT 1",
        )
        .bind(source_id)
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredDownloadSource {
                source_kind: row.get("source_kind"),
                root_path: row.get("root_path"),
                relative_path: row.get("relative_path"),
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_deletable_media_source_paths(
        &self,
        item_id: &str,
    ) -> Result<Vec<StoredMediaSourcePath>, StorageError> {
        self.query(
            "WITH RECURSIVE descendants(id) AS (
                 SELECT id FROM media_items WHERE id = ?
                 UNION ALL
                 SELECT child.id
                 FROM media_items child
                 JOIN descendants parent ON child.parent_id = parent.id
             )
             SELECT ms.id AS source_id, ms.item_id, ms.probe_status,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_sources ms
             JOIN descendants d ON d.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
             ORDER BY ms.item_id, ms.is_default DESC, ms.id",
        )
        .bind(item_id)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredMediaSourcePath {
                    source_id: row.get("source_id"),
                    item_id: row.get("item_id"),
                    probe_status: row.get("probe_status"),
                    root_path: row.get("root_path"),
                    relative_path: row.get("relative_path"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_deletable_media_source_path_by_id(
        &self,
        item_id: &str,
        source_id: &str,
    ) -> Result<Option<StoredMediaSourcePath>, StorageError> {
        self.query(
            "SELECT ms.id AS source_id, ms.item_id, ms.probe_status,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE ms.id = ? AND mi.id = ?
               AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
             LIMIT 1",
        )
        .bind(source_id)
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredMediaSourcePath {
                source_id: row.get("source_id"),
                item_id: row.get("item_id"),
                probe_status: row.get("probe_status"),
                root_path: row.get("root_path"),
                relative_path: row.get("relative_path"),
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_media_item_kind(
        &self,
        item_id: &str,
    ) -> Result<Option<StoredMediaItemKind>, StorageError> {
        self.query("SELECT item_type, season_number FROM media_items WHERE id = ?")
            .bind(item_id)
            .fetch_optional(&self.pool)
            .await
            .map(|row| {
                row.map(|row| StoredMediaItemKind {
                    item_type: row.get("item_type"),
                    season_number: row.get("season_number"),
                })
            })
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn find_first_episode_source_path(
        &self,
        item_id: &str,
    ) -> Result<Option<StoredMediaSourcePath>, StorageError> {
        self.query(
            "SELECT ms.id AS source_id, episode.id AS item_id, ms.probe_status,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_items episode
             JOIN media_sources ms ON ms.item_id = episode.id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE episode.item_type = 'EPISODE'
               AND (episode.series_id = ? OR episode.parent_id = ?)
               AND fe.is_missing = 0
             ORDER BY episode.id, fe.relative_path LIMIT 1",
        )
        .bind(item_id)
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredMediaSourcePath {
                source_id: row.get("source_id"),
                item_id: row.get("item_id"),
                probe_status: row.get("probe_status"),
                root_path: row.get("root_path"),
                relative_path: row.get("relative_path"),
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn save_media_probe(
        &self,
        update: MediaProbeUpdate<'_>,
    ) -> Result<(), StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.save_media_probe_in_transaction(&mut transaction, &update)
            .await?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn save_media_probe_for_scan_file_change(
        &self,
        job_id: &str,
        change: &StoredScanJobPath,
        updates: &[MediaProbeUpdate<'_>],
        clear_source_ids: &[String],
    ) -> Result<bool, StorageError> {
        let Some(revision) = change.file_change_revision else {
            return Err(StorageError::Conflict(
                "scan file change is missing its revision".to_owned(),
            ));
        };
        if !matches!(change.change_kind.as_str(), "ADD" | "CHANGE" | "REMOVE")
            || (change.change_kind == "REMOVE" && !updates.is_empty())
            || (change.change_kind != "REMOVE" && !clear_source_ids.is_empty())
        {
            return Err(StorageError::Conflict(
                "invalid media info file update".to_owned(),
            ));
        }
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let current = self
            .query_scalar::<i64>(
                "SELECT 1 FROM scan_job_file_changes
                 WHERE job_id = ? AND library_root_id = ? AND relative_path = ?
                   AND revision = ? AND processing_state = 'RUNNING'
                 LIMIT 1",
            )
            .bind(job_id)
            .bind(&change.library_root_id)
            .bind(&change.relative_path)
            .bind(revision)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .is_some();
        if !current {
            return Ok(false);
        }

        for update in updates {
            self.save_media_probe_in_transaction(&mut transaction, update)
                .await?;
            self.query(
                "UPDATE scan_job_targets
                 SET probe_state = 'DONE', error = NULL, updated_at = unixepoch()
                 WHERE job_id = ? AND target_type = 'SOURCE' AND target_id = ?",
            )
            .bind(job_id)
            .bind(update.source_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        for source_id in clear_source_ids {
            self.query(
                "UPDATE media_sources
                 SET container = NULL, size = NULL, duration_ticks = NULL, bitrate = NULL,
                     probe_status = 'NOT_CONFIGURED', probe_error = NULL, updated_at = unixepoch()
                 WHERE id = ? AND source_kind = 'STRM_URL'",
            )
            .bind(source_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
            self.query(
                "UPDATE scan_job_targets
                 SET probe_state = 'SKIPPED', error = NULL, updated_at = unixepoch()
                 WHERE job_id = ? AND target_type = 'SOURCE' AND target_id = ?",
            )
            .bind(job_id)
            .bind(source_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
            self.query("DELETE FROM media_streams WHERE media_source_id = ?")
                .bind(source_id)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            self.query("DELETE FROM media_info_chapters WHERE media_source_id = ?")
                .bind(source_id)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }

        let completed = self
            .query(
                "UPDATE scan_job_file_changes
                 SET processing_state = 'DONE', error = NULL, updated_at = unixepoch()
                 WHERE job_id = ? AND library_root_id = ? AND relative_path = ?
                   AND revision = ? AND processing_state = 'RUNNING'",
            )
            .bind(job_id)
            .bind(&change.library_root_id)
            .bind(&change.relative_path)
            .bind(revision)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .rows_affected()
            == 1;
        if !completed {
            return Ok(false);
        }
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(true)
    }

    async fn save_media_probe_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        update: &MediaProbeUpdate<'_>,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE media_sources
             SET container = CASE
                     WHEN source_kind = 'STRM_URL' THEN COALESCE(?, container)
                     ELSE container
                 END,
                 size = COALESCE(?, size),
                 duration_ticks = ?, bitrate = ?,
                 probe_status = 'READY', probe_error = NULL,
                 updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(update.container)
        .bind(update.source_size)
        .bind(update.duration_ticks)
        .bind(update.bitrate)
        .bind(update.source_id)
        .execute(&mut **transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query("DELETE FROM media_streams WHERE media_source_id = ?")
            .bind(update.source_id)
            .execute(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        for batch in update.streams.chunks(MEDIA_STREAM_INSERT_BATCH_SIZE) {
            let values = std::iter::repeat_n("(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)", batch.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "INSERT INTO media_streams (
                    id, media_source_id, stream_index, stream_type,
                    codec, language, title, details_json, external_path,
                    is_external, is_default, is_forced
                 ) VALUES {values}"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for stream in batch {
                statement = statement
                    .bind(Uuid::now_v7().to_string())
                    .bind(update.source_id)
                    .bind(stream.stream_index)
                    .bind(stream.stream_type)
                    .bind(stream.codec)
                    .bind(stream.language)
                    .bind(stream.title)
                    .bind(stream.details_json)
                    .bind(stream.external_path)
                    .bind(database_flag(stream.is_external))
                    .bind(database_flag(stream.is_default))
                    .bind(database_flag(stream.is_forced));
            }
            statement
                .execute(&mut **transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }
        self.query("DELETE FROM media_info_chapters WHERE media_source_id = ?")
            .bind(update.source_id)
            .execute(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        for chapter in update.chapters {
            self.query(
                "INSERT INTO media_info_chapters (
                    id, media_source_id, start_position_ticks, name, chapter_index
                 ) VALUES (?, ?, ?, ?, ?)",
            )
            .bind(Uuid::now_v7().to_string())
            .bind(update.source_id)
            .bind(chapter.start_position_ticks)
            .bind(&chapter.name)
            .bind(chapter.chapter_index)
            .execute(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        Ok(())
    }

    pub(crate) async fn list_series_metadata_sources_page(
        &self,
        library_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<StoredSeriesMetadataSource>, StorageError> {
        self.query(
            "SELECT series.id AS series_id, season.id AS season_id,
                    episode.id AS episode_id, season.season_number,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_items episode
             JOIN media_items season ON season.id = episode.parent_id
             JOIN media_items series ON series.id = episode.series_id
             JOIN media_sources ms ON ms.item_id = episode.id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE episode.item_type = 'EPISODE'
               AND season.item_type = 'SEASON'
               AND series.item_type = 'SERIES'
               AND episode.library_id = ?
               AND episode.removed_at IS NULL
               AND fe.is_missing = 0
             ORDER BY series.id, season.season_number, episode.id, fe.relative_path
             LIMIT ? OFFSET ?",
        )
        .bind(library_id)
        .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
        .bind(offset.max(0))
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredSeriesMetadataSource {
                    series_id: row.get("series_id"),
                    season_id: row.get("season_id"),
                    episode_id: row.get("episode_id"),
                    season_number: row.get("season_number"),
                    root_path: row.get("root_path"),
                    relative_path: row.get("relative_path"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_series_metadata_sources_for_incremental_scan(
        &self,
        scan_job_id: &str,
    ) -> Result<Vec<StoredSeriesMetadataSource>, StorageError> {
        self.query(sqlx::AssertSqlSafe(
            "SELECT series.id AS series_id, season.id AS season_id,
                    episode.id AS episode_id, season.season_number,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_items episode
             JOIN media_items season ON season.id = episode.parent_id
             JOIN media_items series ON series.id = episode.series_id
             JOIN media_sources ms ON ms.item_id = episode.id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE episode.item_type = 'EPISODE'
               AND season.item_type = 'SEASON'
               AND series.item_type = 'SERIES'
               AND episode.removed_at IS NULL
               AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
               AND fe.is_missing = 0
               AND {SCOPE}
             ORDER BY series.id, season.season_number, episode.id, fe.relative_path"
                .replace("{SCOPE}", incremental_scan_scope_sql(self.backend)),
        ))
        .bind(scan_job_id)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredSeriesMetadataSource {
                    series_id: row.get("series_id"),
                    season_id: row.get("season_id"),
                    episode_id: row.get("episode_id"),
                    season_number: row.get("season_number"),
                    root_path: row.get("root_path"),
                    relative_path: row.get("relative_path"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_chapter_detection_sources_page(
        &self,
        library_id: &str,
        plugin_id: &str,
        after_source_id: Option<&str>,
        limit: i64,
        require_fingerprint: bool,
        supported_media_source_kinds: &[String],
    ) -> Result<Vec<StoredChapterDetectionSource>, StorageError> {
        let source_kind_placeholders = if supported_media_source_kinds.is_empty() {
            "NULL".to_owned()
        } else {
            std::iter::repeat_n("?", supported_media_source_kinds.len())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let query_with_fingerprint = format!(
            "SELECT ms.id AS source_id, episode.id AS item_id, season.id AS season_id,
                    fe.fingerprint, fe.relative_path, fe.size, fe.modified_at,
                    fe.device, fe.inode, ms.duration_ticks,
                    episode.provider_ids_json,
                    series.provider_ids_json AS series_provider_ids_json,
                    episode.season_number, episode.episode_number,
                    states.input_fingerprint AS state_input_fingerprint,
                    states.status AS state_status,
                    states.last_checked_at AS state_last_checked_at,
                    states.next_retry_at AS state_next_retry_at,
                    states.intro_fingerprint AS state_intro_fingerprint,
                    states.credits_fingerprint AS state_credits_fingerprint
             FROM media_sources ms
             JOIN media_items episode ON episode.id = ms.item_id
             JOIN media_items season ON season.id = episode.parent_id
             JOIN media_items series ON series.id = episode.series_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             LEFT JOIN chapter_detection_source_states states
               ON states.source_id = ms.id AND states.plugin_id = ?
             WHERE episode.library_id = ?
               AND episode.item_type = 'EPISODE'
               AND season.item_type = 'SEASON'
               AND series.item_type = 'SERIES'
               AND ms.source_kind IN ({source_kind_placeholders})
               AND episode.removed_at IS NULL
               AND fe.is_missing = 0
               AND fe.fingerprint IS NOT NULL
               AND (ms.is_default = 1 OR NOT EXISTS (
                   SELECT 1 FROM media_sources preferred
                   WHERE preferred.item_id = episode.id AND preferred.is_default = 1
               ))
               AND (? IS NULL OR ms.id > ?)
               ORDER BY ms.id
             LIMIT ?"
        );
        let query_without_fingerprint = format!(
            "SELECT ms.id AS source_id, episode.id AS item_id, season.id AS season_id,
                    fe.fingerprint, fe.relative_path, fe.size, fe.modified_at,
                    fe.device, fe.inode, ms.duration_ticks,
                    episode.provider_ids_json,
                    series.provider_ids_json AS series_provider_ids_json,
                    episode.season_number, episode.episode_number,
                    states.input_fingerprint AS state_input_fingerprint,
                    states.status AS state_status,
                    states.last_checked_at AS state_last_checked_at,
                    states.next_retry_at AS state_next_retry_at,
                    states.intro_fingerprint AS state_intro_fingerprint,
                    states.credits_fingerprint AS state_credits_fingerprint
             FROM media_sources ms
             JOIN media_items episode ON episode.id = ms.item_id
             JOIN media_items season ON season.id = episode.parent_id
             JOIN media_items series ON series.id = episode.series_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             LEFT JOIN chapter_detection_source_states states
               ON states.source_id = ms.id AND states.plugin_id = ?
             WHERE episode.library_id = ?
               AND episode.item_type = 'EPISODE'
               AND season.item_type = 'SEASON'
               AND series.item_type = 'SERIES'
               AND ms.source_kind IN ({source_kind_placeholders})
               AND episode.removed_at IS NULL
               AND fe.is_missing = 0
               AND (ms.is_default = 1 OR NOT EXISTS (
                   SELECT 1 FROM media_sources preferred
                   WHERE preferred.item_id = episode.id AND preferred.is_default = 1
               ))
               AND (? IS NULL OR ms.id > ?)
               ORDER BY ms.id
             LIMIT ?"
        );
        let limit = limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE);
        let rows = if require_fingerprint {
            let mut query = self
                .query(sqlx::AssertSqlSafe(query_with_fingerprint))
                .bind(plugin_id)
                .bind(library_id);
            for source_kind in supported_media_source_kinds {
                query = query.bind(source_kind);
            }
            query
                .bind(after_source_id)
                .bind(after_source_id)
                .bind(limit)
                .fetch_all(&self.pool)
                .await
        } else {
            let mut query = self
                .query(sqlx::AssertSqlSafe(query_without_fingerprint))
                .bind(plugin_id)
                .bind(library_id);
            for source_kind in supported_media_source_kinds {
                query = query.bind(source_kind);
            }
            query
                .bind(after_source_id)
                .bind(after_source_id)
                .bind(limit)
                .fetch_all(&self.pool)
                .await
        };
        rows.map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredChapterDetectionSource {
                    source_id: row.get("source_id"),
                    item_id: row.get("item_id"),
                    season_id: row.get("season_id"),
                    fingerprint: row.get("fingerprint"),
                    relative_path: row.get("relative_path"),
                    size: row.get("size"),
                    modified_at: row.get("modified_at"),
                    device: row.get("device"),
                    inode: row.get("inode"),
                    duration_ticks: row.get("duration_ticks"),
                    provider_ids_json: row.get("provider_ids_json"),
                    series_provider_ids_json: row.get("series_provider_ids_json"),
                    season_number: row.get("season_number"),
                    episode_number: row.get("episode_number"),
                    state: row.get::<Option<String>, _>("state_status").map(|status| {
                        StoredChapterDetectionSourceState {
                            input_fingerprint: row.get("state_input_fingerprint"),
                            status,
                            last_checked_at: row.get("state_last_checked_at"),
                            next_retry_at: row.get("state_next_retry_at"),
                            intro_fingerprint: row.get("state_intro_fingerprint"),
                            credits_fingerprint: row.get("state_credits_fingerprint"),
                        }
                    }),
                })
                .collect()
        })
    }

    pub(crate) async fn create_chapter_detection_job(
        &self,
        job: NewChapterDetectionJob<'_>,
    ) -> Result<bool, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if !self
            .lock_scan_fingerprint_migration_state(&mut transaction, job.library_id)
            .await?
        {
            transaction
                .rollback()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(false);
        }
        let result = self
            .query(
                "INSERT INTO chapter_detection_jobs (
                    id, library_id, plugin_id, status, concurrency,
                    intro_window_seconds, credits_window_seconds, match_threshold, total_count
                 ) VALUES (?, ?, ?, 'PENDING', ?, ?, ?, ?, ?)
                 ON CONFLICT DO NOTHING",
            )
            .bind(job.id)
            .bind(job.library_id)
            .bind(job.plugin_id)
            .bind(job.concurrency)
            .bind(job.intro_window_seconds)
            .bind(job.credits_window_seconds)
            .bind(job.match_threshold)
            .bind(job.total_count)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn insert_chapter_detection_job_items(
        &self,
        items: &[NewChapterDetectionJobItem<'_>],
    ) -> Result<(), StorageError> {
        if items.is_empty() {
            return Ok(());
        }
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        for batch in items.chunks(CHAPTER_DETECTION_JOB_ITEM_INSERT_BATCH_SIZE) {
            let values = std::iter::repeat_n("(?, ?, ?, ?, ?, ?, ?, 'PENDING')", batch.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "INSERT INTO chapter_detection_job_items (
                    job_id, source_id, item_id, season_id, source_fingerprint,
                    input_fingerprint, is_context, status
                 ) VALUES {values}"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for item in batch {
                statement = statement
                    .bind(item.job_id)
                    .bind(item.source_id)
                    .bind(item.item_id)
                    .bind(item.season_id)
                    .bind(item.source_fingerprint)
                    .bind(item.input_fingerprint)
                    .bind(database_flag(item.is_context));
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

    pub(crate) async fn delete_chapter_detection_job(&self, id: &str) -> Result<(), StorageError> {
        self.query("DELETE FROM chapter_detection_jobs WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn apply_chapter_detection_outcomes(
        &self,
        job_id: &str,
        plugin_id: &str,
        updates: &[ChapterDetectionOutcomeUpdate],
        cursor: Option<&str>,
        processed_count: i64,
    ) -> Result<(), StorageError> {
        if updates.is_empty() {
            return Ok(());
        }
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;

        let status_cases = std::iter::repeat_n("WHEN ? THEN ?", updates.len())
            .collect::<Vec<_>>()
            .join(" ");
        let error_cases = std::iter::repeat_n("WHEN ? THEN ?", updates.len())
            .collect::<Vec<_>>()
            .join(" ");
        let source_placeholders = std::iter::repeat_n("?", updates.len())
            .collect::<Vec<_>>()
            .join(", ");
        let status_query = format!(
            "UPDATE chapter_detection_job_items
             SET status = CASE source_id {status_cases} ELSE status END,
                 error = CASE source_id {error_cases} ELSE error END,
                 updated_at = unixepoch()
             WHERE job_id = ? AND source_id IN ({source_placeholders})"
        );
        let mut status_statement = self.query(sqlx::AssertSqlSafe(status_query));
        for update in updates {
            status_statement = status_statement
                .bind(&update.source_id)
                .bind(&update.status);
        }
        for update in updates {
            status_statement = status_statement.bind(&update.source_id).bind(&update.error);
        }
        status_statement = status_statement.bind(job_id);
        for update in updates {
            status_statement = status_statement.bind(&update.source_id);
        }
        status_statement
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;

        let source_updates = updates
            .iter()
            .filter(|update| update.source_state.is_some())
            .collect::<Vec<_>>();
        if !source_updates.is_empty() {
            let values = std::iter::repeat_n(
                "(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, unixepoch())",
                source_updates.len(),
            )
            .collect::<Vec<_>>()
            .join(", ");
            let source_query = format!(
                "INSERT INTO chapter_detection_source_states (
                    source_id, plugin_id, input_fingerprint, status, last_checked_at,
                    last_success_at, next_retry_at, error, intro_fingerprint,
                    credits_fingerprint, updated_at
                 ) VALUES {values}
                 ON CONFLICT(source_id, plugin_id) DO UPDATE SET
                    input_fingerprint = excluded.input_fingerprint,
                    status = excluded.status,
                    last_checked_at = excluded.last_checked_at,
                    last_success_at = excluded.last_success_at,
                    next_retry_at = excluded.next_retry_at,
                    error = excluded.error,
                    intro_fingerprint = excluded.intro_fingerprint,
                    credits_fingerprint = excluded.credits_fingerprint,
                    updated_at = unixepoch()"
            );
            let mut source_statement = self.query(sqlx::AssertSqlSafe(source_query));
            for update in source_updates {
                let Some(source_state) = update.source_state.as_ref() else {
                    continue;
                };
                source_statement = source_statement
                    .bind(&update.source_id)
                    .bind(plugin_id)
                    .bind(source_state.input_fingerprint.as_slice())
                    .bind(&source_state.status)
                    .bind(source_state.last_checked_at)
                    .bind(source_state.last_success_at)
                    .bind(source_state.next_retry_at)
                    .bind(&source_state.error)
                    .bind(source_state.intro_fingerprint.as_deref())
                    .bind(source_state.credits_fingerprint.as_deref());
            }
            source_statement
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }

        if cursor.is_some() {
            self.query(
                "UPDATE chapter_detection_jobs
                 SET cursor = ?, processed_count = ?, updated_at = unixepoch()
                 WHERE id = ? AND status = 'RUNNING'",
            )
            .bind(cursor)
            .bind(processed_count)
            .bind(job_id)
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

    pub(crate) async fn find_chapter_detection_job(
        &self,
        id: &str,
    ) -> Result<Option<StoredChapterDetectionJob>, StorageError> {
        self.query(
            "SELECT id, library_id, plugin_id, status, concurrency,
                    intro_window_seconds, credits_window_seconds, match_threshold,
                    cursor, processed_count, total_count, cancel_requested, error,
                    created_at, started_at, finished_at
             FROM chapter_detection_jobs WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_chapter_detection_job))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn has_active_chapter_detection_job_for_library(
        &self,
        library_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(
                SELECT 1 FROM chapter_detection_jobs
                WHERE library_id = ? AND status IN ('PENDING', 'RUNNING')
             ) THEN 1 ELSE 0 END",
        )
        .bind(library_id)
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_chapter_detection_jobs(
        &self,
        status: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredChapterDetectionJob>, StorageError> {
        let query = if status.is_some() {
            "SELECT id, library_id, plugin_id, status, concurrency,
                    intro_window_seconds, credits_window_seconds, match_threshold,
                    cursor, processed_count, total_count, cancel_requested, error,
                    created_at, started_at, finished_at
             FROM chapter_detection_jobs WHERE status = ?
             ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?"
        } else {
            "SELECT id, library_id, plugin_id, status, concurrency,
                    intro_window_seconds, credits_window_seconds, match_threshold,
                    cursor, processed_count, total_count, cancel_requested, error,
                    created_at, started_at, finished_at
             FROM chapter_detection_jobs
             ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?"
        };
        let rows = if let Some(status) = status {
            self.query(query)
                .bind(status)
                .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
                .bind(offset.max(0))
                .fetch_all(&self.pool)
                .await
        } else {
            self.query(query)
                .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
                .bind(offset.max(0))
                .fetch_all(&self.pool)
                .await
        };
        rows.map(|rows| rows.into_iter().map(stored_chapter_detection_job).collect())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_active_chapter_detection_job_ids(
        &self,
    ) -> Result<Vec<String>, StorageError> {
        self.query_scalar(
            "SELECT id FROM chapter_detection_jobs
             WHERE status IN ('PENDING', 'RUNNING') ORDER BY created_at, id LIMIT 10000",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn claim_chapter_detection_job(&self, id: &str) -> Result<bool, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let result = self
            .query(
                "UPDATE chapter_detection_jobs
                 SET status = 'RUNNING', started_at = COALESCE(started_at, unixepoch()),
                     updated_at = unixepoch()
                 WHERE id = ? AND status = 'PENDING'",
            )
            .bind(id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if result.rows_affected() == 1 {
            self.query(
                "UPDATE chapter_detection_job_items SET status = 'PENDING', updated_at = unixepoch()
                 WHERE job_id = ? AND status = 'RUNNING'",
            )
            .bind(id)
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
            })?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn requeue_running_chapter_detection_items(
        &self,
        job_id: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE chapter_detection_job_items
             SET status = 'PENDING', updated_at = unixepoch()
             WHERE job_id = ? AND status = 'RUNNING'",
        )
        .bind(job_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_pending_chapter_detection_items(
        &self,
        job_id: &str,
        limit: i64,
        supported_media_source_kinds: &[String],
    ) -> Result<Vec<StoredChapterDetectionItem>, StorageError> {
        let source_kind_placeholders = if supported_media_source_kinds.is_empty() {
            "NULL".to_owned()
        } else {
            std::iter::repeat_n("?", supported_media_source_kinds.len())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let query = format!(
            "SELECT cdi.source_id, cdi.season_id,
                    cdi.source_fingerprint,
                    cdi.input_fingerprint, cdi.is_context,
                    states.intro_fingerprint, states.credits_fingerprint,
                    ms.duration_ticks,
                    lr.canonical_path AS root_path, fe.relative_path,
                    item.provider_ids_json,
                    series.provider_ids_json AS series_provider_ids_json,
                    item.season_number, item.episode_number
             FROM chapter_detection_job_items cdi
             JOIN chapter_detection_jobs job ON job.id = cdi.job_id
             JOIN media_sources ms ON ms.id = cdi.source_id
             JOIN media_items item ON item.id = cdi.item_id
             LEFT JOIN media_items series ON series.id = item.series_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             LEFT JOIN chapter_detection_source_states states
               ON states.source_id = cdi.source_id AND states.plugin_id = job.plugin_id
             WHERE cdi.job_id = ? AND cdi.status = 'PENDING'
               AND ms.source_kind IN ({source_kind_placeholders})
             ORDER BY cdi.season_id, cdi.source_id
             LIMIT ?"
        );
        let mut query = self.query(sqlx::AssertSqlSafe(query)).bind(job_id);
        for source_kind in supported_media_source_kinds {
            query = query.bind(source_kind);
        }
        query
            .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
            .fetch_all(&self.pool)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(|row| StoredChapterDetectionItem {
                        source_id: row.get("source_id"),
                        season_id: row.get("season_id"),
                        source_fingerprint: row.get("source_fingerprint"),
                        input_fingerprint: row.get("input_fingerprint"),
                        is_context: row.get::<i64, _>("is_context") != 0,
                        intro_fingerprint: row.get("intro_fingerprint"),
                        credits_fingerprint: row.get("credits_fingerprint"),
                        duration_ticks: row.get("duration_ticks"),
                        root_path: row.get("root_path"),
                        relative_path: row.get("relative_path"),
                        provider_ids_json: row.get("provider_ids_json"),
                        series_provider_ids_json: row.get("series_provider_ids_json"),
                        season_number: row.get("season_number"),
                        episode_number: row.get("episode_number"),
                    })
                    .collect()
            })
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn chapter_detection_job_cancel_requested(
        &self,
        id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar("SELECT cancel_requested FROM chapter_detection_jobs WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .map(|value: i64| value != 0)
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn request_chapter_detection_job_cancel(
        &self,
        id: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE chapter_detection_jobs SET cancel_requested = 1, updated_at = unixepoch()
             WHERE id = ? AND status IN ('PENDING', 'RUNNING')",
        )
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn finish_chapter_detection_job(
        &self,
        id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE chapter_detection_jobs
             SET status = CASE WHEN cancel_requested = 1 THEN 'CANCELLED' ELSE ? END,
                 error = CASE WHEN cancel_requested = 1 THEN NULL ELSE ? END,
                 finished_at = unixepoch(), updated_at = unixepoch()
             WHERE id = ? AND status IN ('PENDING', 'RUNNING')",
        )
        .bind(status)
        .bind(error)
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn replace_detected_media_chapters(
        &self,
        source_id: &str,
        provider_id: &str,
        source_fingerprint: &[u8],
        markers: &[NewMediaChapterMarker],
    ) -> Result<bool, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let current = if source_fingerprint.is_empty() {
            None
        } else {
            self.query_scalar::<Vec<u8>>(
                "SELECT fe.fingerprint
                 FROM media_sources ms
                 JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                 WHERE ms.id = ?
                   AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
                   AND fe.is_missing = 0",
            )
            .bind(source_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
        };
        if !source_fingerprint.is_empty() && current.as_deref() != Some(source_fingerprint) {
            transaction
                .rollback()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(false);
        }
        self.query("DELETE FROM media_chapters WHERE media_source_id = ? AND provider_id = ?")
            .bind(source_id)
            .bind(provider_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        for chunk in markers.chunks(MEDIA_CHAPTER_INSERT_BATCH_SIZE) {
            if chunk.is_empty() {
                continue;
            }
            let values = std::iter::repeat_n("(?, ?, ?, ?, ?, ?, ?, ?)", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "INSERT INTO media_chapters (
                    id, media_source_id, start_position_ticks, name, marker_type,
                    chapter_index, provider_id, confidence
                 ) VALUES {values}"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for marker in chunk {
                statement = statement
                    .bind(Uuid::now_v7().to_string())
                    .bind(source_id)
                    .bind(marker.start_position_ticks)
                    .bind(marker.name.clone())
                    .bind(marker.marker_type.clone())
                    .bind(marker.chapter_index)
                    .bind(provider_id)
                    .bind(marker.confidence);
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
            })?;
        Ok(true)
    }

    pub(crate) async fn insert_hierarchy_item(
        &self,
        item: NewHierarchyItem<'_>,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO media_items (
                id, library_id, item_type, parent_id, series_id,
                season_number, episode_number, absolute_number,
                title, sort_title, original_title, production_year,
                provider_ids_json, identification_status, identity_key
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(item.id)
        .bind(item.library_id)
        .bind(item.item_type)
        .bind(item.parent_id)
        .bind(item.series_id)
        .bind(item.season_number)
        .bind(item.episode_number)
        .bind(item.absolute_number)
        .bind(item.title)
        .bind(item.sort_title)
        .bind(item.original_title)
        .bind(item.production_year)
        .bind(item.provider_ids_json)
        .bind(item.identification_status)
        .bind(item.identity_key)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn update_unconfirmed_hierarchy_item(
        &self,
        item_id: &str,
        title: &str,
        sort_title: &str,
        original_title: Option<&str>,
        production_year: Option<i64>,
        provider_ids_json: Option<&str>,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE media_items
             SET title = ?, sort_title = ?, original_title = ?, production_year = ?,
                 provider_ids_json = CASE
                     WHEN ? IS NOT NULL AND (provider_ids_json IS NULL OR provider_ids_json = '{}')
                     THEN ? ELSE provider_ids_json END,
                 updated_at = unixepoch()
             WHERE id = ?
               AND identification_status IN ('LOCAL_CONFIRMED', 'PENDING')
               AND metadata_provenance_json IS NULL
               AND (provider_ids_json IS NULL OR provider_ids_json = '{}')",
        )
        .bind(title)
        .bind(sort_title)
        .bind(original_title)
        .bind(production_year)
        .bind(provider_ids_json)
        .bind(provider_ids_json)
        .bind(item_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn mark_media_probe_failed(
        &self,
        source_id: &str,
        status: &str,
        error: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE media_sources
             SET probe_status = ?, probe_error = ?, updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(status)
        .bind(error)
        .bind(source_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn mark_media_probe_ready(&self, source_id: &str) -> Result<(), StorageError> {
        self.query(
            "UPDATE media_sources
             SET probe_status = 'READY', probe_error = NULL, updated_at = unixepoch()
             WHERE id = ? AND source_kind = 'STRM_URL' AND probe_status = 'PENDING'",
        )
        .bind(source_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn mark_strm_probe_sources_pending_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Any>,
        library_id: &str,
        target_scan_job_id: Option<&str>,
        target_source_id: Option<&str>,
    ) -> Result<(), StorageError> {
        let result = if let Some(source_id) = target_source_id {
            self.query(
                "UPDATE media_sources
                 SET probe_status = 'PENDING', probe_error = NULL, updated_at = unixepoch()
                 WHERE id = ? AND source_kind = 'STRM_URL'
                   AND item_id IN (
                       SELECT id FROM media_items
                       WHERE library_id = ? AND removed_at IS NULL
                   )
                   AND filesystem_entry_id IN (
                       SELECT id FROM filesystem_entries WHERE is_missing = 0
                   )",
            )
            .bind(source_id)
            .bind(library_id)
            .execute(&mut **transaction)
            .await
        } else if let Some(scan_job_id) = target_scan_job_id {
            let query = format!(
                "UPDATE media_sources
                 SET probe_status = 'PENDING', probe_error = NULL, updated_at = unixepoch()
                 WHERE id IN (
                     SELECT ms.id
                     FROM media_sources ms
                     JOIN media_items mi ON mi.id = ms.item_id
                     JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                     WHERE mi.library_id = ? AND mi.removed_at IS NULL
                       AND ms.source_kind = 'STRM_URL' AND fe.is_missing = 0
                       AND {}
                 )",
                incremental_scan_scope_sql(self.backend)
            );
            self.query(sqlx::AssertSqlSafe(query))
                .bind(library_id)
                .bind(scan_job_id)
                .execute(&mut **transaction)
                .await
        } else {
            self.query(
                "UPDATE media_sources
                 SET probe_status = 'PENDING', probe_error = NULL, updated_at = unixepoch()
                 WHERE id IN (
                     SELECT ms.id
                     FROM media_sources ms
                     JOIN media_items mi ON mi.id = ms.item_id
                     JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                     WHERE mi.library_id = ? AND mi.removed_at IS NULL
                       AND ms.source_kind = 'STRM_URL' AND fe.is_missing = 0
                 )",
            )
            .bind(library_id)
            .execute(&mut **transaction)
            .await
        };
        result.map(|_| ()).map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_pending_global_collection_syncs(
        &self,
        limit: usize,
    ) -> Result<Vec<PendingGlobalCollectionSync>, StorageError> {
        let limit = limit.clamp(1, 1_000) as i64;
        self.query_as::<(String, String, i64)>(
            "SELECT collection_provider, collection_provider_id, generation
             FROM collection_sync_outbox
             ORDER BY updated_at, collection_provider, collection_provider_id
             LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(
                    |(collection_provider, collection_provider_id, generation)| {
                        PendingGlobalCollectionSync {
                            collection_provider,
                            collection_provider_id,
                            generation,
                        }
                    },
                )
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    #[cfg(test)]
    pub(crate) async fn list_global_collection_cards_page(
        &self,
        user_id: &str,
        is_admin: bool,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<StoredGlobalCollection>, i64), StorageError> {
        self.list_global_collection_cards_page_with_restrictions(
            user_id, is_admin, offset, limit, None,
        )
        .await
    }

    pub(crate) async fn list_global_collection_cards_page_with_restrictions(
        &self,
        user_id: &str,
        is_admin: bool,
        offset: i64,
        limit: i64,
        content_restrictions: Option<&UserContentRestrictions>,
    ) -> Result<(Vec<StoredGlobalCollection>, i64), StorageError> {
        self.list_global_collection_cards_page_ordered(
            user_id,
            is_admin,
            offset,
            limit,
            false,
            content_restrictions,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn list_recent_global_collection_cards_page(
        &self,
        user_id: &str,
        is_admin: bool,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<StoredGlobalCollection>, i64), StorageError> {
        self.list_recent_global_collection_cards_page_with_restrictions(
            user_id, is_admin, offset, limit, None,
        )
        .await
    }

    pub(crate) async fn list_recent_global_collection_cards_page_with_restrictions(
        &self,
        user_id: &str,
        is_admin: bool,
        offset: i64,
        limit: i64,
        content_restrictions: Option<&UserContentRestrictions>,
    ) -> Result<(Vec<StoredGlobalCollection>, i64), StorageError> {
        self.list_global_collection_cards_page_ordered(
            user_id,
            is_admin,
            offset,
            limit,
            true,
            content_restrictions,
        )
        .await
    }

    async fn list_global_collection_cards_page_ordered(
        &self,
        user_id: &str,
        is_admin: bool,
        offset: i64,
        limit: i64,
        recently_updated_first: bool,
        content_restrictions: Option<&UserContentRestrictions>,
    ) -> Result<(Vec<StoredGlobalCollection>, i64), StorageError> {
        let library_access_filter = user_library_access_filter("source_item.library_id", true);
        let mut content_restriction_predicate = String::new();
        let mut content_restriction_binds = Vec::new();
        append_content_restriction_predicate_for_item_alias(
            &mut content_restriction_predicate,
            &mut content_restriction_binds,
            content_restrictions,
            "source_item",
        );
        let visible_source = |work_item_id: &str| {
            format!(
                "EXISTS (
                    SELECT 1 FROM collection_work_sources cws
                    JOIN media_items source_item ON source_item.id = cws.source_item_id
                    JOIN libraries source_library
                      ON source_library.id = source_item.library_id AND source_library.is_enabled = 1
                    WHERE cws.work_item_id = {work_item_id}
                      AND source_item.item_type = 'MOVIE'
                      AND source_item.removed_at IS NULL
                      AND source_item.has_available_source = 1
                      AND {library_access_filter}
                      {content_restriction_predicate}
                )"
            )
        };
        let visible_collection_filter = visible_source("visible_member.item_id");
        let mut total_statement = self
            .query_scalar::<i64>(sqlx::AssertSqlSafe(format!(
                "SELECT COUNT(DISTINCT collection.id)
                 FROM collections collection
                 JOIN media_items collection_item ON collection_item.id = collection.item_id
                 JOIN collection_items visible_member ON visible_member.collection_id = collection.id
                 WHERE collection.library_id = ?
                   AND collection_item.item_type = 'BOX_SET'
                   AND collection_item.removed_at IS NULL
                   AND collection_item.has_available_source = 1
                   AND {visible_collection_filter}"
            )))
            .bind(GLOBAL_COLLECTION_LIBRARY_ID)
            .bind(database_flag(is_admin))
            .bind(user_id);
        for bind in &content_restriction_binds {
            total_statement = match bind {
                CatalogBind::Text(value) => total_statement.bind(*value),
                CatalogBind::Integer(value) => total_statement.bind(*value),
                CatalogBind::Real(value) => total_statement.bind(*value),
            };
        }
        let total = total_statement
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let visible_member_count = visible_source("member.item_id");
        let visible_fallback_poster = |poster_column: &str| {
            format!(
                "(
                    SELECT {poster_column}
                    FROM collection_items fallback_member
                    JOIN collection_work_sources fallback_source
                      ON fallback_source.work_item_id = fallback_member.item_id
                    JOIN media_items source_item ON source_item.id = fallback_source.source_item_id
                    JOIN libraries source_library
                      ON source_library.id = source_item.library_id AND source_library.is_enabled = 1
                    JOIN item_images poster
                      ON poster.item_id = source_item.id AND poster.image_type = 'POSTER'
                    JOIN media_items work_item ON work_item.id = fallback_member.item_id
                    WHERE fallback_member.collection_id = collection.id
                      AND source_item.item_type = 'MOVIE'
                      AND source_item.removed_at IS NULL
                      AND source_item.has_available_source = 1
                      AND {library_access_filter}
                      {content_restriction_predicate}
                    ORDER BY fallback_member.sort_order, work_item.sort_title,
                             work_item.id, source_library.name, source_item.id,
                             poster.image_index, poster.id
                    LIMIT 1
                )"
            )
        };
        let ordering = if recently_updated_first {
            "collection.updated_at DESC, collection_item.sort_title, collection.item_id"
        } else {
            "collection_item.sort_title, collection.item_id"
        };
        let query = format!(
            "SELECT collection.item_id, collection_item.title,
                    (SELECT COUNT(DISTINCT member.item_id)
                     FROM collection_items member
                     WHERE member.collection_id = collection.id
                       AND {visible_member_count}) AS member_count,
                    (SELECT id FROM item_images
                     WHERE item_id = collection.item_id AND image_type = 'POSTER'
                     ORDER BY image_index, id LIMIT 1) AS collection_poster_tag,
                    {} AS fallback_poster_item_id,
                    {} AS fallback_poster_image_tag
             FROM collections collection
             JOIN media_items collection_item ON collection_item.id = collection.item_id
             WHERE collection.library_id = ?
               AND collection_item.item_type = 'BOX_SET'
               AND collection_item.removed_at IS NULL
               AND collection_item.has_available_source = 1
               AND EXISTS (
                   SELECT 1 FROM collection_items visible_member
                   WHERE visible_member.collection_id = collection.id
                     AND {}
               )
             ORDER BY {ordering}
             LIMIT ? OFFSET ?",
            visible_fallback_poster("source_item.id"),
            visible_fallback_poster("poster.id"),
            visible_collection_filter,
        );
        let mut rows = self
            .query(sqlx::AssertSqlSafe(query))
            .bind(database_flag(is_admin))
            .bind(user_id);
        for bind in &content_restriction_binds {
            rows = match bind {
                CatalogBind::Text(value) => rows.bind(*value),
                CatalogBind::Integer(value) => rows.bind(*value),
                CatalogBind::Real(value) => rows.bind(*value),
            };
        }
        for _ in 0..2 {
            rows = rows.bind(database_flag(is_admin)).bind(user_id);
            for bind in &content_restriction_binds {
                rows = match bind {
                    CatalogBind::Text(value) => rows.bind(*value),
                    CatalogBind::Integer(value) => rows.bind(*value),
                    CatalogBind::Real(value) => rows.bind(*value),
                };
            }
        }
        rows = rows.bind(GLOBAL_COLLECTION_LIBRARY_ID);
        rows = rows.bind(database_flag(is_admin)).bind(user_id);
        for bind in &content_restriction_binds {
            rows = match bind {
                CatalogBind::Text(value) => rows.bind(*value),
                CatalogBind::Integer(value) => rows.bind(*value),
                CatalogBind::Real(value) => rows.bind(*value),
            };
        }
        rows = rows.bind(limit).bind(offset);
        let rows = rows
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let cards = rows
            .into_iter()
            .map(|row| {
                let collection_poster_tag: Option<String> = row.get("collection_poster_tag");
                let fallback_poster_item_id: Option<String> = row.get("fallback_poster_item_id");
                let fallback_poster_image_tag: Option<String> =
                    row.get("fallback_poster_image_tag");
                StoredGlobalCollection {
                    item_id: row.get("item_id"),
                    title: row.get("title"),
                    member_count: row.get("member_count"),
                    poster_item_id: collection_poster_tag
                        .as_ref()
                        .map(|_| row.get("item_id"))
                        .or(fallback_poster_item_id),
                    poster_image_tag: collection_poster_tag.or(fallback_poster_image_tag),
                }
            })
            .collect();
        Ok((cards, total))
    }

    #[cfg(test)]
    pub(crate) async fn list_global_collection_member_ids_page(
        &self,
        collection_item_id: &str,
        user_id: &str,
        is_admin: bool,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<String>, i64), StorageError> {
        self.list_global_collection_member_ids_page_with_restrictions(
            collection_item_id,
            user_id,
            is_admin,
            offset,
            limit,
            None,
        )
        .await
    }

    pub(crate) async fn list_global_collection_member_ids_page_with_restrictions(
        &self,
        collection_item_id: &str,
        user_id: &str,
        is_admin: bool,
        offset: i64,
        limit: i64,
        content_restrictions: Option<&UserContentRestrictions>,
    ) -> Result<(Vec<String>, i64), StorageError> {
        let library_access_filter = user_library_access_filter("source_item.library_id", true);
        let mut content_restriction_predicate = String::new();
        let mut content_restriction_binds = Vec::new();
        append_content_restriction_predicate_for_item_alias(
            &mut content_restriction_predicate,
            &mut content_restriction_binds,
            content_restrictions,
            "source_item",
        );
        let visible_source = format!(
            "EXISTS (
                SELECT 1 FROM collection_work_sources cws
                JOIN media_items source_item ON source_item.id = cws.source_item_id
                JOIN libraries source_library
                  ON source_library.id = source_item.library_id AND source_library.is_enabled = 1
                WHERE cws.work_item_id = member.item_id
                  AND source_item.item_type = 'MOVIE'
                  AND source_item.removed_at IS NULL
                  AND source_item.has_available_source = 1
                  AND {library_access_filter}
                  {content_restriction_predicate}
            )"
        );
        let base_query = format!(
            "FROM collection_items member
             JOIN collections collection ON collection.id = member.collection_id
             JOIN media_items collection_item ON collection_item.id = collection.item_id
             JOIN collection_work_items work ON work.work_item_id = member.item_id
             JOIN media_items work_item ON work_item.id = member.item_id
             WHERE collection.item_id = ?
               AND collection.library_id = ?
               AND collection_item.item_type = 'BOX_SET'
               AND collection_item.removed_at IS NULL
               AND collection_item.has_available_source = 1
               AND {visible_source}"
        );
        let count = self
            .query_scalar::<i64>(sqlx::AssertSqlSafe(format!(
                "SELECT COUNT(DISTINCT member.item_id) {base_query}"
            )))
            .bind(collection_item_id)
            .bind(GLOBAL_COLLECTION_LIBRARY_ID)
            .bind(database_flag(is_admin))
            .bind(user_id);
        let mut count = count;
        for bind in &content_restriction_binds {
            count = match bind {
                CatalogBind::Text(value) => count.bind(*value),
                CatalogBind::Integer(value) => count.bind(*value),
                CatalogBind::Real(value) => count.bind(*value),
            };
        }
        let total = count
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;

        let rows = self
            .query(sqlx::AssertSqlSafe(format!(
                "SELECT member.item_id {base_query}
                 ORDER BY member.sort_order, work_item.sort_title, member.item_id
                 LIMIT ? OFFSET ?"
            )))
            .bind(collection_item_id)
            .bind(GLOBAL_COLLECTION_LIBRARY_ID)
            .bind(database_flag(is_admin))
            .bind(user_id);
        let mut rows = rows;
        for bind in &content_restriction_binds {
            rows = match bind {
                CatalogBind::Text(value) => rows.bind(*value),
                CatalogBind::Integer(value) => rows.bind(*value),
                CatalogBind::Real(value) => rows.bind(*value),
            };
        }
        let rows = rows
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok((
            rows.into_iter().map(|row| row.get("item_id")).collect(),
            total,
        ))
    }

    #[cfg(test)]
    pub(crate) async fn list_global_collection_sources_for_works(
        &self,
        work_item_ids: &[String],
        user_id: &str,
        is_admin: bool,
    ) -> Result<Vec<StoredGlobalCollectionSource>, StorageError> {
        self.list_global_collection_sources_for_works_with_restrictions(
            work_item_ids,
            user_id,
            is_admin,
            None,
        )
        .await
    }

    pub(crate) async fn list_global_collection_sources_for_works_with_restrictions(
        &self,
        work_item_ids: &[String],
        user_id: &str,
        is_admin: bool,
        content_restrictions: Option<&UserContentRestrictions>,
    ) -> Result<Vec<StoredGlobalCollectionSource>, StorageError> {
        if work_item_ids.is_empty() {
            return Ok(Vec::new());
        }
        let work_placeholders = std::iter::repeat_n("?", work_item_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let library_access_filter = user_library_access_filter("source_item.library_id", true);
        let mut content_restriction_predicate = String::new();
        let mut content_restriction_binds = Vec::new();
        append_content_restriction_predicate_for_item_alias(
            &mut content_restriction_predicate,
            &mut content_restriction_binds,
            content_restrictions,
            "source_item",
        );
        let query = format!(
            "SELECT source_work.work_item_id, source_item.id AS source_item_id,
                    source_item.library_id, source_library.name AS library_name,
                    source_item.title, media_source.id AS media_source_id,
                    media_source.edition_name, media_source.quality_label, media_source.container,
                    source_preference.source_item_id AS preferred_source_item_id,
                    source_preference.media_source_id AS preferred_media_source_id,
                    (SELECT id FROM item_images
                     WHERE item_id = source_item.id AND image_type = 'POSTER'
                     ORDER BY image_index, id LIMIT 1) AS poster_image_tag
             FROM collection_work_sources source_work
             JOIN media_items source_item ON source_item.id = source_work.source_item_id
             JOIN libraries source_library
               ON source_library.id = source_item.library_id AND source_library.is_enabled = 1
             LEFT JOIN media_sources media_source
               ON media_source.item_id = source_item.id
              AND (
                  media_source.filesystem_entry_id IS NULL
                  OR EXISTS (
                      SELECT 1 FROM filesystem_entries source_file
                      WHERE source_file.id = media_source.filesystem_entry_id
                        AND source_file.is_missing = 0
                  )
              )
             LEFT JOIN user_collection_source_preferences source_preference
               ON source_preference.user_id = ?
              AND source_preference.work_item_id = source_work.work_item_id
              AND source_preference.source_item_id = source_item.id
              AND (
                  source_preference.media_source_id IS NULL
                  OR source_preference.media_source_id = media_source.id
              )
             WHERE source_work.work_item_id IN ({work_placeholders})
               AND source_item.item_type = 'MOVIE'
               AND source_item.removed_at IS NULL
               AND source_item.has_available_source = 1
               AND {library_access_filter}
               {content_restriction_predicate}
             ORDER BY source_work.work_item_id,
                      source_library.name, source_item.id,
                      media_source.is_default DESC, media_source.id"
        );
        let mut rows = self.query(sqlx::AssertSqlSafe(query)).bind(user_id);
        for work_item_id in work_item_ids {
            rows = rows.bind(work_item_id);
        }
        rows = rows.bind(database_flag(is_admin)).bind(user_id);
        for bind in &content_restriction_binds {
            rows = match bind {
                CatalogBind::Text(value) => rows.bind(*value),
                CatalogBind::Integer(value) => rows.bind(*value),
                CatalogBind::Real(value) => rows.bind(*value),
            };
        }
        let rows = rows
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(rows
            .into_iter()
            .map(|row| StoredGlobalCollectionSource {
                work_item_id: row.get("work_item_id"),
                source_item_id: row.get("source_item_id"),
                library_id: row.get("library_id"),
                library_name: row.get("library_name"),
                title: row.get("title"),
                media_source_id: row.get("media_source_id"),
                edition_name: row.get("edition_name"),
                quality_label: row.get("quality_label"),
                container: row.get("container"),
                preferred_source_item_id: row.get("preferred_source_item_id"),
                preferred_media_source_id: row.get("preferred_media_source_id"),
                poster_image_tag: row.get("poster_image_tag"),
            })
            .collect())
    }

    #[cfg(test)]
    pub(crate) async fn set_global_collection_source_preference(
        &self,
        user_id: &str,
        is_admin: bool,
        collection_item_id: &str,
        work_item_id: &str,
        source_item_id: &str,
        media_source_id: Option<&str>,
    ) -> Result<bool, StorageError> {
        self.set_global_collection_source_preference_with_restrictions(
            GlobalCollectionSourcePreference {
                user_id,
                is_admin,
                collection_item_id,
                work_item_id,
                source_item_id,
                media_source_id,
            },
            None,
        )
        .await
    }

    pub(crate) async fn set_global_collection_source_preference_with_restrictions(
        &self,
        preference: GlobalCollectionSourcePreference<'_>,
        content_restrictions: Option<&UserContentRestrictions>,
    ) -> Result<bool, StorageError> {
        let library_access_filter = user_library_access_filter("source_item.library_id", true);
        let mut content_restriction_predicate = String::new();
        let mut content_restriction_binds = Vec::new();
        append_content_restriction_predicate_for_item_alias(
            &mut content_restriction_predicate,
            &mut content_restriction_binds,
            content_restrictions,
            "source_item",
        );
        let mut statement = self
            .query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO user_collection_source_preferences (
                     user_id, work_item_id, source_item_id, media_source_id, updated_at
                 )
                 SELECT ?, ?, ?, ?, unixepoch()
                 WHERE EXISTS (
                     SELECT 1
                     FROM collections collection
                     JOIN collection_items member ON member.collection_id = collection.id
                     JOIN collection_work_sources source_work
                       ON source_work.work_item_id = member.item_id
                     JOIN media_items source_item ON source_item.id = source_work.source_item_id
                     JOIN libraries source_library
                       ON source_library.id = source_item.library_id AND source_library.is_enabled = 1
                     WHERE collection.item_id = ?
                       AND collection.library_id = ?
                       AND member.item_id = ?
                       AND source_work.source_item_id = ?
                       AND source_item.item_type = 'MOVIE'
                       AND source_item.removed_at IS NULL
                       AND source_item.has_available_source = 1
                       AND {library_access_filter}
                       {content_restriction_predicate}
                       AND (
                           ? IS NULL OR EXISTS (
                               SELECT 1 FROM media_sources selected_source
                               WHERE selected_source.id = ?
                                 AND selected_source.item_id = source_item.id
                                 AND (
                                     selected_source.filesystem_entry_id IS NULL
                                     OR EXISTS (
                                         SELECT 1 FROM filesystem_entries selected_file
                                         WHERE selected_file.id = selected_source.filesystem_entry_id
                                           AND selected_file.is_missing = 0
                                     )
                                 )
                           )
                       )
                 )
                 ON CONFLICT (user_id, work_item_id) DO UPDATE SET
                     source_item_id = excluded.source_item_id,
                     media_source_id = excluded.media_source_id,
                     updated_at = excluded.updated_at"
            )))
            .bind(preference.user_id)
            .bind(preference.work_item_id)
            .bind(preference.source_item_id)
            .bind(preference.media_source_id)
            .bind(preference.collection_item_id)
            .bind(GLOBAL_COLLECTION_LIBRARY_ID)
            .bind(preference.work_item_id)
            .bind(preference.source_item_id)
            .bind(database_flag(preference.is_admin))
            .bind(preference.user_id);
        for bind in &content_restriction_binds {
            statement = match bind {
                CatalogBind::Text(value) => statement.bind(*value),
                CatalogBind::Integer(value) => statement.bind(*value),
                CatalogBind::Real(value) => statement.bind(*value),
            };
        }
        let result = statement
            .bind(preference.media_source_id)
            .bind(preference.media_source_id)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result.rows_affected() > 0)
    }

    /// Rebuild one global collection and its logical movie works in one transaction.
    /// The outbox generation lock prevents a concurrent reference change from being lost.
    pub(crate) async fn sync_global_collection(
        &self,
        collection_provider: &str,
        collection_provider_id: &str,
        expected_generation: i64,
    ) -> Result<bool, StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let lock_clause = if self.backend == DatabaseBackend::Postgres {
            " FOR UPDATE"
        } else {
            ""
        };
        let generation_query = format!(
            "SELECT generation FROM collection_sync_outbox
             WHERE collection_provider = ? AND collection_provider_id = ?{lock_clause}"
        );
        let generation = self
            .query_scalar::<i64>(sqlx::AssertSqlSafe(generation_query))
            .bind(collection_provider)
            .bind(collection_provider_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if generation != Some(expected_generation) {
            transaction
                .rollback()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(false);
        }

        let old_work_keys = self
            .query_as::<(String, String)>(
                "SELECT movie_provider, movie_provider_id
                 FROM global_collection_work_references
                 WHERE collection_provider = ? AND collection_provider_id = ?",
            )
            .bind(collection_provider)
            .bind(collection_provider_id)
            .fetch_all(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let reference_rows = self
            .query_as::<(
                String,
                String,
                String,
                String,
                Option<i64>,
                Option<i64>,
                String,
            )>(
                "SELECT reference.movie_provider, reference.movie_provider_id,
                        item.id, item.title, item.production_year, reference.sort_order,
                        reference.collection_title
                 FROM collection_item_references reference
                 JOIN media_items item ON item.id = reference.item_id
                 JOIN libraries library ON library.id = item.library_id
                 WHERE reference.collection_provider = ?
                   AND reference.collection_provider_id = ?
                   AND item.item_type = 'MOVIE'
                   AND item.removed_at IS NULL
                   AND item.has_available_source = 1
                   AND library.is_enabled = 1
                 ORDER BY reference.movie_provider, reference.movie_provider_id, item.id",
            )
            .bind(collection_provider)
            .bind(collection_provider_id)
            .fetch_all(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let mut members = BTreeMap::new();
        for (
            movie_provider,
            movie_provider_id,
            _source_item_id,
            title,
            production_year,
            sort_order,
            collection_title,
        ) in reference_rows
        {
            members
                .entry((movie_provider.clone(), movie_provider_id.clone()))
                .or_insert(GlobalCollectionSyncMember {
                    movie_provider,
                    movie_provider_id,
                    title,
                    production_year,
                    sort_order,
                    collection_title,
                });
        }

        self.query(
            "DELETE FROM global_collection_work_references
             WHERE collection_provider = ? AND collection_provider_id = ?",
        )
        .bind(collection_provider)
        .bind(collection_provider_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        for member in members.values() {
            self.query(
                "INSERT INTO global_collection_work_references (
                     collection_provider, collection_provider_id,
                     movie_provider, movie_provider_id, sort_order
                 ) VALUES (?, ?, ?, ?, ?)",
            )
            .bind(collection_provider)
            .bind(collection_provider_id)
            .bind(&member.movie_provider)
            .bind(&member.movie_provider_id)
            .bind(
                member
                    .sort_order
                    .unwrap_or(member.production_year.unwrap_or(0)),
            )
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }

        let mut affected_work_keys = old_work_keys;
        affected_work_keys.extend(members.keys().cloned());
        affected_work_keys.sort();
        affected_work_keys.dedup();
        let mut existing_work_ids = BTreeMap::new();
        for (movie_provider, movie_provider_id) in &affected_work_keys {
            let identity_key =
                global_generated_identity_key("work", movie_provider, movie_provider_id);
            let work_item_id = self
                .query_scalar::<String>(
                    "SELECT id FROM media_items WHERE identity_key = ? AND item_type = 'MOVIE'",
                )
                .bind(&identity_key)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if let Some(work_item_id) = work_item_id {
                self.query("DELETE FROM collection_work_sources WHERE work_item_id = ?")
                    .bind(&work_item_id)
                    .execute(&mut *transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                existing_work_ids.insert(
                    (movie_provider.clone(), movie_provider_id.clone()),
                    work_item_id,
                );
            }
        }

        let mut work_item_ids = BTreeMap::new();
        for (movie_provider, movie_provider_id) in &affected_work_keys {
            let source_rows = self
                .query_as::<(String, i64, String, String, Option<String>)>(
                    "SELECT item.id, MIN(COALESCE(reference.sort_order, item.production_year, 0)),
                            item.title, item.sort_title, item.overview
                     FROM collection_item_references reference
                     JOIN media_items item ON item.id = reference.item_id
                     JOIN libraries library ON library.id = item.library_id
                     WHERE reference.movie_provider = ?
                       AND reference.movie_provider_id = ?
                       AND item.item_type = 'MOVIE'
                       AND item.removed_at IS NULL
                       AND item.has_available_source = 1
                       AND library.is_enabled = 1
                     GROUP BY item.id, item.title, item.sort_title, item.overview,
                              item.production_year
                     ORDER BY item.id",
                )
                .bind(movie_provider)
                .bind(movie_provider_id)
                .fetch_all(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            let key = (movie_provider.clone(), movie_provider_id.clone());
            let Some((_, _, title, sort_title, overview)) = source_rows.first() else {
                if let Some(work_item_id) = existing_work_ids.get(&key) {
                    self.query(
                        "UPDATE media_items
                         SET has_available_source = 0,
                             removed_at = COALESCE(removed_at, unixepoch()),
                             updated_at = unixepoch()
                         WHERE id = ?",
                    )
                    .bind(work_item_id)
                    .execute(&mut *transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                }
                continue;
            };
            let identity_key =
                global_generated_identity_key("work", movie_provider, movie_provider_id);
            let work_item_id = self
                .ensure_global_generated_media_item(
                    &mut transaction,
                    GlobalGeneratedMediaItem {
                        item_type: "MOVIE",
                        identity_key: &identity_key,
                        title,
                        sort_title,
                        overview: overview.as_deref(),
                        has_available_source: true,
                    },
                )
                .await?;
            self.query(
                "INSERT INTO collection_work_items (
                     work_item_id, movie_provider, movie_provider_id
                 ) VALUES (?, ?, ?)
                 ON CONFLICT (work_item_id) DO UPDATE SET
                     movie_provider = excluded.movie_provider,
                     movie_provider_id = excluded.movie_provider_id",
            )
            .bind(&work_item_id)
            .bind(movie_provider)
            .bind(movie_provider_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
            for (source_item_id, sort_order, ..) in &source_rows {
                self.query(
                    "INSERT INTO collection_work_sources (
                         work_item_id, source_item_id, sort_order
                     ) VALUES (?, ?, ?)
                     ON CONFLICT (source_item_id) DO UPDATE SET
                         work_item_id = excluded.work_item_id,
                         sort_order = excluded.sort_order",
                )
                .bind(&work_item_id)
                .bind(source_item_id)
                .bind(sort_order)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            }
            self.query(
                "UPDATE media_items
                 SET has_available_source = 1, removed_at = NULL, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(&work_item_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
            work_item_ids.insert(key, work_item_id);
        }

        let stored_collection_title = self
            .query_scalar::<String>(
                "SELECT collection_title FROM collection_item_references
                 WHERE collection_provider = ? AND collection_provider_id = ?
                 ORDER BY item_id LIMIT 1",
            )
            .bind(collection_provider)
            .bind(collection_provider_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let existing_collection = self
            .query_as::<(String, String, String)>(
                "SELECT id, item_id, title FROM collections
                 WHERE library_id = ? AND provider = ? AND provider_id = ?",
            )
            .bind(GLOBAL_COLLECTION_LIBRARY_ID)
            .bind(collection_provider)
            .bind(collection_provider_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let active_collection_title = members
            .values()
            .map(|member| member.collection_title.as_str())
            .find(|title| !global_collection_title_is_placeholder(title))
            .map(str::to_owned);
        let stored_title_is_placeholder = stored_collection_title
            .as_deref()
            .is_some_and(global_collection_title_is_placeholder);
        let stored_collection_title =
            stored_collection_title.filter(|title| !global_collection_title_is_placeholder(title));
        let existing_collection_title = existing_collection
            .as_ref()
            .map(|(_, _, title)| title)
            .filter(|title| !global_collection_title_is_placeholder(title))
            .cloned();
        let collection_title = active_collection_title
            .or(stored_collection_title)
            .or(existing_collection_title)
            .or_else(|| {
                (!members.is_empty() || stored_title_is_placeholder).then(|| {
                    global_collection_title_fallback(collection_provider, collection_provider_id)
                })
            });
        if let Some(collection_title) = collection_title {
            let is_visible = members.len() >= 2;
            let identity_key = global_generated_identity_key(
                "collection",
                collection_provider,
                collection_provider_id,
            );
            let collection_item_id = self
                .ensure_global_generated_media_item(
                    &mut transaction,
                    GlobalGeneratedMediaItem {
                        item_type: "BOX_SET",
                        identity_key: &identity_key,
                        title: &collection_title,
                        sort_title: &collection_title.to_lowercase(),
                        overview: None,
                        has_available_source: is_visible,
                    },
                )
                .await?;
            self.query(
                "INSERT INTO collections (
                     id, item_id, library_id, provider, provider_id, title
                 ) VALUES (?, ?, ?, ?, ?, ?)
                 ON CONFLICT (library_id, provider, provider_id) DO UPDATE SET
                     item_id = excluded.item_id,
                     title = excluded.title,
                     updated_at = unixepoch()",
            )
            .bind(Uuid::now_v7().to_string())
            .bind(&collection_item_id)
            .bind(GLOBAL_COLLECTION_LIBRARY_ID)
            .bind(collection_provider)
            .bind(collection_provider_id)
            .bind(&collection_title)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
            let collection_id = self
                .query_scalar::<String>(
                    "SELECT id FROM collections
                     WHERE library_id = ? AND provider = ? AND provider_id = ?",
                )
                .bind(GLOBAL_COLLECTION_LIBRARY_ID)
                .bind(collection_provider)
                .bind(collection_provider_id)
                .fetch_one(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            self.query("DELETE FROM collection_items WHERE collection_id = ?")
                .bind(&collection_id)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if is_visible {
                let mut ordered_members = members.values().collect::<Vec<_>>();
                ordered_members.sort_by(|left, right| {
                    left.sort_order
                        .unwrap_or(left.production_year.unwrap_or(0))
                        .cmp(
                            &right
                                .sort_order
                                .unwrap_or(right.production_year.unwrap_or(0)),
                        )
                        .then_with(|| left.title.to_lowercase().cmp(&right.title.to_lowercase()))
                        .then_with(|| left.movie_provider.cmp(&right.movie_provider))
                        .then_with(|| left.movie_provider_id.cmp(&right.movie_provider_id))
                });
                for member in ordered_members {
                    if let Some(work_item_id) = work_item_ids.get(&(
                        member.movie_provider.clone(),
                        member.movie_provider_id.clone(),
                    )) {
                        self.query(
                            "INSERT INTO collection_items (collection_id, item_id, sort_order)
                             VALUES (?, ?, ?)",
                        )
                        .bind(&collection_id)
                        .bind(work_item_id)
                        .bind(
                            member
                                .sort_order
                                .unwrap_or(member.production_year.unwrap_or(0)),
                        )
                        .execute(&mut *transaction)
                        .await
                        .map_err(|source| StorageError::Sqlx {
                            path: self.path.clone(),
                            source,
                        })?;
                    }
                }
            }
        }

        self.query(
            "DELETE FROM collection_sync_outbox
             WHERE collection_provider = ? AND collection_provider_id = ? AND generation = ?",
        )
        .bind(collection_provider)
        .bind(collection_provider_id)
        .bind(expected_generation)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(true)
    }

    async fn ensure_global_generated_media_item(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        item: GlobalGeneratedMediaItem<'_>,
    ) -> Result<String, StorageError> {
        let existing_id = self
            .query_scalar::<String>("SELECT id FROM media_items WHERE identity_key = ?")
            .bind(item.identity_key)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let (id, already_exists) = match existing_id {
            Some(id) => (id, true),
            None => (Uuid::now_v7().to_string(), false),
        };
        if already_exists {
            self.query(
                "UPDATE media_items
                 SET library_id = ?, item_type = ?, title = ?, sort_title = ?, overview = ?,
                     production_year = NULL, identification_status = 'LOCAL_CONFIRMED',
                     has_available_source = ?, removed_at = NULL, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(GLOBAL_COLLECTION_LIBRARY_ID)
            .bind(item.item_type)
            .bind(item.title)
            .bind(item.sort_title)
            .bind(item.overview)
            .bind(database_flag(item.has_available_source))
            .bind(&id)
            .execute(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        } else {
            self.query(
                "INSERT INTO media_items (
                     id, library_id, item_type, title, sort_title, overview,
                     identification_status, identity_key, has_available_source
                 ) VALUES (?, ?, ?, ?, ?, ?, 'LOCAL_CONFIRMED', ?, ?)",
            )
            .bind(&id)
            .bind(GLOBAL_COLLECTION_LIBRARY_ID)
            .bind(item.item_type)
            .bind(item.title)
            .bind(item.sort_title)
            .bind(item.overview)
            .bind(item.identity_key)
            .bind(database_flag(item.has_available_source))
            .execute(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        Ok(id)
    }

    pub(crate) async fn update_media_item_metadata(
        &self,
        update: MediaMetadataUpdate<'_>,
    ) -> Result<(), StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        self.update_media_item_metadata_in_transaction(&mut transaction, &update, None)
            .await?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_pending_movie_nfo_collection_reference_reconciliations(
        &self,
        limit: usize,
    ) -> Result<Vec<StoredMovieNfoCollectionReferenceReconciliation>, StorageError> {
        if !(1..=64).contains(&limit) {
            return Err(StorageError::Conflict(
                "invalid NFO collection reference reconciliation page size".to_owned(),
            ));
        }
        let rows: Vec<StoredMovieNfoCollectionReferenceReconciliation> = self
            .query_as(
                "SELECT item.id AS item_id, item.provider_ids_json, item.metadata_scraper_id,
                        library.scraper_id AS library_scraper_id,
                        item.production_year, item.nfo_metadata_json
                 FROM media_items item
                 JOIN libraries library ON library.id = item.library_id
                 WHERE item.item_type = 'MOVIE'
                   AND item.removed_at IS NULL
                   AND item.nfo_metadata_json IS NOT NULL
                   AND item.nfo_collection_reference_key IS NULL
                 ORDER BY item.id
                 LIMIT ?",
            )
            .bind(i64::try_from(limit).map_err(|_| {
                StorageError::Conflict(
                    "NFO collection reference reconciliation page size overflow".to_owned(),
                )
            })?)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(rows)
    }

    pub(crate) async fn commit_movie_nfo_collection_reference_reconciliation(
        &self,
        candidate: &StoredMovieNfoCollectionReferenceReconciliation,
        reference: Option<&GlobalMovieCollectionReference>,
        reference_key: &str,
    ) -> Result<bool, StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let lock_clause = if self.backend == DatabaseBackend::Postgres {
            " FOR UPDATE"
        } else {
            ""
        };
        let current_query = format!(
            "SELECT item.provider_ids_json, item.metadata_scraper_id,
                    library.scraper_id, item.production_year, item.nfo_metadata_json,
                    item.nfo_collection_reference_key
             FROM media_items item
             JOIN libraries library ON library.id = item.library_id
             WHERE item.id = ? AND item.item_type = 'MOVIE'
               AND item.removed_at IS NULL{lock_clause}"
        );
        let current = self
            .query_as::<(
                Option<String>,
                Option<String>,
                Option<String>,
                Option<i64>,
                Option<String>,
                Option<String>,
            )>(sqlx::AssertSqlSafe(current_query))
            .bind(&candidate.item_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let Some((
            provider_ids_json,
            metadata_scraper_id,
            library_scraper_id,
            production_year,
            nfo_metadata_json,
            current_reference_key,
        )) = current
        else {
            transaction
                .commit()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(false);
        };
        if current_reference_key.is_some()
            || provider_ids_json != candidate.provider_ids_json
            || metadata_scraper_id != candidate.metadata_scraper_id
            || library_scraper_id != candidate.library_scraper_id
            || production_year != candidate.production_year
            || nfo_metadata_json.as_deref() != Some(candidate.nfo_metadata_json.as_str())
        {
            transaction
                .commit()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(false);
        }
        let marked = self
            .query(
                "UPDATE media_items
                 SET nfo_collection_reference_key = ?
                 WHERE id = ? AND nfo_collection_reference_key IS NULL",
            )
            .bind(reference_key)
            .bind(&candidate.item_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if marked.rows_affected() != 1 {
            transaction
                .commit()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(false);
        }
        if let Some(reference) = reference {
            self.replace_global_movie_collection_reference_in_transaction(
                &mut transaction,
                &candidate.item_id,
                Some(reference),
                None,
            )
            .await?;
        }
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(true)
    }

    pub(crate) async fn update_media_item_metadata_with_collection_reference(
        &self,
        update: MediaMetadataUpdate<'_>,
        reference: Option<&GlobalMovieCollectionReference>,
        unqualified_nfo_set_id: Option<&str>,
        nfo_collection_reference_key: &str,
    ) -> Result<(), StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        self.update_media_item_metadata_in_transaction(
            &mut transaction,
            &update,
            Some(nfo_collection_reference_key),
        )
        .await?;
        self.replace_global_movie_collection_reference_in_transaction(
            &mut transaction,
            update.item_id,
            reference,
            unqualified_nfo_set_id,
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn update_movie_collection_reference(
        &self,
        item_id: &str,
        reference: Option<&GlobalMovieCollectionReference>,
        unqualified_nfo_set_id: Option<&str>,
    ) -> Result<(), StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        self.replace_global_movie_collection_reference_in_transaction(
            &mut transaction,
            item_id,
            reference,
            unqualified_nfo_set_id,
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn commit_local_nfo_state_batch(
        &self,
        updates: &[MediaMetadataUpdate<'_>],
        defaults_repairs: &[LocalNfoDefaultsRepair<'_>],
    ) -> Result<(), StorageError> {
        self.commit_local_nfo_state_batch_with_collection_references(updates, defaults_repairs, &[])
            .await
    }

    pub(crate) async fn commit_local_nfo_state_batch_with_collection_references(
        &self,
        updates: &[MediaMetadataUpdate<'_>],
        defaults_repairs: &[LocalNfoDefaultsRepair<'_>],
        reference_updates: &[GlobalMovieCollectionReferenceUpdate<'_>],
    ) -> Result<(), StorageError> {
        let defaults_repairs = defaults_repairs
            .iter()
            .filter(|repair| !repair.provider_ids.is_empty() || repair.premiere_date.is_some())
            .collect::<Vec<_>>();
        let mut update_offset = 0;
        let mut repair_offset = 0;
        while update_offset < updates.len() || repair_offset < defaults_repairs.len() {
            let update_end = (update_offset + MEDIA_METADATA_UPDATE_BATCH_SIZE).min(updates.len());
            let update_count = update_end - update_offset;
            let repair_end = (repair_offset + MEDIA_METADATA_UPDATE_BATCH_SIZE - update_count)
                .min(defaults_repairs.len());
            let update_chunk = &updates[update_offset..update_end];
            let repair_chunk = &defaults_repairs[repair_offset..repair_end];
            let _write_guard = self.acquire_metadata_write_lock().await;
            let mut transaction = self.begin_metadata_write_transaction().await?;
            for update in update_chunk {
                let nfo_collection_reference_key = reference_updates
                    .iter()
                    .find(|reference_update| reference_update.item_id == update.item_id)
                    .map(|reference_update| reference_update.nfo_collection_reference_key);
                self.update_media_item_metadata_in_transaction(
                    &mut transaction,
                    update,
                    nfo_collection_reference_key,
                )
                .await?;
            }
            for reference_update in reference_updates.iter().filter(|reference_update| {
                update_chunk
                    .iter()
                    .any(|update| update.item_id == reference_update.item_id)
            }) {
                self.replace_global_movie_collection_reference_in_transaction(
                    &mut transaction,
                    reference_update.item_id,
                    reference_update.reference,
                    reference_update.unqualified_nfo_set_id,
                )
                .await?;
            }
            for repair in repair_chunk {
                self.repair_local_nfo_defaults_in_transaction(&mut transaction, repair)
                    .await?;
            }
            transaction
                .commit()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            update_offset = update_end;
            repair_offset = repair_end;
        }
        Ok(())
    }

    pub(crate) async fn replace_global_movie_collection_reference_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        item_id: &str,
        reference: Option<&GlobalMovieCollectionReference>,
        unqualified_nfo_set_id: Option<&str>,
    ) -> Result<(), StorageError> {
        if reference.is_none()
            && let Some(set_id) = unqualified_nfo_set_id
            && let Some((
                collection_provider_id,
                movie_provider,
                movie_provider_id,
                collection_provider,
                collection_title,
                sort_order,
                provider_ids_json,
            )) = self
                .query_as::<(
                    String,
                    String,
                    String,
                    String,
                    String,
                    Option<i64>,
                    Option<String>,
                )>(
                    "SELECT reference.collection_provider_id, reference.movie_provider,
                            reference.movie_provider_id, reference.collection_provider,
                            reference.collection_title, reference.sort_order,
                            item.provider_ids_json
                     FROM collection_item_references reference
                     JOIN media_items item ON item.id = reference.item_id
                     WHERE reference.item_id = ?
                     LIMIT 1",
                )
                .bind(item_id)
                .fetch_optional(&mut **transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
        {
            let existing = GlobalMovieCollectionReference {
                movie_provider,
                movie_provider_id,
                collection_provider,
                collection_provider_id,
                collection_title,
                sort_order,
            };
            if existing.matches_existing_nfo_identity(set_id, provider_ids_json.as_deref()) {
                return Ok(());
            }
        }
        self.query("DELETE FROM collection_item_references WHERE item_id = ?")
            .bind(item_id)
            .execute(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let Some(reference) = reference else {
            return Ok(());
        };
        self.query(
            "INSERT INTO collection_item_references (
                 item_id, movie_provider, movie_provider_id, collection_provider,
                 collection_provider_id, collection_title, sort_order
             ) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(item_id)
        .bind(&reference.movie_provider)
        .bind(&reference.movie_provider_id)
        .bind(&reference.collection_provider)
        .bind(&reference.collection_provider_id)
        .bind(&reference.collection_title)
        .bind(reference.sort_order)
        .execute(&mut **transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        Ok(())
    }

    async fn repair_local_nfo_defaults_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        repair: &LocalNfoDefaultsRepair<'_>,
    ) -> Result<(), StorageError> {
        if repair.provider_ids.is_empty() && repair.premiere_date.is_none() {
            return Ok(());
        }
        let current = self
            .query_as::<(Option<String>, Option<String>)>(
                "SELECT provider_ids_json, premiere_date
                 FROM media_items
                 WHERE id = ? AND removed_at IS NULL",
            )
            .bind(repair.item_id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let Some((current_provider_ids, current_premiere_date)) = current else {
            return Ok(());
        };

        let mut merged = current_provider_ids
            .as_deref()
            .and_then(|value| serde_json::from_str::<BTreeMap<String, String>>(value).ok())
            .unwrap_or_default();
        let mut provider_ids_json = None;
        for (provider, provider_id) in repair.provider_ids {
            let provider = provider.trim();
            let provider_id = provider_id.trim();
            if provider.is_empty()
                || provider_id.is_empty()
                || merged
                    .keys()
                    .any(|existing| existing.eq_ignore_ascii_case(provider))
            {
                continue;
            }
            merged.insert(provider.to_ascii_lowercase(), provider_id.to_owned());
            provider_ids_json = Some(
                serde_json::to_string(&merged)
                    .map_err(|error| StorageError::Serialization(error.to_string()))?,
            );
        }
        let premiere_date = repair
            .premiere_date
            .filter(|value| !value.trim().is_empty())
            .filter(|_| {
                current_premiere_date
                    .as_deref()
                    .is_none_or(|value| value.trim().is_empty())
            });
        if provider_ids_json.is_some() || premiere_date.is_some() {
            self.query(
                "UPDATE media_items
                 SET provider_ids_json = COALESCE(?, provider_ids_json),
                     premiere_date = COALESCE(?, premiere_date),
                     updated_at = unixepoch()
                 WHERE id = ? AND removed_at IS NULL",
            )
            .bind(provider_ids_json)
            .bind(premiere_date)
            .bind(repair.item_id)
            .execute(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        Ok(())
    }

    async fn update_media_item_metadata_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        update: &MediaMetadataUpdate<'_>,
        nfo_collection_reference_key: Option<&str>,
    ) -> Result<(), StorageError> {
        // The rating comparison binds the presence flag and a plain f64 (like the SET clause)
        // instead of an `Option<f64>`: PostgreSQL rejects the optional float in the repeated
        // `? IS NOT NULL` position ("incorrect binary data format in bind parameter 23"),
        // which failed every update of an item whose NFO carries a rating.
        let sort_title = update.title.to_lowercase();
        self.query(
            "UPDATE media_items
             SET title = ?,
                 sort_title = ?,
                 original_title = ?,
                 overview = ?,
                 production_year = ?,
                 premiere_date = COALESCE(?, premiere_date),
                 rating = CASE WHEN ? = 1 THEN ? ELSE rating END,
                 rating_source = CASE WHEN ? IS NULL THEN rating_source ELSE ? END,
                 provider_ids_json = COALESCE(?, provider_ids_json),
                 metadata_fingerprint = ?,
                 nfo_collection_reference_key = COALESCE(?, nfo_collection_reference_key),
                 metadata_provenance_json = ?,
                 locked_fields_json = ?,
                 updated_at = unixepoch()
             WHERE id = ?
               AND (
                   title IS DISTINCT FROM ?
                   OR sort_title IS DISTINCT FROM ?
                   OR original_title IS DISTINCT FROM ?
                   OR overview IS DISTINCT FROM ?
                   OR production_year IS DISTINCT FROM ?
                   OR (? IS NOT NULL AND premiere_date IS DISTINCT FROM ?)
                   OR (? = 1 AND rating IS DISTINCT FROM ?)
                   OR (? IS NOT NULL AND rating_source IS DISTINCT FROM ?)
                   OR (? IS NOT NULL AND provider_ids_json IS DISTINCT FROM ?)
                   OR metadata_fingerprint IS DISTINCT FROM ?
                   OR (? IS NOT NULL AND nfo_collection_reference_key IS DISTINCT FROM ?)
                   OR metadata_provenance_json IS DISTINCT FROM ?
                   OR locked_fields_json IS DISTINCT FROM ?
               )",
        )
        .bind(update.title)
        .bind(&sort_title)
        .bind(update.original_title)
        .bind(update.overview)
        .bind(update.production_year)
        .bind(update.premiere_date)
        .bind(database_flag(update.rating.is_some()))
        .bind(update.rating.unwrap_or_default())
        .bind(update.rating_source)
        .bind(update.rating_source)
        .bind(update.provider_ids_json)
        .bind(update.metadata_fingerprint)
        .bind(nfo_collection_reference_key)
        .bind(update.provenance_json)
        .bind(update.locked_fields_json)
        .bind(update.item_id)
        .bind(update.title)
        .bind(&sort_title)
        .bind(update.original_title)
        .bind(update.overview)
        .bind(update.production_year)
        .bind(update.premiere_date)
        .bind(update.premiere_date)
        .bind(database_flag(update.rating.is_some()))
        .bind(update.rating.unwrap_or_default())
        .bind(update.rating_source)
        .bind(update.rating_source)
        .bind(update.provider_ids_json)
        .bind(update.provider_ids_json)
        .bind(update.metadata_fingerprint)
        .bind(nfo_collection_reference_key)
        .bind(nfo_collection_reference_key)
        .bind(update.provenance_json)
        .bind(update.locked_fields_json)
        .execute(&mut **transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        Ok(())
    }

    pub(crate) async fn media_item_nfo_metadata_json(
        &self,
        item_id: &str,
    ) -> Result<Option<String>, StorageError> {
        self.query_scalar(
            "SELECT nfo_metadata_json
             FROM media_items
             WHERE id = ? AND nfo_metadata_json IS NOT NULL",
        )
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    /// Batch variant of [`Self::media_item_nfo_metadata_json`] for list endpoints.
    pub(crate) async fn media_item_nfo_metadata_json_batch(
        &self,
        item_ids: &[String],
    ) -> Result<std::collections::HashMap<String, String>, StorageError> {
        let mut cached = std::collections::HashMap::new();
        for chunk in item_ids.chunks(NFO_METADATA_BATCH_SIZE) {
            if chunk.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT id, nfo_metadata_json
                 FROM media_items
                 WHERE id IN ({placeholders}) AND nfo_metadata_json IS NOT NULL"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for item_id in chunk {
                statement = statement.bind(item_id);
            }
            let rows =
                statement
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
            for row in rows {
                cached.insert(row.get("id"), row.get("nfo_metadata_json"));
            }
        }
        Ok(cached)
    }

    // LUX-684 consumes these bounded candidates in the background index worker.
    #[cfg(test)]
    pub(crate) async fn media_content_restriction_index_candidates(
        &self,
        after_item_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MediaContentRestrictionIndexCandidate>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let bounded_limit = limit.min(CONTENT_RESTRICTION_INDEX_BATCH_LIMIT) as i64;
        let query = if after_item_id.is_some() {
            self.query_as(
                "SELECT item.id, item.nfo_metadata_json
                 FROM media_items item
                 LEFT JOIN media_content_restriction_index restriction_index
                   ON restriction_index.item_id = item.id
                 WHERE item.removed_at IS NULL
                   AND restriction_index.item_id IS NULL
                   AND item.id > ?
                 ORDER BY item.id
                 LIMIT ?",
            )
            .bind(after_item_id)
        } else {
            self.query_as(
                "SELECT item.id, item.nfo_metadata_json
                 FROM media_items item
                 LEFT JOIN media_content_restriction_index restriction_index
                   ON restriction_index.item_id = item.id
                 WHERE item.removed_at IS NULL
                   AND restriction_index.item_id IS NULL
                 ORDER BY item.id
                 LIMIT ?",
            )
        };
        let rows: Vec<(String, Option<String>)> = query
            .bind(bounded_limit)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(rows
            .into_iter()
            .map(
                |(item_id, nfo_metadata_json)| MediaContentRestrictionIndexCandidate {
                    item_id,
                    nfo_metadata_json,
                },
            )
            .collect())
    }

    /// Reads the durable queue filled by the media-item triggers. Empty queues
    /// cost one indexed lookup instead of a scan over every visible item.
    pub(crate) async fn media_content_restriction_index_queued_candidates(
        &self,
        limit: usize,
    ) -> Result<Vec<MediaContentRestrictionIndexCandidate>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let bounded_limit = limit.min(CONTENT_RESTRICTION_INDEX_BATCH_LIMIT) as i64;
        let rows: Vec<(String, Option<String>)> = self
            .query_as(
                "SELECT item.id, item.nfo_metadata_json
                 FROM media_content_restriction_index_queue queue
                 JOIN media_items item ON item.id = queue.item_id
                 WHERE item.removed_at IS NULL
                 ORDER BY queue.queued_at, queue.item_id
                 LIMIT ?",
            )
            .bind(bounded_limit)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(rows
            .into_iter()
            .map(
                |(item_id, nfo_metadata_json)| MediaContentRestrictionIndexCandidate {
                    item_id,
                    nfo_metadata_json,
                },
            )
            .collect())
    }

    pub(crate) async fn media_content_restriction_index_status(
        &self,
    ) -> Result<(i64, i64), StorageError> {
        self.query_as(
            "SELECT COUNT(*), COUNT(restriction_index.item_id)
             FROM media_items item
             LEFT JOIN media_content_restriction_index restriction_index
               ON restriction_index.item_id = item.id
             WHERE item.removed_at IS NULL",
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn media_content_restriction_classifications_for_item(
        &self,
        item_id: &str,
    ) -> Result<Vec<(Option<String>, Vec<String>, bool)>, StorageError> {
        let rows = self
            .query(
                "SELECT related.id AS related_item_id,
                        restriction_index.item_id AS indexed_item_id,
                        restriction_index.certification_code,
                        restriction_tag.tag
                 FROM media_items target
                 JOIN media_items related
                   ON related.id IN (target.id, target.parent_id, target.series_id)
                 LEFT JOIN media_content_restriction_index restriction_index
                   ON restriction_index.item_id = related.id
                 LEFT JOIN media_content_restriction_tags restriction_tag
                   ON restriction_tag.item_id = related.id
                 WHERE target.id = ? AND target.removed_at IS NULL
                   AND related.item_type IN ('MOVIE', 'SERIES', 'SEASON', 'EPISODE', 'VIDEO')
                 ORDER BY related.id, restriction_tag.tag",
            )
            .bind(item_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let mut classifications = BTreeMap::new();
        for row in rows {
            let related_item_id: String = row.get("related_item_id");
            let indexed_item_id: Option<String> = row.get("indexed_item_id");
            let certification_code: Option<String> = row.get("certification_code");
            let tag: Option<String> = row.get("tag");
            let classification = classifications.entry(related_item_id).or_insert_with(|| {
                (
                    certification_code,
                    Vec::<String>::new(),
                    indexed_item_id.is_some(),
                )
            });
            if let Some(tag) = tag
                && !classification.1.contains(&tag)
            {
                classification.1.push(tag);
            }
        }
        Ok(classifications.into_values().collect())
    }

    // LUX-684 consumes this compare-and-swap before persisting a projected index.
    #[allow(dead_code)]
    pub(crate) async fn replace_media_content_restriction_index_if_nfo_json_matches(
        &self,
        item_id: &str,
        expected_nfo_json: Option<&str>,
        projection: &ContentRestrictionIndexProjection,
    ) -> Result<bool, StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let snapshot_query = match self.backend() {
            DatabaseBackend::Sqlite => {
                "SELECT nfo_metadata_json FROM media_items
                 WHERE id = ? AND removed_at IS NULL"
            }
            DatabaseBackend::Postgres => {
                "SELECT nfo_metadata_json FROM media_items
                 WHERE id = ? AND removed_at IS NULL FOR UPDATE"
            }
        };
        let current: Option<(Option<String>,)> = self
            .query_as(snapshot_query)
            .bind(item_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let snapshot_matches = current
            .as_ref()
            .is_some_and(|(current_nfo_json,)| current_nfo_json.as_deref() == expected_nfo_json);
        if !snapshot_matches {
            transaction
                .commit()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(false);
        }

        self.query("DELETE FROM media_content_restriction_tags WHERE item_id = ?")
            .bind(item_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.query(
            "INSERT INTO media_content_restriction_index (item_id, certification_code)
             VALUES (?, ?)
             ON CONFLICT (item_id) DO UPDATE SET
                 certification_code = excluded.certification_code,
                 indexed_at = unixepoch()",
        )
        .bind(item_id)
        .bind(projection.certification_code.as_deref())
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        for tag in &projection.tags {
            self.query(
                "INSERT INTO media_content_restriction_tags (item_id, tag)
                 VALUES (?, ?)",
            )
            .bind(item_id)
            .bind(tag)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        self.query("DELETE FROM media_content_restriction_index_queue WHERE item_id = ?")
            .bind(item_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(true)
    }

    /// Applies one batch of prepared projections in a single transaction. The
    /// worker computes the projections from the NFO snapshots outside the write
    /// transaction; this method re-checks each snapshot and only then writes the
    /// index, tags and queue consumption together.
    pub(crate) async fn apply_content_restriction_index_batch(
        &self,
        writes: &[ContentRestrictionIndexWrite],
        invalid_item_ids: &[String],
    ) -> Result<ContentRestrictionIndexBatchOutcome, StorageError> {
        let mut outcome = ContentRestrictionIndexBatchOutcome::default();
        if writes.is_empty() && invalid_item_ids.is_empty() {
            return Ok(outcome);
        }
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        for item_id in invalid_item_ids {
            self.query("DELETE FROM media_content_restriction_index_queue WHERE item_id = ?")
                .bind(item_id)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }
        let snapshot_query = match self.backend() {
            DatabaseBackend::Sqlite => {
                "SELECT nfo_metadata_json, removed_at FROM media_items WHERE id = ?"
            }
            DatabaseBackend::Postgres => {
                "SELECT nfo_metadata_json, removed_at FROM media_items WHERE id = ? FOR UPDATE"
            }
        };
        for write in writes {
            let current: Option<(Option<String>, Option<i64>)> = self
                .query_as(snapshot_query)
                .bind(&write.item_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            let (current_nfo_json, removed_at) = match current {
                Some(row) => row,
                None => {
                    self.query(
                        "DELETE FROM media_content_restriction_index_queue WHERE item_id = ?",
                    )
                    .bind(&write.item_id)
                    .execute(&mut *transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                    outcome.removed += 1;
                    continue;
                }
            };
            if removed_at.is_some() {
                self.query("DELETE FROM media_content_restriction_index_queue WHERE item_id = ?")
                    .bind(&write.item_id)
                    .execute(&mut *transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                outcome.removed += 1;
                continue;
            }
            if current_nfo_json != write.expected_nfo_json {
                // The NFO changed after the worker read it; move the queued item
                // to the back so the next batch retries with a fresh snapshot.
                self.query(
                    "UPDATE media_content_restriction_index_queue
                     SET queued_at = unixepoch() WHERE item_id = ?",
                )
                .bind(&write.item_id)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
                outcome.stale += 1;
                continue;
            }
            self.query("DELETE FROM media_content_restriction_tags WHERE item_id = ?")
                .bind(&write.item_id)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            self.query(
                "INSERT INTO media_content_restriction_index (item_id, certification_code)
                 VALUES (?, ?)
                 ON CONFLICT (item_id) DO UPDATE SET
                     certification_code = excluded.certification_code,
                     indexed_at = unixepoch()",
            )
            .bind(&write.item_id)
            .bind(write.certification_code.as_deref())
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
            for tag in &write.tags {
                self.query(
                    "INSERT INTO media_content_restriction_tags (item_id, tag)
                     VALUES (?, ?)",
                )
                .bind(&write.item_id)
                .bind(tag)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            }
            self.query("DELETE FROM media_content_restriction_index_queue WHERE item_id = ?")
                .bind(&write.item_id)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            outcome.indexed += 1;
        }
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(outcome)
    }

    pub(crate) async fn media_item_nfo_metadata_snapshot(
        &self,
        item_id: &str,
    ) -> Result<Option<(String, Option<Vec<u8>>)>, StorageError> {
        self.query_as(
            "SELECT nfo_metadata_json, nfo_metadata_fingerprint
             FROM media_items
             WHERE id = ? AND nfo_metadata_json IS NOT NULL",
        )
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn media_item_nfo_metadata_state(
        &self,
        item_id: &str,
    ) -> Result<(bool, Option<Vec<u8>>), StorageError> {
        self.query_as(
            "SELECT CASE WHEN nfo_metadata_json IS NULL THEN 0 ELSE 1 END,
                    nfo_metadata_fingerprint
             FROM media_items
             WHERE id = ?",
        )
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row: Option<(i64, Option<Vec<u8>>)>| {
            row.map_or((false, None), |(has_snapshot, fingerprint)| {
                (has_snapshot != 0, fingerprint)
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn update_media_item_nfo_metadata(
        &self,
        item_id: &str,
        nfo_metadata_json: Option<&str>,
        source_fingerprint: Option<&[u8]>,
    ) -> Result<(), StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        self.query(
            "UPDATE media_items
             SET nfo_collection_reference_key = CASE
                     WHEN nfo_metadata_json IS DISTINCT FROM ? THEN NULL
                     ELSE nfo_collection_reference_key
                 END,
                 nfo_metadata_json = ?, nfo_metadata_fingerprint = ?,
                 updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(nfo_metadata_json)
        .bind(nfo_metadata_json)
        .bind(source_fingerprint)
        .bind(item_id)
        .execute(&mut *transaction)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn clear_media_item_nfo_metadata_if_json(
        &self,
        item_id: &str,
        expected_json: &str,
    ) -> Result<(), StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        self.query(
            "UPDATE media_items
             SET nfo_metadata_json = NULL, nfo_metadata_fingerprint = NULL,
                 updated_at = unixepoch()
             WHERE id = ? AND nfo_metadata_json = ?",
        )
        .bind(item_id)
        .bind(expected_json)
        .execute(&mut *transaction)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn mark_media_item_metadata_checked(
        &self,
        item_id: &str,
        metadata_fingerprint: &[u8],
        nfo_collection_reference_key: Option<&str>,
    ) -> Result<(), StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        self.query(
            "UPDATE media_items
             SET metadata_fingerprint = ?,
                 nfo_collection_reference_key = COALESCE(?, nfo_collection_reference_key),
                 updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(metadata_fingerprint)
        .bind(nfo_collection_reference_key)
        .bind(item_id)
        .execute(&mut *transaction)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn sync_media_item_nfo_state(
        &self,
        item_id: &str,
        source_fingerprint: &[u8],
        metadata_fingerprint: &[u8],
    ) -> Result<(), StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        self.query(
            "UPDATE media_items
             SET nfo_metadata_json = CASE
                     WHEN nfo_metadata_fingerprint IS NULL
                       OR nfo_metadata_fingerprint <> ? THEN NULL
                     ELSE nfo_metadata_json
                 END,
                 nfo_metadata_fingerprint = CASE
                     WHEN nfo_metadata_fingerprint IS NULL
                       OR nfo_metadata_fingerprint <> ? THEN NULL
                     ELSE nfo_metadata_fingerprint
                 END,
                 metadata_fingerprint = ?,
                 updated_at = CASE
                     WHEN nfo_metadata_fingerprint IS NULL
                       OR nfo_metadata_fingerprint <> ?
                       OR metadata_fingerprint IS NULL
                       OR metadata_fingerprint <> ? THEN unixepoch()
                     ELSE updated_at
                 END
             WHERE id = ?",
        )
        .bind(source_fingerprint)
        .bind(source_fingerprint)
        .bind(metadata_fingerprint)
        .bind(source_fingerprint)
        .bind(metadata_fingerprint)
        .bind(item_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn claim_metadata_image_attempt(
        &self,
        item_id: &str,
        image_type: &str,
        candidate_key: &str,
        now: i64,
        claimed_until: i64,
        force: bool,
    ) -> Result<bool, StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let result = self
            .query(
                "INSERT INTO metadata_image_attempts (
                item_id, image_type, candidate_key, status, attempt_count,
                last_attempt_at, next_retry_at, claimed_until, error_code, updated_at
            ) VALUES (?, ?, ?, 'RUNNING', 1, ?, NULL, ?, NULL, ?)
            ON CONFLICT(item_id, image_type, candidate_key) DO UPDATE SET
                status = 'RUNNING',
                attempt_count = metadata_image_attempts.attempt_count + 1,
                last_attempt_at = excluded.last_attempt_at,
                next_retry_at = NULL,
                claimed_until = excluded.claimed_until,
                error_code = NULL,
                updated_at = excluded.updated_at
             WHERE (
                    metadata_image_attempts.status <> 'RUNNING'
                    OR metadata_image_attempts.claimed_until IS NULL
                    OR metadata_image_attempts.claimed_until <= ?
               )
               AND (
                    ? = 1
                    OR (
                        metadata_image_attempts.status <> 'UNAVAILABLE'
                        AND (
                            metadata_image_attempts.status <> 'FAILED'
                            OR metadata_image_attempts.next_retry_at <= ?
                        )
                    )
               )",
            )
            .bind(item_id)
            .bind(image_type)
            .bind(candidate_key)
            .bind(now)
            .bind(claimed_until)
            .bind(now)
            .bind(now)
            .bind(database_flag(force))
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn metadata_image_attempt_count(
        &self,
        item_id: &str,
        image_type: &str,
        candidate_key: &str,
    ) -> Result<u32, StorageError> {
        let count = self
            .query_scalar::<i64>(
                "SELECT attempt_count
                 FROM metadata_image_attempts
                 WHERE item_id = ? AND image_type = ? AND candidate_key = ?",
            )
            .bind(item_id)
            .bind(image_type)
            .bind(candidate_key)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .unwrap_or(1);
        Ok(u32::try_from(count.max(1)).unwrap_or(u32::MAX))
    }

    pub(crate) async fn mark_metadata_images_unavailable(
        &self,
        item_id: &str,
        unavailable: &[MetadataImageUnavailable<'_>],
        now: i64,
    ) -> Result<(), StorageError> {
        if unavailable.is_empty() {
            return Ok(());
        }
        let mut unique = Vec::with_capacity(unavailable.len());
        let mut seen = std::collections::HashSet::with_capacity(unavailable.len());
        for image in unavailable {
            if seen.insert((image.image_type, image.candidate_key)) {
                unique.push(image);
            }
        }

        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        for chunk in unique.chunks(100) {
            let values = std::iter::repeat_n(
                "(?, ?, ?, 'UNAVAILABLE', 1, ?, NULL, NULL, 'NO_IMAGE', ?)",
                chunk.len(),
            )
            .collect::<Vec<_>>()
            .join(", ");
            let query = format!(
                "INSERT INTO metadata_image_attempts (
                    item_id, image_type, candidate_key, status, attempt_count,
                    last_attempt_at, next_retry_at, claimed_until, error_code, updated_at
                ) VALUES {values}
                ON CONFLICT(item_id, image_type, candidate_key) DO UPDATE SET
                    status = 'UNAVAILABLE',
                    attempt_count = CASE
                        WHEN metadata_image_attempts.attempt_count < 1 THEN 1
                        ELSE metadata_image_attempts.attempt_count
                    END,
                    last_attempt_at = excluded.last_attempt_at,
                    next_retry_at = NULL,
                    claimed_until = NULL,
                    error_code = 'NO_IMAGE',
                    updated_at = excluded.updated_at"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for image in chunk {
                statement = statement
                    .bind(item_id)
                    .bind(image.image_type)
                    .bind(image.candidate_key)
                    .bind(now)
                    .bind(now);
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

    pub(crate) async fn finish_metadata_image_attempt(
        &self,
        update: MetadataImageAttemptUpdate<'_>,
    ) -> Result<bool, StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let result = self
            .query(
                "UPDATE metadata_image_attempts
             SET status = ?, next_retry_at = ?, claimed_until = NULL,
                 error_code = ?, updated_at = ?
             WHERE item_id = ? AND image_type = ? AND candidate_key = ?
               AND status = 'RUNNING'",
            )
            .bind(update.status)
            .bind(update.next_retry_at)
            .bind(update.error_code)
            .bind(update.now)
            .bind(update.item_id)
            .bind(update.image_type)
            .bind(update.candidate_key)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn insert_item_images_batch_at_indices(
        &self,
        items: &[ItemImageBatchInsert],
    ) -> Result<usize, StorageError> {
        const MAX_ITEMS_PER_BATCH: usize = 16;
        const MAX_IMAGE_ROWS_PER_STATEMENT: usize = 64;

        if items.is_empty() {
            return Ok(0);
        }
        if items.len() > MAX_ITEMS_PER_BATCH {
            return Err(StorageError::Conflict(
                "local image item batch exceeds the storage limit".to_owned(),
            ));
        }
        let mut item_ids = std::collections::HashSet::with_capacity(items.len());
        let mut image_keys = std::collections::HashSet::new();
        let mut image_count = 0_usize;
        for item in items {
            if item.item_id.trim().is_empty() || !item_ids.insert(item.item_id.as_str()) {
                return Err(StorageError::Conflict(
                    "local image item batch contains an invalid or duplicate item id".to_owned(),
                ));
            }
            for image in &item.images {
                if image.image_type.trim().is_empty()
                    || image.image_index < 0
                    || !image_keys.insert((
                        item.item_id.as_str(),
                        image.image_type.as_str(),
                        image.image_index,
                    ))
                {
                    return Err(StorageError::Conflict(
                        "local image batch contains an invalid or duplicate image key".to_owned(),
                    ));
                }
                image_count = image_count.saturating_add(1);
            }
        }
        if image_count == 0 && !items.iter().any(|item| item.clear_poster_fallback) {
            return Ok(0);
        }

        let mut image_rows = Vec::with_capacity(image_count);
        for item in items {
            image_rows.extend(item.images.iter().map(|image| (&item.item_id, image)));
        }
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let mut inserted_count = 0_usize;
        for batch in image_rows.chunks(MAX_IMAGE_ROWS_PER_STATEMENT) {
            let values = std::iter::repeat_n("(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)", batch.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "INSERT INTO item_images (
                    id, item_id, image_type, image_index, local_path, width, height,
                    file_size, content_tag, source, source_url
                ) VALUES {values}
                ON CONFLICT(item_id, image_type, image_index) DO UPDATE SET
                    id = excluded.id,
                    local_path = excluded.local_path,
                    width = excluded.width,
                    height = excluded.height,
                    file_size = excluded.file_size,
                    content_tag = excluded.content_tag,
                    source = excluded.source,
                    source_url = excluded.source_url,
                    updated_at = unixepoch()
                WHERE item_images.local_path <> excluded.local_path
                   OR COALESCE(item_images.content_tag, '') <> COALESCE(excluded.content_tag, '')
                   OR COALESCE(item_images.width, -1) <> COALESCE(excluded.width, -1)
                   OR COALESCE(item_images.height, -1) <> COALESCE(excluded.height, -1)
                   OR item_images.source <> excluded.source
                   OR COALESCE(item_images.source_url, '') <> COALESCE(excluded.source_url, '')"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for (item_id, image) in batch {
                statement = statement
                    .bind(Uuid::now_v7().to_string())
                    .bind(item_id.as_str())
                    .bind(&image.image_type)
                    .bind(image.image_index)
                    .bind(&image.local_path)
                    .bind(image.width)
                    .bind(image.height)
                    .bind(image.file_size)
                    .bind(&image.content_tag)
                    .bind(&image.source)
                    .bind(image.source_url.as_deref());
            }
            let result = statement
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            inserted_count = inserted_count.saturating_add(result.rows_affected() as usize);
        }

        let fallback_item_ids = items
            .iter()
            .filter(|item| item.clear_poster_fallback)
            .map(|item| item.item_id.as_str())
            .collect::<Vec<_>>();
        if !fallback_item_ids.is_empty() {
            let placeholders = std::iter::repeat_n("?", fallback_item_ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "UPDATE media_items SET poster_fallback_required = 0
                 WHERE id IN ({placeholders}) AND poster_fallback_required <> 0"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for item_id in fallback_item_ids {
                statement = statement.bind(item_id);
            }
            if let Err(source) = statement.execute(&mut *transaction).await {
                Self::rollback_write_transaction(transaction, "write failed").await;
                return Err(StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                });
            }
        }

        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(inserted_count)
    }

    #[allow(dead_code)] // Storage tests seed fallback state before exercising batch writes.
    pub(crate) async fn set_poster_fallback_required(
        &self,
        item_id: &str,
        required: bool,
    ) -> Result<(), StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        self.query(
            "UPDATE media_items
             SET poster_fallback_required = ?
             WHERE id = ?",
        )
        .bind(database_flag(required))
        .bind(item_id)
        .execute(&mut *transaction)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn upsert_item_image_at_index(
        &self,
        item_id: &str,
        image_type: &str,
        image_index: i64,
        local_path: &std::path::Path,
        metadata: ItemImageMetadata<'_>,
    ) -> Result<String, StorageError> {
        let id = Uuid::now_v7().to_string();
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        self.query(
            "INSERT INTO item_images (
                id, item_id, image_type, image_index, local_path, width, height,
                file_size, content_tag, source, source_url
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(item_id, image_type, image_index) DO UPDATE SET
                id = excluded.id,
                local_path = excluded.local_path,
                width = excluded.width,
                height = excluded.height,
                file_size = excluded.file_size,
                content_tag = excluded.content_tag,
                source = excluded.source,
                source_url = excluded.source_url,
                updated_at = unixepoch()",
        )
        .bind(&id)
        .bind(item_id)
        .bind(image_type)
        .bind(image_index)
        .bind(local_path.to_string_lossy().as_ref())
        .bind(metadata.width)
        .bind(metadata.height)
        .bind(metadata.file_size)
        .bind(metadata.content_tag)
        .bind(metadata.source)
        .bind(metadata.source_url)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(id)
    }

    pub(crate) async fn list_item_image_candidates(
        &self,
        item_id: &str,
        image_type: &str,
        image_index: i64,
    ) -> Result<Vec<StoredItemImageCandidate>, StorageError> {
        self.query(
            "SELECT ii.id, ii.image_type, ii.local_path, lr.canonical_path AS root_path,
                    ii.content_tag, ii.width, ii.height
             FROM item_images ii
             JOIN media_items mi ON mi.id = ii.item_id
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             JOIN library_roots lr ON lr.library_id = mi.library_id
             WHERE ii.item_id = ? AND ii.image_type = ? AND ii.image_index = ?
             ORDER BY lr.canonical_path",
        )
        .bind(item_id)
        .bind(image_type)
        .bind(image_index)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredItemImageCandidate {
                    id: row.get("id"),
                    image_type: row.get("image_type"),
                    local_path: row.get("local_path"),
                    root_path: row.get("root_path"),
                    content_tag: row.get("content_tag"),
                    width: row.get("width"),
                    height: row.get("height"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_existing_item_image_source_urls(
        &self,
        item_id: &str,
        image_type: &str,
        source_urls: &[String],
    ) -> Result<std::collections::HashSet<String>, StorageError> {
        const MAX_SOURCE_URLS_PER_QUERY: usize = 100;

        let mut existing = std::collections::HashSet::new();
        for chunk in source_urls.chunks(MAX_SOURCE_URLS_PER_QUERY) {
            let unique_urls = chunk
                .iter()
                .map(String::as_str)
                .collect::<std::collections::HashSet<_>>();
            if unique_urls.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", unique_urls.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT source_url FROM item_images
                 WHERE item_id = ? AND image_type = ?
                   AND source_url IN ({placeholders})"
            );
            let mut statement = self.query_scalar::<String>(sqlx::AssertSqlSafe(query));
            statement = statement.bind(item_id).bind(image_type);
            for source_url in unique_urls {
                statement = statement.bind(source_url);
            }
            existing.extend(statement.fetch_all(&self.pool).await.map_err(|source| {
                StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                }
            })?);
        }
        Ok(existing)
    }

    pub(crate) async fn list_item_images(
        &self,
        item_id: &str,
    ) -> Result<Vec<StoredItemImage>, StorageError> {
        self.query(
            "SELECT ii.id, ii.item_id, ii.image_type, ii.image_index,
                    ii.local_path, ii.file_size, ii.content_tag, ii.source,
                    MIN(lr.canonical_path) AS root_path
             FROM item_images ii
             JOIN media_items mi ON mi.id = ii.item_id
             LEFT JOIN library_roots lr ON lr.library_id = mi.library_id
             WHERE ii.item_id = ?
             GROUP BY ii.id
             ORDER BY ii.image_type, ii.image_index, ii.id",
        )
        .bind(item_id)
        .fetch_all(&self.pool)
        .await
        .map(|rows| rows.into_iter().map(stored_item_image).collect())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_item_images_by_ids(
        &self,
        item_ids: &[String],
    ) -> Result<HashMap<String, Vec<StoredItemImage>>, StorageError> {
        let mut images_by_item = HashMap::<String, Vec<StoredItemImage>>::new();
        for chunk in item_ids.chunks(500) {
            if chunk.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT ii.id, ii.item_id, ii.image_type, ii.image_index,
                        ii.local_path, ii.file_size, ii.content_tag, ii.source,
                        MIN(lr.canonical_path) AS root_path
                 FROM item_images ii
                 JOIN media_items mi ON mi.id = ii.item_id
                 LEFT JOIN library_roots lr ON lr.library_id = mi.library_id
                 WHERE ii.item_id IN ({placeholders})
                 GROUP BY ii.id
                 ORDER BY ii.item_id, ii.image_type, ii.image_index, ii.id"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for item_id in chunk {
                statement = statement.bind(item_id);
            }
            let rows =
                statement
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
            for row in rows {
                let image = stored_item_image(row);
                images_by_item
                    .entry(image.item_id.clone())
                    .or_default()
                    .push(image);
            }
        }
        Ok(images_by_item)
    }

    pub(crate) async fn list_item_image_path_conflicts(
        &self,
    ) -> Result<Vec<StoredItemImagePathConflict>, StorageError> {
        self.query(
            "SELECT ii.id, ii.item_id, ii.image_type, ii.local_path
             FROM item_images ii
             JOIN media_items mi ON mi.id = ii.item_id
             WHERE mi.item_type = 'EPISODE'
               AND ii.image_type IN ('FANART', 'THUMB')
               AND EXISTS (
                   SELECT 1
                   FROM item_images other
                   WHERE other.item_id = ii.item_id
                     AND other.local_path = ii.local_path
                     AND other.image_type IN ('FANART', 'THUMB')
                     AND other.image_type <> ii.image_type
               )
             ORDER BY ii.item_id, ii.local_path, ii.image_type, ii.image_index, ii.id",
        )
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredItemImagePathConflict {
                    id: row.get("id"),
                    item_id: row.get("item_id"),
                    image_type: row.get("image_type"),
                    local_path: row.get("local_path"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn item_image_path_is_in_use(
        &self,
        item_id: &str,
        local_path: &std::path::Path,
        excluded_image_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar::<i64>(
            "SELECT CASE WHEN EXISTS (
                 SELECT 1
                 FROM item_images
                 WHERE item_id = ? AND local_path = ? AND id <> ?
             ) THEN 1 ELSE 0 END",
        )
        .bind(item_id)
        .bind(local_path.to_string_lossy().as_ref())
        .bind(excluded_image_id)
        .fetch_one(&self.pool)
        .await
        .map(|value| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn update_item_image_local_path(
        &self,
        image_id: &str,
        local_path: &std::path::Path,
    ) -> Result<bool, StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let result = self
            .query(
                "UPDATE item_images
                 SET local_path = ?, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(local_path.to_string_lossy().as_ref())
            .bind(image_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn find_primary_image_dimensions(
        &self,
        item_id: &str,
    ) -> Result<Option<(i32, i32)>, StorageError> {
        self.query(
            "SELECT width, height
             FROM item_images
             WHERE item_id = ? AND image_type = 'POSTER' AND image_index = 0
               AND width IS NOT NULL AND height IS NOT NULL",
        )
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(|row| (row.get("width"), row.get("height"))))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn set_item_image_dimensions(
        &self,
        item_id: &str,
        image_type: &str,
        image_index: i64,
        width: i32,
        height: i32,
    ) -> Result<(), StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        self.query(
            "UPDATE item_images
             SET width = ?, height = ?, updated_at = unixepoch()
             WHERE item_id = ? AND image_type = ? AND image_index = ?",
        )
        .bind(width)
        .bind(height)
        .bind(item_id)
        .bind(image_type)
        .bind(image_index)
        .execute(&mut *transaction)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_catalog_image_tags_by_ids(
        &self,
        item_ids: &[String],
    ) -> Result<HashMap<String, Vec<StoredCatalogImageTag>>, StorageError> {
        let mut tags = HashMap::with_capacity(item_ids.len());
        for item_ids in item_ids.chunks(500) {
            if item_ids.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", item_ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT item_id, id, image_type, image_index
                 FROM item_images
                 WHERE item_id IN ({placeholders})
                 ORDER BY item_id, image_type, image_index, id"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for item_id in item_ids {
                statement = statement.bind(item_id);
            }
            let rows =
                statement
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
            for row in rows {
                let item_id: String = row.get("item_id");
                tags.entry(item_id.clone())
                    .or_insert_with(Vec::new)
                    .push(StoredCatalogImageTag {
                        id: row.get("id"),
                        image_type: row.get("image_type"),
                        image_index: row.get("image_index"),
                    });
            }
        }
        Ok(tags)
    }

    pub(crate) async fn item_image_path_is_shared(
        &self,
        local_path: &str,
        image_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS (
                 SELECT 1 FROM item_images
                 WHERE local_path = ? AND id <> ?
             ) THEN 1 ELSE 0 END",
        )
        .bind(local_path)
        .bind(image_id)
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_random_library_poster_paths(
        &self,
        library_id: &str,
        limit: i64,
    ) -> Result<Vec<StoredLibraryPoster>, StorageError> {
        self.query(
            "SELECT ii.item_id, ii.local_path, lr.canonical_path AS root_path
             FROM item_images ii
             JOIN media_items mi ON mi.id = ii.item_id
             JOIN library_roots lr ON lr.library_id = mi.library_id
             WHERE mi.library_id = ?
               AND mi.removed_at IS NULL
               AND ii.image_type = 'POSTER'
               AND ii.image_index = 0
             GROUP BY ii.item_id, ii.local_path, lr.canonical_path
             ORDER BY random()
             LIMIT ?",
        )
        .bind(library_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredLibraryPoster {
                    item_id: row.get("item_id"),
                    local_path: row.get("local_path"),
                    root_path: row.get("root_path"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_item_image(
        &self,
        item_id: &str,
        image_id: &str,
    ) -> Result<Option<StoredItemImage>, StorageError> {
        self.query(
            "SELECT ii.id, ii.item_id, ii.image_type, ii.image_index,
                    ii.local_path, ii.file_size, ii.content_tag, ii.source,
                    MIN(lr.canonical_path) AS root_path
             FROM item_images ii
             JOIN media_items mi ON mi.id = ii.item_id
             LEFT JOIN library_roots lr ON lr.library_id = mi.library_id
             WHERE ii.item_id = ? AND ii.id = ?
             GROUP BY ii.id",
        )
        .bind(item_id)
        .bind(image_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_item_image))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn delete_item_image(
        &self,
        item_id: &str,
        image_id: &str,
    ) -> Result<bool, StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let result = self
            .query("DELETE FROM item_images WHERE item_id = ? AND id = ?")
            .bind(item_id)
            .bind(image_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn create_access_token(
        &self,
        token: NewAccessToken<'_>,
    ) -> Result<(), StorageError> {
        let expires_at = current_unix_timestamp().saturating_add(EMBY_ACCESS_TOKEN_TTL_SECONDS);
        self.query(
            "INSERT INTO access_tokens (
                id, token_hash, user_id, device_id, client_name,
                device_name, client_version, device_type, expires_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(token.id)
        .bind(token.token_hash)
        .bind(token.user_id)
        .bind(token.device_id)
        .bind(token.client_name)
        .bind(token.device_name)
        .bind(token.client_version)
        .bind(token.device_type)
        .bind(expires_at)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn revoke_access_token(&self, token_hash: &[u8]) -> Result<(), StorageError> {
        self.query(
            "UPDATE access_tokens
             SET revoked_at = unixepoch(), updated_at = unixepoch()
             WHERE token_hash = ? AND revoked_at IS NULL",
        )
        .bind(token_hash)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn has_valid_access_token(
        &self,
        token_hash: &[u8],
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(
                SELECT 1 FROM access_tokens
                WHERE token_hash = ? AND revoked_at IS NULL
                  AND (expires_at IS NULL OR expires_at > unixepoch())
            ) THEN 1 ELSE 0 END",
        )
        .bind(token_hash)
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn create_web_session(
        &self,
        id: &str,
        user_id: &str,
        session_token_hash: &[u8],
        csrf_token_hash: &[u8],
        lifetime_seconds: i64,
    ) -> Result<(), StorageError> {
        self.create_web_session_with_device(
            id,
            user_id,
            session_token_hash,
            csrf_token_hash,
            None,
            lifetime_seconds,
        )
        .await
    }

    pub(crate) async fn create_web_session_with_device(
        &self,
        id: &str,
        user_id: &str,
        session_token_hash: &[u8],
        csrf_token_hash: &[u8],
        device_id: Option<&str>,
        lifetime_seconds: i64,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO web_sessions (
                id, user_id, session_token_hash, csrf_token_hash, expires_at, device_id
            ) VALUES (?, ?, ?, ?, unixepoch() + ?, ?)",
        )
        .bind(id)
        .bind(user_id)
        .bind(session_token_hash)
        .bind(csrf_token_hash)
        .bind(lifetime_seconds)
        .bind(device_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_web_session(
        &self,
        session_token_hash: &[u8],
    ) -> Result<Option<StoredWebSession>, StorageError> {
        self.query(
            "SELECT ws.csrf_token_hash, ws.device_id, u.id AS user_id,
                    u.username_normalized, u.display_name, u.has_password,
                    u.is_disabled, u.is_admin, u.can_manage_server,
                    u.can_remote_access, u.can_download, u.last_login_at,
                    COALESCE(
                        (SELECT MAX(COALESCE(at.last_seen_at, at.created_at))
                         FROM access_tokens at WHERE at.user_id = u.id),
                        u.last_login_at
                    ) AS last_activity_at
             FROM web_sessions ws
             JOIN users u ON u.id = ws.user_id
             WHERE ws.session_token_hash = ?
               AND ws.revoked_at IS NULL
               AND ws.expires_at > unixepoch()",
        )
        .bind(session_token_hash)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredWebSession {
                csrf_token_hash: row.get("csrf_token_hash"),
                device_id: row.get("device_id"),
                user_id: row.get("user_id"),
                username_normalized: row.get("username_normalized"),
                display_name: row.get("display_name"),
                has_password: row.get::<i64, _>("has_password") != 0,
                is_disabled: row.get::<i64, _>("is_disabled") != 0,
                is_admin: row.get::<i64, _>("is_admin") != 0,
                can_manage_server: row.get::<i64, _>("can_manage_server") != 0,
                can_remote_access: row.get::<i64, _>("can_remote_access") != 0,
                can_download: row.get::<i64, _>("can_download") != 0,
                last_login_at: row.get("last_login_at"),
                last_activity_at: row.get("last_activity_at"),
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn revoke_web_session(
        &self,
        session_token_hash: &[u8],
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE web_sessions
             SET revoked_at = unixepoch(), updated_at = unixepoch()
             WHERE session_token_hash = ? AND revoked_at IS NULL",
        )
        .bind(session_token_hash)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    /// Revokes every active web session of a user, optionally keeping the caller's own session.
    pub(crate) async fn revoke_all_web_sessions_for_user(
        &self,
        user_id: &str,
        keep_session_token_hash: Option<&[u8]>,
    ) -> Result<u64, StorageError> {
        let statement = match keep_session_token_hash {
            Some(keep) => self
                .query(
                    "UPDATE web_sessions
                     SET revoked_at = unixepoch(), updated_at = unixepoch()
                     WHERE user_id = ? AND revoked_at IS NULL AND session_token_hash <> ?",
                )
                .bind(user_id)
                .bind(keep),
            None => self
                .query(
                    "UPDATE web_sessions
                     SET revoked_at = unixepoch(), updated_at = unixepoch()
                     WHERE user_id = ? AND revoked_at IS NULL",
                )
                .bind(user_id),
        };
        statement
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_web_session_summaries(
        &self,
        user_id: &str,
        current_session_token_hash: &[u8],
    ) -> Result<Vec<StoredWebSessionSummary>, StorageError> {
        self.query(
            "SELECT id, created_at, updated_at, expires_at, last_seen_at,
                    session_token_hash = ? AS is_current
             FROM web_sessions
             WHERE user_id = ? AND revoked_at IS NULL AND expires_at > unixepoch()
             ORDER BY updated_at DESC, id DESC",
        )
        .bind(current_session_token_hash)
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredWebSessionSummary {
                    id: row.get("id"),
                    created_at: row.get("created_at"),
                    updated_at: row.get("updated_at"),
                    expires_at: row.get("expires_at"),
                    last_seen_at: row.get("last_seen_at"),
                    is_current: row.get::<i64, _>("is_current") != 0,
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_web_session_summaries_page(
        &self,
        user_id: &str,
        current_session_token_hash: &[u8],
        page_size: i64,
        offset: i64,
    ) -> Result<(Vec<StoredWebSessionSummary>, i64), StorageError> {
        let page_size = page_size.clamp(1, 100);
        let offset = offset.max(0);
        let total_count = self
            .query_scalar(
                "SELECT COUNT(*) FROM web_sessions
                 WHERE user_id = ? AND revoked_at IS NULL AND expires_at > unixepoch()",
            )
            .bind(user_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let sessions = self
            .query(
                "SELECT id, created_at, updated_at, expires_at, last_seen_at,
                        session_token_hash = ? AS is_current
                 FROM web_sessions
                 WHERE user_id = ? AND revoked_at IS NULL AND expires_at > unixepoch()
                 ORDER BY updated_at DESC, id DESC
                 LIMIT ? OFFSET ?",
            )
            .bind(current_session_token_hash)
            .bind(user_id)
            .bind(page_size)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(|row| StoredWebSessionSummary {
                        id: row.get("id"),
                        created_at: row.get("created_at"),
                        updated_at: row.get("updated_at"),
                        expires_at: row.get("expires_at"),
                        last_seen_at: row.get("last_seen_at"),
                        is_current: row.get::<i64, _>("is_current") != 0,
                    })
                    .collect()
            })
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok((sessions, total_count))
    }

    pub(crate) async fn revoke_web_session_by_id(
        &self,
        user_id: &str,
        session_id: &str,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE web_sessions
             SET revoked_at = unixepoch(), updated_at = unixepoch()
             WHERE id = ? AND user_id = ? AND revoked_at IS NULL
               AND expires_at > unixepoch()",
        )
        .bind(session_id)
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn is_active_web_session_token(
        &self,
        user_id: &str,
        session_id: &str,
        session_token_hash: &[u8],
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(
                SELECT 1 FROM web_sessions
                WHERE user_id = ? AND id = ? AND session_token_hash = ?
                  AND revoked_at IS NULL AND expires_at > unixepoch()
            ) THEN 1 ELSE 0 END",
        )
        .bind(user_id)
        .bind(session_id)
        .bind(session_token_hash)
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }
}

fn filtered_catalog_list_query<'a>(
    filter: &CatalogFilterQuery<'a>,
) -> (String, Vec<CatalogBind<'a>>) {
    let (where_clause, binds) = catalog_filter_where_clause(filter);
    let item_order = catalog_item_order(filter.sort_by, filter.descending);
    let fetch_limit = if filter.include_total {
        filter.limit
    } else {
        filter.limit.saturating_add(1)
    };
    if let Some((selected_sql, union_binds)) =
        catalog_list_union_selected_sql(filter, &where_clause, &binds, item_order, fetch_limit)
    {
        return (
            catalog_list_query_from_selected(&selected_sql, item_order),
            union_binds,
        );
    }
    let selected_sql = format!(
        "SELECT mi.id
         FROM media_items mi
         JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
         {where_clause}
         ORDER BY {item_order}
         LIMIT ? OFFSET ?"
    );
    let mut binds = binds;
    binds.push(CatalogBind::Integer(fetch_limit));
    binds.push(CatalogBind::Integer(filter.offset));
    (
        catalog_list_query_from_selected(&selected_sql, item_order),
        binds,
    )
}

fn catalog_list_query_from_selected(selected_sql: &str, item_order: &str) -> String {
    format!(
        "SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                mi.title, mi.sort_title, mi.original_title, mi.overview,
                mi.production_year, mi.rating, mi.rating_source, mi.runtime_ticks,
                mi.added_at, mi.updated_at,
                (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                 ORDER BY image_index LIMIT 1) AS poster_image_tag,
                (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                 ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'THUMB'
                 ORDER BY image_index LIMIT 1) AS thumb_image_tag,
                (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'LOGO'
                 ORDER BY image_index LIMIT 1) AS logo_image_tag,
                (SELECT fe.relative_path FROM filesystem_entries fe
                 WHERE fe.id = ms.filesystem_entry_id) AS source_relative_path,
                ms.id AS source_id, ms.source_kind, ms.container, ms.size, ms.external_url,
                ms.edition_name, ms.quality_label,
                ms.bitrate, ms.duration_ticks, ms.is_default, ms.probe_status,
                mt.id AS stream_id, mt.stream_index, mt.stream_type,
                mt.codec, mt.language, mt.title AS stream_title,
                mt.details_json AS stream_details_json,
                mt.is_external AS stream_is_external,
                mt.is_default AS stream_is_default,
                mt.is_forced AS stream_is_forced
         FROM ({selected_sql}) selected
         JOIN media_items mi ON mi.id = selected.id
         LEFT JOIN media_sources ms
           ON ms.item_id = mi.id
          AND EXISTS (
              SELECT 1 FROM filesystem_entries fe
              WHERE fe.id = ms.filesystem_entry_id AND fe.is_missing = 0
          )
         LEFT JOIN media_streams mt ON mt.media_source_id = ms.id
         ORDER BY {item_order}, ms.id, mt.stream_index"
    )
}

/// For the default "recently added" view over multiple libraries, each library
/// contributes at most `offset + limit` rows to the global page. Querying each
/// library separately keeps the existing per-library sort index usable and
/// avoids sorting the whole catalog. Other filters keep the single-query plan.
fn catalog_list_union_selected_sql<'a>(
    filter: &CatalogFilterQuery<'a>,
    where_clause: &str,
    binds: &[CatalogBind<'a>],
    item_order: &str,
    fetch_limit: i64,
) -> Option<(String, Vec<CatalogBind<'a>>)> {
    if filter.library_ids.len() <= 1
        || filter.offset > 200
        || filter.parent_id_scope.is_some()
        || filter.item_ids.is_some()
        || filter.person_id.is_some()
        || filter.media_source_ids.is_some()
        || filter.provider_id_equals.is_some()
        || !filter.excluded_item_types.is_empty()
        || !filter.years.is_empty()
        || filter.is_played.is_some()
        || filter.is_favorite.is_some()
        || filter.min_date_last_saved.is_some()
        || filter.metadata_pending
        || filter.sort_by != CatalogSort::DateCreated
        || !filter.descending
    {
        return None;
    }
    let library_count = filter.library_ids.len();
    let rest_binds = binds.get(library_count..)?.to_vec();
    let marker = "AND mi.library_id IN (";
    let start = where_clause.find(marker)?;
    let values_start = start + marker.len();
    let values_end = where_clause[values_start..].find(')')? + values_start;
    let branch_where = format!(
        "{}AND mi.library_id = ?{}",
        &where_clause[..start],
        &where_clause[values_end + 1..]
    );
    let branch_limit = filter.offset.saturating_add(fetch_limit);
    let mut branches = Vec::with_capacity(library_count);
    let mut union_binds = Vec::with_capacity(library_count * (rest_binds.len() + 2) + 2);
    for (index, library_id) in filter.library_ids.iter().enumerate() {
        // The branch columns carry the sort keys so the union can be ordered and paged
        // globally before the per-source detail join runs.
        branches.push(format!(
            "SELECT * FROM (
                 SELECT mi.id AS id, mi.added_at AS added_at, mi.title AS title
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 {branch_where}
                 ORDER BY {item_order}
                 LIMIT ? OFFSET 0
             ) AS branch_{index}"
        ));
        union_binds.push(CatalogBind::Text(library_id.as_str()));
        union_binds.extend(rest_binds.iter().copied());
        union_binds.push(CatalogBind::Integer(branch_limit));
    }
    // Page on item ids, not on the joined media_source/media_stream rows: one item can expand
    // into several detail rows, so the row-level LIMIT would drop items from a full page.
    let paged_sql = format!(
        "SELECT paged.id
         FROM (
             {}
         ) paged
         ORDER BY paged.added_at DESC, LOWER(paged.title) ASC, paged.id ASC
         LIMIT ? OFFSET ?",
        branches.join("\nUNION ALL\n")
    );
    union_binds.push(CatalogBind::Integer(fetch_limit));
    union_binds.push(CatalogBind::Integer(filter.offset));
    Some((paged_sql, union_binds))
}

fn global_generated_identity_key(kind: &str, provider: &str, provider_id: &str) -> String {
    format!(
        "lux-global-{kind}:{}:{provider}:{}:{provider_id}",
        provider.len(),
        provider_id.len()
    )
}

/// Restricts a query over `filesystem_entries fe` to the entries touched by the paths of one
/// incremental scan job (one `?` placeholder: the job id).
///
/// SQLite keeps the correlated `EXISTS`. PostgreSQL cannot use an index for the
/// `substr(...) = path || '/'` form and evaluated it for every candidate entry (the STRM probe
/// target count averaged about a minute on a library of several hundred thousand files), so there
/// the matching entries are collected per scanned path with byte-order range operators served by
/// `idx_filesystem_entries_dir_prefix` (same approach as the sidecar target lookup).
fn incremental_scan_scope_sql(backend: DatabaseBackend) -> &'static str {
    if backend == DatabaseBackend::Postgres {
        "fe.id IN (
                   SELECT matched.id
                   FROM scan_job_paths sjp
                   CROSS JOIN LATERAL (
                       SELECT entry.id FROM filesystem_entries entry
                       WHERE sjp.relative_path = '.'
                         AND entry.library_root_id = sjp.library_root_id
                         AND entry.entry_kind = 'FILE' AND entry.is_missing = 0
                       UNION ALL
                       SELECT entry.id FROM filesystem_entries entry
                       WHERE entry.library_root_id = sjp.library_root_id
                         AND entry.relative_path = sjp.relative_path
                         AND entry.entry_kind = 'FILE' AND entry.is_missing = 0
                       UNION ALL
                       SELECT entry.id FROM filesystem_entries entry
                       WHERE entry.library_root_id = sjp.library_root_id
                         AND entry.entry_kind = 'FILE' AND entry.is_missing = 0
                         AND entry.relative_path ~>=~ (sjp.relative_path || '/')
                         AND entry.relative_path ~<~ (sjp.relative_path || '0')
                   ) matched
                   WHERE sjp.job_id = ? AND sjp.processed_at IS NOT NULL
               )"
    } else {
        "EXISTS (
                   SELECT 1 FROM scan_job_paths sjp
                   WHERE sjp.job_id = ? AND sjp.processed_at IS NOT NULL
                     AND sjp.library_root_id = fe.library_root_id
                     AND (
                           sjp.relative_path = '.'
                           OR fe.relative_path = sjp.relative_path
                           OR substr(fe.relative_path, 1, length(sjp.relative_path) + 1)
                              = sjp.relative_path || '/'
                         )
               )"
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Database, GLOBAL_COLLECTION_LIBRARY_ID, RECOMMENDATION_STATS_CLEANUP_QUERY,
        filtered_catalog_list_query, postgres_recent_catalog_rows_by_library_query,
        sqlite_recent_catalog_rows_by_library_query,
    };
    use crate::storage::repository::{CatalogFilterQuery, CatalogSort};

    #[test]
    fn recommendation_stats_cleanup_uses_an_indexable_exists_lookup() {
        assert!(RECOMMENDATION_STATS_CLEANUP_QUERY.contains(
            "WHERE NOT EXISTS (\n                 SELECT 1\n                 FROM media_items\n                 WHERE media_items.id = recommendation_item_stats.item_id"
        ));
        assert!(!RECOMMENDATION_STATS_CLEANUP_QUERY.contains("NOT IN"));
    }

    #[test]
    fn postgres_recent_catalog_limits_each_library_before_loading_details() {
        let query = postgres_recent_catalog_rows_by_library_query(2, "");

        assert!(query.contains("CROSS JOIN LATERAL"));
        assert!(query.contains("VALUES (?), (?)"));
        assert_eq!(query.matches("LIMIT ?").count(), 1);
        assert!(!query.contains("ROW_NUMBER()"));
        assert!(query.contains("home_candidates AS MATERIALIZED"));
    }

    #[test]
    fn postgres_recent_catalog_aggregates_series_and_parent_episode_dates() {
        let query = postgres_recent_catalog_rows_by_library_query(2, "");

        assert!(query.contains("episode_latest_by_root AS MATERIALIZED"));
        assert_eq!(query.matches("MAX(episode.added_at)").count(), 2);
        assert_eq!(query.matches("LEFT JOIN episode_latest_by_root").count(), 1);
        assert!(query.contains("GROUP BY episode.series_id"));
        assert!(query.contains("GROUP BY episode.parent_id"));
        assert!(!query.contains("episode_roots"));
        assert!(!query.contains("MAX(child.added_at)"));
    }

    #[tokio::test]
    async fn multi_library_union_list_preserves_global_order()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let database = Database::connect(&crate::config::Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        })
        .await?;
        database
            .query(
                "INSERT INTO libraries (id, name, kind) VALUES
                     ('union-a', 'A', 'MOVIE'), ('union-b', 'B', 'MOVIE')",
            )
            .execute(&database.pool)
            .await?;
        database
            .query(
                "INSERT INTO media_items
                     (id, library_id, item_type, title, sort_title, added_at,
                      has_available_source, identification_status)
                 VALUES
                     ('old', 'union-a', 'MOVIE', 'Old', 'old', 100, 1, 'LOCAL_CONFIRMED'),
                     ('newest', 'union-b', 'MOVIE', 'Newest', 'newest', 300, 1, 'LOCAL_CONFIRMED'),
                     ('middle', 'union-a', 'MOVIE', 'Middle', 'middle', 200, 1, 'LOCAL_CONFIRMED')",
            )
            .execute(&database.pool)
            .await?;
        let libraries = vec!["union-a".to_owned(), "union-b".to_owned()];
        let filter = CatalogFilterQuery {
            library_ids: &libraries,
            user_id: "user",
            item_types: &[],
            excluded_item_types: &[],
            item_ids: None,
            person_id: None,
            media_source_ids: None,
            provider_id_equals: None,
            years: &[],
            is_played: None,
            is_favorite: None,
            min_date_last_saved: None,
            metadata_pending: false,
            parent_id_scope: None,
            content_restrictions: None,
            sort_by: CatalogSort::DateCreated,
            descending: true,
            offset: 0,
            limit: 2,
            include_total: true,
        };
        let (rows, total) = database.list_filtered_catalog_rows(&filter).await?;
        assert_eq!(total, 3);
        assert_eq!(
            rows.iter()
                .map(|row| row.item_id.as_str())
                .collect::<Vec<_>>(),
            ["newest", "middle"]
        );

        let second_page = CatalogFilterQuery {
            offset: 2,
            ..filter
        };
        let (rows, _) = database.list_filtered_catalog_rows(&second_page).await?;
        assert_eq!(
            rows.iter()
                .map(|row| row.item_id.as_str())
                .collect::<Vec<_>>(),
            ["old"]
        );
        Ok(())
    }

    #[test]
    fn multi_library_default_date_page_binds_match_placeholders() {
        let libraries = vec!["library-a".to_owned(), "library-b".to_owned()];
        let filter = CatalogFilterQuery {
            library_ids: &libraries,
            user_id: "user",
            item_types: &[],
            excluded_item_types: &[],
            item_ids: None,
            person_id: None,
            media_source_ids: None,
            provider_id_equals: None,
            years: &[],
            is_played: None,
            is_favorite: None,
            min_date_last_saved: None,
            metadata_pending: false,
            parent_id_scope: None,
            content_restrictions: None,
            sort_by: CatalogSort::DateCreated,
            descending: true,
            offset: 50,
            limit: 24,
            include_total: false,
        };
        let (query, binds) = filtered_catalog_list_query(&filter);
        assert!(query.contains("UNION ALL"));
        assert_eq!(
            query.matches('?').count(),
            binds.len(),
            "every placeholder must have exactly one bound value"
        );
    }

    #[test]
    fn multi_library_default_date_list_queries_each_library_separately() {
        let libraries = vec!["library-a".to_owned(), "library-b".to_owned()];
        let filter = CatalogFilterQuery {
            library_ids: &libraries,
            user_id: "user",
            item_types: &[],
            excluded_item_types: &[],
            item_ids: None,
            person_id: None,
            media_source_ids: None,
            provider_id_equals: None,
            years: &[],
            is_played: None,
            is_favorite: None,
            min_date_last_saved: None,
            metadata_pending: false,
            parent_id_scope: None,
            content_restrictions: None,
            sort_by: CatalogSort::DateCreated,
            descending: true,
            offset: 0,
            limit: 24,
            include_total: true,
        };
        let (query, binds) = filtered_catalog_list_query(&filter);
        assert!(query.contains("UNION ALL"));
        assert_eq!(query.matches("mi.library_id = ?").count(), 2);
        assert_eq!(binds.len(), 6);

        let deep = CatalogFilterQuery {
            offset: 201,
            ..filter
        };
        let (query, _) = filtered_catalog_list_query(&deep);
        assert!(!query.contains("UNION ALL"));
    }

    #[test]
    fn sqlite_recent_catalog_limits_each_library_before_loading_details() {
        let query = sqlite_recent_catalog_rows_by_library_query(2, "");

        assert!(!query.contains("LATERAL"));
        assert!(!query.contains("ROW_NUMBER()"));
        assert_eq!(query.matches("LIMIT ?").count(), 6);
        assert_eq!(query.matches("UNION ALL").count(), 3);
    }

    #[tokio::test]
    async fn recommendation_refresh_skips_unchanged_inputs_within_the_idle_window()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::{application::libraries::LibraryService, config::Config, library::LibraryKind};

        let directory = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        })
        .await?;
        let library = LibraryService::new(database.clone())
            .create_library("Recommendations", LibraryKind::Movie, false)
            .await?;
        database
            .query(
                "INSERT INTO media_items (
                     id, library_id, item_type, title, sort_title, identification_status,
                     has_available_source
                 ) VALUES ('recommendation-skip-item', ?, 'MOVIE', 'Skip', 'skip',
                           'LOCAL_CONFIRMED', 1)",
            )
            .bind(library.id.to_string())
            .execute(database.pool())
            .await?;
        database
            .query(
                "UPDATE recommendation_stats_state
                 SET batch_key = batch_key - 1, refreshed_at = 0",
            )
            .execute(database.pool())
            .await?;
        assert!(database.refresh_recommendation_stats_if_needed().await?);
        let fingerprint: Option<String> = sqlx::query_scalar(
            "SELECT source_fingerprint FROM recommendation_stats_state WHERE id = 1",
        )
        .fetch_one(database.pool())
        .await?;
        assert!(fingerprint.is_some());

        // Next batch, same inputs: skip the materialization.
        sqlx::query("UPDATE recommendation_stats_state SET batch_key = batch_key - 1")
            .execute(database.pool())
            .await?;
        assert!(!database.refresh_recommendation_stats_if_needed().await?);

        // A new item changes the fingerprint and forces a refresh.
        database
            .query(
                "INSERT INTO media_items (
                     id, library_id, item_type, title, sort_title, identification_status,
                     has_available_source
                 ) VALUES ('recommendation-skip-item-2', ?, 'MOVIE', 'Skip 2', 'skip 2',
                           'LOCAL_CONFIRMED', 1)",
            )
            .bind(library.id.to_string())
            .execute(database.pool())
            .await?;
        sqlx::query("UPDATE recommendation_stats_state SET batch_key = batch_key - 1")
            .execute(database.pool())
            .await?;
        assert!(database.refresh_recommendation_stats_if_needed().await?);
        Ok(())
    }

    #[tokio::test]
    async fn dashboard_stats_reuses_a_recent_cache_entry() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = tempfile::tempdir()?;
        let database = Database::connect(&crate::config::Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        })
        .await?;
        database.reset_query_count();
        let first = database.dashboard_stats().await?;
        assert_eq!(database.query_count(), 1);
        let second = database.dashboard_stats().await?;
        assert_eq!(database.query_count(), 1);
        assert_eq!(first, second);
        Ok(())
    }

    #[tokio::test]
    async fn global_collection_sync_queue_tracks_reference_replacements_and_removals()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::{application::libraries::LibraryService, config::Config, library::LibraryKind};

        let directory = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let library = LibraryService::new(database.clone())
            .create_library("Movies", LibraryKind::Movie, false)
            .await?;
        sqlx::query(
            "INSERT INTO media_items (
                 id, library_id, item_type, title, sort_title, identification_status,
                 has_available_source
             ) VALUES ('collection-sync-movie', ?, 'MOVIE', 'Movie', 'movie', 'ONLINE_CONFIRMED', 1)",
        )
        .bind(library.id.to_string())
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO collection_item_references (
                 item_id, movie_provider, movie_provider_id, collection_provider,
                 collection_provider_id, collection_title, sort_order
             ) VALUES ('collection-sync-movie', 'tmdb', 'movie-1', 'tmdb', 'series-1', 'Series', 2020)",
        )
        .execute(database.pool())
        .await?;
        let first_generation: i64 = sqlx::query_scalar(
            "SELECT generation FROM collection_sync_outbox
             WHERE collection_provider = 'tmdb' AND collection_provider_id = 'series-1'",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(first_generation, 1);

        sqlx::query(
            "UPDATE collection_item_references
             SET collection_provider = 'imdb', collection_provider_id = 'series-1'
             WHERE item_id = 'collection-sync-movie'",
        )
        .execute(database.pool())
        .await?;
        let queued_keys: Vec<(String, String, i64)> = sqlx::query_as(
            "SELECT collection_provider, collection_provider_id, generation
             FROM collection_sync_outbox ORDER BY collection_provider",
        )
        .fetch_all(database.pool())
        .await?;
        assert_eq!(
            queued_keys,
            vec![
                ("imdb".to_owned(), "series-1".to_owned(), 1),
                ("tmdb".to_owned(), "series-1".to_owned(), 2),
            ]
        );
        assert!(
            !database
                .sync_global_collection("tmdb", "series-1", first_generation)
                .await?
        );
        let latest_tmdb_generation: i64 = sqlx::query_scalar(
            "SELECT generation FROM collection_sync_outbox
             WHERE collection_provider = 'tmdb' AND collection_provider_id = 'series-1'",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(latest_tmdb_generation, 2);

        sqlx::query(
            "DELETE FROM collection_item_references WHERE item_id = 'collection-sync-movie'",
        )
        .execute(database.pool())
        .await?;
        let removed_generation: i64 = sqlx::query_scalar(
            "SELECT generation FROM collection_sync_outbox
             WHERE collection_provider = 'imdb' AND collection_provider_id = 'series-1'",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(removed_generation, 2);
        sqlx::query(
            "INSERT INTO collection_item_references (
                 item_id, movie_provider, movie_provider_id, collection_provider,
                 collection_provider_id, collection_title
             ) VALUES ('collection-sync-movie', 'tmdb', 'movie-1', 'imdb', 'series-1', 'Series')",
        )
        .execute(database.pool())
        .await?;
        sqlx::query("DELETE FROM libraries WHERE id = ?")
            .bind(library.id.to_string())
            .execute(database.pool())
            .await?;
        let deleted_library_generation: i64 = sqlx::query_scalar(
            "SELECT generation FROM collection_sync_outbox
             WHERE collection_provider = 'imdb' AND collection_provider_id = 'series-1'",
        )
        .fetch_one(database.pool())
        .await?;
        assert!(deleted_library_generation >= 4);
        let system_library_exists: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM libraries WHERE id = ? AND system_kind = 'COLLECTIONS'",
        )
        .bind(GLOBAL_COLLECTION_LIBRARY_ID)
        .fetch_one(database.pool())
        .await?;
        assert_eq!(system_library_exists, 1);
        Ok(())
    }

    #[tokio::test]
    async fn global_collection_reads_filter_members_and_copies_by_accessible_library()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::{application::libraries::LibraryService, config::Config, library::LibraryKind};

        let directory = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let libraries = LibraryService::new(database.clone());
        let visible_library = libraries
            .create_library("Visible Movies", LibraryKind::Movie, false)
            .await?;
        let hidden_library = libraries
            .create_library("Hidden Movies", LibraryKind::Movie, false)
            .await?;
        let empty_library = libraries
            .create_library("Empty Movies", LibraryKind::Movie, false)
            .await?;
        let users = crate::auth::users::UserStore::new(database.clone())?;
        let viewer = users
            .create_user(
                "collection-viewer",
                "Collection Viewer",
                "test password",
                false,
            )
            .await?;
        database
            .set_user_library_access(
                &viewer.id.to_string(),
                &visible_library.id.to_string(),
                true,
            )
            .await?;

        for (item_id, library_id, movie_id, collection_id, collection_title, year) in [
            (
                "visible-alpha-one",
                visible_library.id.to_string(),
                "movie-one",
                "series-alpha",
                "Alpha Collection",
                2020,
            ),
            (
                "hidden-alpha-one",
                hidden_library.id.to_string(),
                "movie-one",
                "series-alpha",
                "Alpha Collection",
                2020,
            ),
            (
                "hidden-alpha-two",
                hidden_library.id.to_string(),
                "movie-two",
                "series-alpha",
                "Alpha Collection",
                2021,
            ),
            (
                "visible-beta-one",
                visible_library.id.to_string(),
                "movie-three",
                "series-beta",
                "Beta Collection",
                2022,
            ),
            (
                "visible-beta-two",
                visible_library.id.to_string(),
                "movie-four",
                "series-beta",
                "Beta Collection",
                2023,
            ),
        ] {
            sqlx::query(
                "INSERT INTO media_items (
                     id, library_id, item_type, title, sort_title, production_year,
                     identification_status, has_available_source
                 ) VALUES (?, ?, 'MOVIE', ?, ?, ?, 'ONLINE_CONFIRMED', 1)",
            )
            .bind(item_id)
            .bind(library_id)
            .bind(item_id)
            .bind(item_id.to_lowercase())
            .bind(year)
            .execute(database.pool())
            .await?;
            sqlx::query(
                "INSERT INTO collection_item_references (
                     item_id, movie_provider, movie_provider_id, collection_provider,
                     collection_provider_id, collection_title, sort_order
                 ) VALUES (?, 'tmdb', ?, 'tmdb', ?, ?, ?)",
            )
            .bind(item_id)
            .bind(movie_id)
            .bind(collection_id)
            .bind(collection_title)
            .bind(year)
            .execute(database.pool())
            .await?;
        }
        for job in database.list_pending_global_collection_syncs(10).await? {
            database
                .sync_global_collection(
                    &job.collection_provider,
                    &job.collection_provider_id,
                    job.generation,
                )
                .await?;
        }
        sqlx::query(
            "INSERT INTO item_images (id, item_id, image_type, image_index, local_path)
             VALUES ('visible-alpha-poster', 'visible-alpha-one', 'POSTER', 0, '/visible.jpg'),
                    ('hidden-alpha-poster', 'hidden-alpha-two', 'POSTER', 0, '/hidden.jpg')",
        )
        .execute(database.pool())
        .await?;
        sqlx::query(
            "INSERT INTO user_item_state (user_id, item_id, last_played_at)
             VALUES (?, 'visible-alpha-one', 10), (?, 'hidden-alpha-one', 20)",
        )
        .bind(viewer.id.to_string())
        .bind(viewer.id.to_string())
        .execute(database.pool())
        .await?;

        let (first_page, first_total) = database
            .list_global_collection_cards_page(&viewer.id.to_string(), false, 0, 1)
            .await?;
        assert_eq!(first_total, 2);
        assert_eq!(first_page.len(), 1);
        assert_eq!(first_page[0].title, "Alpha Collection");
        assert_eq!(first_page[0].member_count, 1);
        assert_eq!(
            first_page[0].poster_item_id.as_deref(),
            Some("visible-alpha-one")
        );
        assert_eq!(
            first_page[0].poster_image_tag.as_deref(),
            Some("visible-alpha-poster")
        );
        let (second_page, second_total) = database
            .list_global_collection_cards_page(&viewer.id.to_string(), false, 1, 1)
            .await?;
        assert_eq!(second_total, 2);
        assert_eq!(second_page[0].title, "Beta Collection");
        assert_eq!(second_page[0].member_count, 2);

        let (alpha_member_ids, alpha_total) = database
            .list_global_collection_member_ids_page(
                &first_page[0].item_id,
                &viewer.id.to_string(),
                false,
                0,
                10,
            )
            .await?;
        assert_eq!(alpha_total, 1);
        assert!(
            database
                .set_global_collection_source_preference(
                    &viewer.id.to_string(),
                    false,
                    &first_page[0].item_id,
                    &alpha_member_ids[0],
                    "visible-alpha-one",
                    None,
                )
                .await?
        );
        assert!(
            !database
                .set_global_collection_source_preference(
                    &viewer.id.to_string(),
                    false,
                    &first_page[0].item_id,
                    &alpha_member_ids[0],
                    "hidden-alpha-one",
                    None,
                )
                .await?
        );
        let sources = database
            .list_global_collection_sources_for_works(
                &alpha_member_ids,
                &viewer.id.to_string(),
                false,
            )
            .await?;
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].source_item_id, "visible-alpha-one");
        assert_eq!(sources[0].library_id, visible_library.id.to_string());
        assert_eq!(
            sources[0].preferred_source_item_id.as_deref(),
            Some("visible-alpha-one")
        );
        assert_eq!(sources[0].preferred_media_source_id, None);
        assert_eq!(
            sources[0].poster_image_tag.as_deref(),
            Some("visible-alpha-poster")
        );

        let isolated_user = users
            .create_user(
                "collection-isolated",
                "Collection Isolated",
                "test password",
                false,
            )
            .await?;
        database
            .set_user_library_access(
                &isolated_user.id.to_string(),
                &empty_library.id.to_string(),
                true,
            )
            .await?;
        let (hidden_cards, hidden_total) = database
            .list_global_collection_cards_page(&isolated_user.id.to_string(), false, 0, 10)
            .await?;
        assert!(hidden_cards.is_empty());
        assert_eq!(hidden_total, 0);
        let (_, hidden_members_total) = database
            .list_global_collection_member_ids_page(
                &first_page[0].item_id,
                &isolated_user.id.to_string(),
                false,
                0,
                10,
            )
            .await?;
        assert_eq!(hidden_members_total, 0);
        let (admin_cards, admin_total) = database
            .list_global_collection_cards_page(&viewer.id.to_string(), true, 0, 10)
            .await?;
        assert_eq!(admin_total, 2);
        assert_eq!(admin_cards[0].member_count, 2);

        sqlx::query("UPDATE users SET library_access_mode = 'NONE' WHERE id = ?")
            .bind(viewer.id.to_string())
            .execute(database.pool())
            .await?;
        let (none_cards, none_total) = database
            .list_global_collection_cards_page(&viewer.id.to_string(), false, 0, 10)
            .await?;
        assert!(none_cards.is_empty());
        assert_eq!(none_total, 0);
        let (none_recent_cards, none_recent_total) = database
            .list_recent_global_collection_cards_page(&viewer.id.to_string(), false, 0, 10)
            .await?;
        assert!(none_recent_cards.is_empty());
        assert_eq!(none_recent_total, 0);
        let (_, none_members_total) = database
            .list_global_collection_member_ids_page(
                &first_page[0].item_id,
                &viewer.id.to_string(),
                false,
                0,
                10,
            )
            .await?;
        assert_eq!(none_members_total, 0);
        assert!(
            database
                .list_global_collection_sources_for_works(
                    &alpha_member_ids,
                    &viewer.id.to_string(),
                    false,
                )
                .await?
                .is_empty()
        );
        assert!(
            !database
                .set_global_collection_source_preference(
                    &viewer.id.to_string(),
                    false,
                    &first_page[0].item_id,
                    &alpha_member_ids[0],
                    "visible-alpha-one",
                    None,
                )
                .await?
        );
        Ok(())
    }

    #[tokio::test]
    async fn global_collection_sync_deduplicates_copies_and_retains_stable_items()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::{application::libraries::LibraryService, config::Config, library::LibraryKind};

        let directory = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8098".parse()?,
            config_dir: directory.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let libraries = LibraryService::new(database.clone());
        let first_library = libraries
            .create_library("Movies A", LibraryKind::Movie, false)
            .await?;
        let second_library = libraries
            .create_library("Movies B", LibraryKind::Movie, false)
            .await?;
        let movies = [
            ("copy-a", first_library.id.to_string(), "movie-1", "Movie 1"),
            (
                "movie-b",
                first_library.id.to_string(),
                "movie-2",
                "Movie 2",
            ),
            (
                "copy-a-2",
                second_library.id.to_string(),
                "movie-1",
                "Movie 1 alt",
            ),
            (
                "movie-c",
                second_library.id.to_string(),
                "movie-3",
                "Movie 3",
            ),
        ];
        for (item_id, library_id, movie_id, title) in &movies {
            sqlx::query(
                "INSERT INTO media_items (
                     id, library_id, item_type, title, sort_title, production_year,
                     identification_status, has_available_source
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 2020, 'ONLINE_CONFIRMED', 1)",
            )
            .bind(item_id)
            .bind(library_id)
            .bind(title)
            .bind(title.to_ascii_lowercase())
            .execute(database.pool())
            .await?;
            sqlx::query(
                "INSERT INTO collection_item_references (
                     item_id, movie_provider, movie_provider_id, collection_provider,
                     collection_provider_id, collection_title, sort_order
                 ) VALUES (?, 'tmdb', ?, 'tmdb', 'series-1', 'Series One', ?)",
            )
            .bind(item_id)
            .bind(movie_id)
            .bind(match *movie_id {
                "movie-1" => 1_i64,
                "movie-2" => 2_i64,
                _ => 3_i64,
            })
            .execute(database.pool())
            .await?;
        }

        let mut pending = database.list_pending_global_collection_syncs(10).await?;
        assert_eq!(pending.len(), 1);
        let job = pending.pop().ok_or("expected one collection sync job")?;
        let first_database = database.clone();
        let second_database = database.clone();
        let (first_sync, second_sync) = tokio::join!(
            first_database.sync_global_collection(
                &job.collection_provider,
                &job.collection_provider_id,
                job.generation,
            ),
            second_database.sync_global_collection(
                &job.collection_provider,
                &job.collection_provider_id,
                job.generation,
            )
        );
        assert_eq!((first_sync?, second_sync?), (true, false));
        let collection_id: String = sqlx::query_scalar(
            "SELECT id FROM collections
             WHERE library_id = ? AND provider = 'tmdb' AND provider_id = 'series-1'",
        )
        .bind(GLOBAL_COLLECTION_LIBRARY_ID)
        .fetch_one(database.pool())
        .await?;
        let original_collection_item_id: String =
            sqlx::query_scalar("SELECT item_id FROM collections WHERE id = ?")
                .bind(&collection_id)
                .fetch_one(database.pool())
                .await?;
        let original_work_ids: Vec<String> = sqlx::query_scalar(
            "SELECT work_item_id FROM collection_work_items ORDER BY movie_provider_id",
        )
        .fetch_all(database.pool())
        .await?;
        let initial_counts: (i64, i64, i64) = sqlx::query_as(
            "SELECT
                 (SELECT COUNT(*) FROM collection_items WHERE collection_id = ?),
                 (SELECT COUNT(*) FROM collection_work_items),
                 (SELECT COUNT(*) FROM collection_work_sources)",
        )
        .bind(&collection_id)
        .fetch_one(database.pool())
        .await?;
        assert_eq!(initial_counts, (3, 3, 4));

        sqlx::query("UPDATE libraries SET is_enabled = 0 WHERE id = ?")
            .bind(second_library.id.to_string())
            .execute(database.pool())
            .await?;
        pending = database.list_pending_global_collection_syncs(10).await?;
        assert_eq!(pending.len(), 1);
        let job = pending.pop().ok_or("expected disable sync job")?;
        assert!(
            database
                .sync_global_collection(
                    &job.collection_provider,
                    &job.collection_provider_id,
                    job.generation,
                )
                .await?
        );
        let disabled_library_counts: (i64, i64) = sqlx::query_as(
            "SELECT
                 (SELECT COUNT(*) FROM collection_items WHERE collection_id = ?),
                 (SELECT COUNT(*) FROM collection_work_sources)",
        )
        .bind(&collection_id)
        .fetch_one(database.pool())
        .await?;
        assert_eq!(disabled_library_counts, (2, 2));

        sqlx::query("UPDATE media_items SET removed_at = unixepoch() WHERE id = 'movie-b'")
            .execute(database.pool())
            .await?;
        pending = database.list_pending_global_collection_syncs(10).await?;
        assert_eq!(pending.len(), 1);
        let job = pending.pop().ok_or("expected removal sync job")?;
        assert!(
            database
                .sync_global_collection(
                    &job.collection_provider,
                    &job.collection_provider_id,
                    job.generation,
                )
                .await?
        );
        let hidden_member_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM collection_items WHERE collection_id = ?")
                .bind(&collection_id)
                .fetch_one(database.pool())
                .await?;
        assert_eq!(hidden_member_count, 0);

        sqlx::query("UPDATE libraries SET is_enabled = 1 WHERE id = ?")
            .bind(second_library.id.to_string())
            .execute(database.pool())
            .await?;
        pending = database.list_pending_global_collection_syncs(10).await?;
        let job = pending.pop().ok_or("expected enable sync job")?;
        assert!(
            database
                .sync_global_collection(
                    &job.collection_provider,
                    &job.collection_provider_id,
                    job.generation,
                )
                .await?
        );
        let restored: (String, String, i64) = sqlx::query_as(
            "SELECT collection.id, collection.item_id,
                    (SELECT COUNT(*) FROM collection_items WHERE collection_id = collection.id)
             FROM collections collection WHERE collection.id = ?",
        )
        .bind(&collection_id)
        .fetch_one(database.pool())
        .await?;
        let restored_work_ids: Vec<String> = sqlx::query_scalar(
            "SELECT work_item_id FROM collection_work_items ORDER BY movie_provider_id",
        )
        .fetch_all(database.pool())
        .await?;
        assert_eq!(restored.0, collection_id);
        assert_eq!(restored.1, original_collection_item_id);
        assert_eq!(restored.2, 2);
        assert_eq!(restored_work_ids, original_work_ids);

        sqlx::query(
            "UPDATE collection_item_references
             SET collection_provider_id = 'series-2', collection_title = 'Series Two'
             WHERE item_id = 'movie-c'",
        )
        .execute(database.pool())
        .await?;
        sqlx::query("UPDATE media_items SET removed_at = NULL WHERE id = 'movie-b'")
            .execute(database.pool())
            .await?;
        sqlx::query(
            "UPDATE collection_item_references
             SET collection_provider_id = 'series-2', collection_title = 'Series Two'
             WHERE item_id = 'movie-b'",
        )
        .execute(database.pool())
        .await?;
        let correction_jobs = database.list_pending_global_collection_syncs(10).await?;
        assert_eq!(correction_jobs.len(), 2);
        for job in correction_jobs {
            assert!(
                database
                    .sync_global_collection(
                        &job.collection_provider,
                        &job.collection_provider_id,
                        job.generation,
                    )
                    .await?
            );
        }
        let old_series_members: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM collection_items WHERE collection_id = ?")
                .bind(&collection_id)
                .fetch_one(database.pool())
                .await?;
        let corrected_series_counts: (i64, i64) = sqlx::query_as(
            "SELECT
                 (SELECT COUNT(*) FROM collections
                  WHERE library_id = ? AND provider = 'tmdb' AND provider_id = 'series-2'),
                 (SELECT COUNT(*) FROM collection_items member
                  JOIN collections collection ON collection.id = member.collection_id
                  WHERE collection.library_id = ?
                    AND collection.provider = 'tmdb' AND collection.provider_id = 'series-2')",
        )
        .bind(GLOBAL_COLLECTION_LIBRARY_ID)
        .bind(GLOBAL_COLLECTION_LIBRARY_ID)
        .fetch_one(database.pool())
        .await?;
        assert_eq!(old_series_members, 0);
        assert_eq!(corrected_series_counts, (1, 2));
        Ok(())
    }

    #[tokio::test]
    async fn global_collection_sync_failure_keeps_previous_mapping_and_pending_job()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::{application::libraries::LibraryService, config::Config, library::LibraryKind};

        let directory = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8099".parse()?,
            config_dir: directory.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let library = LibraryService::new(database.clone())
            .create_library("Movies", LibraryKind::Movie, false)
            .await?;
        for (item_id, movie_id) in [("movie-a", "a"), ("movie-b", "b")] {
            sqlx::query(
                "INSERT INTO media_items (
                     id, library_id, item_type, title, sort_title,
                     identification_status, has_available_source
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'ONLINE_CONFIRMED', 1)",
            )
            .bind(item_id)
            .bind(library.id.to_string())
            .bind(format!("Movie {movie_id}"))
            .bind(format!("movie {movie_id}"))
            .execute(database.pool())
            .await?;
            sqlx::query(
                "INSERT INTO collection_item_references (
                     item_id, movie_provider, movie_provider_id, collection_provider,
                     collection_provider_id, collection_title
                 ) VALUES (?, 'tmdb', ?, 'tmdb', 'series-1', 'Series One')",
            )
            .bind(item_id)
            .bind(movie_id)
            .execute(database.pool())
            .await?;
        }
        let initial_job = database
            .list_pending_global_collection_syncs(10)
            .await?
            .pop()
            .ok_or("expected initial sync job")?;
        assert!(
            database
                .sync_global_collection(
                    &initial_job.collection_provider,
                    &initial_job.collection_provider_id,
                    initial_job.generation,
                )
                .await?
        );
        let collection_id: String = sqlx::query_scalar(
            "SELECT id FROM collections WHERE provider = 'tmdb' AND provider_id = 'series-1'",
        )
        .fetch_one(database.pool())
        .await?;
        sqlx::query(
            "UPDATE collection_item_references SET sort_order = 8 WHERE item_id = 'movie-a'",
        )
        .execute(database.pool())
        .await?;
        sqlx::query(
            "CREATE TRIGGER fail_global_collection_member_insert
             BEFORE INSERT ON collection_items
             BEGIN
                 SELECT RAISE(ABORT, 'injected collection sync failure');
             END",
        )
        .execute(database.pool())
        .await?;
        let failed_job = database
            .list_pending_global_collection_syncs(10)
            .await?
            .pop()
            .ok_or("expected retryable sync job")?;
        let result = database
            .sync_global_collection(
                &failed_job.collection_provider,
                &failed_job.collection_provider_id,
                failed_job.generation,
            )
            .await;
        assert!(result.is_err());
        let preserved_counts: (i64, i64, i64) = sqlx::query_as(
            "SELECT
                 (SELECT COUNT(*) FROM collection_items WHERE collection_id = ?),
                 (SELECT COUNT(*) FROM collection_work_items),
                 (SELECT COUNT(*) FROM collection_sync_outbox)",
        )
        .bind(&collection_id)
        .fetch_one(database.pool())
        .await?;
        assert_eq!(preserved_counts, (2, 2, 1));

        sqlx::query("DROP TRIGGER fail_global_collection_member_insert")
            .execute(database.pool())
            .await?;
        let retry_job = database
            .list_pending_global_collection_syncs(10)
            .await?
            .pop()
            .ok_or("expected sync job to remain pending")?;
        assert!(
            database
                .sync_global_collection(
                    &retry_job.collection_provider,
                    &retry_job.collection_provider_id,
                    retry_job.generation,
                )
                .await?
        );
        let retry_result: (i64, i64) = sqlx::query_as(
            "SELECT
                 (SELECT COUNT(*) FROM collection_items WHERE collection_id = ?),
                 (SELECT COUNT(*) FROM collection_sync_outbox)",
        )
        .bind(&collection_id)
        .fetch_one(database.pool())
        .await?;
        assert_eq!(retry_result, (2, 0));
        Ok(())
    }

    #[tokio::test]
    async fn admin_web_session_page_is_bounded_and_filters_inactive_sessions()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::{application::setup::SetupService, config::Config};

        let directory = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let setup = SetupService::new(database.clone())?;
        let user = setup
            .complete("Session Admin", "Session Admin", "admin password")
            .await?;
        let current_token_hash = vec![100_u8; 32];

        for index in 0..101 {
            let token_hash = vec![index as u8; 32];
            database
                .create_web_session(
                    &format!("session-{index:03}"),
                    &user.id.to_string(),
                    &token_hash,
                    &[0_u8; 32],
                    3600,
                )
                .await?;
        }

        let (first_page, total_count) = database
            .list_web_session_summaries_page(&user.id.to_string(), &current_token_hash, 500, 0)
            .await?;
        assert_eq!(first_page.len(), 100);
        assert_eq!(total_count, 101);
        assert_eq!(first_page[0].id, "session-100");
        assert!(first_page[0].is_current);

        let (second_page, second_count) = database
            .list_web_session_summaries_page(&user.id.to_string(), &[], 500, 100)
            .await?;
        assert_eq!(second_page.len(), 1);
        assert_eq!(second_page[0].id, "session-000");
        assert_eq!(second_count, 101);

        sqlx::query("UPDATE web_sessions SET revoked_at = unixepoch() WHERE id = 'session-100'")
            .execute(database.pool())
            .await?;
        sqlx::query(
            "UPDATE web_sessions SET expires_at = unixepoch() - 1 WHERE id = 'session-099'",
        )
        .execute(database.pool())
        .await?;
        let (active_sessions, active_count) = database
            .list_web_session_summaries_page(&user.id.to_string(), &[], 100, 0)
            .await?;
        assert_eq!(active_count, 99);
        assert_eq!(active_sessions.len(), 99);
        assert!(
            active_sessions
                .iter()
                .all(|session| session.id != "session-100" && session.id != "session-099")
        );
        Ok(())
    }

    #[tokio::test]
    async fn admin_current_web_session_guard_checks_full_identity_and_activity()
    -> Result<(), Box<dyn std::error::Error>> {
        use crate::{application::setup::SetupService, config::Config};

        let directory = tempfile::tempdir()?;
        let config = Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: directory.path().join("config"),
        };
        let database = Database::connect(&config).await?;
        let setup = SetupService::new(database.clone())?;
        let user = setup
            .complete("Session Admin", "Session Admin", "admin password")
            .await?;
        let user_id = user.id.to_string();
        let session_id = "active-session";
        let token_hash = vec![7_u8; 32];
        database
            .create_web_session(session_id, &user_id, &token_hash, &[0_u8; 32], 3600)
            .await?;

        assert!(
            database
                .is_active_web_session_token(&user_id, session_id, &token_hash)
                .await?
        );
        assert!(
            !database
                .is_active_web_session_token("other-user", session_id, &token_hash)
                .await?
        );
        assert!(
            !database
                .is_active_web_session_token(&user_id, "other-session", &token_hash)
                .await?
        );
        assert!(
            !database
                .is_active_web_session_token(&user_id, session_id, &[8_u8; 32])
                .await?
        );

        sqlx::query("UPDATE web_sessions SET expires_at = unixepoch() - 1 WHERE id = ?")
            .bind(session_id)
            .execute(database.pool())
            .await?;
        assert!(
            !database
                .is_active_web_session_token(&user_id, session_id, &token_hash)
                .await?
        );
        sqlx::query(
            "UPDATE web_sessions SET expires_at = unixepoch() + 3600, revoked_at = unixepoch()
             WHERE id = ?",
        )
        .bind(session_id)
        .execute(database.pool())
        .await?;
        assert!(
            !database
                .is_active_web_session_token(&user_id, session_id, &token_hash)
                .await?
        );
        Ok(())
    }
}
