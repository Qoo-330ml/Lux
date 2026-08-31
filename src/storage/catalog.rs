use super::*;

impl Database {
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
               {favorite_filter}"
        );
        let mut statement = self.query(sqlx::AssertSqlSafe(query));
        for library_id in library_ids {
            statement = statement.bind(library_id);
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

    pub(crate) async fn dashboard_stats(&self) -> Result<DashboardStats, StorageError> {
        self.query(
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
        })
    }

    pub(crate) async fn search_catalog_item_ids(
        &self,
        query: &str,
        like_query: &str,
        library_ids: Option<&[String]>,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<String>, i64), StorageError> {
        if self.backend == DatabaseBackend::Postgres {
            return self
                .search_catalog_item_ids_postgres(like_query, library_ids, offset, limit)
                .await;
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
        let fts_query = format!(
            "SELECT mi.id FROM media_search
             JOIN media_items mi ON mi.id = media_search.item_id
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             WHERE media_search MATCH ? AND mi.removed_at IS NULL
               AND mi.item_type <> 'FOLDER'{CATALOG_VISIBLE_PREDICATE}{}",
            library_filter.as_deref().unwrap_or_default()
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
                Some(query),
                None,
                library_ids,
                offset,
                limit,
            )
            .await?;
        if !fts_page.0.is_empty() {
            return Ok(fts_page);
        }

        let like_query_sql = format!(
            "SELECT mi.id FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             WHERE (mi.title LIKE ? OR COALESCE(mi.original_title, '') LIKE ?
                    OR EXISTS (SELECT 1 FROM item_aliases ia
                               WHERE ia.item_id = mi.id AND ia.alias LIKE ?))
               AND mi.removed_at IS NULL
               AND mi.item_type <> 'FOLDER'{CATALOG_VISIBLE_PREDICATE}{}",
            library_filter.as_deref().unwrap_or_default()
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
                    None,
                    Some(like_query),
                    library_ids,
                    offset,
                    limit,
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
            Some(query),
            Some(like_query),
            library_ids,
            offset,
            limit,
        )
        .await
    }

    async fn search_catalog_item_ids_postgres(
        &self,
        like_query: &str,
        library_ids: Option<&[String]>,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<String>, i64), StorageError> {
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
        let like_query_sql = format!(
            "SELECT mi.id FROM media_search ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             WHERE (ms.title ILIKE ? ESCAPE '\\' OR ms.original_title ILIKE ? ESCAPE '\\'
                    OR ms.aliases ILIKE ? ESCAPE '\\')
               AND mi.removed_at IS NULL
               AND mi.item_type <> 'FOLDER'{CATALOG_VISIBLE_PREDICATE}{}",
            library_filter.as_deref().unwrap_or_default()
        );
        let page_query = format!(
            "SELECT matches.id, COUNT(*) OVER() AS total FROM ({like_query_sql}) matches
             JOIN media_items mi ON mi.id = matches.id
             ORDER BY mi.sort_title, mi.id LIMIT ? OFFSET ?"
        );
        self.fetch_catalog_search_page(
            &page_query,
            None,
            Some(like_query),
            library_ids,
            offset,
            limit,
        )
        .await
    }

    async fn fetch_catalog_search_page(
        &self,
        query: &str,
        fts_query: Option<&str>,
        like_query: Option<&str>,
        library_ids: Option<&[String]>,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<String>, i64), StorageError> {
        let mut statement = self.query(sqlx::AssertSqlSafe(query));
        if let Some(fts_query) = fts_query {
            statement = statement.bind(fts_query);
            if let Some(library_ids) = library_ids {
                for library_id in library_ids {
                    statement = statement.bind(library_id);
                }
            }
        }
        if let Some(like_query) = like_query {
            statement = statement.bind(like_query).bind(like_query).bind(like_query);
            if let Some(library_ids) = library_ids {
                for library_id in library_ids {
                    statement = statement.bind(library_id);
                }
            }
        }
        let rows = statement
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let total = rows.first().map(|row| row.get("total")).unwrap_or(0);
        Ok((rows.into_iter().map(|row| row.get("id")).collect(), total))
    }

    pub(crate) async fn list_recent_catalog_item_ids(
        &self,
        library_ids: &[String],
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<String>, i64), StorageError> {
        if library_ids.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let placeholders = std::iter::repeat_n("?", library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let count_query = format!(
            "SELECT CAST(COALESCE(SUM(item_count), 0) AS BIGINT)
             FROM (
                 SELECT COUNT(*) AS item_count
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 WHERE mi.removed_at IS NULL
                   AND mi.item_type <> 'FOLDER'
                   AND mi.has_available_source = 1
                   AND mi.library_id IN ({placeholders})
                 UNION ALL
                 SELECT COUNT(*) AS item_count
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 WHERE mi.removed_at IS NULL
                   AND mi.item_type IN ('SERIES', 'SEASON', 'BOX_SET')
                   AND mi.has_available_source = 0
                   AND mi.library_id IN ({placeholders})
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
        for library_id in library_ids {
            count_statement = count_statement.bind(library_id);
        }
        let list_query = format!(
            "WITH visible_catalog AS (
                 SELECT mi.id, mi.library_id, mi.added_at, mi.sort_title
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 WHERE mi.removed_at IS NULL
                   AND mi.item_type <> 'FOLDER'
                   AND mi.has_available_source = 1
                   AND mi.library_id IN ({placeholders})
                 UNION ALL
                 SELECT mi.id, mi.library_id, mi.added_at, mi.sort_title
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 WHERE mi.removed_at IS NULL
                   AND mi.item_type IN ('SERIES', 'SEASON', 'BOX_SET')
                   AND mi.has_available_source = 0
                   AND mi.library_id IN ({placeholders})
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
        for library_id in library_ids {
            list_statement = list_statement.bind(library_id);
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

    pub(crate) async fn list_recent_catalog_rows_by_library(
        &self,
        library_ids: &[String],
        limit: i64,
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        if library_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut rows = Vec::new();
        for library_ids in library_ids.chunks(500) {
            let placeholders = std::iter::repeat_n("?", library_ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
            "WITH visible_catalog AS (
                 SELECT mi.id, mi.library_id, mi.added_at, mi.sort_title
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 WHERE mi.library_id IN ({placeholders})
                   AND mi.item_type IN ('MOVIE', 'SERIES')
                   AND mi.removed_at IS NULL
                   AND mi.has_available_source = 1
                 UNION ALL
                 SELECT mi.id, mi.library_id, mi.added_at, mi.sort_title
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 WHERE mi.library_id IN ({placeholders})
                   AND mi.item_type = 'SERIES'
                   AND mi.removed_at IS NULL
                   AND mi.has_available_source = 0
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
             ), ranked AS (
                 SELECT id, library_id,
                        ROW_NUMBER() OVER (
                            PARTITION BY library_id
                            ORDER BY added_at DESC, sort_title ASC, id ASC
                        ) AS library_rank
                 FROM visible_catalog
             )
             SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                    mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                    mi.title, mi.sort_title, mi.original_title, mi.overview,
                    mi.production_year, mi.rating, mi.rating_source, mi.runtime_ticks,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                     ORDER BY image_index LIMIT 1) AS poster_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                     ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'THUMB'
                     ORDER BY image_index LIMIT 1) AS thumb_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'LOGO'
                     ORDER BY image_index LIMIT 1) AS logo_image_tag,
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
             WHERE ranked.library_rank <= ?
             ORDER BY ranked.library_id, mi.added_at DESC, mi.sort_title ASC,
                      mi.id ASC, ms.id, mt.stream_index"
            );
            let mut binds = Vec::with_capacity(library_ids.len() * 2 + 1);
            binds.extend(library_ids.iter().map(|id| CatalogBind::Text(id)));
            binds.extend(library_ids.iter().map(|id| CatalogBind::Text(id)));
            binds.push(CatalogBind::Integer(limit));
            rows.extend(self.fetch_catalog_rows(&query, &binds).await?);
        }
        Ok(rows)
    }

    pub(crate) async fn list_recommended_catalog_rows(
        &self,
        user_id: &str,
        library_ids: &[String],
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        if library_ids.is_empty() {
            return Ok(Vec::new());
        }
        let max_function = self.scalar_max_function();
        let min_function = self.scalar_min_function();
        let placeholders = std::iter::repeat_n("?", library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "WITH ranked AS (
                 SELECT mi.id,
                        (
                            CASE WHEN COALESCE(us.is_favorite, 0) = 1 THEN 100 ELSE 0 END
                            + CASE WHEN us.item_id IS NULL THEN 35 ELSE 0 END
                            + CASE WHEN COALESCE(us.position_ticks, 0) > 0
                                      AND COALESCE(us.is_played, 0) = 0 THEN 55 ELSE 0 END
                            + CASE WHEN us.item_id IS NOT NULL
                                      AND COALESCE(us.is_played, 0) = 0 THEN 20 ELSE 0 END
                            + CASE WHEN COALESCE(us.is_played, 0) = 1 THEN -35 ELSE 0 END
                            + {min_function}(7, {max_function}(0, 7 - CAST((unixepoch() - mi.added_at) / 86400 AS INTEGER)))
                            + CASE WHEN us.last_played_at IS NULL THEN 0 ELSE
                                {min_function}(30, {max_function}(0, 30 - CAST((unixepoch() - us.last_played_at) / 86400 AS INTEGER)))
                              END
                        ) AS recommendation_score,
                        mi.added_at,
                        mi.sort_title
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 LEFT JOIN user_item_state us
                   ON us.item_id = mi.id AND us.user_id = ?
                 WHERE mi.removed_at IS NULL{CATALOG_VISIBLE_PREDICATE}
                   AND mi.item_type IN ('MOVIE', 'SERIES')
                   AND mi.library_id IN ({placeholders})
                 ORDER BY recommendation_score DESC, mi.added_at DESC,
                          mi.sort_title, mi.id
                 LIMIT ? OFFSET ?
             )
             SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                    mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                    mi.title, mi.sort_title, mi.original_title, mi.overview,
                    mi.production_year, mi.rating, mi.rating_source, mi.runtime_ticks,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                     ORDER BY image_index LIMIT 1) AS poster_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                     ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'THUMB'
                     ORDER BY image_index LIMIT 1) AS thumb_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'LOGO'
                     ORDER BY image_index LIMIT 1) AS logo_image_tag,
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
        let mut binds = Vec::with_capacity(library_ids.len() + 3);
        binds.push(CatalogBind::Text(user_id));
        binds.extend(library_ids.iter().map(|value| CatalogBind::Text(value)));
        binds.push(CatalogBind::Integer(limit));
        binds.push(CatalogBind::Integer(offset));
        self.fetch_catalog_rows(&query, &binds).await
    }

    pub(crate) async fn count_catalog_children(
        &self,
        parent_id: &str,
        item_type: &str,
    ) -> Result<i64, StorageError> {
        let query = format!(
            "SELECT COUNT(*) FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             WHERE mi.parent_id = ? AND mi.item_type = ? AND mi.removed_at IS NULL
               {CATALOG_VISIBLE_PREDICATE}"
        );
        self.query_scalar::<i64>(sqlx::AssertSqlSafe(query))
            .bind(parent_id)
            .bind(item_type)
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
              )
             WHERE parent.id IN ({placeholders})
               AND parent.item_type IN ('SERIES', 'SEASON')
               AND parent.removed_at IS NULL
             GROUP BY parent.id"
        );
        let mut statement = self.query(sqlx::AssertSqlSafe(query));
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

    pub(crate) async fn list_catalog_children(
        &self,
        parent_id: &str,
        item_type: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        let query = format!(
            "SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                    mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                    mi.title, mi.sort_title, mi.original_title, mi.overview,
                    mi.production_year, mi.rating, mi.rating_source, mi.runtime_ticks,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                     ORDER BY image_index LIMIT 1) AS poster_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                     ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'THUMB'
                     ORDER BY image_index LIMIT 1) AS thumb_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'LOGO'
                     ORDER BY image_index LIMIT 1) AS logo_image_tag,
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
             WHERE mi.parent_id = ? AND mi.item_type = ? AND mi.removed_at IS NULL
               {CATALOG_VISIBLE_PREDICATE}
             ORDER BY mi.season_number, mi.episode_number, mi.sort_title, mi.id,
                      ms.id, mt.stream_index
             LIMIT ? OFFSET ?"
        );
        self.fetch_catalog_rows(
            &query,
            &[
                CatalogBind::Text(parent_id),
                CatalogBind::Text(item_type),
                CatalogBind::Integer(limit),
                CatalogBind::Integer(offset),
            ],
        )
        .await
    }

    pub(crate) async fn list_series_episode_ids(
        &self,
        series_id: &str,
        season_id: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<String>, i64), StorageError> {
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
               {CATALOG_VISIBLE_PREDICATE}"
        );
        let mut count_statement = self
            .query_scalar::<i64>(sqlx::AssertSqlSafe(count_sql))
            .bind(series_id);
        if let Some(season_id) = season_id {
            count_statement = count_statement.bind(season_id);
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
               {CATALOG_VISIBLE_PREDICATE}
             ORDER BY mi.season_number, mi.episode_number, mi.sort_title, mi.id
             LIMIT ? OFFSET ?"
        );
        let mut list_statement = self.query(sqlx::AssertSqlSafe(list_sql)).bind(series_id);
        if let Some(season_id) = season_id {
            list_statement = list_statement.bind(season_id);
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

    pub(crate) async fn count_resume_items(
        &self,
        user_id: &str,
        library_ids: &[String],
        item_types: &[&str],
        played_percent: i64,
        minimum_ticks: i64,
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
        let runtime_ticks = resume_runtime_ticks_sql();
        let statement_sql = format!(
            "WITH candidates AS (
                 SELECT us.position_ticks,
                        {runtime_ticks} AS resume_runtime_ticks
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 JOIN user_item_state us ON us.item_id = mi.id AND us.user_id = ?
                 WHERE mi.item_type IN ({item_type_placeholders}) AND mi.removed_at IS NULL{CATALOG_VISIBLE_PREDICATE}
                   AND us.is_played = 0 AND us.position_ticks >= ?
                   AND mi.library_id IN ({library_placeholders})
             )
             SELECT COUNT(*) FROM candidates
             WHERE resume_runtime_ticks > 0
               AND position_ticks * 100 < resume_runtime_ticks * ?"
        );
        let mut statement = self
            .query_scalar::<i64>(sqlx::AssertSqlSafe(statement_sql))
            .bind(user_id);
        for item_type in item_types {
            statement = statement.bind(*item_type);
        }
        statement = statement.bind(minimum_ticks);
        for library_id in library_ids {
            statement = statement.bind(library_id);
        }
        statement
            .bind(played_percent)
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_resume_items(
        &self,
        query: &ResumeItemsQuery<'_>,
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        if query.library_ids.is_empty() || query.item_types.is_empty() {
            return Ok(Vec::new());
        }
        let item_type_placeholders = std::iter::repeat_n("?", query.item_types.len())
            .collect::<Vec<_>>()
            .join(", ");
        let library_placeholders = std::iter::repeat_n("?", query.library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let runtime_ticks = resume_runtime_ticks_sql();
        let statement_sql = format!(
            "WITH candidates AS (
                 SELECT mi.id, mi.sort_title, us.position_ticks, us.last_played_at,
                        {runtime_ticks} AS resume_runtime_ticks
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 JOIN user_item_state us ON us.item_id = mi.id AND us.user_id = ?
                 WHERE mi.item_type IN ({item_type_placeholders}) AND mi.removed_at IS NULL{CATALOG_VISIBLE_PREDICATE}
                   AND us.is_played = 0 AND us.position_ticks >= ?
                   AND mi.library_id IN ({library_placeholders})
             ),
             ranked AS (
                 SELECT id, sort_title, last_played_at
                 FROM candidates
                 WHERE resume_runtime_ticks > 0
                   AND position_ticks * 100 < resume_runtime_ticks * ?
                 ORDER BY last_played_at DESC, sort_title, id
                 LIMIT ? OFFSET ?
             )
             SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                    mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                    mi.title, mi.sort_title, mi.original_title, mi.overview,
                    mi.production_year, mi.rating, mi.rating_source, mi.runtime_ticks,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                     ORDER BY image_index LIMIT 1) AS poster_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                     ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'THUMB'
                     ORDER BY image_index LIMIT 1) AS thumb_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'LOGO'
                     ORDER BY image_index LIMIT 1) AS logo_image_tag,
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
             ORDER BY ranked.last_played_at DESC, ranked.sort_title, ranked.id,
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
        binds.push(CatalogBind::Integer(query.played_percent));
        binds.push(CatalogBind::Integer(query.limit));
        binds.push(CatalogBind::Integer(query.offset));
        self.fetch_catalog_rows(&statement_sql, &binds).await
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
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                     ORDER BY image_index LIMIT 1) AS poster_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                     ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'THUMB'
                     ORDER BY image_index LIMIT 1) AS thumb_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'LOGO'
                     ORDER BY image_index LIMIT 1) AS logo_image_tag,
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
             JOIN user_item_state us ON us.item_id = mi.id AND us.user_id = ?
             LEFT JOIN media_sources ms
               ON ms.item_id = mi.id
              AND EXISTS (
                  SELECT 1 FROM filesystem_entries fe
                  WHERE fe.id = ms.filesystem_entry_id AND fe.is_missing = 0
              )
             LEFT JOIN media_streams mt ON mt.media_source_id = ms.id
             WHERE mi.item_type IN ({item_type_placeholders}) AND mi.removed_at IS NULL{CATALOG_VISIBLE_PREDICATE}
               AND us.is_played = 0 AND us.position_ticks > 0
               AND mi.library_id IN ({library_placeholders}){series_predicate}
             ORDER BY us.last_played_at DESC, mi.series_id, mi.season_number,
                      mi.episode_number, mi.id
             LIMIT ? OFFSET ?"
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
            };
        }
        let item_order = match (filter.sort_by, filter.descending) {
            (CatalogSort::DateCreated, true) => "mi.added_at DESC, LOWER(mi.title) ASC, mi.id ASC",
            (CatalogSort::DateCreated, false) => "mi.added_at ASC, LOWER(mi.title) ASC, mi.id ASC",
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
            (CatalogSort::Rating, true) => {
                "CASE WHEN mi.rating IS NULL THEN 1 ELSE 0 END ASC,
                 mi.rating DESC, LOWER(mi.title) ASC, mi.id ASC"
            }
            (CatalogSort::Rating, false) => {
                "CASE WHEN mi.rating IS NULL THEN 1 ELSE 0 END ASC,
                 mi.rating ASC, LOWER(mi.title) ASC, mi.id ASC"
            }
            (CatalogSort::Name, true) => "LOWER(mi.title) DESC, mi.id DESC",
            (CatalogSort::Name, false) => "LOWER(mi.title) ASC, mi.id ASC",
        };
        let query = format!(
            "SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                    mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                    mi.title, mi.sort_title, mi.original_title, mi.overview,
                    mi.production_year, mi.rating, mi.rating_source, mi.runtime_ticks,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                     ORDER BY image_index LIMIT 1) AS poster_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                     ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'THUMB'
                     ORDER BY image_index LIMIT 1) AS thumb_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'LOGO'
                     ORDER BY image_index LIMIT 1) AS logo_image_tag,
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
                 SELECT mi.id
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 {where_clause}
                 ORDER BY {item_order}
                 LIMIT ? OFFSET ?
             ) selected
             JOIN media_items mi ON mi.id = selected.id
             LEFT JOIN media_sources ms
               ON ms.item_id = mi.id
              AND EXISTS (
                  SELECT 1 FROM filesystem_entries fe
                  WHERE fe.id = ms.filesystem_entry_id AND fe.is_missing = 0
              )
             LEFT JOIN media_streams mt ON mt.media_source_id = ms.id
             ORDER BY {item_order}, ms.id, mt.stream_index"
        );
        let mut list_binds = filter_binds.clone();
        list_binds.push(CatalogBind::Integer(filter.limit));
        list_binds.push(CatalogBind::Integer(filter.offset));
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
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                         ORDER BY image_index LIMIT 1) AS poster_image_tag,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                         ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'THUMB'
                         ORDER BY image_index LIMIT 1) AS thumb_image_tag,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'LOGO'
                         ORDER BY image_index LIMIT 1) AS logo_image_tag,
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
                            mi.rating, mi.rating_source, mi.runtime_ticks
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
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                         ORDER BY image_index LIMIT 1) AS poster_image_tag,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                         ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'THUMB'
                         ORDER BY image_index LIMIT 1) AS thumb_image_tag,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'LOGO'
                         ORDER BY image_index LIMIT 1) AS logo_image_tag,
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
                            mi.rating, mi.rating_source, mi.runtime_ticks
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
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                     ORDER BY image_index LIMIT 1) AS poster_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                     ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'THUMB'
                     ORDER BY image_index LIMIT 1) AS thumb_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'LOGO'
                     ORDER BY image_index LIMIT 1) AS logo_image_tag,
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
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                         ORDER BY image_index LIMIT 1) AS poster_image_tag,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                         ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'THUMB'
                         ORDER BY image_index LIMIT 1) AS thumb_image_tag,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'LOGO'
                         ORDER BY image_index LIMIT 1) AS logo_image_tag,
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
        Ok(chapters)
    }

    async fn fetch_catalog_rows(
        &self,
        query: &str,
        binds: &[CatalogBind<'_>],
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        let mut statement = self.query(sqlx::AssertSqlSafe(query));
        for bind in binds {
            statement = match bind {
                CatalogBind::Text(value) => statement.bind(*value),
                CatalogBind::Integer(value) => statement.bind(*value),
            };
        }
        statement
            .fetch_all(&self.pool)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(|row| StoredCatalogRow {
                        item_id: row.get("item_id"),
                        library_id: row.get("library_id"),
                        item_type: row.get("item_type"),
                        parent_id: row.get("parent_id"),
                        series_id: row.get("series_id"),
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
                        poster_image_tag: row.get("poster_image_tag"),
                        fanart_image_tag: row.get("fanart_image_tag"),
                        thumb_image_tag: row.get("thumb_image_tag"),
                        logo_image_tag: row.get("logo_image_tag"),
                        source_id: row.get("source_id"),
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
                    .collect()
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
             SET provider_ids_json = ?
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

    pub(crate) async fn merge_local_provider_ids(
        &self,
        item_id: &str,
        provider_ids: &BTreeMap<String, String>,
    ) -> Result<(), StorageError> {
        if provider_ids.is_empty() {
            return Ok(());
        }
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let current = self
            .query_scalar::<Option<String>>(
                "SELECT provider_ids_json
                 FROM media_items
                 WHERE id = ? AND removed_at IS NULL",
            )
            .bind(item_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .flatten();
        let mut merged = current
            .as_deref()
            .and_then(|value| serde_json::from_str::<BTreeMap<String, String>>(value).ok())
            .unwrap_or_default();
        let mut changed = false;
        for (provider, provider_id) in provider_ids {
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
            changed = true;
        }
        if changed {
            let provider_ids_json = serde_json::to_string(&merged)
                .map_err(|error| StorageError::Serialization(error.to_string()))?;
            self.query(
                "UPDATE media_items
                 SET provider_ids_json = ?
                 WHERE id = ? AND removed_at IS NULL",
            )
            .bind(provider_ids_json)
            .bind(item_id)
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

    pub(crate) async fn update_local_provider_ids_for_identity_if_empty(
        &self,
        identity_key: &str,
        provider_ids_json: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE media_items
             SET provider_ids_json = ?
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
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'PENDING')",
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
        self.query(
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
               AND EXISTS (
                   SELECT 1 FROM scan_job_paths sjp
                   WHERE sjp.job_id = ?
                     AND sjp.processed_at IS NOT NULL
                     AND sjp.library_root_id = fe.library_root_id
                     AND (
                           sjp.relative_path = '.'
                           OR
                           fe.relative_path = sjp.relative_path
                           OR substr(fe.relative_path, 1, length(sjp.relative_path) + 1)
                              = sjp.relative_path || '/'
                     )
               )
             ORDER BY ms.item_id, fe.relative_path",
        )
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
             SET status = ?, error = ?, finished_at = unixepoch(), updated_at = unixepoch()
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
                    ii.local_path AS thumbnail_path
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             LEFT JOIN item_images ii
               ON ii.item_id = ms.item_id
              AND ii.image_type = 'THUMB'
              AND ii.image_index = 0
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
                    thumbnail_path: row.get("thumbnail_path"),
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
        self.query(
            "SELECT ms.item_id, lr.canonical_path AS root_path, fe.relative_path,
                    ii.local_path AS thumbnail_path
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             LEFT JOIN item_images ii
               ON ii.item_id = ms.item_id
              AND ii.image_type = 'THUMB'
              AND ii.image_index = 0
             WHERE ms.source_kind = 'LOCAL_FILE'
               AND fe.is_missing = 0
               AND mi.removed_at IS NULL
               AND EXISTS (
                   SELECT 1 FROM scan_job_paths sjp
                   WHERE sjp.job_id = ?
                     AND sjp.processed_at IS NOT NULL
                     AND sjp.library_root_id = fe.library_root_id
                     AND (
                           sjp.relative_path = '.'
                           OR
                           fe.relative_path = sjp.relative_path
                           OR substr(fe.relative_path, 1, length(sjp.relative_path) + 1)
                              = sjp.relative_path || '/'
                     )
               )
             ORDER BY ms.item_id, ms.is_default DESC, ms.id
             LIMIT ? OFFSET ?",
        )
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
                    thumbnail_path: row.get("thumbnail_path"),
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
                    ii.local_path AS thumbnail_path
             FROM scan_job_targets t
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
             LEFT JOIN item_images ii
               ON ii.item_id = t.item_id
              AND ii.image_type = 'THUMB'
              AND ii.image_index = 0
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
                    thumbnail_path: row.get("thumbnail_path"),
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
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn count_strm_media_sources_for_library(
        &self,
        library_id: &str,
    ) -> Result<i64, StorageError> {
        self.query_scalar(
            "SELECT COUNT(*)
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             WHERE mi.library_id = ? AND ms.source_kind = 'STRM_URL'
               AND fe.is_missing = 0",
        )
        .bind(library_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_strm_media_sources_for_incremental_scan_page(
        &self,
        scan_job_id: &str,
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
                   AND ms.id > ? AND EXISTS (
                       SELECT 1 FROM scan_job_paths sjp
                       WHERE sjp.job_id = ? AND sjp.processed_at IS NOT NULL
                         AND sjp.library_root_id = fe.library_root_id
                         AND (
                               sjp.relative_path = '.'
                               OR
                               fe.relative_path = sjp.relative_path
                               OR substr(fe.relative_path, 1, length(sjp.relative_path) + 1)
                                  = sjp.relative_path || '/'
                             )
                   )
                 ORDER BY ms.id, fe.relative_path
                 LIMIT ?",
            )
            .bind(after_source_id)
            .bind(scan_job_id)
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
                   AND EXISTS (
                       SELECT 1 FROM scan_job_paths sjp
                       WHERE sjp.job_id = ? AND sjp.processed_at IS NOT NULL
                         AND sjp.library_root_id = fe.library_root_id
                         AND (
                               sjp.relative_path = '.'
                               OR
                               fe.relative_path = sjp.relative_path
                               OR substr(fe.relative_path, 1, length(sjp.relative_path) + 1)
                                  = sjp.relative_path || '/'
                             )
                   )
                 ORDER BY ms.id, fe.relative_path
                 LIMIT ?",
            )
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
        self.query_scalar(
            "SELECT COUNT(DISTINCT ms.id)
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             WHERE ms.source_kind = 'STRM_URL'
               AND fe.is_missing = 0 AND mi.removed_at IS NULL
               AND EXISTS (
                   SELECT 1 FROM scan_job_paths sjp
                   WHERE sjp.job_id = ? AND sjp.processed_at IS NOT NULL
                     AND sjp.library_root_id = fe.library_root_id
                     AND (
                           sjp.relative_path = '.'
                           OR
                           fe.relative_path = sjp.relative_path
                           OR substr(fe.relative_path, 1, length(sjp.relative_path) + 1)
                              = sjp.relative_path || '/'
                         )
               )",
        )
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

    pub(crate) async fn find_authorized_playback_source(
        &self,
        item_id: &str,
        source_id: Option<&str>,
        user_id: &str,
        is_admin: bool,
    ) -> Result<Option<StoredPlaybackSource>, StorageError> {
        let access_join = if is_admin {
            ""
        } else {
            "JOIN user_library_access ula
               ON ula.user_id = ?
              AND ula.library_id = mi.library_id
              AND ula.can_view = 1"
        };
        let query = format!(
            "SELECT ms.id AS source_id, ms.source_kind, ms.container, ms.external_url,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             {access_join}
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE mi.id = ? AND mi.removed_at IS NULL
               AND (? IS NULL OR ms.id = ?)
               AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
               AND fe.is_missing = 0
             ORDER BY ms.is_default DESC, ms.id
             LIMIT 1"
        );
        let mut statement = self.query(sqlx::AssertSqlSafe(query));
        if !is_admin {
            statement = statement.bind(user_id);
        }
        statement = statement.bind(item_id).bind(source_id).bind(source_id);
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
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query("DELETE FROM media_streams WHERE media_source_id = ?")
            .bind(update.source_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        for stream in update.streams {
            self.query(
                "INSERT INTO media_streams (
                    id, media_source_id, stream_index, stream_type,
                    codec, language, title, details_json, external_path,
                    is_external, is_default, is_forced
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
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
            .bind(database_flag(stream.is_forced))
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
               AND episode.removed_at IS NULL
               AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
               AND fe.is_missing = 0
               AND EXISTS (
                   SELECT 1 FROM scan_job_paths sjp
                   WHERE sjp.job_id = ?
                     AND sjp.processed_at IS NOT NULL
                     AND sjp.library_root_id = fe.library_root_id
                     AND (
                           sjp.relative_path = '.'
                           OR
                           fe.relative_path = sjp.relative_path
                           OR substr(fe.relative_path, 1, length(sjp.relative_path) + 1)
                              = sjp.relative_path || '/'
                     )
               )
             ORDER BY series.id, season.season_number, episode.id, fe.relative_path",
        )
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
                    fe.fingerprint, ms.duration_ticks,
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
                    fe.fingerprint, ms.duration_ticks,
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
        self.query(
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
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
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
        for item in items {
            self.query(
                "INSERT INTO chapter_detection_job_items (
                    job_id, source_id, item_id, season_id, source_fingerprint,
                    input_fingerprint, is_context, status
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, 'PENDING')",
            )
            .bind(item.job_id)
            .bind(item.source_id)
            .bind(item.item_id)
            .bind(item.season_id)
            .bind(item.source_fingerprint)
            .bind(item.input_fingerprint)
            .bind(database_flag(item.is_context))
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
        for marker in markers {
            self.query(
                "INSERT INTO media_chapters (
                    id, media_source_id, start_position_ticks, name, marker_type,
                    chapter_index, provider_id, confidence
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(Uuid::now_v7().to_string())
            .bind(source_id)
            .bind(marker.start_position_ticks)
            .bind(marker.name.clone())
            .bind(marker.marker_type.clone())
            .bind(marker.chapter_index)
            .bind(provider_id)
            .bind(marker.confidence)
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
                     THEN ? ELSE provider_ids_json END
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

    pub(crate) async fn update_media_item_metadata(
        &self,
        update: MediaMetadataUpdate<'_>,
    ) -> Result<(), StorageError> {
        let sort_title = update.title.to_lowercase();
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        self.query(
            "UPDATE media_items
             SET title = ?,
                 sort_title = ?,
                 original_title = ?,
                 overview = ?,
                 production_year = ?,
                 rating = COALESCE(?, rating),
                 rating_source = CASE WHEN ? IS NULL THEN rating_source ELSE ? END,
                 metadata_fingerprint = ?,
                 metadata_provenance_json = ?,
                 locked_fields_json = ?
             WHERE id = ?",
        )
        .bind(update.title)
        .bind(sort_title)
        .bind(update.original_title)
        .bind(update.overview)
        .bind(update.production_year)
        .bind(update.rating)
        .bind(update.rating_source)
        .bind(update.rating_source)
        .bind(update.metadata_fingerprint)
        .bind(update.provenance_json)
        .bind(update.locked_fields_json)
        .bind(update.item_id)
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

    pub(crate) async fn media_item_metadata_fingerprint(
        &self,
        item_id: &str,
    ) -> Result<Option<Vec<u8>>, StorageError> {
        self.query_scalar(
            "SELECT metadata_fingerprint
             FROM media_items
             WHERE id = ? AND metadata_fingerprint IS NOT NULL",
        )
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
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
             SET nfo_metadata_json = ?, nfo_metadata_fingerprint = ?
             WHERE id = ?",
        )
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
             SET nfo_metadata_json = NULL, nfo_metadata_fingerprint = NULL
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

    pub(crate) async fn invalidate_media_item_nfo_metadata_if_source_changed(
        &self,
        item_id: &str,
        source_fingerprint: &[u8],
    ) -> Result<(), StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        self.query(
            "UPDATE media_items
             SET nfo_metadata_json = NULL, nfo_metadata_fingerprint = NULL
             WHERE id = ?
               AND (nfo_metadata_fingerprint IS NULL OR nfo_metadata_fingerprint <> ?)",
        )
        .bind(item_id)
        .bind(source_fingerprint)
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
    ) -> Result<(), StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        self.query("UPDATE media_items SET metadata_fingerprint = ? WHERE id = ?")
            .bind(metadata_fingerprint)
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

    pub(crate) async fn mark_metadata_image_unavailable(
        &self,
        item_id: &str,
        image_type: &str,
        candidate_key: &str,
        now: i64,
    ) -> Result<(), StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        self.query(
            "INSERT INTO metadata_image_attempts (
                item_id, image_type, candidate_key, status, attempt_count,
                last_attempt_at, next_retry_at, claimed_until, error_code, updated_at
            ) VALUES (?, ?, ?, 'UNAVAILABLE', 1, ?, NULL, NULL, 'NO_IMAGE', ?)
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
                updated_at = excluded.updated_at",
        )
        .bind(item_id)
        .bind(image_type)
        .bind(candidate_key)
        .bind(now)
        .bind(now)
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

    pub(crate) async fn insert_item_images_at_indices(
        &self,
        item_id: &str,
        images: &[ItemImageInsert],
    ) -> Result<usize, StorageError> {
        if images.is_empty() {
            return Ok(0);
        }

        const MAX_ROWS_PER_BATCH: usize = 64;
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let mut inserted_count = 0_usize;
        for batch in images.chunks(MAX_ROWS_PER_BATCH) {
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
            for image in batch {
                statement = statement
                    .bind(Uuid::now_v7().to_string())
                    .bind(item_id)
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
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(inserted_count)
    }

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

    pub(crate) async fn upsert_item_image(
        &self,
        item_id: &str,
        image_type: &str,
        local_path: &std::path::Path,
        metadata: ItemImageMetadata<'_>,
    ) -> Result<String, StorageError> {
        self.upsert_item_image_at_index(item_id, image_type, 0, local_path, metadata)
            .await
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
            "SELECT ii.id, ii.local_path, lr.canonical_path AS root_path
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

    pub(crate) async fn item_image_source_url_exists(
        &self,
        item_id: &str,
        image_type: &str,
        source_url: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar::<i64>(
            "SELECT 1 FROM item_images
             WHERE item_id = ? AND image_type = ? AND source_url = ?
             LIMIT 1",
        )
        .bind(item_id)
        .bind(image_type)
        .bind(source_url)
        .fetch_optional(&self.pool)
        .await
        .map(|value| value.is_some())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
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

    pub(crate) async fn find_item_image_source(
        &self,
        item_id: &str,
        image_type: &str,
    ) -> Result<Option<String>, StorageError> {
        self.query_scalar(
            "SELECT source
             FROM item_images
             WHERE item_id = ? AND image_type = ? AND image_index = 0",
        )
        .bind(item_id)
        .bind(image_type)
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
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
        self.query(
            "INSERT INTO access_tokens (
                id, token_hash, user_id, device_id, client_name,
                device_name, client_version
            ) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(token.id)
        .bind(token.token_hash)
        .bind(token.user_id)
        .bind(token.device_id)
        .bind(token.client_name)
        .bind(token.device_name)
        .bind(token.client_version)
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
        self.query(
            "INSERT INTO web_sessions (
                id, user_id, session_token_hash, csrf_token_hash, expires_at
            ) VALUES (?, ?, ?, ?, unixepoch() + ?)",
        )
        .bind(id)
        .bind(user_id)
        .bind(session_token_hash)
        .bind(csrf_token_hash)
        .bind(lifetime_seconds)
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
            "SELECT ws.csrf_token_hash, u.id AS user_id,
                    u.username_normalized, u.display_name, u.is_disabled,
                    u.is_admin, u.can_manage_server, u.can_remote_access,
                    u.can_download
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
                user_id: row.get("user_id"),
                username_normalized: row.get("username_normalized"),
                display_name: row.get("display_name"),
                is_disabled: row.get::<i64, _>("is_disabled") != 0,
                is_admin: row.get::<i64, _>("is_admin") != 0,
                can_manage_server: row.get::<i64, _>("can_manage_server") != 0,
                can_remote_access: row.get::<i64, _>("can_remote_access") != 0,
                can_download: row.get::<i64, _>("can_download") != 0,
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

    pub(crate) async fn revoke_web_session_by_id(
        &self,
        user_id: &str,
        session_id: &str,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE web_sessions
             SET revoked_at = unixepoch(), updated_at = unixepoch()
             WHERE id = ? AND user_id = ? AND revoked_at IS NULL",
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
}
