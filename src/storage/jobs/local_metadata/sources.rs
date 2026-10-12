use super::*;

// One item ID per bind keeps source freshness checks below SQLite's conservative limit.
const SCAN_LOCAL_METADATA_SOURCE_IDENTITY_BATCH_SIZE: usize = 500;

pub(in crate::storage::repository) fn escape_sql_like_pattern(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

impl Database {
    pub(crate) async fn list_scan_local_metadata_sources(
        &self,
        filesystem_entry_ids: &[String],
    ) -> Result<Vec<StoredScanLocalMetadataSource>, StorageError> {
        let mut referenced_directories = Vec::new();
        let mut seen_directories = std::collections::HashSet::new();
        for entry_ids in filesystem_entry_ids.chunks(SCAN_DML_CHUNK_SIZE) {
            if entry_ids.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", entry_ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT DISTINCT library_root_id, relative_path
                 FROM filesystem_entries
                 WHERE id IN ({placeholders}) AND entry_kind = 'FILE' AND is_missing = 0"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for entry_id in entry_ids {
                statement = statement.bind(entry_id);
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
                let library_root_id: String = row.get("library_root_id");
                let relative_path: String = row.get("relative_path");
                let directory_path = relative_path
                    .rsplit_once('/')
                    .map_or_else(String::new, |(parent, _)| parent.to_owned());
                let directory = (library_root_id, directory_path);
                if seen_directories.insert(directory.clone()) {
                    referenced_directories.push(directory);
                }
            }
        }
        if referenced_directories.is_empty() {
            return Ok(Vec::new());
        }

        let mut sources = Vec::new();
        let mut directories_with_direct_sources = std::collections::HashSet::new();
        for directories in referenced_directories.chunks(MAX_SCAN_LOCAL_METADATA_BATCH_SOURCES) {
            let predicates = directories
                .iter()
                .map(|(_, directory_path)| {
                    if directory_path.is_empty() {
                        "(source_entry.library_root_id = ?
                          AND source_entry.relative_path NOT LIKE '%/%' ESCAPE '\\')"
                            .to_owned()
                    } else {
                        "(source_entry.library_root_id = ?
                          AND source_entry.relative_path LIKE ? ESCAPE '\\'
                          AND source_entry.relative_path NOT LIKE ? ESCAPE '\\')"
                            .to_owned()
                    }
                })
                .collect::<Vec<_>>()
                .join(" OR ");
            let query = format!(
                "SELECT DISTINCT preferred.id AS source_id, mi.id AS item_id, mi.item_type,
                        preferred.probe_status, series.id AS series_id,
                        season.id AS season_id, season.season_number,
                        lr.canonical_path AS root_path,
                        source_entry.library_root_id AS queued_library_root_id,
                        source_entry.relative_path AS queued_relative_path,
                        preferred_entry.relative_path
                 FROM media_sources queued
                 JOIN filesystem_entries source_entry
                   ON source_entry.id = queued.filesystem_entry_id
                 JOIN media_items mi ON mi.id = queued.item_id
                 JOIN media_sources preferred ON preferred.id = (
                     SELECT candidate.id FROM media_sources candidate
                     JOIN filesystem_entries candidate_entry
                       ON candidate_entry.id = candidate.filesystem_entry_id
                     WHERE candidate.item_id = mi.id AND candidate_entry.is_missing = 0
                     ORDER BY candidate.is_default DESC, candidate.id
                     LIMIT 1
                 )
                 JOIN filesystem_entries preferred_entry
                   ON preferred_entry.id = preferred.filesystem_entry_id
                 JOIN library_roots lr ON lr.id = preferred_entry.library_root_id
                 LEFT JOIN media_items season
                   ON season.id = mi.parent_id AND season.item_type = 'SEASON'
                 LEFT JOIN media_items series
                   ON series.id = mi.series_id AND series.item_type = 'SERIES'
                 WHERE source_entry.entry_kind = 'FILE' AND source_entry.is_missing = 0
                   AND ({predicates})
                   AND mi.removed_at IS NULL AND preferred_entry.is_missing = 0
                 ORDER BY mi.id"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for (library_root_id, directory_path) in directories {
                statement = statement.bind(library_root_id);
                if !directory_path.is_empty() {
                    let escaped = escape_sql_like_pattern(directory_path);
                    statement = statement
                        .bind(format!("{escaped}/%"))
                        .bind(format!("{escaped}/%/%"));
                }
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
                let queued_relative_path: String = row.get("queued_relative_path");
                let queued_library_root_id: String = row.get("queued_library_root_id");
                let directory_path = queued_relative_path
                    .rsplit_once('/')
                    .map_or_else(String::new, |(parent, _)| parent.to_owned());
                directories_with_direct_sources.insert((queued_library_root_id, directory_path));
                let relative_path: String = row.get("relative_path");
                sources.push(StoredScanLocalMetadataSource {
                    source_id: row.get("source_id"),
                    item_id: row.get("item_id"),
                    item_type: row.get("item_type"),
                    probe_status: row.get("probe_status"),
                    series_id: row.get("series_id"),
                    season_id: row.get("season_id"),
                    season_number: row.get("season_number"),
                    root_path: row.get("root_path"),
                    relative_path,
                });
            }
        }

        // A series-level poster may be the only changed file in its directory. If that
        // directory contains season subdirectories, associate it with one source per season
        // so the existing series image indexer can find the poster and season artwork.
        for (library_root_id, directory_path) in &referenced_directories {
            if directory_path.is_empty()
                || directories_with_direct_sources
                    .contains(&(library_root_id.clone(), directory_path.clone()))
            {
                continue;
            }
            let escaped = escape_sql_like_pattern(directory_path);
            let query = "SELECT DISTINCT preferred.id AS source_id, mi.id AS item_id, mi.item_type,
                        preferred.probe_status, series.id AS series_id,
                        season.id AS season_id, season.season_number,
                        lr.canonical_path AS root_path, preferred_entry.relative_path
                 FROM media_sources queued
                 JOIN filesystem_entries source_entry
                   ON source_entry.id = queued.filesystem_entry_id
                 JOIN media_items mi ON mi.id = queued.item_id
                 JOIN media_sources preferred ON preferred.id = (
                     SELECT candidate.id FROM media_sources candidate
                     JOIN filesystem_entries candidate_entry
                       ON candidate_entry.id = candidate.filesystem_entry_id
                     WHERE candidate.item_id = mi.id AND candidate_entry.is_missing = 0
                     ORDER BY candidate.is_default DESC, candidate.id
                     LIMIT 1
                 )
                 JOIN filesystem_entries preferred_entry
                   ON preferred_entry.id = preferred.filesystem_entry_id
                 JOIN library_roots lr ON lr.id = preferred_entry.library_root_id
                 LEFT JOIN media_items season
                   ON season.id = mi.parent_id AND season.item_type = 'SEASON'
                 LEFT JOIN media_items series
                   ON series.id = mi.series_id AND series.item_type = 'SERIES'
                 WHERE source_entry.library_root_id = ?
                   AND source_entry.relative_path LIKE ? ESCAPE '\\'
                   AND source_entry.entry_kind = 'FILE' AND source_entry.is_missing = 0
                   AND mi.item_type = 'EPISODE' AND mi.removed_at IS NULL
                   AND preferred_entry.is_missing = 0
                 ORDER BY mi.id
                 LIMIT ?";
            let rows = self
                .query(query)
                .bind(library_root_id)
                .bind(format!("{escaped}/%"))
                .bind(MAX_SCAN_LOCAL_METADATA_BATCH_SOURCES as i64)
                .fetch_all(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            let mut seen_seasons = std::collections::HashSet::new();
            for row in rows {
                let season_id: String = row.get("season_id");
                if !seen_seasons.insert(season_id) {
                    continue;
                }
                sources.push(StoredScanLocalMetadataSource {
                    source_id: row.get("source_id"),
                    item_id: row.get("item_id"),
                    item_type: row.get("item_type"),
                    probe_status: row.get("probe_status"),
                    series_id: row.get("series_id"),
                    season_id: row.get("season_id"),
                    season_number: row.get("season_number"),
                    root_path: row.get("root_path"),
                    relative_path: row.get("relative_path"),
                });
            }
        }
        let mut seen_items = std::collections::HashSet::with_capacity(sources.len());
        sources.retain(|source| seen_items.insert(source.item_id.clone()));
        Ok(sources)
    }

    pub(crate) async fn list_current_scan_local_metadata_item_ids(
        &self,
        source_identities: &[(String, String)],
    ) -> Result<Vec<String>, StorageError> {
        let mut expected_source_by_item = HashMap::with_capacity(source_identities.len());
        for (item_id, source_id) in source_identities {
            expected_source_by_item.insert(item_id.as_str(), source_id.as_str());
        }
        let mut item_ids = expected_source_by_item.keys().copied().collect::<Vec<_>>();
        item_ids.sort_unstable();

        let mut current_item_ids = Vec::with_capacity(item_ids.len());
        for chunk in item_ids.chunks(SCAN_LOCAL_METADATA_SOURCE_IDENTITY_BATCH_SIZE) {
            if chunk.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "WITH current_sources AS (
                     SELECT mi.id AS source_item_id, mi.parent_id, mi.series_id,
                            mi.library_id, preferred.id AS source_id
                     FROM media_items mi
                     JOIN media_sources preferred ON preferred.id = (
                         SELECT candidate.id FROM media_sources candidate
                         JOIN filesystem_entries candidate_entry
                           ON candidate_entry.id = candidate.filesystem_entry_id
                         WHERE candidate.item_id = mi.id AND candidate_entry.is_missing = 0
                         ORDER BY candidate.is_default DESC, candidate.id
                         LIMIT 1
                     )
                     JOIN filesystem_entries preferred_entry
                       ON preferred_entry.id = preferred.filesystem_entry_id
                     JOIN library_roots lr ON lr.id = preferred_entry.library_root_id
                     WHERE mi.id IN ({placeholders}) AND mi.removed_at IS NULL
                       AND preferred_entry.is_missing = 0
                 )
                 SELECT source_item_id, source_id, source_item_id AS item_id
                 FROM current_sources
                 UNION ALL
                 SELECT current_sources.source_item_id, current_sources.source_id,
                        related.id AS item_id
                 FROM current_sources
                 JOIN media_items related
                   ON related.id IN (current_sources.parent_id, current_sources.series_id)
                  AND related.library_id = current_sources.library_id
                  AND related.removed_at IS NULL
                  AND related.item_type IN ('SERIES', 'SEASON')"
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
                let source_item_id: String = row.get("source_item_id");
                let item_id: String = row.get("item_id");
                let source_id: String = row.get("source_id");
                if expected_source_by_item.get(source_item_id.as_str()) == Some(&source_id.as_str())
                {
                    current_item_ids.push(item_id);
                }
            }
        }

        current_item_ids.sort_unstable();
        current_item_ids.dedup();
        Ok(current_item_ids)
    }
}
