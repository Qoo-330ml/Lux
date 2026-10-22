pub(crate) const MAX_MEDIA_SOURCE_DELETE_BATCH_SIZE: usize = 250;

use super::*;

impl Database {
    pub(crate) async fn restore_media_item(&self, item_id: &str) -> Result<(), StorageError> {
        self.query(
            "UPDATE media_items
             SET removed_at = NULL, updated_at = unixepoch()
             WHERE id = ?",
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

    /// Filesystem entry ids of the media sources (video / STRM files) below the given library
    /// root directories, at any depth.
    ///
    /// Used to re-run local metadata (NFO and image) indexing for exact directories without
    /// walking the whole root. Directories are relative to the root and must not be empty.
    pub(crate) async fn list_media_source_entry_ids_under_directories(
        &self,
        library_root_id: &str,
        directories: &[String],
        limit: i64,
    ) -> Result<Vec<String>, StorageError> {
        let mut ids = Vec::new();
        for chunk in directories.chunks(SCAN_DML_CHUNK_SIZE) {
            if chunk.is_empty() {
                continue;
            }
            let remaining = limit.saturating_sub(i64::try_from(ids.len()).unwrap_or(i64::MAX));
            if remaining <= 0 {
                break;
            }
            let values = std::iter::repeat_n("(?)", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            // PostgreSQL: byte-order range operators served by idx_filesystem_entries_dir_prefix,
            // driven from the directories with LATERAL (a plain join is planned as a full scan of
            // filesystem_entries; see postgres_sidecar_target_query).
            let query = if self.backend == DatabaseBackend::Postgres {
                format!(
                    "WITH sd(directory) AS (VALUES {values})
                     SELECT DISTINCT matched.id
                     FROM sd
                     CROSS JOIN LATERAL (
                         SELECT fe.id FROM filesystem_entries fe
                         WHERE fe.library_root_id = ? AND fe.is_missing = 0
                           AND fe.entry_kind = 'FILE'
                           AND (fe.relative_path ~>=~ (sd.directory || '/'))
                           AND (fe.relative_path ~<~ (sd.directory || '0'))
                           AND EXISTS (
                               SELECT 1 FROM media_sources ms
                               WHERE ms.filesystem_entry_id = fe.id
                           )
                         OFFSET 0
                     ) matched
                     ORDER BY matched.id
                     LIMIT ?"
                )
            } else {
                format!(
                    "WITH sd(directory) AS (VALUES {values})
                     SELECT DISTINCT fe.id
                     FROM sd
                     CROSS JOIN filesystem_entries fe
                     WHERE fe.library_root_id = ? AND fe.is_missing = 0
                       AND fe.entry_kind = 'FILE'
                       AND fe.relative_path >= sd.directory || '/'
                       AND fe.relative_path < sd.directory || '0'
                       AND EXISTS (
                           SELECT 1 FROM media_sources ms WHERE ms.filesystem_entry_id = fe.id
                       )
                     ORDER BY fe.id
                     LIMIT ?"
                )
            };
            let mut statement = self.query_scalar::<String>(sqlx::AssertSqlSafe(query));
            for directory in chunk {
                statement = statement.bind(directory);
            }
            let rows = statement
                .bind(library_root_id)
                .bind(remaining)
                .fetch_all(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            ids.extend(rows);
        }
        ids.sort_unstable();
        ids.dedup();
        Ok(ids)
    }

    pub(crate) async fn find_item_id_by_media_source_id(
        &self,
        source_id: &str,
    ) -> Result<Option<String>, StorageError> {
        self.query_scalar(
            "SELECT ms.item_id
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             WHERE ms.id = ? AND mi.removed_at IS NULL
             LIMIT 1",
        )
        .bind(source_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_movie_item_for_filesystem_paths(
        &self,
        library_root_id: &str,
        relative_paths: &[String],
    ) -> Result<Option<String>, StorageError> {
        if relative_paths.is_empty() {
            return Ok(None);
        }
        let placeholders = std::iter::repeat_n("?", relative_paths.len())
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "SELECT DISTINCT current_item.id
             FROM filesystem_entries entry
             JOIN media_sources source ON source.filesystem_entry_id = entry.id
             JOIN media_items item ON item.id = source.item_id
             JOIN media_items current_item
               ON current_item.id = COALESCE(item.merged_into_item_id, item.id)
             JOIN library_roots root ON root.id = entry.library_root_id
             WHERE entry.library_root_id = ? AND entry.relative_path IN ({placeholders})
               AND entry.entry_kind = 'FILE' AND entry.is_missing = 0
               AND current_item.library_id = root.library_id
               AND current_item.item_type = 'MOVIE' AND current_item.removed_at IS NULL
               AND current_item.merged_into_item_id IS NULL
             LIMIT 2"
        );
        let mut statement = self.query_scalar::<String>(sqlx::AssertSqlSafe(query));
        statement = statement.bind(library_root_id);
        for relative_path in relative_paths {
            statement = statement.bind(relative_path);
        }
        let item_ids =
            statement
                .fetch_all(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        Ok(match item_ids.as_slice() {
            [item_id] => Some(item_id.clone()),
            _ => None,
        })
    }

    pub(crate) async fn refresh_removed_media_items(
        &self,
        library_root_id: &str,
    ) -> Result<(), StorageError> {
        let mut transaction = self.begin_scan_write_transaction().await?;
        self.refresh_removed_media_items_in_transaction(&mut transaction, library_root_id)
            .await?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_episode_identity_repair_candidates(
        &self,
    ) -> Result<Vec<StoredEpisodeIdentityCandidate>, StorageError> {
        self.query(
            "SELECT DISTINCT ms.item_id, fe.id, fe.library_root_id, fe.relative_path
             FROM media_sources ms
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN media_items episode ON episode.id = ms.item_id
             WHERE episode.item_type = 'EPISODE' AND fe.is_missing = 0
             ORDER BY fe.library_root_id, fe.relative_path, ms.item_id",
        )
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredEpisodeIdentityCandidate {
                    episode_id: row.get("item_id"),
                    filesystem_entry_id: row.get("id"),
                    library_root_id: row.get("library_root_id"),
                    relative_path: row.get("relative_path"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn restore_media_items_for_filesystem_entries(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        entry_ids: &[String],
    ) -> Result<(), StorageError> {
        for chunk in entry_ids.chunks(500) {
            if chunk.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "WITH source_items(item_id) AS (
                     SELECT item_id
                     FROM media_sources
                     WHERE filesystem_entry_id IN ({placeholders})
                 ),
                 items_to_restore(item_id) AS (
                     SELECT item_id FROM source_items
                     UNION
                     SELECT parent_id
                     FROM media_items
                     WHERE id IN (SELECT item_id FROM source_items)
                     UNION
                     SELECT series_id
                     FROM media_items
                     WHERE id IN (SELECT item_id FROM source_items)
                 )
                 UPDATE media_items
                 SET removed_at = NULL, updated_at = unixepoch()
                 WHERE removed_at IS NOT NULL
                   AND id IN (SELECT item_id FROM items_to_restore)"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for entry_id in chunk {
                statement = statement.bind(entry_id);
            }
            statement
                .execute(&mut **transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }
        Ok(())
    }

    pub(super) async fn refresh_removed_media_items_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        library_root_id: &str,
    ) -> Result<(), StorageError> {
        let library_id = self
            .query_scalar::<String>("SELECT library_id FROM library_roots WHERE id = ?")
            .bind(library_root_id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if let Some(library_id) = library_id {
            self.query(
                "UPDATE media_items
                 SET removed_at = unixepoch(), updated_at = unixepoch()
                 WHERE library_id = ?
                   AND item_type IN ('MOVIE', 'EPISODE', 'UNRESOLVED', 'VIDEO')
                   AND removed_at IS NULL
                   AND EXISTS (
                       SELECT 1
                       FROM media_sources source
                       WHERE source.item_id = media_items.id
                   )
                   AND NOT EXISTS (
                       SELECT 1
                       FROM media_sources source
                       JOIN filesystem_entries entry
                         ON entry.id = source.filesystem_entry_id
                       WHERE source.item_id = media_items.id
                         AND entry.is_missing = 0
                   )",
            )
            .bind(&library_id)
            .execute(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
            for item_type in ["SEASON", "SERIES"] {
                self.query(
                    "UPDATE media_items
                     SET removed_at = unixepoch(), updated_at = unixepoch()
                     WHERE library_id = ?
                       AND item_type = ?
                       AND removed_at IS NULL
                       AND NOT EXISTS (
                           SELECT 1
                           FROM media_items child
                           WHERE child.removed_at IS NULL
                             AND (
                                 child.parent_id = media_items.id
                                 OR child.series_id = media_items.id
                             )
                       )",
                )
                .bind(&library_id)
                .bind(item_type)
                .execute(&mut **transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            }
        }
        Ok(())
    }

    pub(crate) async fn reset_media_probe_for_filesystem_entry(
        &self,
        filesystem_entry_id: &str,
        size: i64,
    ) -> Result<(), StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.reset_media_probe_for_filesystem_entry_in_transaction(
            &mut transaction,
            filesystem_entry_id,
            size,
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

    pub(crate) async fn reset_media_probe_for_filesystem_entry_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Any>,
        filesystem_entry_id: &str,
        size: i64,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE media_sources
             SET size = ?,
                 probe_status = CASE WHEN source_kind = 'STRM_URL'
                     THEN 'NOT_CONFIGURED' ELSE 'PENDING' END,
                 probe_error = NULL,
                 updated_at = unixepoch()
             WHERE filesystem_entry_id = ?",
        )
        .bind(size)
        .bind(filesystem_entry_id)
        .execute(&mut **transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query(
            "DELETE FROM media_chapters
             WHERE media_source_id IN (
                 SELECT id FROM media_sources WHERE filesystem_entry_id = ?
             )",
        )
        .bind(filesystem_entry_id)
        .execute(&mut **transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        Ok(())
    }

    pub(crate) async fn update_media_source_strm_target(
        &self,
        filesystem_entry_id: &str,
        strm_target_kind: Option<&str>,
        strm_target: Option<&str>,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE media_sources
             SET external_url = ?, strm_target_kind = ?, updated_at = unixepoch()
             WHERE filesystem_entry_id = ?",
        )
        .bind(strm_target)
        .bind(strm_target_kind)
        .bind(filesystem_entry_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn update_media_source_variant_labels(
        &self,
        filesystem_entry_id: &str,
        edition_name: Option<&str>,
        quality_label: Option<&str>,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE media_sources
             SET edition_name = ?, quality_label = ?, updated_at = unixepoch()
             WHERE filesystem_entry_id = ?",
        )
        .bind(edition_name)
        .bind(quality_label)
        .bind(filesystem_entry_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn reassign_media_source_item(
        &self,
        filesystem_entry_id: &str,
        new_item_id: &str,
    ) -> Result<bool, StorageError> {
        let Some((old_item_id, parent_id, series_id)) = self
            .query_as::<(String, Option<String>, Option<String>)>(
                "SELECT ms.item_id, old_item.parent_id, old_item.series_id
             FROM media_sources ms
             JOIN media_items old_item ON old_item.id = ms.item_id
             WHERE ms.filesystem_entry_id = ?",
            )
            .bind(filesystem_entry_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
        else {
            return Ok(false);
        };
        if old_item_id == new_item_id {
            return Ok(false);
        }

        let mut transaction = self.begin_scan_write_transaction().await?;
        let max_function = self.scalar_max_function();
        let query = format!(
            "INSERT INTO user_item_state (
                user_id, item_id, position_ticks, is_played, is_favorite,
                play_count, last_played_at, version
             )
             SELECT user_id, ?, position_ticks, is_played, is_favorite,
                    play_count, last_played_at, version
             FROM user_item_state
             WHERE item_id = ?
             ON CONFLICT(user_id, item_id) DO UPDATE SET
                position_ticks = {max_function}(user_item_state.position_ticks, excluded.position_ticks),
                is_played = {max_function}(user_item_state.is_played, excluded.is_played),
                is_favorite = {max_function}(user_item_state.is_favorite, excluded.is_favorite),
                play_count = {max_function}(user_item_state.play_count, excluded.play_count),
                last_played_at = {max_function}(user_item_state.last_played_at, excluded.last_played_at),
                version = {max_function}(user_item_state.version, excluded.version)"
        );
        self.query(sqlx::AssertSqlSafe(query))
            .bind(new_item_id)
            .bind(&old_item_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.query("DELETE FROM user_item_state WHERE item_id = ?")
            .bind(&old_item_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.query(
            "UPDATE media_sources
             SET item_id = ?, updated_at = unixepoch()
             WHERE filesystem_entry_id = ?",
        )
        .bind(new_item_id)
        .bind(filesystem_entry_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        // The moved source may now be a second default of the target item, and the old
        // item may have lost its only default.
        self.normalize_default_source_in_transaction(&mut transaction, new_item_id)
            .await?;
        self.normalize_default_source_in_transaction(&mut transaction, &old_item_id)
            .await?;

        for item_id in [Some(old_item_id), parent_id, series_id]
            .into_iter()
            .flatten()
        {
            self.query(
                "UPDATE media_items
                 SET removed_at = unixepoch(), updated_at = unixepoch()
                 WHERE id = ?
                   AND removed_at IS NULL
                   AND NOT EXISTS (
                       SELECT 1 FROM media_sources WHERE item_id = media_items.id
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM media_items child
                       WHERE child.parent_id = media_items.id
                         AND child.removed_at IS NULL
                   )",
            )
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
            })?;
        Ok(true)
    }

    pub(crate) async fn delete_media_sources_atomically(
        &self,
        sources: &[(String, String)],
    ) -> Result<bool, StorageError> {
        if sources.is_empty() {
            return Ok(false);
        }
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        for source_batch in sources.chunks(MAX_MEDIA_SOURCE_DELETE_BATCH_SIZE) {
            let source_pairs = source_batch
                .iter()
                .map(|(item_id, source_id)| (item_id.as_str(), source_id.as_str()))
                .collect::<Vec<_>>();
            if !self
                .delete_media_sources_in_transaction(&mut transaction, &source_pairs)
                .await?
            {
                return Ok(false);
            }
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

    async fn delete_media_sources_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        source_pairs: &[(&str, &str)],
    ) -> Result<bool, StorageError> {
        if source_pairs.is_empty() {
            return Ok(true);
        }
        let source_placeholders = std::iter::repeat_n("?", source_pairs.len())
            .collect::<Vec<_>>()
            .join(", ");
        let mut lookup = self.query_as::<(String, String, Option<String>, Option<String>)>(
            sqlx::AssertSqlSafe(format!(
                "SELECT ms.id, ms.item_id, old_item.parent_id, old_item.series_id
                 FROM media_sources ms
                 JOIN media_items old_item ON old_item.id = ms.item_id
                 WHERE ms.id IN ({source_placeholders})"
            )),
        );
        for (_, source_id) in source_pairs {
            lookup = lookup.bind(source_id);
        }
        let rows = lookup
            .fetch_all(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let requested_pairs = source_pairs.iter().copied().collect::<HashSet<_>>();
        if rows.len() != source_pairs.len()
            || rows.iter().any(|(stored_source_id, stored_item_id, _, _)| {
                !requested_pairs.contains(&(stored_item_id.as_str(), stored_source_id.as_str()))
            })
        {
            return Ok(false);
        }

        let mut item_ids = HashSet::with_capacity(rows.len());
        let mut parent_ids = HashSet::with_capacity(rows.len());
        let mut series_ids = HashSet::with_capacity(rows.len());
        for (_, old_item_id, parent_id, series_id) in rows {
            item_ids.insert(old_item_id);
            if let Some(parent_id) = parent_id {
                parent_ids.insert(parent_id);
            }
            if let Some(series_id) = series_id {
                series_ids.insert(series_id);
            }
        }

        let delete_predicates = std::iter::repeat_n("(id = ? AND item_id = ?)", source_pairs.len())
            .collect::<Vec<_>>()
            .join(" OR ");
        let mut delete_query = self.query(sqlx::AssertSqlSafe(format!(
            "DELETE FROM media_sources WHERE {delete_predicates}"
        )));
        for (item_id, source_id) in source_pairs {
            delete_query = delete_query.bind(source_id).bind(item_id);
        }
        let deleted = delete_query
            .execute(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if usize::try_from(deleted.rows_affected()).unwrap_or(usize::MAX) != source_pairs.len() {
            return Ok(false);
        }
        self.mark_media_items_removed_in_transaction(transaction, &item_ids)
            .await?;
        self.mark_media_items_removed_in_transaction(transaction, &parent_ids)
            .await?;
        self.mark_media_items_removed_in_transaction(transaction, &series_ids)
            .await?;
        Ok(true)
    }

    async fn mark_media_items_removed_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        item_ids: &HashSet<String>,
    ) -> Result<(), StorageError> {
        if item_ids.is_empty() {
            return Ok(());
        }
        let placeholders = std::iter::repeat_n("?", item_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let mut update_query = self.query(sqlx::AssertSqlSafe(format!(
            "UPDATE media_items
             SET removed_at = unixepoch(), updated_at = unixepoch()
             WHERE id IN ({placeholders}) AND removed_at IS NULL
               AND NOT EXISTS (
                   SELECT 1 FROM media_sources WHERE item_id = media_items.id
               )
               AND NOT EXISTS (
                   SELECT 1 FROM media_items child
                   WHERE child.parent_id = media_items.id
                     AND child.removed_at IS NULL
               )"
        )));
        for item_id in item_ids {
            update_query = update_query.bind(item_id);
        }
        update_query
            .execute(&mut **transaction)
            .await
            .map(|_| ())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }
}
