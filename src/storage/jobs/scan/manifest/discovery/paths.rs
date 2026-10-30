use super::*;
use crate::storage::sql_templates::manifest_discovery_known_paths_template;

pub(super) struct ManifestDiscoveryFilePathCounts<'a> {
    pub(super) file_paths: Vec<&'a str>,
    pub(super) inserted_file_count: usize,
    pub(super) unchanged_file_count: usize,
}

impl Database {
    pub(super) async fn query_manifest_discovery_file_paths<'a>(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Any>,
        chunk: &NewScanManifestDiscoveryChunk<'_>,
        discovery_format_version: i64,
        generation: &str,
        entries: &[&'a NewScanManifestEntry],
        unchanged_paths: &[&str],
    ) -> Result<ManifestDiscoveryFilePathCounts<'a>, StorageError> {
        let file_paths = entries
            .iter()
            .filter(|entry| entry.entry_kind == "FILE")
            .map(|entry| entry.relative_path.as_str())
            .collect::<Vec<_>>();
        let known_path_started = Instant::now();
        let mut known_file_paths = std::collections::HashSet::new();
        for paths in file_paths.chunks(super::manifest_path_query_chunk_size(self.backend())) {
            if paths.is_empty() {
                continue;
            }
            let template = manifest_discovery_known_paths_template(
                &self.filesystem_path_templates,
                self.backend(),
                discovery_format_version,
                paths.len(),
            );
            let mut statement = match template {
                Ok(sql) => self.query(sqlx::AssertSqlSafe(sql)),
                Err(_) => {
                    let query = if discovery_format_version == 3 {
                        let values = std::iter::repeat_n("(?)", paths.len())
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!(
                            "WITH incoming(relative_path) AS (VALUES {values})
                             SELECT incoming.relative_path FROM incoming
                             WHERE EXISTS (
                                 SELECT 1 FROM filesystem_entries entry
                                   WHERE entry.library_root_id = ?
                                   AND entry.relative_path = incoming.relative_path
                                   AND entry.last_seen_generation = ?
                             )"
                        )
                    } else {
                        let placeholders = std::iter::repeat_n("?", paths.len())
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!(
                            "SELECT DISTINCT relative_path FROM scan_manifest_entries
                             WHERE manifest_id = ? AND library_root_id = ? AND entry_kind = 'FILE'
                               AND relative_path IN ({placeholders})"
                        )
                    };
                    self.query(sqlx::AssertSqlSafe(query))
                }
            };
            if discovery_format_version == 3 {
                for path in paths {
                    statement = statement.bind(path);
                }
                statement = statement.bind(chunk.library_root_id).bind(generation);
            } else {
                statement = statement
                    .bind(chunk.manifest_id)
                    .bind(chunk.library_root_id);
                for path in paths {
                    statement = statement.bind(path);
                }
            }
            known_file_paths.extend(
                statement
                    .fetch_all(&mut **transaction)
                    .await
                    .map_err(self.sqlx_err())?
                    .into_iter()
                    .map(|row| row.get::<String, _>("relative_path")),
            );
        }
        let inserted_file_count = file_paths
            .iter()
            .filter(|path| !known_file_paths.contains(**path))
            .count();
        let unchanged_file_count = unchanged_paths
            .iter()
            .filter(|path| !known_file_paths.contains(**path))
            .count();
        record_manifest_storage_stage(
            "known_path_query",
            known_path_started,
            file_paths.len().saturating_add(unchanged_paths.len()),
            file_paths.len(),
            0,
        );

        Ok(ManifestDiscoveryFilePathCounts {
            file_paths,
            inserted_file_count,
            unchanged_file_count,
        })
    }
}
