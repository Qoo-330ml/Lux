use super::*;

impl Database {
    pub(crate) async fn list_media_sources_for_external_subtitle(
        &self,
        library_root_id: &str,
        relative_directory: &str,
    ) -> Result<Vec<StoredMediaSourcePath>, StorageError> {
        let directory = relative_directory.trim_matches('/');
        let prefix = if directory.is_empty() {
            String::new()
        } else {
            format!("{directory}/")
        };
        let escaped_prefix = prefix
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let direct_children = format!("{escaped_prefix}%");
        let nested_children = format!("{escaped_prefix}%/%");
        let rows = self
            .query(
                "SELECT ms.id AS source_id, ms.item_id, ms.probe_status,
                        lr.canonical_path AS root_path, fe.relative_path
                 FROM media_sources ms
                 JOIN media_items mi ON mi.id = ms.item_id
                 JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                 JOIN library_roots lr ON lr.id = fe.library_root_id
                 WHERE fe.library_root_id = ? AND fe.is_missing = 0
                   AND mi.removed_at IS NULL
                   AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
                   AND fe.relative_path LIKE ? ESCAPE '\\'
                   AND fe.relative_path NOT LIKE ? ESCAPE '\\'
                 ORDER BY fe.relative_path, ms.id",
            )
            .bind(library_root_id)
            .bind(direct_children)
            .bind(nested_children)
            .fetch_all(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        Ok(rows
            .into_iter()
            .map(|row| StoredMediaSourcePath {
                source_id: row.get("source_id"),
                item_id: row.get("item_id"),
                probe_status: row.get("probe_status"),
                root_path: row.get("root_path"),
                relative_path: row.get("relative_path"),
            })
            .collect())
    }

    pub(crate) async fn apply_scan_external_subtitle_file_change(
        &self,
        job_id: &str,
        change: &StoredScanJobPath,
        tracks: &[StoredSubtitleStream],
    ) -> Result<bool, StorageError> {
        let Some(revision) = change.file_change_revision else {
            return Err(StorageError::Conflict(
                "scan file change is missing its revision".to_owned(),
            ));
        };
        if !matches!(change.change_kind.as_str(), "ADD" | "CHANGE" | "REMOVE") {
            return Err(StorageError::Conflict(
                "invalid scan file change kind".to_owned(),
            ));
        }
        if tracks.iter().any(|track| {
            !track.is_external
                || track.stream_type != "SUBTITLE"
                || track.external_path.as_deref() != Some(change.relative_path.as_str())
        }) {
            return Err(StorageError::Conflict(
                "invalid external subtitle file update".to_owned(),
            ));
        }

        let mut transaction = self.pool.begin().await.map_err(self.sqlx_err())?;
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
            .map_err(self.sqlx_err())?
            .is_some();
        if !current {
            return Ok(false);
        }

        if change.change_kind == "REMOVE" {
            for track in tracks {
                self.query(
                    "DELETE FROM media_streams
                     WHERE media_source_id = ? AND stream_type = 'SUBTITLE'
                       AND is_external = 1 AND external_path = ?",
                )
                .bind(&track.media_source_id)
                .bind(&change.relative_path)
                .execute(&mut *transaction)
                .await
                .map_err(self.sqlx_err())?;
            }
        } else {
            for track in tracks {
                if track.is_default {
                    self.query(
                        "UPDATE media_streams
                         SET is_default = 0, updated_at = unixepoch()
                         WHERE media_source_id = ? AND stream_type = 'SUBTITLE'
                           AND is_external = 1",
                    )
                    .bind(&track.media_source_id)
                    .execute(&mut *transaction)
                    .await
                    .map_err(self.sqlx_err())?;
                }
                let existing = self
                    .query(
                        "SELECT id FROM media_streams
                         WHERE media_source_id = ? AND stream_type = 'SUBTITLE'
                           AND is_external = 1 AND external_path = ?
                         ORDER BY stream_index LIMIT 1",
                    )
                    .bind(&track.media_source_id)
                    .bind(&change.relative_path)
                    .fetch_optional(&mut *transaction)
                    .await
                    .map_err(self.sqlx_err())?;
                if let Some(existing) = existing {
                    self.query(
                        "UPDATE media_streams
                         SET codec = ?, language = ?, title = ?,
                             is_default = ?, is_forced = ?, updated_at = unixepoch()
                         WHERE id = ?",
                    )
                    .bind(&track.codec)
                    .bind(&track.language)
                    .bind(&track.title)
                    .bind(database_flag(track.is_default))
                    .bind(database_flag(track.is_forced))
                    .bind(existing.get::<String, _>("id"))
                    .execute(&mut *transaction)
                    .await
                    .map_err(self.sqlx_err())?;
                } else {
                    let stream_index = self
                        .query_scalar::<i64>(
                            "SELECT COALESCE(MAX(stream_index), -1) + 1
                             FROM media_streams WHERE media_source_id = ?",
                        )
                        .bind(&track.media_source_id)
                        .fetch_one(&mut *transaction)
                        .await
                        .map_err(self.sqlx_err())?;
                    self.query(
                        "INSERT INTO media_streams (
                             id, media_source_id, stream_index, stream_type, codec,
                             language, title, details_json, external_path,
                             is_external, is_default, is_forced
                         ) VALUES (?, ?, ?, 'SUBTITLE', ?, ?, ?, NULL, ?, 1, ?, ?)",
                    )
                    .bind(Uuid::now_v7().to_string())
                    .bind(&track.media_source_id)
                    .bind(stream_index)
                    .bind(&track.codec)
                    .bind(&track.language)
                    .bind(&track.title)
                    .bind(&change.relative_path)
                    .bind(database_flag(track.is_default))
                    .bind(database_flag(track.is_forced))
                    .execute(&mut *transaction)
                    .await
                    .map_err(self.sqlx_err())?;
                }
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
            .map_err(self.sqlx_err())?
            .rows_affected()
            == 1;
        if !completed {
            return Ok(false);
        }
        transaction.commit().await.map_err(self.sqlx_err())?;
        Ok(true)
    }

    pub(crate) async fn find_external_subtitle(
        &self,
        item_id: &str,
        media_source_id: Option<&str>,
        stream_index: i64,
    ) -> Result<Option<StoredExternalSubtitle>, StorageError> {
        let row = if let Some(media_source_id) = media_source_id {
            self.query(
                "SELECT ms.id AS media_source_id, ms.item_id, mt.external_path,
                        mt.language, mt.title, lr.canonical_path AS root_path
                 FROM media_streams mt
                 JOIN media_sources ms ON ms.id = mt.media_source_id
                 JOIN media_items mi ON mi.id = ms.item_id
                 JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                 JOIN library_roots lr ON lr.id = fe.library_root_id
                 WHERE ms.id = ? AND mi.id = ? AND mt.stream_index = ?
                   AND mt.stream_type = 'SUBTITLE' AND mt.external_path IS NOT NULL
                   AND fe.is_missing = 0
                 LIMIT 1",
            )
            .bind(media_source_id)
            .bind(item_id)
            .bind(stream_index)
            .fetch_optional(&self.pool)
            .await
        } else {
            self.query(
                "SELECT ms.id AS media_source_id, ms.item_id, mt.external_path,
                        mt.language, mt.title, lr.canonical_path AS root_path
                 FROM media_streams mt
                 JOIN media_sources ms ON ms.id = mt.media_source_id
                 JOIN media_items mi ON mi.id = ms.item_id
                 JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                 JOIN library_roots lr ON lr.id = fe.library_root_id
                 WHERE mi.id = ? AND mt.stream_index = ?
                   AND mt.stream_type = 'SUBTITLE' AND mt.external_path IS NOT NULL
                   AND fe.is_missing = 0
                 ORDER BY ms.is_default DESC, ms.id LIMIT 1",
            )
            .bind(item_id)
            .bind(stream_index)
            .fetch_optional(&self.pool)
            .await
        };
        row.map(|row| {
            row.map(|row| StoredExternalSubtitle {
                media_source_id: row.get("media_source_id"),
                item_id: row.get("item_id"),
                external_path: row.get("external_path"),
                language: row.get("language"),
                title: row.get("title"),
                root_path: row.get("root_path"),
            })
        })
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn list_subtitle_streams(
        &self,
        item_id: &str,
        media_source_id: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredSubtitleStream>, StorageError> {
        let limit = limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE);
        let offset = offset.max(0);
        let rows = if let Some(media_source_id) = media_source_id {
            self.query(
                "SELECT ms.id AS media_source_id, ms.item_id, ms.source_kind, ms.probe_status,
                        lr.canonical_path AS root_path, fe.relative_path,
                        mt.stream_index, mt.stream_type, mt.codec, mt.language, mt.title,
                        mt.details_json, mt.external_path, mt.is_external,
                        mt.is_default, mt.is_forced
                 FROM media_streams mt
                 JOIN media_sources ms ON ms.id = mt.media_source_id
                 JOIN media_items mi ON mi.id = ms.item_id
                 JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                 JOIN library_roots lr ON lr.id = fe.library_root_id
                 WHERE ms.id = ? AND mi.id = ? AND mi.removed_at IS NULL
                   AND mt.stream_type = 'SUBTITLE' AND fe.is_missing = 0
                 ORDER BY mt.stream_index
                 LIMIT ? OFFSET ?",
            )
            .bind(media_source_id)
            .bind(item_id)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        } else {
            self.query(
                "SELECT ms.id AS media_source_id, ms.item_id, ms.source_kind, ms.probe_status,
                        lr.canonical_path AS root_path, fe.relative_path,
                        mt.stream_index, mt.stream_type, mt.codec, mt.language, mt.title,
                        mt.details_json, mt.external_path, mt.is_external,
                        mt.is_default, mt.is_forced
                 FROM media_streams mt
                 JOIN media_sources ms ON ms.id = mt.media_source_id
                 JOIN media_items mi ON mi.id = ms.item_id
                 JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                 JOIN library_roots lr ON lr.id = fe.library_root_id
                 WHERE mi.id = ? AND mi.removed_at IS NULL
                   AND mt.stream_type = 'SUBTITLE' AND fe.is_missing = 0
                 ORDER BY ms.is_default DESC, ms.id, mt.stream_index
                 LIMIT ? OFFSET ?",
            )
            .bind(item_id)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        };
        rows.map(|rows| {
            rows.into_iter()
                .map(|row| StoredSubtitleStream {
                    media_source_id: row.get("media_source_id"),
                    item_id: row.get("item_id"),
                    source_kind: row.get("source_kind"),
                    probe_status: row.get("probe_status"),
                    root_path: row.get("root_path"),
                    relative_path: row.get("relative_path"),
                    stream_index: row.get("stream_index"),
                    stream_type: row.get("stream_type"),
                    codec: row.get("codec"),
                    language: row.get("language"),
                    title: row.get("title"),
                    details_json: row.get("details_json"),
                    external_path: row.get("external_path"),
                    is_external: row.get::<i64, _>("is_external") != 0,
                    is_default: row.get::<i64, _>("is_default") != 0,
                    is_forced: row.get::<i64, _>("is_forced") != 0,
                })
                .collect()
        })
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn update_external_subtitle(
        &self,
        update: ExternalSubtitleUpdate<'_>,
    ) -> Result<bool, StorageError> {
        let mut transaction = self.pool.begin().await.map_err(self.sqlx_err())?;
        let exists = self
            .query_scalar::<i64>(
                "SELECT 1 FROM media_streams mt
             JOIN media_sources ms ON ms.id = mt.media_source_id
             WHERE ms.id = ? AND ms.item_id = ? AND mt.stream_index = ?
               AND mt.stream_type = 'SUBTITLE' AND mt.is_external = 1
             LIMIT 1",
            )
            .bind(update.media_source_id)
            .bind(update.item_id)
            .bind(update.stream_index)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?
            .is_some();
        if !exists {
            return Ok(false);
        }
        if update.is_default {
            self.query(
                "UPDATE media_streams
                 SET is_default = 0, updated_at = unixepoch()
                 WHERE media_source_id = ? AND stream_type = 'SUBTITLE'
                   AND is_external = 1",
            )
            .bind(update.media_source_id)
            .execute(&mut *transaction)
            .await
            .map_err(self.sqlx_err())?;
        }
        self.query(
            "UPDATE media_streams
             SET title = ?, language = ?, is_default = ?, is_forced = ?,
                 updated_at = unixepoch()
             WHERE media_source_id = ? AND stream_index = ?
               AND stream_type = 'SUBTITLE' AND is_external = 1",
        )
        .bind(update.title)
        .bind(update.language)
        .bind(database_flag(update.is_default))
        .bind(database_flag(update.is_forced))
        .bind(update.media_source_id)
        .bind(update.stream_index)
        .execute(&mut *transaction)
        .await
        .map_err(self.sqlx_err())?;
        transaction.commit().await.map_err(self.sqlx_err())?;
        Ok(true)
    }
}
