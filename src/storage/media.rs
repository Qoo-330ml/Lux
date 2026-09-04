use super::*;

struct BatchHierarchyRow {
    id: String,
    library_id: String,
    item_type: &'static str,
    parent_id: Option<String>,
    series_id: Option<String>,
    season_number: Option<i64>,
    episode_number: Option<i64>,
    absolute_number: Option<i64>,
    title: String,
    sort_title: String,
    original_title: Option<String>,
    production_year: Option<i64>,
    provider_ids_json: Option<String>,
    identity_key: String,
}

fn stored_media_metadata(row: sqlx::any::AnyRow) -> StoredMediaMetadata {
    let scraper_id = row.get::<Option<String>, _>("scraper_id");
    let series_scraper_id = row
        .get::<Option<String>, _>("series_metadata_scraper_id")
        .or_else(|| scraper_id.clone());
    let series_provider = first_provider_id(
        row.get("series_provider_ids_json"),
        None,
        series_scraper_id.as_deref(),
    );
    StoredMediaMetadata {
        item_type: row.get("item_type"),
        title: row.get("title"),
        original_title: row.get("original_title"),
        overview: row.get("overview"),
        production_year: row.get("production_year"),
        premiere_date: row.get("premiere_date"),
        last_air_date: row.get("last_air_date"),
        status: row.get("status"),
        original_language: row.get("original_language"),
        rating: row.get("rating"),
        provider_ids_json: row.get("provider_ids_json"),
        metadata_scraper_id: row.get("metadata_scraper_id"),
        identification_status: row.get("identification_status"),
        scraper_id,
        provenance_json: row.get("metadata_provenance_json"),
        locked_fields_json: row.get("locked_fields_json"),
        nfo_metadata_json: row.get("nfo_metadata_json"),
        series_item_id: row.get("series_id"),
        series_title: row.get("series_title"),
        series_production_year: row.get("series_production_year"),
        series_provider_name: series_provider.as_ref().map(|(name, _)| name.clone()),
        series_provider_id: series_provider.map(|(_, id)| id),
        season_number: row.get("season_number"),
        episode_number: row.get("episode_number"),
    }
}

impl Database {
    pub(crate) async fn media_source_belongs_to_item(
        &self,
        source_id: &str,
        item_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(
                SELECT 1 FROM media_sources WHERE id = ? AND item_id = ?
            ) THEN 1 ELSE 0 END",
        )
        .bind(source_id)
        .bind(item_id)
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_item_library_id(
        &self,
        item_id: &str,
    ) -> Result<Option<String>, StorageError> {
        self.query_scalar(
            "SELECT mi.library_id
             FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             WHERE mi.id = ? AND mi.removed_at IS NULL",
        )
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_item_scan_source_path(
        &self,
        item_id: &str,
    ) -> Result<Option<StoredItemScanPath>, StorageError> {
        self.query(
            "SELECT source_item.library_id, fe.library_root_id, fe.relative_path
             FROM media_items source_item
             JOIN media_sources ms ON ms.item_id = source_item.id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             WHERE source_item.removed_at IS NULL
               AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
               AND fe.is_missing = 0
               AND (
                    source_item.id = ?
                    OR (
                        source_item.item_type = 'EPISODE'
                        AND (source_item.series_id = ? OR source_item.parent_id = ?)
                    )
               )
             ORDER BY CASE WHEN source_item.id = ? THEN 0 ELSE 1 END,
                      ms.is_default DESC, ms.id
             LIMIT 1",
        )
        .bind(item_id)
        .bind(item_id)
        .bind(item_id)
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredItemScanPath {
                library_id: row.get("library_id"),
                library_root_id: row.get("library_root_id"),
                relative_path: row.get("relative_path"),
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_item_source_locator(
        &self,
        item_id: &str,
    ) -> Result<Option<StoredItemSourceLocator>, StorageError> {
        self.query(
            "SELECT lr.canonical_path, fe.relative_path,
                    fe.fingerprint, fe.size, fe.modified_at,
                    mi.title, mi.production_year
             FROM media_sources ms
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             JOIN media_items mi ON mi.id = ms.item_id
             WHERE ms.item_id = ? AND mi.removed_at IS NULL AND fe.is_missing = 0
             ORDER BY ms.is_default DESC, ms.id
             LIMIT 1",
        )
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredItemSourceLocator {
                root_path: row.get("canonical_path"),
                relative_path: row.get("relative_path"),
                fingerprint: row.get("fingerprint"),
                size: row.get("size"),
                modified_at: row.get("modified_at"),
                title: row.get("title"),
                production_year: row.get("production_year"),
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_item_by_source_locator(
        &self,
        root_path: &str,
        relative_path: &str,
    ) -> Result<Option<StoredItemSourceLocator>, StorageError> {
        self.query(
            "SELECT lr.canonical_path, fe.relative_path,
                    fe.fingerprint, fe.size, fe.modified_at,
                    mi.title, mi.production_year
             FROM media_sources ms
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             JOIN media_items mi ON mi.id = ms.item_id
             WHERE lr.canonical_path = ? AND fe.relative_path = ?
               AND mi.removed_at IS NULL AND fe.is_missing = 0
             ORDER BY ms.is_default DESC, ms.id
             LIMIT 1",
        )
        .bind(root_path)
        .bind(relative_path)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredItemSourceLocator {
                root_path: row.get("canonical_path"),
                relative_path: row.get("relative_path"),
                fingerprint: row.get("fingerprint"),
                size: row.get("size"),
                modified_at: row.get("modified_at"),
                title: row.get("title"),
                production_year: row.get("production_year"),
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_items_by_source_fingerprint(
        &self,
        fingerprint: &[u8],
    ) -> Result<Vec<StoredItemSourceLocator>, StorageError> {
        self.query(
            "SELECT lr.canonical_path, fe.relative_path,
                    fe.fingerprint, fe.size, fe.modified_at,
                    mi.title, mi.production_year
             FROM media_sources ms
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             JOIN media_items mi ON mi.id = ms.item_id
             WHERE fe.fingerprint = ?
               AND mi.removed_at IS NULL AND fe.is_missing = 0
             ORDER BY ms.is_default DESC, ms.id",
        )
        .bind(fingerprint)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredItemSourceLocator {
                    root_path: row.get("canonical_path"),
                    relative_path: row.get("relative_path"),
                    fingerprint: row.get("fingerprint"),
                    size: row.get("size"),
                    modified_at: row.get("modified_at"),
                    title: row.get("title"),
                    production_year: row.get("production_year"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_item_scraper_id(
        &self,
        item_id: &str,
    ) -> Result<Option<String>, StorageError> {
        let value = self
            .query_scalar::<String>(
                "SELECT COALESCE(l.scraper_id, '')
             FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             WHERE mi.id = ? AND mi.removed_at IS NULL",
            )
            .bind(item_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(value.filter(|value| !value.trim().is_empty()))
    }

    pub(crate) async fn find_item_scrapers(
        &self,
        item_id: &str,
    ) -> Result<Vec<StoredLibraryScraper>, StorageError> {
        self.query(
            "SELECT ls.scraper_id, ls.position, ls.role
             FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             JOIN library_scrapers ls ON ls.library_id = l.id
             WHERE mi.id = ? AND mi.removed_at IS NULL
             ORDER BY ls.position",
        )
        .bind(item_id)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredLibraryScraper {
                    scraper_id: row.get("scraper_id"),
                    position: row.get("position"),
                    role: row.get("role"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn insert_filesystem_entry(
        &self,
        entry: NewFilesystemEntry<'_>,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO filesystem_entries (
                id, library_root_id, relative_path, entry_kind, size,
                modified_at, inode, fingerprint, last_seen_generation, is_missing
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 0)",
        )
        .bind(entry.id)
        .bind(entry.library_root_id)
        .bind(entry.relative_path)
        .bind(entry.entry_kind)
        .bind(entry.size)
        .bind(entry.modified_at)
        .bind(entry.inode)
        .bind(entry.fingerprint)
        .bind(entry.last_seen_generation)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    async fn ensure_movie_parent_folder_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        library_id: &str,
        library_root_id: &str,
        relative_path: &str,
    ) -> Result<Option<String>, StorageError> {
        let directory = relative_path
            .rsplit_once('/')
            .map(|(directory, _)| directory)
            .or_else(|| {
                relative_path
                    .rsplit_once('\\')
                    .map(|(directory, _)| directory)
            })
            .unwrap_or_default();
        let mut parent_folder_id = None;
        let mut parent_id = library_id.to_owned();
        let mut directory_key = String::new();
        for component in directory.split(['/', '\\']) {
            if component.is_empty() || component == "." {
                continue;
            }
            if !directory_key.is_empty() {
                directory_key.push('/');
            }
            directory_key.push_str(component);
            let identity_key = format!("folder:{library_root_id}:{directory_key}");
            let folder_id = self
                .query_scalar::<String>("SELECT id FROM media_items WHERE identity_key = ? LIMIT 1")
                .bind(&identity_key)
                .fetch_optional(&mut **transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            let folder_id = if let Some(folder_id) = folder_id {
                self.query(
                    "UPDATE media_items
                     SET library_id = ?, item_type = 'FOLDER', parent_id = ?,
                         title = ?, sort_title = ?, original_title = ?,
                         identification_status = 'LOCAL_CONFIRMED', removed_at = NULL
                     WHERE id = ?",
                )
                .bind(library_id)
                .bind(&parent_id)
                .bind(component)
                .bind(component.to_ascii_lowercase())
                .bind(component)
                .bind(&folder_id)
                .execute(&mut **transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
                folder_id
            } else {
                let folder_id = Uuid::now_v7().to_string();
                self.query(
                    "INSERT INTO media_items (
                        id, library_id, item_type, parent_id, title, sort_title,
                        original_title, identification_status, identity_key
                    ) VALUES (?, ?, 'FOLDER', ?, ?, ?, ?, 'LOCAL_CONFIRMED', ?)",
                )
                .bind(&folder_id)
                .bind(library_id)
                .bind(&parent_id)
                .bind(component)
                .bind(component.to_ascii_lowercase())
                .bind(component)
                .bind(&identity_key)
                .execute(&mut **transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
                folder_id
            };
            parent_id = folder_id.clone();
            parent_folder_id = Some(folder_id);
        }
        Ok(parent_folder_id)
    }

    async fn prefetch_movie_items_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        library_id: &str,
        files: &[NewMovieFile],
    ) -> Result<HashMap<(String, Option<i64>), PrefetchedMovieItem>, StorageError> {
        let mut sort_titles = files
            .iter()
            .map(|file| file.sort_title.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        sort_titles.sort_unstable();
        let mut movie_items = HashMap::new();
        for chunk in sort_titles.chunks(BATCH_INSERT_CHUNK_SIZE) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT id, sort_title, production_year, parent_id, provider_ids_json, removed_at
                 FROM media_items
                 WHERE library_id = ? AND item_type = 'MOVIE'
                   AND sort_title IN ({placeholders})
                 ORDER BY CASE WHEN removed_at IS NULL THEN 0 ELSE 1 END, id"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query)).bind(library_id);
            for sort_title in chunk {
                statement = statement.bind(sort_title);
            }
            let rows = statement
                .fetch_all(&mut **transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            for row in rows {
                let id = row
                    .try_get::<String, _>("id")
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                let sort_title = row.try_get::<String, _>("sort_title").map_err(|source| {
                    StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    }
                })?;
                let production_year =
                    row.try_get::<Option<i64>, _>("production_year")
                        .map_err(|source| StorageError::Sqlx {
                            path: self.path.clone(),
                            source,
                        })?;
                let parent_id =
                    row.try_get::<Option<String>, _>("parent_id")
                        .map_err(|source| StorageError::Sqlx {
                            path: self.path.clone(),
                            source,
                        })?;
                let provider_ids_json = row
                    .try_get::<Option<String>, _>("provider_ids_json")
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                let removed_at = row
                    .try_get::<Option<i64>, _>("removed_at")
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                movie_items
                    .entry((sort_title, production_year))
                    .or_insert(PrefetchedMovieItem {
                        id,
                        parent_id,
                        provider_ids_json,
                        removed_at,
                    });
            }
        }
        Ok(movie_items)
    }

    async fn prefetch_movie_folders_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        library_root_id: &str,
        files: &[NewMovieFile],
    ) -> Result<HashMap<String, String>, StorageError> {
        let mut identity_keys = HashSet::new();
        for file in files {
            let mut directory_key = String::new();
            let directory = file
                .relative_path
                .rsplit_once('/')
                .map(|(directory, _)| directory)
                .or_else(|| {
                    file.relative_path
                        .rsplit_once('\\')
                        .map(|(directory, _)| directory)
                })
                .unwrap_or_default();
            for component in directory.split(['/', '\\']) {
                if component.is_empty() || component == "." {
                    continue;
                }
                if !directory_key.is_empty() {
                    directory_key.push('/');
                }
                directory_key.push_str(component);
                identity_keys.insert(format!("folder:{library_root_id}:{directory_key}"));
            }
        }
        let mut identity_keys = identity_keys.into_iter().collect::<Vec<_>>();
        identity_keys.sort_unstable();
        let mut folders = HashMap::new();
        for chunk in identity_keys.chunks(BATCH_INSERT_CHUNK_SIZE) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT id, identity_key
                 FROM media_items
                 WHERE item_type = 'FOLDER' AND identity_key IN ({placeholders})"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for identity_key in chunk {
                statement = statement.bind(identity_key);
            }
            let rows = statement
                .fetch_all(&mut **transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            for row in rows {
                let id = row
                    .try_get::<String, _>("id")
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                let identity_key = row.try_get::<String, _>("identity_key").map_err(|source| {
                    StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    }
                })?;
                folders.insert(identity_key, id);
            }
        }
        Ok(folders)
    }

    async fn update_movie_parents_in_batches(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        updates: &[(String, Option<String>)],
    ) -> Result<(), StorageError> {
        for chunk in updates.chunks(BATCH_INSERT_CHUNK_SIZE) {
            if chunk.is_empty() {
                continue;
            }
            let cases = std::iter::repeat_n("WHEN ? THEN ?", chunk.len())
                .collect::<Vec<_>>()
                .join(" ");
            let ids = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "UPDATE media_items
                 SET parent_id = CASE id {cases} END
                 WHERE item_type = 'MOVIE' AND id IN ({ids})"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for (item_id, parent_id) in chunk {
                statement = statement.bind(item_id).bind(parent_id.as_deref());
            }
            for (item_id, _) in chunk {
                statement = statement.bind(item_id);
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

    async fn update_movie_provider_ids_in_batches(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        updates: &[(String, String)],
    ) -> Result<(), StorageError> {
        for chunk in updates.chunks(BATCH_INSERT_CHUNK_SIZE) {
            if chunk.is_empty() {
                continue;
            }
            let cases = std::iter::repeat_n("WHEN ? THEN ?", chunk.len())
                .collect::<Vec<_>>()
                .join(" ");
            let ids = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "UPDATE media_items
                 SET provider_ids_json = CASE id {cases} END
                 WHERE item_type = 'MOVIE'
                   AND id IN ({ids})
                   AND (provider_ids_json IS NULL OR provider_ids_json = '{{}}')"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for (item_id, provider_ids_json) in chunk {
                statement = statement.bind(item_id).bind(provider_ids_json);
            }
            for (item_id, _) in chunk {
                statement = statement.bind(item_id);
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

    async fn ensure_movie_parent_folder_cached(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        library_id: &str,
        library_root_id: &str,
        relative_path: &str,
        folder_cache: &mut HashMap<String, String>,
        touched_folders: &mut HashSet<String>,
    ) -> Result<Option<String>, StorageError> {
        let directory = relative_path
            .rsplit_once('/')
            .map(|(directory, _)| directory)
            .or_else(|| {
                relative_path
                    .rsplit_once('\\')
                    .map(|(directory, _)| directory)
            })
            .unwrap_or_default();
        let mut parent_folder_id = None;
        let mut parent_id = library_id.to_owned();
        let mut directory_key = String::new();
        for component in directory.split(['/', '\\']) {
            if component.is_empty() || component == "." {
                continue;
            }
            if !directory_key.is_empty() {
                directory_key.push('/');
            }
            directory_key.push_str(component);
            let identity_key = format!("folder:{library_root_id}:{directory_key}");
            let folder_id = if let Some(folder_id) = folder_cache.get(&identity_key) {
                let folder_id = folder_id.clone();
                if touched_folders.insert(identity_key.clone()) {
                    self.query(
                        "UPDATE media_items
                         SET library_id = ?, item_type = 'FOLDER', parent_id = ?,
                             title = ?, sort_title = ?, original_title = ?,
                             identification_status = 'LOCAL_CONFIRMED', removed_at = NULL
                         WHERE id = ?",
                    )
                    .bind(library_id)
                    .bind(&parent_id)
                    .bind(component)
                    .bind(component.to_ascii_lowercase())
                    .bind(component)
                    .bind(&folder_id)
                    .execute(&mut **transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                }
                folder_id
            } else {
                let folder_id = Uuid::now_v7().to_string();
                self.query(
                    "INSERT INTO media_items (
                        id, library_id, item_type, parent_id, title, sort_title,
                        original_title, identification_status, identity_key
                    ) VALUES (?, ?, 'FOLDER', ?, ?, ?, ?, 'LOCAL_CONFIRMED', ?)",
                )
                .bind(&folder_id)
                .bind(library_id)
                .bind(&parent_id)
                .bind(component)
                .bind(component.to_ascii_lowercase())
                .bind(component)
                .bind(&identity_key)
                .execute(&mut **transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
                folder_cache.insert(identity_key.clone(), folder_id.clone());
                touched_folders.insert(identity_key);
                folder_id
            };
            parent_id = folder_id.clone();
            parent_folder_id = Some(folder_id);
        }
        Ok(parent_folder_id)
    }

    pub(crate) async fn repair_movie_parent_folder(
        &self,
        library_id: &str,
        library_root_id: &str,
        relative_path: &str,
        item_id: &str,
    ) -> Result<(), StorageError> {
        let expected_identity_key = movie_parent_folder_identity(library_root_id, relative_path);
        let parent_is_current = if let Some(expected_identity_key) = expected_identity_key {
            self.query_scalar::<i64>(
                "SELECT CASE WHEN EXISTS (
                     SELECT 1
                     FROM media_items movie
                     JOIN media_items parent ON parent.id = movie.parent_id
                     WHERE movie.id = ? AND movie.item_type = 'MOVIE'
                       AND parent.item_type = 'FOLDER'
                       AND parent.identity_key = ? AND parent.removed_at IS NULL
                 ) THEN 1 ELSE 0 END",
            )
            .bind(item_id)
            .bind(expected_identity_key)
            .fetch_one(&self.pool)
            .await
            .map(|value| value != 0)
        } else {
            self.query_scalar::<i64>(
                "SELECT CASE WHEN EXISTS (
                     SELECT 1 FROM media_items
                     WHERE id = ? AND item_type = 'MOVIE' AND parent_id IS NULL
                 ) THEN 1 ELSE 0 END",
            )
            .bind(item_id)
            .fetch_one(&self.pool)
            .await
            .map(|value| value != 0)
        }
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        if parent_is_current {
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
        let parent_folder_id = self
            .ensure_movie_parent_folder_in_transaction(
                &mut transaction,
                library_id,
                library_root_id,
                relative_path,
            )
            .await?;
        self.query(
            "UPDATE media_items SET parent_id = ?
             WHERE id = ? AND item_type = 'MOVIE'",
        )
        .bind(parent_folder_id.as_deref())
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

    pub(crate) async fn insert_movie_files_batch(
        &self,
        library_id: &str,
        library_root_id: &str,
        generation: &str,
        files: &[NewMovieFile],
    ) -> Result<usize, StorageError> {
        if files.is_empty() {
            return Ok(0);
        }
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let mut folder_cache = self
            .prefetch_movie_folders_in_transaction(&mut transaction, library_root_id, files)
            .await?;
        let mut touched_folders = HashSet::new();
        let mut movie_cache = self
            .prefetch_movie_items_in_transaction(&mut transaction, library_id, files)
            .await?;
        let existing_movie_items = movie_cache
            .values()
            .cloned()
            .map(|item| (item.id.clone(), item))
            .collect::<HashMap<_, _>>();
        let mut provider_baselines = existing_movie_items
            .iter()
            .map(|(item_id, item)| (item_id.clone(), item.provider_ids_json.clone()))
            .collect::<HashMap<_, _>>();

        for chunk in files.chunks(BATCH_INSERT_CHUNK_SIZE) {
            let values = std::iter::repeat_n("(?, ?, ?, 'FILE', ?, ?, ?, ?, ?, 0)", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "INSERT INTO filesystem_entries (
                    id, library_root_id, relative_path, entry_kind, size,
                    modified_at, inode, fingerprint, last_seen_generation, is_missing
                ) VALUES {values}"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for file in chunk {
                statement = statement
                    .bind(&file.filesystem_entry_id)
                    .bind(library_root_id)
                    .bind(&file.relative_path)
                    .bind(file.size)
                    .bind(file.modified_at)
                    .bind(Option::<i64>::None)
                    .bind(&file.fingerprint)
                    .bind(generation);
            }
            statement
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }

        let mut new_items = Vec::new();
        let mut new_item_ids = HashSet::new();
        let mut parent_updates = HashMap::new();
        let mut provider_updates = HashMap::new();
        let mut source_rows = Vec::with_capacity(files.len());
        for (index, file) in files.iter().enumerate() {
            let parent_folder_id = self
                .ensure_movie_parent_folder_cached(
                    &mut transaction,
                    library_id,
                    library_root_id,
                    &file.relative_path,
                    &mut folder_cache,
                    &mut touched_folders,
                )
                .await?;
            let identity = (file.sort_title.clone(), file.production_year);
            let (item_id, is_new_item) = if let Some(item) = movie_cache.get(&identity) {
                (item.id.clone(), false)
            } else {
                let item_id = Uuid::now_v7().to_string();
                movie_cache.insert(
                    identity,
                    PrefetchedMovieItem {
                        id: item_id.clone(),
                        parent_id: None,
                        provider_ids_json: None,
                        removed_at: None,
                    },
                );
                provider_baselines.insert(item_id.clone(), file.provider_ids_json.clone());
                new_items.push((item_id.clone(), index));
                new_item_ids.insert(item_id.clone());
                (item_id, true)
            };
            parent_updates.insert(item_id.clone(), parent_folder_id);
            if let Some(provider_ids_json) = file.provider_ids_json.as_deref() {
                provider_updates
                    .entry(item_id.clone())
                    .or_insert_with(|| provider_ids_json.to_owned());
            }
            source_rows.push((index, item_id, is_new_item));
        }

        for chunk in new_items.chunks(BATCH_INSERT_CHUNK_SIZE) {
            let values = std::iter::repeat_n(
                "(?, ?, 'MOVIE', ?, ?, ?, ?, ?, ?, 'LOCAL_CONFIRMED')",
                chunk.len(),
            )
            .collect::<Vec<_>>()
            .join(", ");
            let query = format!(
                "INSERT INTO media_items (
                    id, library_id, item_type, parent_id, title, sort_title,
                    original_title, production_year, provider_ids_json, identification_status
                ) VALUES {values}"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for (item_id, index) in chunk {
                let file = &files[*index];
                statement = statement
                    .bind(item_id)
                    .bind(library_id)
                    .bind(parent_updates.get(item_id).and_then(Option::as_deref))
                    .bind(&file.title)
                    .bind(&file.sort_title)
                    .bind(&file.original_title)
                    .bind(file.production_year)
                    .bind(file.provider_ids_json.as_deref());
            }
            statement
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }

        let parent_updates = parent_updates
            .into_iter()
            .filter(|(item_id, parent_id)| {
                !new_item_ids.contains(item_id)
                    && existing_movie_items
                        .get(item_id)
                        .is_some_and(|item| item.parent_id.as_deref() != parent_id.as_deref())
            })
            .collect::<Vec<_>>();
        self.update_movie_parents_in_batches(&mut transaction, &parent_updates)
            .await?;

        let provider_updates = provider_updates
            .into_iter()
            .filter(|(item_id, provider_ids_json)| {
                !provider_ids_json.is_empty()
                    && provider_ids_json != "{}"
                    && provider_baselines.get(item_id).is_some_and(|value| {
                        value
                            .as_deref()
                            .is_none_or(|value| value.is_empty() || value == "{}")
                    })
            })
            .collect::<Vec<_>>();
        self.update_movie_provider_ids_in_batches(&mut transaction, &provider_updates)
            .await?;

        for chunk in source_rows.chunks(BATCH_INSERT_CHUNK_SIZE) {
            let values =
                std::iter::repeat_n("(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'PENDING')", chunk.len())
                    .collect::<Vec<_>>()
                    .join(", ");
            let query = format!(
                "INSERT INTO media_sources (
                    id, item_id, source_kind, filesystem_entry_id,
                    edition_name, quality_label, container, size,
                    external_url, strm_target_kind, is_default, probe_status
                ) VALUES {values}"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for (index, item_id, is_new_item) in chunk {
                let file = &files[*index];
                statement = statement
                    .bind(&file.source_id)
                    .bind(item_id)
                    .bind(&file.source_kind)
                    .bind(&file.filesystem_entry_id)
                    .bind(file.edition_name.as_deref())
                    .bind(file.quality_label.as_deref())
                    .bind(&file.container)
                    .bind(file.size)
                    .bind(file.external_url.as_deref())
                    .bind(file.strm_target_kind.as_deref())
                    .bind(database_flag(*is_new_item));
            }
            statement
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }
        if movie_cache.values().any(|item| item.removed_at.is_some()) {
            let filesystem_entry_ids = source_rows
                .iter()
                .map(|(index, _, _)| files[*index].filesystem_entry_id.clone())
                .collect::<Vec<_>>();
            self.restore_media_items_for_filesystem_entries(
                &mut transaction,
                &filesystem_entry_ids,
            )
            .await?;
        }
        let strm_item_ids = source_rows
            .iter()
            .filter(|(index, _, _)| files[*index].source_kind == "STRM_URL")
            .map(|(_, item_id, _)| item_id)
            .collect::<HashSet<_>>();
        for item_id in strm_item_ids {
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
        Ok(new_items.len())
    }

    pub(crate) async fn insert_episode_files_batch(
        &self,
        library_id: &str,
        library_root_id: &str,
        generation: &str,
        files: &[NewEpisodeFile],
    ) -> Result<usize, StorageError> {
        if files.is_empty() {
            return Ok(0);
        }
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;

        for chunk in files.chunks(BATCH_INSERT_CHUNK_SIZE) {
            let values = std::iter::repeat_n("(?, ?, ?, 'FILE', ?, ?, ?, ?, ?, 0)", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "INSERT INTO filesystem_entries (
                    id, library_root_id, relative_path, entry_kind, size,
                    modified_at, inode, fingerprint, last_seen_generation, is_missing
                ) VALUES {values}"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for file in chunk {
                statement = statement
                    .bind(&file.filesystem_entry_id)
                    .bind(library_root_id)
                    .bind(&file.relative_path)
                    .bind(file.size)
                    .bind(file.modified_at)
                    .bind(file.inode)
                    .bind(&file.fingerprint)
                    .bind(generation);
            }
            statement
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }

        let mut series_rows = BTreeMap::<String, BatchHierarchyRow>::new();
        for file in files {
            series_rows
                .entry(file.series_identity.clone())
                .or_insert_with(|| BatchHierarchyRow {
                    id: Uuid::now_v7().to_string(),
                    library_id: library_id.to_owned(),
                    item_type: "SERIES",
                    parent_id: None,
                    series_id: None,
                    season_number: None,
                    episode_number: None,
                    absolute_number: None,
                    title: file.series_title.clone(),
                    sort_title: file.series_sort_title.clone(),
                    original_title: Some(file.series_title.clone()),
                    production_year: file.series_production_year,
                    provider_ids_json: file.series_provider_ids_json.clone(),
                    identity_key: file.series_identity.clone(),
                });
        }
        let series_keys = series_rows.keys().cloned().collect::<Vec<_>>();
        let (mut series_ids, series_removed_ids) = self
            .list_hierarchy_ids_in_transaction(&mut transaction, &series_keys)
            .await?;
        let missing_series = series_rows
            .values()
            .filter(|row| !series_ids.contains_key(&row.identity_key))
            .collect::<Vec<_>>();
        self.insert_hierarchy_rows_in_transaction(&mut transaction, missing_series.iter().copied())
            .await?;
        self.revive_hierarchy_rows_in_transaction(
            &mut transaction,
            series_rows.values(),
            &series_ids,
            &series_removed_ids,
        )
        .await?;
        series_ids = self
            .list_hierarchy_ids_in_transaction(&mut transaction, &series_keys)
            .await?
            .0;

        let mut season_rows = BTreeMap::<String, BatchHierarchyRow>::new();
        for file in files {
            let series_id = series_ids
                .get(&file.series_identity)
                .ok_or_else(|| StorageError::Conflict("批量扫描未找到剧集层级".to_owned()))?;
            season_rows
                .entry(file.season_identity.clone())
                .or_insert_with(|| BatchHierarchyRow {
                    id: Uuid::now_v7().to_string(),
                    library_id: library_id.to_owned(),
                    item_type: "SEASON",
                    parent_id: Some(series_id.clone()),
                    series_id: Some(series_id.clone()),
                    season_number: Some(file.season_number),
                    episode_number: None,
                    absolute_number: None,
                    title: if file.season_number == 0 {
                        "Specials".to_owned()
                    } else {
                        format!("Season {:02}", file.season_number)
                    },
                    sort_title: if file.season_number == 0 {
                        "specials".to_owned()
                    } else {
                        format!("season {:02}", file.season_number)
                    },
                    original_title: Some(if file.season_number == 0 {
                        "Specials".to_owned()
                    } else {
                        format!("Season {:02}", file.season_number)
                    }),
                    production_year: None,
                    provider_ids_json: None,
                    identity_key: file.season_identity.clone(),
                });
        }
        let season_keys = season_rows.keys().cloned().collect::<Vec<_>>();
        let (mut season_ids, season_removed_ids) = self
            .list_hierarchy_ids_in_transaction(&mut transaction, &season_keys)
            .await?;
        let missing_seasons = season_rows
            .values()
            .filter(|row| !season_ids.contains_key(&row.identity_key))
            .collect::<Vec<_>>();
        self.insert_hierarchy_rows_in_transaction(
            &mut transaction,
            missing_seasons.iter().copied(),
        )
        .await?;
        self.revive_hierarchy_rows_in_transaction(
            &mut transaction,
            season_rows.values(),
            &season_ids,
            &season_removed_ids,
        )
        .await?;
        season_ids = self
            .list_hierarchy_ids_in_transaction(&mut transaction, &season_keys)
            .await?
            .0;

        let mut episode_rows = BTreeMap::<String, BatchHierarchyRow>::new();
        for file in files {
            let season_id = season_ids
                .get(&file.season_identity)
                .ok_or_else(|| StorageError::Conflict("批量扫描未找到季度层级".to_owned()))?;
            let series_id = series_ids
                .get(&file.series_identity)
                .ok_or_else(|| StorageError::Conflict("批量扫描未找到剧集层级".to_owned()))?;
            episode_rows
                .entry(file.episode_identity.clone())
                .or_insert_with(|| BatchHierarchyRow {
                    id: Uuid::now_v7().to_string(),
                    library_id: library_id.to_owned(),
                    item_type: "EPISODE",
                    parent_id: Some(season_id.clone()),
                    series_id: Some(series_id.clone()),
                    season_number: Some(file.season_number),
                    episode_number: Some(file.episode_number),
                    absolute_number: file.episode_absolute_number,
                    title: file.episode_title.clone(),
                    sort_title: file.episode_sort_title.clone(),
                    original_title: Some(file.episode_title.clone()),
                    production_year: None,
                    provider_ids_json: None,
                    identity_key: file.episode_identity.clone(),
                });
        }
        let episode_keys = episode_rows.keys().cloned().collect::<Vec<_>>();
        let (episode_ids, episode_removed_ids) = self
            .list_hierarchy_ids_in_transaction(&mut transaction, &episode_keys)
            .await?;
        let missing_episodes = episode_rows
            .values()
            .filter(|row| !episode_ids.contains_key(&row.identity_key))
            .collect::<Vec<_>>();
        let new_episode_identities = missing_episodes
            .iter()
            .map(|row| row.identity_key.clone())
            .collect::<HashSet<_>>();
        self.insert_hierarchy_rows_in_transaction(
            &mut transaction,
            missing_episodes.iter().copied(),
        )
        .await?;
        self.revive_hierarchy_rows_in_transaction(
            &mut transaction,
            episode_rows.values(),
            &episode_ids,
            &episode_removed_ids,
        )
        .await?;
        let episode_ids = self
            .list_hierarchy_ids_in_transaction(&mut transaction, &episode_keys)
            .await?
            .0;

        let mut defaulted_episode_identities = HashSet::new();
        for chunk in files.chunks(BATCH_INSERT_CHUNK_SIZE) {
            let values =
                std::iter::repeat_n("(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'PENDING')", chunk.len())
                    .collect::<Vec<_>>()
                    .join(", ");
            let query = format!(
                "INSERT INTO media_sources (
                    id, item_id, source_kind, filesystem_entry_id,
                    edition_name, quality_label, container, size,
                    external_url, strm_target_kind, is_default, probe_status
                ) VALUES {values}"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for file in chunk {
                let item_id = episode_ids
                    .get(&file.episode_identity)
                    .ok_or_else(|| StorageError::Conflict("批量扫描未找到分集层级".to_owned()))?;
                let is_default = new_episode_identities.contains(&file.episode_identity)
                    && defaulted_episode_identities.insert(file.episode_identity.clone());
                statement = statement
                    .bind(&file.source_id)
                    .bind(item_id)
                    .bind(&file.source_kind)
                    .bind(&file.filesystem_entry_id)
                    .bind(file.edition_name.as_deref())
                    .bind(file.quality_label.as_deref())
                    .bind(&file.container)
                    .bind(file.size)
                    .bind(file.external_url.as_deref())
                    .bind(file.strm_target_kind.as_deref())
                    .bind(database_flag(is_default));
            }
            statement
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }

        let strm_item_ids = files
            .iter()
            .filter(|file| file.source_kind == "STRM_URL")
            .filter_map(|file| episode_ids.get(&file.episode_identity))
            .cloned()
            .collect::<HashSet<_>>();
        if !strm_item_ids.is_empty() {
            let placeholders = std::iter::repeat_n("?", strm_item_ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            let mut statement = self.query(sqlx::AssertSqlSafe(format!(
                "UPDATE media_items
                 SET poster_fallback_required = 1
                 WHERE id IN ({placeholders})
                   AND NOT EXISTS (
                       SELECT 1 FROM item_images
                       WHERE item_id = media_items.id
                         AND image_type IN ('POSTER', 'THUMB')
                         AND image_index = 0
                   )"
            )));
            for item_id in &strm_item_ids {
                statement = statement.bind(item_id);
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
        Ok(missing_series.len() + missing_seasons.len() + missing_episodes.len())
    }

    async fn list_hierarchy_ids_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        identity_keys: &[String],
    ) -> Result<(HashMap<String, String>, HashSet<String>), StorageError> {
        let mut ids = HashMap::new();
        let mut removed_ids = HashSet::new();
        for chunk in identity_keys.chunks(BATCH_INSERT_CHUNK_SIZE) {
            if chunk.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT id, identity_key, removed_at
                 FROM media_items WHERE identity_key IN ({placeholders})"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for identity_key in chunk {
                statement = statement.bind(identity_key);
            }
            let rows = statement
                .fetch_all(&mut **transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            for row in rows {
                let identity_key = row.try_get::<String, _>("identity_key").map_err(|source| {
                    StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    }
                })?;
                let id = row
                    .try_get::<String, _>("id")
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                if row
                    .try_get::<Option<i64>, _>("removed_at")
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?
                    .is_some()
                {
                    removed_ids.insert(identity_key.clone());
                }
                ids.insert(identity_key, id);
            }
        }
        Ok((ids, removed_ids))
    }

    async fn insert_hierarchy_rows_in_transaction<'a, I>(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        rows: I,
    ) -> Result<(), StorageError>
    where
        I: IntoIterator<Item = &'a BatchHierarchyRow>,
    {
        let rows = rows.into_iter().collect::<Vec<_>>();
        for chunk in rows.chunks(BATCH_INSERT_CHUNK_SIZE) {
            if chunk.is_empty() {
                continue;
            }
            let values =
                std::iter::repeat_n("(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)", chunk.len())
                    .collect::<Vec<_>>()
                    .join(", ");
            let query = format!(
                "INSERT INTO media_items (
                    id, library_id, item_type, parent_id, series_id,
                    season_number, episode_number, absolute_number,
                    title, sort_title, original_title, production_year,
                    provider_ids_json, identification_status, identity_key
                ) VALUES {values}"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for row in chunk {
                statement = statement
                    .bind(&row.id)
                    .bind(&row.library_id)
                    .bind(row.item_type)
                    .bind(row.parent_id.as_deref())
                    .bind(row.series_id.as_deref())
                    .bind(row.season_number)
                    .bind(row.episode_number)
                    .bind(row.absolute_number)
                    .bind(&row.title)
                    .bind(&row.sort_title)
                    .bind(row.original_title.as_deref())
                    .bind(row.production_year)
                    .bind(row.provider_ids_json.as_deref())
                    .bind("PENDING")
                    .bind(&row.identity_key);
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

    async fn revive_hierarchy_rows_in_transaction<'a, I>(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        rows: I,
        ids: &HashMap<String, String>,
        removed_identities: &HashSet<String>,
    ) -> Result<(), StorageError>
    where
        I: IntoIterator<Item = &'a BatchHierarchyRow>,
    {
        for row in rows {
            if !removed_identities.contains(&row.identity_key) {
                continue;
            }
            let Some(item_id) = ids.get(&row.identity_key) else {
                continue;
            };
            self.query(
                "UPDATE media_items
                 SET library_id = ?, item_type = ?, parent_id = ?, series_id = ?,
                     season_number = ?, episode_number = ?, absolute_number = ?,
                     removed_at = NULL
                 WHERE id = ?",
            )
            .bind(&row.library_id)
            .bind(row.item_type)
            .bind(row.parent_id.as_deref())
            .bind(row.series_id.as_deref())
            .bind(row.season_number)
            .bind(row.episode_number)
            .bind(row.absolute_number)
            .bind(item_id)
            .execute(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        Ok(())
    }

    pub(crate) async fn find_media_item(
        &self,
        library_id: &str,
        sort_title: &str,
        production_year: Option<i64>,
    ) -> Result<Option<StoredMediaItem>, StorageError> {
        let row = match production_year {
            Some(year) => {
                self.query(
                    "SELECT id
                     FROM media_items
                     WHERE library_id = ? AND item_type = 'MOVIE'
                       AND sort_title = ? AND production_year = ?
                     ORDER BY CASE WHEN removed_at IS NULL THEN 0 ELSE 1 END, id
                     LIMIT 1",
                )
                .bind(library_id)
                .bind(sort_title)
                .bind(year)
                .fetch_optional(&self.pool)
                .await
            }
            None => {
                self.query(
                    "SELECT id
                     FROM media_items
                     WHERE library_id = ? AND item_type = 'MOVIE'
                       AND sort_title = ? AND production_year IS NULL
                     ORDER BY CASE WHEN removed_at IS NULL THEN 0 ELSE 1 END, id
                     LIMIT 1",
                )
                .bind(library_id)
                .bind(sort_title)
                .fetch_optional(&self.pool)
                .await
            }
        };
        row.map(|row| row.map(stored_media_item))
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn movie_metadata_identity_conflicts(
        &self,
        item_id: &str,
        sort_title: &str,
        production_year: i64,
    ) -> Result<bool, StorageError> {
        self.query_scalar::<i64>(
            "SELECT CASE WHEN EXISTS (
                 SELECT 1
                 FROM media_items current_item
                 JOIN media_items conflicting_item
                   ON conflicting_item.library_id = current_item.library_id
                  AND conflicting_item.id <> current_item.id
                  AND conflicting_item.item_type = 'MOVIE'
                  AND conflicting_item.sort_title = ?
                  AND conflicting_item.production_year = ?
                  AND conflicting_item.removed_at IS NULL
                  AND conflicting_item.has_available_source = 1
                  AND (
                      current_item.parent_id IS NULL
                      OR conflicting_item.parent_id IS NULL
                      OR conflicting_item.parent_id IS DISTINCT FROM current_item.parent_id
                  )
                 WHERE current_item.id = ?
                   AND current_item.item_type = 'MOVIE'
                   AND current_item.removed_at IS NULL
             ) THEN 1 ELSE 0 END",
        )
        .bind(sort_title)
        .bind(production_year)
        .bind(item_id)
        .fetch_one(&self.pool)
        .await
        .map(|value| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_media_item_by_identity(
        &self,
        identity_key: &str,
    ) -> Result<Option<StoredMediaItem>, StorageError> {
        self.query("SELECT id FROM media_items WHERE identity_key = ?")
            .bind(identity_key)
            .fetch_optional(&self.pool)
            .await
            .map(|row| row.map(stored_media_item))
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn adopt_media_item_identity(
        &self,
        item_id: &str,
        identity_key: &str,
    ) -> Result<bool, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let occupied = self
            .query_scalar::<i64>(
                "SELECT COUNT(*) FROM media_items
                 WHERE identity_key = ? AND id <> ?",
            )
            .bind(identity_key)
            .bind(item_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if occupied != 0 {
            transaction
                .rollback()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(false);
        }
        self.query(
            "UPDATE media_items
             SET identity_key = ?, removed_at = NULL
             WHERE id = ?",
        )
        .bind(identity_key)
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

    pub(crate) async fn repair_episode_hierarchy_identities(
        &self,
        episode_id: &str,
        series_identity: &str,
        season_identity: &str,
        episode_identity: &str,
    ) -> Result<bool, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let hierarchy = self
            .query_as::<(String, String, String)>(
                "SELECT episode.id, season.id, series.id
                 FROM media_items episode
                 JOIN media_items season
                   ON season.id = episode.parent_id AND season.item_type = 'SEASON'
                 JOIN media_items series
                   ON series.id = episode.series_id AND series.item_type = 'SERIES'
                 WHERE episode.id = ? AND episode.item_type = 'EPISODE'",
            )
            .bind(episode_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let Some((episode_id, season_id, series_id)) = hierarchy else {
            transaction
                .rollback()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(false);
        };

        let conflicts = self
            .query_scalar::<i64>(
                "SELECT COUNT(*)
                 FROM media_items
                 WHERE identity_key IN (?, ?, ?)
                   AND id NOT IN (?, ?, ?)",
            )
            .bind(series_identity)
            .bind(season_identity)
            .bind(episode_identity)
            .bind(&series_id)
            .bind(&season_id)
            .bind(&episode_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if conflicts != 0 {
            transaction
                .rollback()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(false);
        }

        for (item_id, identity_key) in [
            (&series_id, series_identity),
            (&season_id, season_identity),
            (&episode_id, episode_identity),
        ] {
            self.query("UPDATE media_items SET identity_key = ?, removed_at = NULL WHERE id = ?")
                .bind(identity_key)
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

    pub(crate) async fn find_media_item_metadata(
        &self,
        item_id: &str,
    ) -> Result<Option<StoredMediaMetadata>, StorageError> {
        let mut metadata = self
            .list_media_item_metadata_by_ids(&[item_id.to_owned()])
            .await?;
        Ok(metadata.remove(item_id))
    }

    pub(crate) async fn list_media_item_metadata_by_ids(
        &self,
        item_ids: &[String],
    ) -> Result<HashMap<String, StoredMediaMetadata>, StorageError> {
        let mut metadata = HashMap::with_capacity(item_ids.len());
        for chunk in item_ids.chunks(500) {
            if chunk.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT mi.id AS item_id, mi.item_type, mi.title, mi.original_title, mi.overview,
                        mi.production_year, mi.premiere_date, mi.last_air_date, mi.status,
                        mi.original_language, mi.rating, mi.provider_ids_json,
                        mi.metadata_scraper_id, mi.identification_status,
                        mi.metadata_provenance_json, mi.locked_fields_json,
                        mi.nfo_metadata_json, mi.series_id, mi.season_number, mi.episode_number,
                        series.title AS series_title,
                        series.production_year AS series_production_year,
                        series.provider_ids_json AS series_provider_ids_json,
                        series.metadata_scraper_id AS series_metadata_scraper_id,
                        libraries.scraper_id AS scraper_id
                 FROM media_items mi
                 LEFT JOIN media_items series ON series.id = mi.series_id
                 LEFT JOIN libraries ON libraries.id = mi.library_id
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
                let item_id = row.get::<String, _>("item_id");
                metadata.insert(item_id, stored_media_metadata(row));
            }
        }
        Ok(metadata)
    }

    pub(crate) async fn list_metadata_refresh_item_ids(
        &self,
        item_id: &str,
    ) -> Result<Vec<String>, StorageError> {
        let item_type = self
            .query_scalar::<String>(
                "SELECT item_type FROM media_items
             WHERE id = ? AND removed_at IS NULL",
            )
            .bind(item_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let Some(item_type) = item_type else {
            return Ok(Vec::new());
        };
        let query = match item_type.as_str() {
            "SERIES" => {
                "SELECT id FROM media_items
                 WHERE removed_at IS NULL AND (id = ? OR series_id = ?)
                 ORDER BY CASE item_type WHEN 'SERIES' THEN 0 WHEN 'SEASON' THEN 1 ELSE 2 END,
                          season_number, episode_number, id"
            }
            "SEASON" => {
                "SELECT id FROM media_items
                 WHERE removed_at IS NULL AND (id = ? OR parent_id = ?)
                 ORDER BY CASE item_type WHEN 'SEASON' THEN 0 ELSE 1 END,
                          episode_number, id"
            }
            _ => "SELECT id FROM media_items WHERE id = ? AND removed_at IS NULL",
        };
        let mut query = self.query_scalar::<String>(query).bind(item_id);
        if matches!(item_type.as_str(), "SERIES" | "SEASON") {
            query = query.bind(item_id);
        }
        query
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn find_media_item_image_identity(
        &self,
        item_id: &str,
    ) -> Result<Option<StoredImageIdentity>, StorageError> {
        self.query(
            "SELECT mi.item_type, mi.provider_ids_json,
                    series.provider_ids_json AS series_provider_ids_json,
                    COALESCE(series.metadata_scraper_id, l.scraper_id) AS series_scraper_id,
                    mi.season_number, mi.episode_number,
                    COALESCE(mi.metadata_scraper_id, l.scraper_id) AS scraper_id
             FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id
             LEFT JOIN media_items series
               ON series.id = COALESCE(mi.series_id, mi.parent_id)
             WHERE mi.id = ? AND mi.removed_at IS NULL",
        )
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| {
                let series_scraper_id = row.get::<Option<String>, _>("series_scraper_id");
                let scraper_id = row.get::<Option<String>, _>("scraper_id");
                let provider = first_provider_id(
                    row.get("provider_ids_json"),
                    row.get("series_provider_ids_json"),
                    series_scraper_id.as_deref().or(scraper_id.as_deref()),
                );
                StoredImageIdentity {
                    item_type: row.get("item_type"),
                    provider_name: provider.as_ref().map(|(name, _)| name.clone()),
                    provider_id: provider.map(|(_, id)| id),
                    season_number: row.get("season_number"),
                    episode_number: row.get("episode_number"),
                }
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_movie_identity(
        &self,
        item_id: &str,
    ) -> Result<Option<StoredMovieIdentity>, StorageError> {
        self.query(
            "SELECT mi.library_id, mi.provider_ids_json,
                    COALESCE(mi.metadata_scraper_id, l.scraper_id) AS scraper_id
             FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id
             WHERE mi.id = ? AND mi.item_type = 'MOVIE' AND mi.removed_at IS NULL",
        )
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.and_then(|row| {
                let scraper_id = row.get::<Option<String>, _>("scraper_id");
                let provider =
                    first_provider_id(row.get("provider_ids_json"), None, scraper_id.as_deref())?;
                Some(StoredMovieIdentity {
                    library_id: row.get("library_id"),
                    provider_name: provider.0,
                    provider_id: provider.1,
                })
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn upsert_collection(
        &self,
        collection: NewCollection<'_>,
    ) -> Result<StoredCollectionRefresh, StorageError> {
        let NewCollection {
            library_id,
            provider,
            provider_id,
            title,
            overview,
            poster_path,
            backdrop_path,
            member_provider_ids,
        } = collection;
        let provider_name = provider.to_ascii_uppercase();
        let provider_key = provider.to_ascii_lowercase();
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let existing = self
            .query(
                "SELECT id, item_id
             FROM collections
             WHERE library_id = ? AND lower(provider) = lower(?) AND provider_id = ?",
            )
            .bind(library_id)
            .bind(provider)
            .bind(provider_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let (collection_id, item_id) = if let Some(row) = existing {
            (row.get::<String, _>("id"), row.get::<String, _>("item_id"))
        } else {
            let collection_id = Uuid::now_v7().to_string();
            let item_id = Uuid::now_v7().to_string();
            let identity_key = format!("collection:{provider_key}:{library_id}:{provider_id}");
            let provider_ids_json = serde_json::json!({
                format!("{provider_key}Collection"): provider_id
            })
            .to_string();
            self.query(
                "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title, original_title,
                    overview, provider_ids_json, identification_status, identity_key
                ) VALUES (?, ?, 'BOX_SET', ?, ?, ?, ?, ?, 'ONLINE_CONFIRMED', ?)",
            )
            .bind(&item_id)
            .bind(library_id)
            .bind(title)
            .bind(title.to_ascii_lowercase())
            .bind(title)
            .bind(overview)
            .bind(provider_ids_json)
            .bind(identity_key)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
            self.query(
                "INSERT INTO collections (
                    id, item_id, library_id, provider, provider_id,
                    title, overview, poster_path, backdrop_path
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&collection_id)
            .bind(&item_id)
            .bind(library_id)
            .bind(&provider_name)
            .bind(provider_id)
            .bind(title)
            .bind(overview)
            .bind(poster_path)
            .bind(backdrop_path)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
            (collection_id, item_id)
        };
        self.query(
            "UPDATE collections
             SET title = ?, overview = ?, poster_path = ?, backdrop_path = ?,
                 updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(title)
        .bind(overview)
        .bind(poster_path)
        .bind(backdrop_path)
        .bind(&collection_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query(
            "UPDATE media_items
             SET title = ?, sort_title = ?, original_title = ?, overview = ?
             WHERE id = ?",
        )
        .bind(title)
        .bind(title.to_ascii_lowercase())
        .bind(title)
        .bind(overview)
        .bind(&item_id)
        .execute(&mut *transaction)
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
        let mut matched_members = Vec::with_capacity(member_provider_ids.len());
        for (chunk_index, chunk) in member_provider_ids
            .chunks(BATCH_INSERT_CHUNK_SIZE)
            .enumerate()
        {
            if chunk.is_empty() {
                continue;
            }
            let values = std::iter::repeat_n("(?, ?, ?, ?)", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "WITH requested(provider, provider_id, sort_order, ordinal) AS (VALUES {values})
                 SELECT requested.ordinal, requested.sort_order, provider.media_item_id
                 FROM requested
                 JOIN media_item_provider_ids provider
                   ON provider.item_type = 'MOVIE'
                  AND provider.provider = lower(requested.provider)
                  AND provider.provider_id = requested.provider_id
                 JOIN media_items mi ON mi.id = provider.media_item_id
                 WHERE mi.library_id = ? AND mi.removed_at IS NULL
                 ORDER BY requested.ordinal, provider.media_item_id"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for (offset, (member_provider, member_provider_id, sort_order)) in
                chunk.iter().enumerate()
            {
                statement = statement
                    .bind(member_provider.to_ascii_lowercase())
                    .bind(member_provider_id)
                    .bind(*sort_order)
                    .bind((chunk_index * BATCH_INSERT_CHUNK_SIZE + offset) as i64);
            }
            let rows = statement
                .bind(library_id)
                .fetch_all(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            let mut seen_ordinals = HashSet::new();
            for row in rows {
                let ordinal = row.get::<i64, _>("ordinal");
                if !seen_ordinals.insert(ordinal) {
                    continue;
                }
                matched_members.push((
                    row.get::<String, _>("media_item_id"),
                    row.get::<i64, _>("sort_order"),
                ));
            }
        }
        let mut member_count = 0_usize;
        for chunk in matched_members.chunks(BATCH_INSERT_CHUNK_SIZE) {
            if chunk.is_empty() {
                continue;
            }
            let values = std::iter::repeat_n("(?, ?, ?)", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "INSERT INTO collection_items (collection_id, item_id, sort_order)
                 VALUES {values}
                 ON CONFLICT (collection_id, item_id) DO NOTHING"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for (member_item_id, sort_order) in chunk {
                statement = statement
                    .bind(&collection_id)
                    .bind(member_item_id)
                    .bind(*sort_order);
            }
            let result = statement
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            member_count += result.rows_affected() as usize;
        }
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(StoredCollectionRefresh {
            collection_item_id: item_id,
            member_count,
        })
    }

    pub(crate) async fn list_collection_member_ids_page(
        &self,
        collection_item_id: &str,
        library_ids: Option<&[String]>,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<String>, i64), StorageError> {
        if library_ids.is_some_and(|library_ids| library_ids.is_empty()) {
            return Ok((Vec::new(), 0));
        }
        let library_filter = library_ids
            .map(|library_ids| {
                let placeholders = std::iter::repeat_n("?", library_ids.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(" AND mi.library_id IN ({placeholders})")
            })
            .unwrap_or_default();
        let from_where = format!(
            "FROM collection_items ci
             JOIN collections c ON c.id = ci.collection_id
             JOIN media_items mi ON mi.id = ci.item_id
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             WHERE c.item_id = ? AND mi.removed_at IS NULL
               {CATALOG_VISIBLE_PREDICATE}{library_filter}"
        );
        let mut count_statement = self
            .query_scalar::<i64>(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) {from_where}")))
            .bind(collection_item_id);
        if let Some(library_ids) = library_ids {
            for library_id in library_ids {
                count_statement = count_statement.bind(library_id);
            }
        }
        let total = count_statement
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;

        let mut list_statement = self
            .query(sqlx::AssertSqlSafe(format!(
                "SELECT ci.item_id {from_where}
                 ORDER BY ci.sort_order, ci.item_id
                 LIMIT ? OFFSET ?"
            )))
            .bind(collection_item_id);
        if let Some(library_ids) = library_ids {
            for library_id in library_ids {
                list_statement = list_statement.bind(library_id);
            }
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
        Ok((
            rows.into_iter().map(|row| row.get("item_id")).collect(),
            total,
        ))
    }
}
