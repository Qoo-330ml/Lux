use super::*;

#[path = "jobs/local_metadata.rs"]
mod local_metadata;
use local_metadata::MAX_SCAN_LOCAL_METADATA_BATCH_SOURCES;
pub(crate) use local_metadata::StoredScanLocalMetadataBackfillPage;
#[path = "jobs/metadata_reidentify.rs"]
mod metadata_reidentify;
use local_metadata::escape_sql_like_pattern;
#[path = "jobs/scan.rs"]
mod scan;
use scan::prune_sidecar_directories;
#[cfg(test)]
use scan::sidecar_target_query;
#[path = "jobs/shutdown.rs"]
mod shutdown;

#[path = "jobs/strm_probe.rs"]
mod strm_probe;

#[cfg(test)]
pub(crate) use super::media_sources::MAX_MEDIA_SOURCE_DELETE_BATCH_SIZE;
use std::{collections::HashMap, time::Instant};

const SCAN_MANIFEST_DIFF_TRANSACTION_BATCH_SIZE: usize = 500;
const MAX_SCAN_MANIFEST_APPLY_BATCH_SIZE: i64 = 500;

struct ManifestPostprocessingTargetRange<'a> {
    job_id: &'a str,
    library_root_id: &'a str,
    generation: &'a str,
    target_stage: &'a str,
    after_relative_path: Option<&'a str>,
    through_relative_path: Option<&'a str>,
}

#[derive(Default)]
struct ManifestDiscoveryPositiveIndexResult<'a> {
    created_items: usize,
    metadata_targets_changed: bool,
    add_count: i64,
    change_count: i64,
    reappeared_count: i64,
    applied_count: i64,
    indexed_paths: Vec<&'a str>,
    local_metadata_refs: Vec<(&'a str, &'a str)>,
}

struct ManifestDiscoveryPositiveIndexCommit<'a, 'b> {
    job_id: &'b str,
    library_id: &'b str,
    library_root_id: &'b str,
    generation: &'b str,
    positives: &'a [&'a NewScanManifestPositiveIndex],
    observations: &'a [&'a NewScanManifestEntry],
}

fn record_manifest_positive_applied<'a>(
    result: &mut ManifestDiscoveryPositiveIndexResult<'a>,
    positive: &'a NewScanManifestPositiveIndex,
) {
    result.indexed_paths.push(positive.relative_path.as_str());
    result.local_metadata_refs.push((
        positive.relative_path.as_str(),
        manifest_positive_file_filesystem_entry_id(&positive.file),
    ));
    result.applied_count = result.applied_count.saturating_add(1);
    match positive.delta_kind.as_str() {
        "ADD" => result.add_count = result.add_count.saturating_add(1),
        "CHANGE" => result.change_count = result.change_count.saturating_add(1),
        "REAPPEARED" => result.reappeared_count = result.reappeared_count.saturating_add(1),
        _ => {}
    }
}

fn manifest_positive_file_path(file: &NewScanManifestIndexedFile) -> &str {
    match file {
        NewScanManifestIndexedFile::Movie(file) => &file.relative_path,
        NewScanManifestIndexedFile::Episode(file) => &file.relative_path,
        NewScanManifestIndexedFile::Unresolved(file) => &file.relative_path,
        NewScanManifestIndexedFile::Sidecar(file) => &file.relative_path,
    }
}

fn manifest_positive_file_filesystem_entry_id(file: &NewScanManifestIndexedFile) -> &str {
    match file {
        NewScanManifestIndexedFile::Movie(file) => &file.filesystem_entry_id,
        NewScanManifestIndexedFile::Episode(file) => &file.filesystem_entry_id,
        NewScanManifestIndexedFile::Unresolved(file) => &file.filesystem_entry_id,
        NewScanManifestIndexedFile::Sidecar(file) => &file.filesystem_entry_id,
    }
}

impl Database {
    pub(crate) const LEGACY_SCAN_REQUIRES_NEW_MANIFEST: &'static str =
        "LEGACY_SCAN_REQUIRES_NEW_MANIFEST";
}

impl Database {
    pub(crate) async fn materialize_scan_manifest_postprocessing_target_page(
        &self,
        page: ManifestPostprocessingTargetPage<'_>,
    ) -> Result<ManifestPostprocessingTargetBatchResult, StorageError> {
        let ManifestPostprocessingTargetPage {
            job_id,
            manifest_id,
            library_root_id,
            generation,
            target_stage,
            target_cursor,
            page_size,
        } = page;
        if !matches!(target_stage, "NEW" | "CHANGED") {
            return Err(StorageError::Conflict(
                "manifest target stage is not materializable".to_owned(),
            ));
        }
        let page_limit = page_size.clamp(1, MANIFEST_POSTPROCESSING_TARGET_PAGE_SIZE);
        let limit = i64::try_from(page_limit).unwrap_or(i64::MAX);
        let mut transaction = self.begin_scan_write_transaction().await?;
        let state: Option<(String, i64, i64, i64, String, String, String)> = self
            .query_as(
                "SELECT manifest.state, manifest.workflow_version,
                        manifest.discovery_format_version,
                        manifest.postprocessing_targets_ready,
                        job.status, job.scan_phase, job.generation
                 FROM scan_manifests manifest
                 JOIN scan_jobs job ON job.id = manifest.job_id
                 WHERE manifest.id = ? AND job.id = ?",
            )
            .bind(manifest_id)
            .bind(job_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let Some((manifest_state, workflow, format, ready, job_status, scan_phase, job_generation)) =
            state
        else {
            return Err(StorageError::Conflict(
                "manifest target checkpoint is missing".to_owned(),
            ));
        };
        if manifest_state != "POSTPROCESSING"
            || !matches!(workflow, 2 | 3)
            || format != 3
            || ready != 0
            || job_status != "COMPLETED"
            || scan_phase != "POSTPROCESSING"
            || job_generation != generation
        {
            return Err(StorageError::Conflict(
                "manifest is not ready for v3 postprocessing target materialization".to_owned(),
            ));
        }
        let root_checkpoint: Option<(String, Option<String>)> = self
            .query_as(
                "SELECT postprocessing_target_stage, postprocessing_target_cursor
                 FROM scan_manifest_roots
                 WHERE manifest_id = ? AND library_root_id = ?",
            )
            .bind(manifest_id)
            .bind(library_root_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if root_checkpoint.as_ref()
            != Some(&(target_stage.to_owned(), target_cursor.map(str::to_owned)))
        {
            return Err(StorageError::Conflict(
                "manifest target root checkpoint changed before commit".to_owned(),
            ));
        }
        let relative_paths: Vec<String> = self
            .query_scalar(
                "SELECT relative_path FROM filesystem_entries
                 WHERE library_root_id = ? AND last_seen_generation = ?
                   AND entry_kind = 'FILE'
                   AND last_seen_change_kind = ?
                   AND relative_path > COALESCE(?, '')
                 ORDER BY relative_path LIMIT ?",
            )
            .bind(library_root_id)
            .bind(generation)
            .bind(target_stage)
            .bind(target_cursor)
            .bind(limit)
            .fetch_all(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let mut targets_changed = false;
        let next_stage;
        let next_cursor;
        if relative_paths.is_empty() || relative_paths.len() < page_limit {
            if target_stage == "NEW" {
                next_stage = "CHANGED";
                next_cursor = None;
            } else {
                next_stage = "DONE";
                next_cursor = None;
            }
            if !relative_paths.is_empty() {
                targets_changed = self
                    .insert_scan_manifest_postprocessing_targets_in_transaction(
                        &mut transaction,
                        ManifestPostprocessingTargetRange {
                            job_id,
                            library_root_id,
                            generation,
                            target_stage,
                            after_relative_path: Some(target_cursor.unwrap_or_default()),
                            through_relative_path: relative_paths.last().map(String::as_str),
                        },
                    )
                    .await?;
            }
        } else {
            let last_path = relative_paths
                .last()
                .ok_or_else(|| StorageError::Conflict("empty target page".to_owned()))?;
            targets_changed = self
                .insert_scan_manifest_postprocessing_targets_in_transaction(
                    &mut transaction,
                    ManifestPostprocessingTargetRange {
                        job_id,
                        library_root_id,
                        generation,
                        target_stage,
                        after_relative_path: Some(target_cursor.unwrap_or_default()),
                        through_relative_path: Some(last_path),
                    },
                )
                .await?;
            next_stage = target_stage;
            next_cursor = Some(last_path.as_str());
        }

        let root_update = self
            .query(
                "UPDATE scan_manifest_roots
                 SET postprocessing_target_stage = ?, postprocessing_target_cursor = ?,
                     updated_at = unixepoch()
                 WHERE manifest_id = ? AND library_root_id = ?
                   AND postprocessing_target_stage = ?
                   AND (postprocessing_target_cursor = ? OR
                        (postprocessing_target_cursor IS NULL AND ? IS NULL))",
            )
            .bind(next_stage)
            .bind(next_cursor)
            .bind(manifest_id)
            .bind(library_root_id)
            .bind(target_stage)
            .bind(target_cursor)
            .bind(target_cursor)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if root_update.rows_affected() != 1 {
            return Err(StorageError::Conflict(
                "manifest target root checkpoint could not advance".to_owned(),
            ));
        }
        let unfinished_roots: i64 = self
            .query_scalar(
                "SELECT COUNT(*) FROM scan_manifest_roots
                 WHERE manifest_id = ? AND postprocessing_target_stage <> 'DONE'",
            )
            .bind(manifest_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let targets_ready = unfinished_roots == 0;
        if targets_ready {
            self.query(
                "UPDATE scan_manifests SET postprocessing_targets_ready = 1,
                     updated_at = unixepoch()
                 WHERE id = ? AND state = 'POSTPROCESSING'",
            )
            .bind(manifest_id)
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
        Ok(ManifestPostprocessingTargetBatchResult {
            targets_changed,
            targets_ready,
        })
    }

    pub(crate) async fn finish_empty_scan_manifest_postprocessing_targets(
        &self,
        manifest_id: &str,
    ) -> Result<bool, StorageError> {
        let mut transaction = self.begin_scan_write_transaction().await?;
        let ready = self
            .query(
                "UPDATE scan_manifests
                 SET postprocessing_targets_ready = 1, updated_at = unixepoch()
                 WHERE id = ? AND state = 'POSTPROCESSING'
                   AND NOT EXISTS (
                       SELECT 1 FROM scan_manifest_roots
                       WHERE manifest_id = ? AND postprocessing_target_stage <> 'DONE'
                   )",
            )
            .bind(manifest_id)
            .bind(manifest_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .rows_affected()
            == 1;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(ready)
    }

    async fn insert_scan_manifest_postprocessing_targets_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        target_range: ManifestPostprocessingTargetRange<'_>,
    ) -> Result<bool, StorageError> {
        let ManifestPostprocessingTargetRange {
            job_id,
            library_root_id,
            generation,
            target_stage,
            after_relative_path,
            through_relative_path,
        } = target_range;
        let (Some(after_relative_path), Some(through_relative_path)) =
            (after_relative_path, through_relative_path)
        else {
            return Ok(false);
        };
        // The bounded page is materialized once and supplies both target types.
        // NEW-stage entries imply NEW item targets; CHANGED retains the global
        // lookup so an item with any NEW source keeps NEW precedence.
        let item_change_kind = if target_stage == "NEW" {
            "'NEW'"
        } else {
            "CASE WHEN EXISTS (
                 SELECT 1
                 FROM media_sources new_source
                 JOIN filesystem_entries new_entry
                   ON new_entry.id = new_source.filesystem_entry_id
                 WHERE new_source.item_id = page_sources.item_id
                   AND new_entry.last_seen_generation = ?
                   AND new_entry.last_seen_change_kind = 'NEW'
             ) THEN 'NEW' ELSE 'CHANGED' END"
        };
        let statement = format!(
            "WITH page_sources AS MATERIALIZED (
                 SELECT source.id AS source_id, source.item_id,
                        entry.last_seen_change_kind AS change_kind
                 FROM filesystem_entries entry
                 JOIN media_sources source ON source.filesystem_entry_id = entry.id
                 WHERE entry.library_root_id = ? AND entry.last_seen_generation = ?
                   AND entry.entry_kind = 'FILE'
                   AND entry.last_seen_change_kind = ?
                   AND entry.relative_path > COALESCE(?, '')
                   AND entry.relative_path <= ?
             ),
             target_rows (
                 target_type, target_id, source_id, item_id, change_kind,
                 probe_state, metadata_state, thumbnail_state
             ) AS (
                 SELECT 'SOURCE', source_id, source_id, item_id, change_kind,
                        'PENDING', 'SKIPPED', 'SKIPPED'
                 FROM page_sources
                 UNION ALL
                 SELECT 'ITEM', page_sources.item_id, NULL, page_sources.item_id,
                        {item_change_kind}, 'SKIPPED', 'PENDING', 'PENDING'
                 FROM page_sources
                 GROUP BY page_sources.item_id
             )
             INSERT INTO scan_job_targets (
                 job_id, target_type, target_id, source_id, item_id, change_kind,
                 probe_state, metadata_state, thumbnail_state
             )
             SELECT ?, target_type, target_id, source_id, item_id, change_kind,
                    probe_state, metadata_state, thumbnail_state
             FROM target_rows
             WHERE 1 = 1
             ON CONFLICT(job_id, target_type, target_id) DO NOTHING"
        );
        // Only the stage-specific SQL expression above is interpolated; all data stays bound.
        let mut insert_query = self
            .query(sqlx::AssertSqlSafe(statement))
            .bind(library_root_id)
            .bind(generation)
            .bind(target_stage)
            .bind(after_relative_path)
            .bind(through_relative_path);
        if target_stage == "CHANGED" {
            insert_query = insert_query.bind(generation);
        }
        let target_result = insert_query
            .bind(job_id)
            .execute(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(target_result.rows_affected() > 0)
    }

    pub(crate) async fn list_scan_manifest_directories(
        &self,
        manifest_id: &str,
        limit: i64,
    ) -> Result<Vec<StoredScanManifestDirectory>, StorageError> {
        self.query(
            "SELECT library_root_id, relative_path
             FROM scan_manifest_directories
             WHERE manifest_id = ? AND state = 'PENDING'
             ORDER BY library_root_id, relative_path
             LIMIT ?",
        )
        .bind(manifest_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredScanManifestDirectory {
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

    pub(crate) async fn list_scan_manifest_complete_directories(
        &self,
        manifest_id: &str,
        after_library_root_id: Option<&str>,
        after_relative_path: Option<&str>,
        limit: i64,
    ) -> Result<Vec<StoredScanManifestDirectory>, StorageError> {
        self.query(
            "SELECT directory.library_root_id, directory.relative_path
             FROM scan_manifest_directories directory
             JOIN scan_manifest_roots root
               ON root.manifest_id = directory.manifest_id
              AND root.library_root_id = directory.library_root_id
              AND root.state = 'COMPLETE'
             WHERE directory.manifest_id = ?
               AND directory.state = 'COMPLETE'
               AND (directory.library_root_id, directory.relative_path) > (?, ?)
             ORDER BY directory.library_root_id, directory.relative_path
             LIMIT ?",
        )
        .bind(manifest_id)
        .bind(after_library_root_id.unwrap_or_default())
        .bind(after_relative_path.unwrap_or_default())
        .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredScanManifestDirectory {
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

    pub(crate) async fn scan_manifest_has_uncovered_files(
        &self,
        manifest_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS (
                 SELECT 1
                 FROM filesystem_entries fe
                 JOIN scan_manifest_roots root
                   ON root.manifest_id = ?
                  AND root.library_root_id = fe.library_root_id
                  AND root.state = 'COMPLETE'
                 JOIN scan_manifests manifest ON manifest.id = root.manifest_id
                 JOIN scan_jobs job ON job.id = manifest.job_id
                 WHERE fe.entry_kind = 'FILE' AND fe.is_missing = 0
                   AND COALESCE(fe.last_seen_generation, '') <> job.generation
                   AND NOT EXISTS (
                       SELECT 1 FROM scan_manifest_seen_paths seen
                       WHERE seen.manifest_id = root.manifest_id
                         AND seen.library_root_id = fe.library_root_id
                         AND seen.relative_path = fe.relative_path
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM scan_manifest_deltas delta
                       WHERE delta.manifest_id = root.manifest_id
                         AND delta.library_root_id = fe.library_root_id
                         AND delta.relative_path = fe.relative_path
                   )
                   AND NOT EXISTS (
                       SELECT 1
                       FROM scan_manifest_directories directory
                       WHERE directory.manifest_id = root.manifest_id
                         AND directory.library_root_id = fe.library_root_id
                         AND directory.state = 'COMPLETE'
                         AND (
                             (directory.relative_path = ''
                              AND fe.relative_path NOT LIKE '%/%' ESCAPE '\\')
                             OR (directory.relative_path <> ''
                                 AND fe.relative_path LIKE (
                                     REPLACE(
                                         REPLACE(
                                             REPLACE(directory.relative_path, '\\', '\\\\'),
                                             '%',
                                             '\\%'
                                         ),
                                         '_',
                                         '\\_'
                                     ) || '/%'
                                 ) ESCAPE '\\'
                                 AND fe.relative_path NOT LIKE (
                                     REPLACE(
                                         REPLACE(
                                             REPLACE(directory.relative_path, '\\', '\\\\'),
                                             '%',
                                             '\\%'
                                         ),
                                         '_',
                                         '\\_'
                                     ) || '/%/%'
                                 ) ESCAPE '\\')
                         )
                   )
             ) THEN 1 ELSE 0 END",
        )
        .bind(manifest_id)
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_scan_manifest_removal_candidates_for_directories(
        &self,
        manifest_id: &str,
        directories: &[StoredScanManifestDirectory],
        limit: i64,
    ) -> Result<Vec<StoredScanManifestRemovalCandidate>, StorageError> {
        if directories.is_empty() {
            return Ok(Vec::new());
        }
        let predicates = directories
            .iter()
            .map(|directory| {
                if directory.relative_path.is_empty() {
                    "(fe.library_root_id = ? AND fe.relative_path NOT LIKE '%/%' ESCAPE '\\')"
                        .to_owned()
                } else {
                    "(fe.library_root_id = ?
                      AND fe.relative_path LIKE ? ESCAPE '\\'
                      AND fe.relative_path NOT LIKE ? ESCAPE '\\')"
                        .to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join(" OR ");
        let query = format!(
            "SELECT fe.library_root_id, fe.relative_path,
                    fe.id AS base_filesystem_entry_id,
                    fe.fingerprint AS base_fingerprint
             FROM filesystem_entries fe
             JOIN scan_manifest_roots root
              ON root.manifest_id = ?
              AND root.library_root_id = fe.library_root_id
              AND root.state = 'COMPLETE'
             JOIN scan_manifests manifest ON manifest.id = root.manifest_id
             JOIN scan_jobs job ON job.id = manifest.job_id
             WHERE fe.entry_kind = 'FILE' AND fe.is_missing = 0
               AND ({predicates})
               AND COALESCE(fe.last_seen_generation, '') <> job.generation
               AND NOT EXISTS (
                   SELECT 1 FROM scan_manifest_seen_paths seen
                   WHERE seen.manifest_id = ?
                     AND seen.library_root_id = fe.library_root_id
                     AND seen.relative_path = fe.relative_path
               )
               AND NOT EXISTS (
                   SELECT 1 FROM scan_manifest_deltas delta
                   WHERE delta.manifest_id = ?
                     AND delta.library_root_id = fe.library_root_id
                     AND delta.relative_path = fe.relative_path
               )
             ORDER BY fe.library_root_id, fe.relative_path
             LIMIT ?"
        );
        let mut statement = self.query(sqlx::AssertSqlSafe(query)).bind(manifest_id);
        for directory in directories {
            statement = statement.bind(&directory.library_root_id);
            if !directory.relative_path.is_empty() {
                let escaped = escape_sql_like_pattern(&directory.relative_path);
                statement = statement
                    .bind(format!("{escaped}/%"))
                    .bind(format!("{escaped}/%/%"));
            }
        }
        statement = statement
            .bind(manifest_id)
            .bind(manifest_id)
            .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE));
        statement
            .fetch_all(&self.pool)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(|row| StoredScanManifestRemovalCandidate {
                        library_root_id: row.get("library_root_id"),
                        relative_path: row.get("relative_path"),
                        base_filesystem_entry_id: row.get("base_filesystem_entry_id"),
                        base_fingerprint: row.get("base_fingerprint"),
                    })
                    .collect()
            })
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    #[allow(dead_code)] // Kept for focused storage contract tests.
    pub(crate) async fn commit_scan_manifest_discovery_chunk(
        &self,
        chunk: &NewScanManifestDiscoveryChunk<'_>,
    ) -> Result<i64, StorageError> {
        self.commit_scan_manifest_discovery_chunks(std::slice::from_ref(chunk))
            .await
            .map(|result| result.observed_file_count)
    }

    pub(crate) async fn commit_scan_manifest_discovery_chunks(
        &self,
        chunks: &[NewScanManifestDiscoveryChunk<'_>],
    ) -> Result<ManifestDiscoveryCommitResult, StorageError> {
        let Some(chunk) = chunks.first() else {
            return Ok(ManifestDiscoveryCommitResult::default());
        };
        let input_validation_started = Instant::now();
        let mut child_directories = Vec::new();
        let mut entries = Vec::new();
        let mut completed_directories = Vec::new();
        let mut positive_indexes = Vec::new();
        let mut unchanged_paths = Vec::new();
        let mut seen_filesystem_entries = Vec::new();
        for candidate in chunks {
            if candidate.manifest_id != chunk.manifest_id
                || candidate.job_id != chunk.job_id
                || candidate.library_root_id != chunk.library_root_id
            {
                return Err(StorageError::Conflict(
                    "manifest discovery transaction cannot span jobs or roots".to_owned(),
                ));
            }
            child_directories.extend(candidate.child_directories.iter().map(String::as_str));
            entries.extend(candidate.entries.iter());
            positive_indexes.extend(candidate.positive_indexes.iter());
            unchanged_paths.extend(candidate.unchanged_paths.iter().map(String::as_str));
            seen_filesystem_entries.extend(candidate.seen_filesystem_entries.iter());
            if let Some(directory) = candidate.completed_directory {
                completed_directories.push(directory);
            }
        }
        if child_directories.iter().any(|path| {
            let path = std::path::Path::new(path);
            path.is_absolute()
                || path.components().any(|component| {
                    matches!(
                        component,
                        std::path::Component::ParentDir
                            | std::path::Component::RootDir
                            | std::path::Component::Prefix(_)
                    )
                })
        }) || entries.iter().any(|entry| {
            let path = std::path::Path::new(&entry.relative_path);
            !matches!(entry.entry_kind.as_str(), "FILE" | "DIRECTORY")
                || entry.size < 0
                || path.is_absolute()
                || path.components().any(|component| {
                    matches!(
                        component,
                        std::path::Component::ParentDir
                            | std::path::Component::RootDir
                            | std::path::Component::Prefix(_)
                    )
                })
        }) {
            return Err(StorageError::Conflict(
                "manifest discovery chunk contains an invalid relative path or observation"
                    .to_owned(),
            ));
        }
        let mut observed_paths = std::collections::HashSet::with_capacity(entries.len());
        if entries
            .iter()
            .any(|entry| !observed_paths.insert(entry.relative_path.as_str()))
        {
            return Err(StorageError::Conflict(
                "manifest discovery chunk contains duplicate observation paths".to_owned(),
            ));
        }
        let observations_by_path = entries
            .iter()
            .map(|entry| (entry.relative_path.as_str(), *entry))
            .collect::<HashMap<_, _>>();
        let mut indexed_paths = std::collections::HashSet::with_capacity(positive_indexes.len());
        for positive in &positive_indexes {
            let observed_path = manifest_positive_file_path(&positive.file);
            let Some(observation) = observations_by_path.get(positive.relative_path.as_str())
            else {
                return Err(StorageError::Conflict(
                    "manifest positive index is missing its observation".to_owned(),
                ));
            };
            if observed_path != positive.relative_path
                || observation.entry_kind != "FILE"
                || !indexed_paths.insert(positive.relative_path.as_str())
            {
                return Err(StorageError::Conflict(
                    "manifest positive index does not match a unique file observation".to_owned(),
                ));
            }
            let valid_baseline = match positive.delta_kind.as_str() {
                "ADD" => {
                    positive.base_filesystem_entry_id.is_none()
                        && positive.base_fingerprint.is_none()
                }
                "CHANGE" | "REAPPEARED" => positive.base_filesystem_entry_id.is_some(),
                _ => false,
            };
            if !valid_baseline {
                return Err(StorageError::Conflict(
                    "manifest positive index has an invalid baseline".to_owned(),
                ));
            }
        }
        let mut unchanged_path_set =
            std::collections::HashSet::with_capacity(unchanged_paths.len());
        for path in &unchanged_paths {
            if observations_by_path
                .get(path)
                .is_none_or(|observation| observation.entry_kind != "FILE")
                || indexed_paths.contains(path)
                || !unchanged_path_set.insert(*path)
            {
                return Err(StorageError::Conflict(
                    "manifest unchanged path does not match a unique unindexed file observation"
                        .to_owned(),
                ));
            }
        }
        let mut seen_filesystem_entry_paths =
            std::collections::HashSet::with_capacity(seen_filesystem_entries.len());
        for seen in &seen_filesystem_entries {
            let Some(observation) = observations_by_path.get(seen.relative_path.as_str()) else {
                return Err(StorageError::Conflict(
                    "manifest seen filesystem entry does not match a unique unindexed file observation"
                        .to_owned(),
                ));
            };
            if observation.entry_kind != "FILE"
                || indexed_paths.contains(seen.relative_path.as_str())
                || !seen_filesystem_entry_paths.insert(seen.relative_path.as_str())
                || seen.filesystem_entry_id.is_empty()
                || seen.fingerprint != observation.fingerprint
            {
                return Err(StorageError::Conflict(
                    "manifest seen filesystem entry does not match a unique unindexed file observation"
                        .to_owned(),
                ));
            }
        }
        if child_directories.is_empty() && entries.is_empty() && completed_directories.is_empty() {
            return Ok(ManifestDiscoveryCommitResult::default());
        }

        record_manifest_storage_stage(
            "transaction_input_validation",
            input_validation_started,
            entries.len().saturating_add(child_directories.len()),
            entries
                .iter()
                .filter(|entry| entry.entry_kind == "FILE")
                .count(),
            child_directories.len(),
        );
        let transaction_total_started = Instant::now();
        let transaction_begin_started = Instant::now();
        let mut transaction = self.begin_scan_write_transaction().await?;
        record_manifest_storage_stage(
            "transaction_begin",
            transaction_begin_started,
            entries.len().saturating_add(child_directories.len()),
            entries
                .iter()
                .filter(|entry| entry.entry_kind == "FILE")
                .count(),
            child_directories.len(),
        );
        let manifest_state_started = Instant::now();
        let (
            workflow_version,
            discovery_format_version,
            discovery_mode,
            library_id,
            generation,
            job_status,
            cancel_requested,
        ): (i64, i64, String, String, String, String, i64) = self
            .query_as(
                "SELECT manifest.workflow_version, manifest.discovery_format_version,
                        manifest.discovery_mode, manifest.library_id, job.generation,
                        job.status, job.cancel_requested
                 FROM scan_manifests manifest
                 JOIN scan_jobs job ON job.id = manifest.job_id
                 WHERE manifest.id = ? AND manifest.state = 'DISCOVERING'
                   AND job.id = ?",
            )
            .bind(chunk.manifest_id)
            .bind(chunk.job_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let lite_mode =
            is_lite_manifest_discovery(workflow_version, discovery_format_version, &discovery_mode);

        record_manifest_storage_stage("manifest_state_check", manifest_state_started, 1, 0, 0);
        let directory_frontier_started = Instant::now();
        let mut inserted_directory_count = 0_u64;
        let directory_chunks = if lite_mode {
            Vec::new()
        } else {
            child_directories
                .chunks(SCAN_DML_CHUNK_SIZE)
                .collect::<Vec<_>>()
        };
        for paths in directory_chunks {
            if paths.is_empty() {
                continue;
            }
            let values = std::iter::repeat_n("(?, ?, ?, 'PENDING')", paths.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "INSERT INTO scan_manifest_directories (
                     manifest_id, library_root_id, relative_path, state
                 ) VALUES {values}
                 ON CONFLICT(manifest_id, library_root_id, relative_path) DO NOTHING"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for path in paths {
                statement = statement
                    .bind(chunk.manifest_id)
                    .bind(chunk.library_root_id)
                    .bind(*path);
            }
            let result = statement
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            inserted_directory_count = inserted_directory_count
                .checked_add(result.rows_affected())
                .ok_or_else(|| {
                    StorageError::Conflict("manifest directory count overflow".to_owned())
                })?;
        }
        record_manifest_storage_stage(
            "directory_frontier_insert",
            directory_frontier_started,
            child_directories.len(),
            0,
            child_directories.len(),
        );

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
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            if discovery_format_version == 3 {
                for path in paths {
                    statement = statement.bind(path);
                }
                statement = statement.bind(chunk.library_root_id).bind(&generation);
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
                    .fetch_all(&mut *transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?
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

        let inserted_file_count_i64 = i64::try_from(inserted_file_count)
            .map_err(|_| StorageError::Conflict("manifest file count overflow".to_owned()))?;
        let inserted_directory_count_i64 = i64::try_from(inserted_directory_count)
            .map_err(|_| StorageError::Conflict("manifest directory count overflow".to_owned()))?;
        let directory_frontier_completion_started = Instant::now();
        let mut completed_directory_count = 0_i64;
        let completed_directory_chunks = if lite_mode {
            Vec::new()
        } else {
            completed_directories
                .chunks(SCAN_DML_CHUNK_SIZE)
                .collect::<Vec<_>>()
        };
        for directories in completed_directory_chunks {
            let placeholders = std::iter::repeat_n("?", directories.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "UPDATE scan_manifest_directories
                 SET state = 'COMPLETE', error = NULL, updated_at = unixepoch()
                 WHERE manifest_id = ? AND library_root_id = ? AND state <> 'COMPLETE'
                   AND relative_path IN ({placeholders})"
            );
            let mut statement = self
                .query(sqlx::AssertSqlSafe(query))
                .bind(chunk.manifest_id)
                .bind(chunk.library_root_id);
            for directory in directories {
                statement = statement.bind(*directory);
            }
            let completed = statement
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            completed_directory_count = completed_directory_count
                .checked_add(i64::try_from(completed.rows_affected()).map_err(|_| {
                    StorageError::Conflict("manifest directory count overflow".to_owned())
                })?)
                .ok_or_else(|| {
                    StorageError::Conflict("manifest directory count overflow".to_owned())
                })?;
        }
        record_manifest_storage_stage(
            "directory_frontier_completion",
            directory_frontier_completion_started,
            completed_directories.len(),
            0,
            completed_directories.len(),
        );

        let root_checkpoint_started = Instant::now();
        let observation_entries = if discovery_format_version == 3 {
            entries
                .iter()
                .copied()
                .filter(|entry| {
                    entry.entry_kind == "DIRECTORY"
                        && (!lite_mode || entry.relative_path.is_empty())
                })
                .collect::<Vec<_>>()
        } else {
            entries.clone()
        };
        let observation_count_i64 = if matches!(workflow_version, 2 | 3) {
            i64::try_from(observation_entries.len()).map_err(|_| {
                StorageError::Conflict("manifest observation count overflow".to_owned())
            })?
        } else {
            0
        };
        let reserved_local_batch_sequence_count = if workflow_version == 3 {
            i64::try_from(
                positive_indexes
                    .len()
                    .div_ceil(MAX_SCAN_LOCAL_METADATA_BATCH_SOURCES),
            )
            .map_err(|_| StorageError::Conflict("local metadata batch count overflow".to_owned()))?
        } else {
            0
        };
        let root_sequence_increment = observation_count_i64
            .checked_add(reserved_local_batch_sequence_count)
            .ok_or_else(|| StorageError::Conflict("manifest root sequence overflow".into()))?;
        let discovered_directory_count_i64 = if lite_mode {
            i64::try_from(child_directories.len()).map_err(|_| {
                StorageError::Conflict("manifest directory count overflow".to_owned())
            })?
        } else {
            inserted_directory_count_i64
        };
        let completed_directory_count_for_root = if lite_mode {
            i64::try_from(completed_directories.len()).map_err(|_| {
                StorageError::Conflict("manifest directory count overflow".to_owned())
            })?
        } else {
            completed_directory_count
        };
        let last_sequence = self
            .query_scalar::<i64>(
                "UPDATE scan_manifest_roots
                 SET state = CASE WHEN ? = 'LITE' THEN 'SCANNING'
                     WHEN NOT EXISTS (
                         SELECT 1 FROM scan_manifest_directories
                         WHERE manifest_id = ? AND library_root_id = ? AND state = 'PENDING'
                     ) THEN 'COMPLETE' ELSE 'SCANNING' END,
                     started_at = COALESCE(started_at, unixepoch()),
                     finished_at = CASE WHEN ? = 'LITE' THEN finished_at
                         WHEN NOT EXISTS (
                         SELECT 1 FROM scan_manifest_directories
                         WHERE manifest_id = ? AND library_root_id = ? AND state = 'PENDING'
                     ) THEN COALESCE(finished_at, unixepoch()) ELSE finished_at END,
                     directory_count = directory_count + ?,
                     completed_directory_count = completed_directory_count + ?,
                     observed_file_count = observed_file_count + ?,
                     next_observation_sequence = next_observation_sequence + ?,
                     updated_at = unixepoch()
                 WHERE manifest_id = ? AND library_root_id = ?
                   AND state IN ('PENDING', 'SCANNING')
                   AND EXISTS (
                       SELECT 1 FROM scan_manifests
                       WHERE id = ? AND state = 'DISCOVERING'
                   )
                 RETURNING next_observation_sequence",
            )
            .bind(&discovery_mode)
            .bind(chunk.manifest_id)
            .bind(chunk.library_root_id)
            .bind(&discovery_mode)
            .bind(chunk.manifest_id)
            .bind(chunk.library_root_id)
            .bind(discovered_directory_count_i64)
            .bind(completed_directory_count_for_root)
            .bind(inserted_file_count_i64)
            .bind(root_sequence_increment)
            .bind(chunk.manifest_id)
            .bind(chunk.library_root_id)
            .bind(chunk.manifest_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .ok_or_else(|| {
                StorageError::Conflict("manifest root is not available for discovery".to_owned())
            })?;
        record_manifest_storage_stage(
            "root_checkpoint",
            root_checkpoint_started,
            inserted_directory_count
                .try_into()
                .unwrap_or(usize::MAX)
                .saturating_add(completed_directories.len())
                .saturating_add(inserted_file_count),
            inserted_file_count,
            completed_directories.len(),
        );
        let observation_sequence_start = if observation_count_i64 > 0 {
            Some(
                last_sequence
                    .checked_sub(root_sequence_increment)
                    .and_then(|sequence| sequence.checked_add(1))
                    .ok_or_else(|| {
                        StorageError::Conflict("manifest observation sequence overflow".to_owned())
                    })?,
            )
        } else {
            None
        };
        let local_metadata_sequence_start = if reserved_local_batch_sequence_count > 0 {
            Some(
                last_sequence
                    .checked_sub(reserved_local_batch_sequence_count)
                    .and_then(|sequence| sequence.checked_add(1))
                    .ok_or_else(|| {
                        StorageError::Conflict("local metadata batch sequence overflow".to_owned())
                    })?,
            )
        } else {
            None
        };

        let observation_batch_size = if matches!(workflow_version, 2 | 3) {
            super::manifest_path_query_chunk_size(self.backend())
        } else {
            80
        };
        let observation_started = Instant::now();
        let mut observation_offset = 0_i64;
        for entry_chunk in observation_entries.chunks(observation_batch_size) {
            if entry_chunk.is_empty() {
                continue;
            }
            if let Some(sequence_start) = observation_sequence_start {
                let values = std::iter::repeat_n("(?, ?, ?, ?, ?, ?, ?, ?)", entry_chunk.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                let query = format!(
                    "WITH incoming (
                         relative_path, observation_sequence, entry_kind, size,
                         modified_at, device, inode, fingerprint
                     ) AS (VALUES {values})
                     INSERT INTO scan_manifest_entries (
                         manifest_id, library_root_id, relative_path, observation_sequence,
                         entry_kind, size, modified_at, device, inode, fingerprint, observed_at
                     )
                     SELECT ?, ?, relative_path, observation_sequence, entry_kind, size,
                            modified_at, device, inode, fingerprint, unixepoch()
                     FROM incoming"
                );
                let mut statement = self.query(sqlx::AssertSqlSafe(query));
                for entry in entry_chunk {
                    let sequence =
                        sequence_start
                            .checked_add(observation_offset)
                            .ok_or_else(|| {
                                StorageError::Conflict(
                                    "manifest observation sequence overflow".to_owned(),
                                )
                            })?;
                    observation_offset = observation_offset.saturating_add(1);
                    statement = statement
                        .bind(entry.relative_path.as_str())
                        .bind(sequence)
                        .bind(entry.entry_kind.as_str())
                        .bind(entry.size)
                        .bind(entry.modified_at)
                        .bind(entry.device)
                        .bind(entry.inode)
                        .bind(entry.fingerprint.as_slice());
                }
                statement = statement
                    .bind(chunk.manifest_id)
                    .bind(chunk.library_root_id);
                statement
                    .execute(&mut *transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
            } else {
                let selects = std::iter::repeat_n(
                    "SELECT ?, ?, ?,
                            (SELECT COALESCE(MAX(observation_sequence), 0) + 1
                             FROM scan_manifest_entries
                             WHERE manifest_id = ? AND library_root_id = ? AND relative_path = ?),
                            ?, ?, ?, ?, ?, ?, unixepoch()",
                    entry_chunk.len(),
                )
                .collect::<Vec<_>>()
                .join(" UNION ALL ");
                let query = format!(
                    "INSERT INTO scan_manifest_entries (
                         manifest_id, library_root_id, relative_path, observation_sequence,
                         entry_kind, size, modified_at, device, inode, fingerprint, observed_at
                     ) {selects}"
                );
                let mut statement = self.query(sqlx::AssertSqlSafe(query));
                for entry in entry_chunk {
                    statement = statement
                        .bind(chunk.manifest_id)
                        .bind(chunk.library_root_id)
                        .bind(entry.relative_path.as_str())
                        .bind(chunk.manifest_id)
                        .bind(chunk.library_root_id)
                        .bind(entry.relative_path.as_str())
                        .bind(entry.entry_kind.as_str())
                        .bind(entry.size)
                        .bind(entry.modified_at)
                        .bind(entry.device)
                        .bind(entry.inode)
                        .bind(entry.fingerprint.as_slice());
                }
                statement
                    .execute(&mut *transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
            }
        }
        record_manifest_storage_stage(
            "manifest_observation_insert",
            observation_started,
            observation_entries.len(),
            observation_entries
                .iter()
                .filter(|entry| entry.entry_kind == "FILE")
                .count(),
            observation_entries
                .iter()
                .filter(|entry| entry.entry_kind == "DIRECTORY")
                .count(),
        );

        if !matches!(workflow_version, 2 | 3) && !positive_indexes.is_empty() {
            return Err(StorageError::Conflict(
                "legacy manifest cannot receive streamed positive indexes".to_owned(),
            ));
        }
        let mut positive_result = ManifestDiscoveryPositiveIndexResult::default();
        let positive_index_started = Instant::now();
        if matches!(workflow_version, 2 | 3) && !positive_indexes.is_empty() {
            if job_status != "RUNNING" || cancel_requested != 0 {
                return Err(StorageError::Conflict(
                    "streamed indexing requires an active manifest scan job".to_owned(),
                ));
            }
            positive_result = self
                .apply_scan_manifest_discovery_positive_indexes_in_transaction(
                    &mut transaction,
                    ManifestDiscoveryPositiveIndexCommit {
                        job_id: chunk.job_id,
                        library_id: &library_id,
                        library_root_id: chunk.library_root_id,
                        generation: &generation,
                        positives: &positive_indexes,
                        observations: &entries,
                    },
                )
                .await?;
        }
        record_manifest_storage_stage(
            "positive_index_apply",
            positive_index_started,
            positive_indexes.len(),
            positive_indexes.len(),
            0,
        );

        let local_metadata_batches_changed =
            workflow_version == 3 && !positive_result.local_metadata_refs.is_empty();
        if local_metadata_batches_changed {
            let local_metadata_started = Instant::now();
            let sequence_start = local_metadata_sequence_start.ok_or_else(|| {
                StorageError::Conflict(
                    "workflow 3 local metadata references require reserved batch sequences".into(),
                )
            })?;
            self.enqueue_manifest_local_metadata_refs_in_transaction(
                &mut transaction,
                chunk.job_id,
                chunk.library_root_id,
                &positive_result.local_metadata_refs,
                sequence_start,
            )
            .await?;
            record_manifest_storage_stage(
                "local_metadata_outbox",
                local_metadata_started,
                positive_result.local_metadata_refs.len(),
                positive_result.local_metadata_refs.len(),
                0,
            );
        }

        let presence_ledger_started = Instant::now();
        let mut ledger_paths = Vec::new();
        let mut successfully_seen_paths = std::collections::HashSet::new();
        if discovery_format_version == 3 && !seen_filesystem_entries.is_empty() {
            for entries in seen_filesystem_entries
                .chunks(super::manifest_path_query_chunk_size(self.backend()))
            {
                if entries.is_empty() {
                    continue;
                }
                let values = std::iter::repeat_n("(?, ?, ?)", entries.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                let postgres_update_from = self.backend() == DatabaseBackend::Postgres;
                let query = if postgres_update_from {
                    format!(
                        "WITH incoming(filesystem_entry_id, relative_path, fingerprint) AS (VALUES {values})
                         UPDATE filesystem_entries AS current
                         SET last_seen_generation = ?,
                             last_seen_change_kind = NULL
                         FROM incoming
                         WHERE current.id = incoming.filesystem_entry_id
                           AND current.library_root_id = ?
                           AND current.relative_path = incoming.relative_path
                           AND current.fingerprint = incoming.fingerprint
                           AND current.entry_kind = 'FILE'
                           AND current.is_missing = 0
                           AND (current.last_seen_generation IS NULL
                                OR current.last_seen_generation <> ?
                                OR current.last_seen_change_kind IS NOT NULL)"
                    )
                } else {
                    format!(
                        "WITH incoming(filesystem_entry_id, relative_path, fingerprint) AS (VALUES {values}),
                              matching(filesystem_entry_id) AS (
                                  SELECT incoming.filesystem_entry_id
                                  FROM incoming
                                  JOIN filesystem_entries current
                                    ON current.id = incoming.filesystem_entry_id
                                   AND current.relative_path = incoming.relative_path
                                   AND current.fingerprint = incoming.fingerprint
                                  WHERE current.library_root_id = ?
                                    AND current.entry_kind = 'FILE'
                                    AND current.is_missing = 0
                              )
                         UPDATE filesystem_entries
                         SET last_seen_generation = ?,
                             last_seen_change_kind = NULL
                         WHERE id IN (SELECT filesystem_entry_id FROM matching)
                           AND (last_seen_generation IS NULL
                                OR last_seen_generation <> ?
                                OR last_seen_change_kind IS NOT NULL)"
                    )
                };
                let mut statement = self.query(sqlx::AssertSqlSafe(query));
                for entry in entries {
                    statement = statement
                        .bind(&entry.filesystem_entry_id)
                        .bind(&entry.relative_path)
                        .bind(&entry.fingerprint);
                }
                statement = if postgres_update_from {
                    statement
                        .bind(&generation)
                        .bind(chunk.library_root_id)
                        .bind(&generation)
                } else {
                    statement
                        .bind(chunk.library_root_id)
                        .bind(&generation)
                        .bind(&generation)
                };
                let updated = statement
                    .execute(&mut *transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                let updated_count = usize::try_from(updated.rows_affected()).unwrap_or(usize::MAX);
                if updated_count == entries.len() {
                    successfully_seen_paths
                        .extend(entries.iter().map(|entry| entry.relative_path.clone()));
                    continue;
                }

                let values = std::iter::repeat_n("(?, ?, ?)", entries.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                let query = format!(
                    "WITH incoming(filesystem_entry_id, relative_path, fingerprint) AS (VALUES {values})
                     SELECT incoming.relative_path
                     FROM incoming
                     JOIN filesystem_entries entry
                       ON entry.id = incoming.filesystem_entry_id
                      AND entry.relative_path = incoming.relative_path
                     WHERE entry.library_root_id = ?
                       AND entry.entry_kind = 'FILE'
                       AND entry.is_missing = 0
                       AND entry.last_seen_generation = ?
                       AND entry.fingerprint = incoming.fingerprint"
                );
                let mut statement = self.query(sqlx::AssertSqlSafe(query));
                for entry in entries {
                    statement = statement
                        .bind(&entry.filesystem_entry_id)
                        .bind(&entry.relative_path)
                        .bind(&entry.fingerprint);
                }
                let successful_rows = statement
                    .bind(chunk.library_root_id)
                    .bind(&generation)
                    .fetch_all(&mut *transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                successfully_seen_paths.extend(
                    successful_rows
                        .into_iter()
                        .map(|row| row.get::<String, _>("relative_path")),
                );
            }
        }
        if discovery_format_version == 3 {
            positive_result.indexed_paths.sort_unstable();
            ledger_paths = file_paths
                .iter()
                .copied()
                .filter(|path| {
                    positive_result.indexed_paths.binary_search(path).is_err()
                        && !successfully_seen_paths.contains(*path)
                })
                .collect::<Vec<_>>();
        }
        for paths in ledger_paths.chunks(super::manifest_path_query_chunk_size(self.backend())) {
            if paths.is_empty() {
                continue;
            }
            let values = std::iter::repeat_n("(?)", paths.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "WITH incoming_seen_paths(relative_path) AS (VALUES {values})
                 INSERT INTO scan_manifest_seen_paths (
                     manifest_id, library_root_id, relative_path
                 )
                 SELECT ?, ?, incoming_seen_paths.relative_path
                 FROM incoming_seen_paths WHERE TRUE
                 ON CONFLICT(manifest_id, library_root_id, relative_path) DO NOTHING"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for path in paths {
                statement = statement.bind(path);
            }
            statement = statement
                .bind(chunk.manifest_id)
                .bind(chunk.library_root_id);
            statement
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }
        record_manifest_storage_stage(
            "presence_ledger",
            presence_ledger_started,
            successfully_seen_paths
                .len()
                .saturating_add(ledger_paths.len()),
            successfully_seen_paths
                .len()
                .saturating_add(ledger_paths.len()),
            0,
        );

        let manifest_counters_started = Instant::now();
        self.query(
            "UPDATE scan_manifests
             SET discovered_directory_count = discovered_directory_count + ?,
                 observed_file_count = observed_file_count + ?,
                 completed_directory_count = completed_directory_count + ?,
                 unchanged_count = unchanged_count + ?, add_count = add_count + ?,
                 change_count = change_count + ?, reappeared_count = reappeared_count + ?,
                 applied_delta_count = applied_delta_count + ?,
                 updated_at = unixepoch()
             WHERE id = ? AND state = 'DISCOVERING'",
        )
        .bind(inserted_directory_count_i64)
        .bind(inserted_file_count_i64)
        .bind(completed_directory_count)
        .bind(
            i64::try_from(unchanged_file_count).map_err(|_| {
                StorageError::Conflict("manifest unchanged count overflow".to_owned())
            })?,
        )
        .bind(positive_result.add_count)
        .bind(positive_result.change_count)
        .bind(positive_result.reappeared_count)
        .bind(positive_result.applied_count)
        .bind(chunk.manifest_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;

        let accepting_discovery = self
            .query(
                "UPDATE scan_jobs
                 SET total_count = total_count + ?,
                     processed_count = processed_count + ?, updated_at = unixepoch()
                 WHERE id = ? AND status = 'RUNNING' AND cancel_requested = 0",
            )
            .bind(inserted_file_count_i64)
            .bind(if matches!(workflow_version, 2 | 3) {
                inserted_file_count_i64
            } else {
                0
            })
            .bind(chunk.job_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if accepting_discovery.rows_affected() != 1 {
            return Err(StorageError::Conflict(
                "scan job is no longer accepting manifest discovery chunks".to_owned(),
            ));
        }

        record_manifest_storage_stage(
            "manifest_counter_checkpoint",
            manifest_counters_started,
            inserted_file_count
                .saturating_add(completed_directories.len())
                .saturating_add(positive_indexes.len()),
            inserted_file_count,
            completed_directories.len(),
        );
        let transaction_commit_started = Instant::now();
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        record_manifest_storage_stage(
            "transaction_commit",
            transaction_commit_started,
            entries.len().saturating_add(child_directories.len()),
            file_paths.len(),
            child_directories.len(),
        );
        record_manifest_storage_stage(
            "transaction_total",
            transaction_total_started,
            entries.len().saturating_add(child_directories.len()),
            file_paths.len(),
            child_directories.len(),
        );
        Ok(ManifestDiscoveryCommitResult {
            observed_file_count: i64::try_from(inserted_file_count)
                .map_err(|_| StorageError::Conflict("manifest file count overflow".to_owned()))?,
            created_items: positive_result.created_items,
            metadata_targets_changed: positive_result.metadata_targets_changed,
            local_metadata_batches_changed,
        })
    }

    async fn apply_scan_manifest_discovery_positive_indexes_in_transaction<'a, 'b>(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        commit: ManifestDiscoveryPositiveIndexCommit<'a, 'b>,
    ) -> Result<ManifestDiscoveryPositiveIndexResult<'a>, StorageError> {
        let ManifestDiscoveryPositiveIndexCommit {
            job_id,
            library_id,
            library_root_id,
            generation,
            positives,
            observations,
        } = commit;
        if positives.is_empty() {
            return Ok(ManifestDiscoveryPositiveIndexResult::default());
        }

        let observations = observations
            .iter()
            .map(|observation| (observation.relative_path.as_str(), *observation))
            .collect::<HashMap<_, _>>();
        let mut add_filesystem_entries = Vec::new();
        for positive in positives
            .iter()
            .filter(|positive| positive.delta_kind == "ADD")
        {
            let observation = observations
                .get(positive.relative_path.as_str())
                .ok_or_else(|| {
                    StorageError::Conflict(
                        "manifest add is missing its filesystem observation".to_owned(),
                    )
                })?;
            let fingerprint = observation.fingerprint.as_slice();
            add_filesystem_entries.push(NewScanManifestFilesystemEntry {
                id: manifest_positive_file_filesystem_entry_id(&positive.file),
                relative_path: &positive.relative_path,
                size: observation.size,
                modified_at: observation.modified_at,
                inode: observation.inode,
                fingerprint,
                last_seen_change_kind: Some(
                    if matches!(&positive.file, NewScanManifestIndexedFile::Sidecar(_)) {
                        "SIDECAR"
                    } else {
                        "NEW"
                    },
                ),
            });
        }
        let add_filesystem_claim_started = Instant::now();
        let claimed_add_paths = self
            .claim_manifest_add_filesystem_entries_in_transaction(
                transaction,
                library_root_id,
                generation,
                &add_filesystem_entries,
            )
            .await?;
        record_manifest_storage_stage(
            "positive_add_filesystem_claim",
            add_filesystem_claim_started,
            add_filesystem_entries.len(),
            add_filesystem_entries.len(),
            0,
        );

        let claimed_movie_files = positives
            .iter()
            .filter(|positive| {
                positive.delta_kind == "ADD" && claimed_add_paths.contains(&positive.relative_path)
            })
            .filter_map(|positive| match &positive.file {
                NewScanManifestIndexedFile::Movie(file) => Some(file.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let claimed_episode_files = positives
            .iter()
            .filter(|positive| {
                positive.delta_kind == "ADD" && claimed_add_paths.contains(&positive.relative_path)
            })
            .filter_map(|positive| match &positive.file {
                NewScanManifestIndexedFile::Episode(file) => Some(file.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();

        let mut result = ManifestDiscoveryPositiveIndexResult::default();
        let movie_materialization_started = Instant::now();
        result.created_items = self
            .insert_movie_files_without_filesystem_entries_in_transaction(
                transaction,
                library_id,
                library_root_id,
                generation,
                &claimed_movie_files,
            )
            .await?;
        record_manifest_storage_stage(
            "positive_add_movie_materialization",
            movie_materialization_started,
            claimed_movie_files.len(),
            claimed_movie_files.len(),
            0,
        );
        let episode_materialization_started = Instant::now();
        result.created_items = result.created_items.saturating_add(
            self.insert_episode_files_without_filesystem_entries_in_transaction(
                transaction,
                library_id,
                library_root_id,
                generation,
                &claimed_episode_files,
            )
            .await?,
        );
        record_manifest_storage_stage(
            "positive_add_episode_materialization",
            episode_materialization_started,
            claimed_episode_files.len(),
            claimed_episode_files.len(),
            0,
        );

        let mut changed_sidecar_paths = Vec::new();
        let mut existing_updates = Vec::new();
        let positive_change_application_started = Instant::now();

        for positive in positives {
            match positive.delta_kind.as_str() {
                "ADD" => {
                    if !claimed_add_paths.contains(&positive.relative_path) {
                        continue;
                    }
                    match &positive.file {
                        NewScanManifestIndexedFile::Movie(_)
                        | NewScanManifestIndexedFile::Episode(_) => {}
                        NewScanManifestIndexedFile::Unresolved(file) => {
                            self.materialize_manifest_unresolved_file_after_filesystem_insert_in_transaction(
                                transaction,
                                library_id,
                                library_root_id,
                                file,
                            )
                            .await?;
                            result.created_items = result.created_items.saturating_add(1);
                        }
                        NewScanManifestIndexedFile::Sidecar(_) => {
                            changed_sidecar_paths.push(positive.relative_path.clone());
                        }
                    }
                    record_manifest_positive_applied(&mut result, positive);
                }
                "CHANGE" | "REAPPEARED" => {
                    let Some(filesystem_entry_id) = positive.base_filesystem_entry_id.as_deref()
                    else {
                        return Err(StorageError::Conflict(
                            "manifest changed file is missing its filesystem baseline".to_owned(),
                        ));
                    };
                    let observation = observations
                        .get(positive.relative_path.as_str())
                        .ok_or_else(|| {
                            StorageError::Conflict(
                                "manifest changed file is missing its observation".to_owned(),
                            )
                        })?;
                    let expected_missing = positive.delta_kind == "REAPPEARED";
                    match &positive.file {
                        NewScanManifestIndexedFile::Movie(file) => {
                            existing_updates.push(ManifestExistingFileUpdate {
                                filesystem_entry_id,
                                library_root_id,
                                relative_path: &positive.relative_path,
                                base_fingerprint: positive.base_fingerprint.as_deref(),
                                expected_missing,
                                size: observation.size,
                                modified_at: observation.modified_at,
                                inode: observation.inode,
                                fingerprint: observation.fingerprint.as_slice(),
                                generation,
                                last_seen_change_kind: Some("CHANGED"),
                                source_kind: &file.source_kind,
                                edition_name: file.edition_name.as_deref(),
                                quality_label: file.quality_label.as_deref(),
                                container: &file.container,
                                external_url: file.external_url.as_deref(),
                                strm_target_kind: file.strm_target_kind.as_deref(),
                            });
                        }
                        NewScanManifestIndexedFile::Episode(file) => {
                            existing_updates.push(ManifestExistingFileUpdate {
                                filesystem_entry_id,
                                library_root_id,
                                relative_path: &positive.relative_path,
                                base_fingerprint: positive.base_fingerprint.as_deref(),
                                expected_missing,
                                size: observation.size,
                                modified_at: observation.modified_at,
                                inode: observation.inode,
                                fingerprint: observation.fingerprint.as_slice(),
                                generation,
                                last_seen_change_kind: Some("CHANGED"),
                                source_kind: &file.source_kind,
                                edition_name: file.edition_name.as_deref(),
                                quality_label: file.quality_label.as_deref(),
                                container: &file.container,
                                external_url: file.external_url.as_deref(),
                                strm_target_kind: file.strm_target_kind.as_deref(),
                            });
                        }
                        NewScanManifestIndexedFile::Unresolved(file) => {
                            existing_updates.push(ManifestExistingFileUpdate {
                                filesystem_entry_id,
                                library_root_id,
                                relative_path: &positive.relative_path,
                                base_fingerprint: positive.base_fingerprint.as_deref(),
                                expected_missing,
                                size: observation.size,
                                modified_at: observation.modified_at,
                                inode: observation.inode,
                                fingerprint: observation.fingerprint.as_slice(),
                                generation,
                                last_seen_change_kind: Some("CHANGED"),
                                source_kind: &file.source_kind,
                                edition_name: None,
                                quality_label: None,
                                container: &file.container,
                                external_url: file.external_url.as_deref(),
                                strm_target_kind: file.strm_target_kind.as_deref(),
                            });
                        }
                        NewScanManifestIndexedFile::Sidecar(_) => {
                            let applied = self
                                .apply_manifest_existing_sidecar_in_transaction(
                                    transaction,
                                    library_root_id,
                                    generation,
                                    positive,
                                    observation,
                                    expected_missing,
                                )
                                .await?;
                            if applied {
                                changed_sidecar_paths.push(positive.relative_path.clone());
                                record_manifest_positive_applied(&mut result, positive);
                            }
                        }
                    }
                }
                _ => {
                    return Err(StorageError::Conflict(
                        "manifest positive index has an unknown delta kind".to_owned(),
                    ));
                }
            }
        }

        let applied_existing_ids = self
            .apply_manifest_existing_files_batch_in_transaction(transaction, &existing_updates)
            .await?;
        for positive in positives.iter().filter(|positive| {
            matches!(positive.delta_kind.as_str(), "CHANGE" | "REAPPEARED")
                && !matches!(&positive.file, NewScanManifestIndexedFile::Sidecar(_))
        }) {
            if positive
                .base_filesystem_entry_id
                .as_deref()
                .is_some_and(|id| applied_existing_ids.contains(id))
            {
                record_manifest_positive_applied(&mut result, positive);
            }
        }
        record_manifest_storage_stage(
            "positive_change_application",
            positive_change_application_started,
            positives.len(),
            positives.len(),
            0,
        );

        let sidecar_directories = prune_sidecar_directories(
            changed_sidecar_paths
                .iter()
                .filter_map(|path| {
                    Path::new(path)
                        .parent()
                        .and_then(|parent| parent.to_str())
                        .map(|parent| {
                            if parent.is_empty() {
                                ".".to_owned()
                            } else {
                                parent.to_owned()
                            }
                        })
                })
                .collect(),
        );
        let sidecar_target_registration_started = Instant::now();
        result.metadata_targets_changed |= self
            .record_scan_job_sidecar_targets_in_transaction(
                transaction,
                job_id,
                library_root_id,
                &sidecar_directories,
            )
            .await?;
        record_manifest_storage_stage(
            "positive_sidecar_target_registration",
            sidecar_target_registration_started,
            sidecar_directories.len(),
            0,
            sidecar_directories.len(),
        );
        Ok(result)
    }

    pub(crate) async fn list_scan_manifest_diff_candidates(
        &self,
        manifest_id: &str,
        after_library_root_id: Option<&str>,
        after_relative_path: Option<&str>,
        limit: i64,
    ) -> Result<Vec<StoredScanManifestDiffCandidate>, StorageError> {
        self.query(
            "SELECT observed.library_root_id, observed.relative_path,
                    observed.observation_sequence,
                    CASE WHEN fe.id IS NULL THEN 'ADD'
                         WHEN fe.is_missing = 1 THEN 'REAPPEARED'
                         ELSE 'CHANGE' END AS delta_kind,
                    fe.id AS base_filesystem_entry_id,
                    fe.fingerprint AS base_fingerprint,
                    fe.entry_kind AS base_entry_kind,
                    fe.is_missing AS base_is_missing
             FROM scan_manifest_entries observed
             JOIN scan_manifest_roots root
               ON root.manifest_id = observed.manifest_id
              AND root.library_root_id = observed.library_root_id
              AND root.state = 'COMPLETE'
             LEFT JOIN filesystem_entries fe
               ON fe.library_root_id = observed.library_root_id
              AND fe.relative_path = observed.relative_path
             WHERE observed.manifest_id = ?
               AND observed.entry_kind = 'FILE'
               AND (observed.library_root_id, observed.relative_path) > (?, ?)
               AND observed.observation_sequence = (
                   SELECT latest.observation_sequence
                   FROM scan_manifest_entries latest
                   WHERE latest.manifest_id = observed.manifest_id
                     AND latest.library_root_id = observed.library_root_id
                     AND latest.relative_path = observed.relative_path
                   ORDER BY latest.observation_sequence DESC
                   LIMIT 1
               )
               AND (fe.id IS NULL OR fe.is_missing = 1 OR fe.fingerprint IS NULL
                    OR observed.fingerprint IS NULL OR fe.fingerprint <> observed.fingerprint)
               AND NOT EXISTS (
                   SELECT 1 FROM scan_manifest_deltas delta
                   WHERE delta.manifest_id = observed.manifest_id
                     AND delta.library_root_id = observed.library_root_id
                     AND delta.relative_path = observed.relative_path
               )
             ORDER BY observed.library_root_id, observed.relative_path
             LIMIT ?",
        )
        .bind(manifest_id)
        .bind(after_library_root_id.unwrap_or_default())
        .bind(after_relative_path.unwrap_or_default())
        .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredScanManifestDiffCandidate {
                    library_root_id: row.get("library_root_id"),
                    relative_path: row.get("relative_path"),
                    observation_sequence: row.get("observation_sequence"),
                    delta_kind: row.get("delta_kind"),
                    base_filesystem_entry_id: row.get("base_filesystem_entry_id"),
                    base_fingerprint: row.get("base_fingerprint"),
                    base_entry_kind: row.get("base_entry_kind"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_scan_manifest_filesystem_baselines(
        &self,
        library_root_id: &str,
        relative_paths: &[String],
    ) -> Result<HashMap<String, StoredScanManifestFilesystemBaseline>, StorageError> {
        let mut baselines = HashMap::with_capacity(relative_paths.len());
        for paths in relative_paths.chunks(super::manifest_path_query_chunk_size(self.backend())) {
            if paths.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", paths.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT id, relative_path, entry_kind, fingerprint, is_missing
                 FROM filesystem_entries
                 WHERE library_root_id = ? AND relative_path IN ({placeholders})"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query)).bind(library_root_id);
            for path in paths {
                statement = statement.bind(path);
            }
            for row in
                statement
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?
            {
                let baseline = StoredScanManifestFilesystemBaseline {
                    id: row.get("id"),
                    entry_kind: row.get("entry_kind"),
                    fingerprint: row.get("fingerprint"),
                    is_missing: row.get::<i64, _>("is_missing") != 0,
                };
                baselines.insert(row.get("relative_path"), baseline);
            }
        }
        Ok(baselines)
    }

    pub(crate) async fn scan_manifest_movie_variant_identity_is_current(
        &self,
        library_root_id: &str,
        relative_path: &str,
        base_item_id: &str,
        edition_name: Option<&str>,
    ) -> Result<bool, StorageError> {
        self.query_scalar::<i64>(
            "SELECT 1
             FROM filesystem_entries entry
             JOIN media_sources source ON source.filesystem_entry_id = entry.id
             JOIN media_items item ON item.id = source.item_id
             WHERE entry.library_root_id = ? AND entry.relative_path = ?
               AND entry.entry_kind = 'FILE' AND entry.is_missing = 0
               AND item.removed_at IS NULL
               AND COALESCE(item.merged_into_item_id, item.id) = ?
               AND COALESCE(source.edition_name, '') = COALESCE(?, '')
             LIMIT 1",
        )
        .bind(library_root_id)
        .bind(relative_path)
        .bind(base_item_id)
        .bind(edition_name)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.is_some())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_scan_manifest_removal_candidates(
        &self,
        manifest_id: &str,
        discovery_format_version: i64,
        uncovered_only: bool,
        after_library_root_id: Option<&str>,
        after_relative_path: Option<&str>,
        limit: i64,
    ) -> Result<Vec<StoredScanManifestRemovalCandidate>, StorageError> {
        let unseen_path_predicate = if discovery_format_version == 3 {
            "COALESCE(fe.last_seen_generation, '') <> job.generation
             AND NOT EXISTS (
                 SELECT 1 FROM scan_manifest_seen_paths seen
                 WHERE seen.manifest_id = ?
                   AND seen.library_root_id = fe.library_root_id
                   AND seen.relative_path = fe.relative_path
             )"
        } else {
            "COALESCE((
                   SELECT observed.entry_kind
                   FROM scan_manifest_entries observed
                   WHERE observed.manifest_id = ?
                     AND observed.library_root_id = fe.library_root_id
                     AND observed.relative_path = fe.relative_path
                   ORDER BY observed.observation_sequence DESC
                   LIMIT 1
               ), '') <> 'FILE'"
        };
        let uncovered_predicate = if discovery_format_version == 3 && uncovered_only {
            "AND NOT EXISTS (
                    SELECT 1
                    FROM scan_manifest_directories directory
                    WHERE directory.manifest_id = root.manifest_id
                      AND directory.library_root_id = fe.library_root_id
                      AND directory.state = 'COMPLETE'
                      AND (
                          (directory.relative_path = ''
                           AND fe.relative_path NOT LIKE '%/%' ESCAPE '\\')
                          OR (directory.relative_path <> ''
                              AND fe.relative_path LIKE (
                                  REPLACE(
                                      REPLACE(
                                          REPLACE(directory.relative_path, '\\', '\\\\'),
                                          '%',
                                          '\\%'
                                      ),
                                      '_',
                                      '\\_'
                                  ) || '/%'
                              ) ESCAPE '\\'
                              AND fe.relative_path NOT LIKE (
                                  REPLACE(
                                      REPLACE(
                                          REPLACE(directory.relative_path, '\\', '\\\\'),
                                          '%',
                                          '\\%'
                                      ),
                                      '_',
                                      '\\_'
                                  ) || '/%/%'
                              ) ESCAPE '\\')
                      )
                )"
        } else {
            ""
        };
        let query = format!(
            "SELECT fe.library_root_id, fe.relative_path, fe.id AS base_filesystem_entry_id,
                    fe.fingerprint AS base_fingerprint
             FROM filesystem_entries fe
             JOIN scan_manifest_roots root
              ON root.manifest_id = ?
              AND root.library_root_id = fe.library_root_id
              AND root.state = 'COMPLETE'
             JOIN scan_manifests manifest ON manifest.id = root.manifest_id
             JOIN scan_jobs job ON job.id = manifest.job_id
             WHERE fe.entry_kind = 'FILE' AND fe.is_missing = 0
               AND (fe.library_root_id, fe.relative_path) > (?, ?)
               AND {unseen_path_predicate}
               {uncovered_predicate}
               AND NOT EXISTS (
                   SELECT 1 FROM scan_manifest_deltas delta
                   WHERE delta.manifest_id = ?
                     AND delta.library_root_id = fe.library_root_id
                     AND delta.relative_path = fe.relative_path
               )
            ORDER BY fe.library_root_id, fe.relative_path
            LIMIT ?"
        );
        let mut statement = self
            .query(sqlx::AssertSqlSafe(query))
            .bind(manifest_id)
            .bind(after_library_root_id.unwrap_or_default())
            .bind(after_relative_path.unwrap_or_default());
        statement = statement.bind(manifest_id).bind(manifest_id);
        statement
            .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
            .fetch_all(&self.pool)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(|row| StoredScanManifestRemovalCandidate {
                        library_root_id: row.get("library_root_id"),
                        relative_path: row.get("relative_path"),
                        base_filesystem_entry_id: row.get("base_filesystem_entry_id"),
                        base_fingerprint: row.get("base_fingerprint"),
                    })
                    .collect()
            })
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn finish_scan_manifest_diff(
        &self,
        manifest_id: &str,
        job_id: &str,
    ) -> Result<bool, StorageError> {
        let mut transaction = self.begin_scan_write_transaction().await?;
        let (workflow_version, discovery_format_version, discovery_mode): (i64, i64, String) = self
            .query_as(
                "SELECT workflow_version, discovery_format_version, discovery_mode
                 FROM scan_manifests WHERE id = ?",
            )
            .bind(manifest_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let remaining_changes: i64 = if is_lite_manifest_discovery(
            workflow_version,
            discovery_format_version,
            &discovery_mode,
        ) {
            self.query_scalar(
                "SELECT COUNT(*)
                 FROM filesystem_entries fe
                 JOIN scan_manifest_roots root
                   ON root.manifest_id = ?
                  AND root.library_root_id = fe.library_root_id
                  AND root.state = 'COMPLETE'
                 JOIN scan_manifests manifest ON manifest.id = root.manifest_id
                 JOIN scan_jobs job ON job.id = manifest.job_id
                 WHERE fe.entry_kind = 'FILE' AND fe.is_missing = 0
                   AND COALESCE(fe.last_seen_generation, '') <> job.generation
                   AND NOT EXISTS (
                       SELECT 1 FROM scan_manifest_seen_paths seen
                       WHERE seen.manifest_id = root.manifest_id
                         AND seen.library_root_id = fe.library_root_id
                         AND seen.relative_path = fe.relative_path
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM scan_manifest_deltas delta
                       WHERE delta.manifest_id = root.manifest_id
                         AND delta.library_root_id = fe.library_root_id
                         AND delta.relative_path = fe.relative_path
                   )",
            )
            .bind(manifest_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
        } else if matches!(workflow_version, 2 | 3) && discovery_format_version == 3 {
            self.query_scalar(
                "SELECT COUNT(*)
                 FROM filesystem_entries fe
                 JOIN scan_manifest_roots root
                   ON root.manifest_id = ?
                  AND root.library_root_id = fe.library_root_id
                  AND root.state = 'COMPLETE'
                 JOIN scan_manifests manifest ON manifest.id = root.manifest_id
                 JOIN scan_jobs job ON job.id = manifest.job_id
                 WHERE fe.entry_kind = 'FILE' AND fe.is_missing = 0
                   AND EXISTS (
                       SELECT 1
                       FROM scan_manifest_directories directory
                       WHERE directory.manifest_id = root.manifest_id
                         AND directory.library_root_id = fe.library_root_id
                         AND directory.state = 'COMPLETE'
                         AND (
                             (directory.relative_path = ''
                              AND fe.relative_path NOT LIKE '%/%' ESCAPE '\\')
                             OR (directory.relative_path <> ''
                                 AND fe.relative_path LIKE (
                                     REPLACE(
                                         REPLACE(
                                             REPLACE(directory.relative_path, '\\', '\\\\'),
                                             '%',
                                             '\\%'
                                         ),
                                         '_',
                                         '\\_'
                                     ) || '/%'
                                 ) ESCAPE '\\'
                                 AND fe.relative_path NOT LIKE (
                                     REPLACE(
                                         REPLACE(
                                             REPLACE(directory.relative_path, '\\', '\\\\'),
                                             '%',
                                             '\\%'
                                         ),
                                         '_',
                                         '\\_'
                                     ) || '/%/%'
                                 ) ESCAPE '\\')
                         )
                   )
                   AND COALESCE(fe.last_seen_generation, '') <> job.generation
                   AND NOT EXISTS (
                       SELECT 1 FROM scan_manifest_seen_paths seen
                       WHERE seen.manifest_id = ?
                         AND seen.library_root_id = fe.library_root_id
                         AND seen.relative_path = fe.relative_path
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM scan_manifest_deltas delta
                       WHERE delta.manifest_id = ?
                         AND delta.library_root_id = fe.library_root_id
                         AND delta.relative_path = fe.relative_path
                   )",
            )
            .bind(manifest_id)
            .bind(manifest_id)
            .bind(manifest_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
        } else if matches!(workflow_version, 2 | 3) {
            self.query_scalar(
                "SELECT COUNT(*)
                 FROM filesystem_entries fe
                 JOIN scan_manifest_roots root
                   ON root.manifest_id = ?
                  AND root.library_root_id = fe.library_root_id
                  AND root.state = 'COMPLETE'
                 WHERE fe.entry_kind = 'FILE' AND fe.is_missing = 0
                   AND COALESCE((
                       SELECT observed.entry_kind
                       FROM scan_manifest_entries observed
                       WHERE observed.manifest_id = ?
                         AND observed.library_root_id = fe.library_root_id
                         AND observed.relative_path = fe.relative_path
                       ORDER BY observed.observation_sequence DESC LIMIT 1
                   ), '') <> 'FILE'
                   AND NOT EXISTS (
                       SELECT 1 FROM scan_manifest_deltas delta
                       WHERE delta.manifest_id = ?
                         AND delta.library_root_id = fe.library_root_id
                         AND delta.relative_path = fe.relative_path
                   )",
            )
            .bind(manifest_id)
            .bind(manifest_id)
            .bind(manifest_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
        } else {
            self.query_scalar(
                "SELECT
                    (SELECT COUNT(*)
                     FROM scan_manifest_entries observed
                     JOIN scan_manifest_roots root
                       ON root.manifest_id = observed.manifest_id
                      AND root.library_root_id = observed.library_root_id
                      AND root.state = 'COMPLETE'
                     LEFT JOIN filesystem_entries fe
                       ON fe.library_root_id = observed.library_root_id
                      AND fe.relative_path = observed.relative_path
                     WHERE observed.manifest_id = ?
                       AND observed.entry_kind = 'FILE'
                       AND observed.observation_sequence = (
                           SELECT latest.observation_sequence
                           FROM scan_manifest_entries latest
                           WHERE latest.manifest_id = observed.manifest_id
                             AND latest.library_root_id = observed.library_root_id
                             AND latest.relative_path = observed.relative_path
                           ORDER BY latest.observation_sequence DESC
                           LIMIT 1
                       )
                       AND (fe.id IS NULL OR fe.is_missing = 1 OR fe.fingerprint IS NULL
                            OR observed.fingerprint IS NULL OR fe.fingerprint <> observed.fingerprint)
                       AND NOT EXISTS (
                           SELECT 1 FROM scan_manifest_deltas delta
                           WHERE delta.manifest_id = observed.manifest_id
                             AND delta.library_root_id = observed.library_root_id
                             AND delta.relative_path = observed.relative_path
                       ))
                    +
                    (SELECT COUNT(*)
                     FROM filesystem_entries fe
                     JOIN scan_manifest_roots root
                       ON root.manifest_id = ?
                      AND root.library_root_id = fe.library_root_id
                      AND root.state = 'COMPLETE'
                     WHERE fe.entry_kind = 'FILE' AND fe.is_missing = 0
                       AND COALESCE((
                           SELECT observed.entry_kind
                           FROM scan_manifest_entries observed
                           WHERE observed.manifest_id = ?
                             AND observed.library_root_id = fe.library_root_id
                             AND observed.relative_path = fe.relative_path
                           ORDER BY observed.observation_sequence DESC
                           LIMIT 1
                       ), '') <> 'FILE'
                       AND NOT EXISTS (
                           SELECT 1 FROM scan_manifest_deltas delta
                           WHERE delta.manifest_id = ?
                             AND delta.library_root_id = fe.library_root_id
                             AND delta.relative_path = fe.relative_path
                       ))",
            )
            .bind(manifest_id)
            .bind(manifest_id)
            .bind(manifest_id)
            .bind(manifest_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
        };
        if remaining_changes > 0 {
            return Ok(false);
        }
        let unchanged_count: i64 = if matches!(workflow_version, 2 | 3) {
            self.query_scalar("SELECT unchanged_count FROM scan_manifests WHERE id = ?")
                .bind(manifest_id)
                .fetch_one(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
        } else {
            self.query_scalar(
                "SELECT COUNT(*)
                 FROM scan_manifest_entries observed
                 JOIN scan_manifest_roots root
                   ON root.manifest_id = observed.manifest_id
                  AND root.library_root_id = observed.library_root_id
                  AND root.state = 'COMPLETE'
                 JOIN filesystem_entries fe
                   ON fe.library_root_id = observed.library_root_id
                  AND fe.relative_path = observed.relative_path
                  AND fe.entry_kind = 'FILE'
                  AND fe.is_missing = 0
                  AND fe.fingerprint = observed.fingerprint
                 WHERE observed.manifest_id = ?
                   AND observed.entry_kind = 'FILE'
                   AND observed.observation_sequence = (
                       SELECT latest.observation_sequence
                       FROM scan_manifest_entries latest
                       WHERE latest.manifest_id = observed.manifest_id
                         AND latest.library_root_id = observed.library_root_id
                         AND latest.relative_path = observed.relative_path
                       ORDER BY latest.observation_sequence DESC
                       LIMIT 1
                   )",
            )
            .bind(manifest_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
        };
        let manifest_update = self
            .query(
                "UPDATE scan_manifests
                 SET state = 'APPLYING', unchanged_count = ?, updated_at = unixepoch()
                 WHERE id = ? AND state = 'READY_TO_DIFF'",
            )
            .bind(unchanged_count)
            .bind(manifest_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if manifest_update.rows_affected() != 1 {
            return Ok(false);
        }
        let total_count: i64 = self
            .query_scalar(if matches!(workflow_version, 2 | 3) {
                "SELECT observed_file_count + remove_count FROM scan_manifests WHERE id = ?"
            } else {
                "SELECT unchanged_count + add_count + change_count + remove_count + reappeared_count
                 FROM scan_manifests WHERE id = ?"
            })
            .bind(manifest_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let processed_count = if matches!(workflow_version, 2 | 3) {
            self.query_scalar("SELECT processed_count FROM scan_jobs WHERE id = ?")
                .bind(job_id)
                .fetch_one(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
        } else {
            unchanged_count
        };
        let job_update = self
            .query(
                "UPDATE scan_jobs
                 SET processed_count = ?, total_count = ?, updated_at = unixepoch()
                 WHERE id = ? AND status = 'RUNNING' AND cancel_requested = 0",
            )
            .bind(processed_count)
            .bind(total_count)
            .bind(job_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if job_update.rows_affected() != 1 {
            return Err(StorageError::Conflict(
                "scan job stopped before manifest diff completion".to_owned(),
            ));
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

    pub(crate) async fn list_pending_scan_manifest_deltas(
        &self,
        manifest_id: &str,
        limit: i64,
    ) -> Result<Vec<StoredScanManifestDelta>, StorageError> {
        self.query(
            "SELECT delta.id, delta.library_root_id, delta.relative_path,
                    delta.observation_sequence, delta.delta_kind,
                    delta.base_filesystem_entry_id, delta.base_fingerprint,
                    observed.entry_kind, observed.size, observed.modified_at,
                    observed.device, observed.inode, observed.fingerprint
             FROM scan_manifest_deltas delta
             LEFT JOIN scan_manifest_entries observed
               ON observed.manifest_id = delta.manifest_id
              AND observed.library_root_id = delta.library_root_id
              AND observed.relative_path = delta.relative_path
              AND observed.observation_sequence = delta.observation_sequence
             WHERE delta.manifest_id = ? AND delta.state = 'PENDING'
             ORDER BY delta.library_root_id, delta.relative_path
             LIMIT ?",
        )
        .bind(manifest_id)
        .bind(limit.clamp(1, MAX_SCAN_MANIFEST_APPLY_BATCH_SIZE))
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredScanManifestDelta {
                    id: row.get("id"),
                    library_root_id: row.get("library_root_id"),
                    relative_path: row.get("relative_path"),
                    observation_sequence: row.get("observation_sequence"),
                    delta_kind: row.get("delta_kind"),
                    base_filesystem_entry_id: row.get("base_filesystem_entry_id"),
                    base_fingerprint: row.get("base_fingerprint"),
                    entry_kind: row.get("entry_kind"),
                    size: row.get("size"),
                    modified_at: row.get("modified_at"),
                    device: row.get("device"),
                    inode: row.get("inode"),
                    fingerprint: row.get("fingerprint"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn count_applied_scan_manifest_removals(
        &self,
        manifest_id: &str,
    ) -> Result<i64, StorageError> {
        self.query_scalar(
            "SELECT COUNT(*) FROM scan_manifest_deltas
             WHERE manifest_id = ? AND delta_kind = 'REMOVE' AND state = 'APPLIED'",
        )
        .bind(manifest_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn commit_scan_manifest_delta_batch(
        &self,
        batch: &ManifestDeltaBatchCommit<'_>,
    ) -> Result<ManifestDeltaBatchCommitResult, StorageError> {
        if batch.deltas.is_empty() {
            return Ok(ManifestDeltaBatchCommitResult::default());
        }
        if batch
            .deltas
            .iter()
            .any(|delta| delta.library_root_id != batch.library_root_id)
        {
            return Err(StorageError::Conflict(
                "manifest apply batch cannot span library roots".to_owned(),
            ));
        }
        let delta_ids = batch
            .deltas
            .iter()
            .map(|delta| delta.id.as_str())
            .collect::<std::collections::HashSet<_>>();
        if batch
            .unstable_delta_ids
            .iter()
            .any(|id| !delta_ids.contains(id.as_str()))
        {
            return Err(StorageError::Conflict(
                "manifest unstable set contains an entry outside the batch".to_owned(),
            ));
        }

        let mut transaction = self.begin_scan_write_transaction().await?;
        let active_job: i64 = self
            .query_scalar(
                "SELECT COUNT(*) FROM scan_jobs
                 WHERE id = ? AND status = 'RUNNING' AND cancel_requested = 0",
            )
            .bind(batch.job_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let manifest_state: Option<String> = self
            .query_scalar("SELECT state FROM scan_manifests WHERE id = ? AND job_id = ?")
            .bind(batch.manifest_id)
            .bind(batch.job_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if active_job != 1 || manifest_state.as_deref() != Some("APPLYING") {
            return Err(StorageError::Conflict(
                "manifest batch requires an active applying scan".to_owned(),
            ));
        }
        let root_state: Option<String> = self
            .query_scalar(
                "SELECT state FROM scan_manifest_roots
                 WHERE manifest_id = ? AND library_root_id = ?",
            )
            .bind(batch.manifest_id)
            .bind(batch.library_root_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;

        let movie_files = batch
            .movie_files
            .iter()
            .map(|file| (file.relative_path.as_str(), file))
            .collect::<HashMap<_, _>>();
        let episode_files = batch
            .episode_files
            .iter()
            .map(|file| (file.relative_path.as_str(), file))
            .collect::<HashMap<_, _>>();
        let unresolved_files = batch
            .unresolved_files
            .iter()
            .map(|file| (file.relative_path.as_str(), file))
            .collect::<HashMap<_, _>>();
        let sidecar_entries = batch
            .sidecar_entries
            .iter()
            .map(|entry| (entry.relative_path.as_str(), entry))
            .collect::<HashMap<_, _>>();
        let unstable_ids = batch
            .unstable_delta_ids
            .iter()
            .map(String::as_str)
            .collect::<std::collections::HashSet<_>>();

        let mut add_filesystem_entries = Vec::new();
        if root_state.as_deref() == Some("COMPLETE") {
            for delta in batch.deltas.iter().filter(|delta| {
                delta.delta_kind == "ADD" && !unstable_ids.contains(delta.id.as_str())
            }) {
                if delta.observation_sequence.is_none() {
                    return Err(StorageError::Conflict(
                        "add delta is missing its observation".to_owned(),
                    ));
                }
                let entry_kind = delta.entry_kind.as_deref().ok_or_else(|| {
                    StorageError::Conflict(
                        "manifest delta observation is missing its entry kind".to_owned(),
                    )
                })?;
                let size = delta.size.ok_or_else(|| {
                    StorageError::Conflict(
                        "manifest delta observation is missing its size".to_owned(),
                    )
                })?;
                let modified_at = delta.modified_at.ok_or_else(|| {
                    StorageError::Conflict(
                        "manifest delta observation is missing its modified time".to_owned(),
                    )
                })?;
                let fingerprint = delta.fingerprint.as_deref().ok_or_else(|| {
                    StorageError::Conflict(
                        "manifest delta observation is missing its fingerprint".to_owned(),
                    )
                })?;
                if entry_kind != "FILE" || delta.base_filesystem_entry_id.is_some() {
                    return Err(StorageError::Conflict(
                        "add delta has an invalid observation or baseline".to_owned(),
                    ));
                }
                let filesystem_entry_id = movie_files
                    .get(delta.relative_path.as_str())
                    .map(|file| file.filesystem_entry_id.as_str())
                    .or_else(|| {
                        episode_files
                            .get(delta.relative_path.as_str())
                            .map(|file| file.filesystem_entry_id.as_str())
                    })
                    .or_else(|| {
                        unresolved_files
                            .get(delta.relative_path.as_str())
                            .map(|file| file.filesystem_entry_id.as_str())
                    })
                    .or_else(|| {
                        sidecar_entries
                            .get(delta.relative_path.as_str())
                            .map(|entry| entry.filesystem_entry_id.as_str())
                    })
                    .ok_or_else(|| {
                        StorageError::Conflict(
                            "manifest add delta has no prepared filesystem record".to_owned(),
                        )
                    })?;
                add_filesystem_entries.push(NewScanManifestFilesystemEntry {
                    id: filesystem_entry_id,
                    relative_path: &delta.relative_path,
                    size,
                    modified_at,
                    inode: delta.inode,
                    fingerprint,
                    last_seen_change_kind: None,
                });
            }
        }
        let claimed_add_paths = self
            .claim_manifest_add_filesystem_entries_in_transaction(
                &mut transaction,
                batch.library_root_id,
                batch.generation,
                &add_filesystem_entries,
            )
            .await?;
        let claimed_movie_files = batch
            .movie_files
            .iter()
            .filter(|file| claimed_add_paths.contains(&file.relative_path))
            .cloned()
            .collect::<Vec<_>>();
        let claimed_episode_files = batch
            .episode_files
            .iter()
            .filter(|file| claimed_add_paths.contains(&file.relative_path))
            .cloned()
            .collect::<Vec<_>>();

        let mut result = ManifestDeltaBatchCommitResult::default();
        result.created_items = self
            .insert_movie_files_without_filesystem_entries_in_transaction(
                &mut transaction,
                batch.library_id,
                batch.library_root_id,
                batch.generation,
                &claimed_movie_files,
            )
            .await?;
        result.created_items = result.created_items.saturating_add(
            self.insert_episode_files_without_filesystem_entries_in_transaction(
                &mut transaction,
                batch.library_id,
                batch.library_root_id,
                batch.generation,
                &claimed_episode_files,
            )
            .await?,
        );
        let mut new_paths = Vec::new();
        let mut changed_paths = Vec::new();
        let mut removed_media_paths = Vec::new();
        let mut changed_sidecar_paths = Vec::new();
        let mut removed_sidecar_paths = Vec::new();
        let mut removed_media_entry_ids = Vec::new();
        let mut delta_updates = Vec::with_capacity(batch.deltas.len());
        for delta in batch.deltas {
            let mut state = "APPLIED";
            let mut error = None;
            let force_unstable = unstable_ids.contains(delta.id.as_str())
                || root_state.as_deref() != Some("COMPLETE");
            if force_unstable {
                state = "UNSTABLE";
                error = Some(if root_state.as_deref() == Some("COMPLETE") {
                    "filesystem observation changed or became unavailable before apply"
                } else {
                    "library root is no longer complete and available"
                });
                result.unstable_count = result.unstable_count.saturating_add(1);
            } else {
                let expected_baseline = delta.base_filesystem_entry_id.as_deref();
                let expected_fingerprint = delta.base_fingerprint.as_deref();
                let expected_missing = delta.delta_kind == "REAPPEARED";
                let observation = delta
                    .observation_sequence
                    .map(|_| {
                        Ok((
                            delta.entry_kind.as_deref().ok_or_else(|| {
                                StorageError::Conflict(
                                    "manifest delta observation is missing its entry kind"
                                        .to_owned(),
                                )
                            })?,
                            delta.size.ok_or_else(|| {
                                StorageError::Conflict(
                                    "manifest delta observation is missing its size".to_owned(),
                                )
                            })?,
                            delta.modified_at.ok_or_else(|| {
                                StorageError::Conflict(
                                    "manifest delta observation is missing its modified time"
                                        .to_owned(),
                                )
                            })?,
                            delta.inode,
                            delta.fingerprint.as_deref().ok_or_else(|| {
                                StorageError::Conflict(
                                    "manifest delta observation is missing its fingerprint"
                                        .to_owned(),
                                )
                            })?,
                        ))
                    })
                    .transpose()?;

                let applied = match delta.delta_kind.as_str() {
                    "ADD" => {
                        let Some((entry_kind, _, _, _, _)) = observation else {
                            return Err(StorageError::Conflict(
                                "add delta is missing its observation".to_owned(),
                            ));
                        };
                        if entry_kind != "FILE" || expected_baseline.is_some() {
                            return Err(StorageError::Conflict(
                                "add delta has an invalid observation or baseline".to_owned(),
                            ));
                        }
                        if !claimed_add_paths.contains(&delta.relative_path) {
                            false
                        } else if movie_files.contains_key(delta.relative_path.as_str())
                            || episode_files.contains_key(delta.relative_path.as_str())
                        {
                            new_paths.push(delta.relative_path.clone());
                            true
                        } else if let Some(file) =
                            unresolved_files.get(delta.relative_path.as_str())
                        {
                            self.materialize_manifest_unresolved_file_after_filesystem_insert_in_transaction(
                                &mut transaction,
                                batch.library_id,
                                batch.library_root_id,
                                file,
                            )
                            .await?;
                            result.created_items = result.created_items.saturating_add(1);
                            new_paths.push(delta.relative_path.clone());
                            true
                        } else if sidecar_entries.contains_key(delta.relative_path.as_str()) {
                            changed_sidecar_paths.push(delta.relative_path.clone());
                            true
                        } else {
                            return Err(StorageError::Conflict(
                                "manifest add delta has no prepared filesystem record".to_owned(),
                            ));
                        }
                    }
                    "CHANGE" | "REAPPEARED" => {
                        let Some(filesystem_entry_id) = expected_baseline else {
                            return Err(StorageError::Conflict(
                                "change delta has no baseline entry".to_owned(),
                            ));
                        };
                        let Some((entry_kind, size, modified_at, inode, fingerprint)) = observation
                        else {
                            return Err(StorageError::Conflict(
                                "change delta is missing its observation".to_owned(),
                            ));
                        };
                        if entry_kind != "FILE" {
                            return Err(StorageError::Conflict(
                                "change delta observation is not a file".to_owned(),
                            ));
                        }
                        let applied = if let Some(file) =
                            movie_files.get(delta.relative_path.as_str())
                        {
                            self.apply_manifest_existing_file_in_transaction(
                                &mut transaction,
                                ManifestExistingFileUpdate {
                                    filesystem_entry_id,
                                    library_root_id: batch.library_root_id,
                                    relative_path: &delta.relative_path,
                                    base_fingerprint: expected_fingerprint,
                                    expected_missing,
                                    size,
                                    modified_at,
                                    inode,
                                    fingerprint,
                                    generation: batch.generation,
                                    last_seen_change_kind: None,
                                    source_kind: &file.source_kind,
                                    edition_name: file.edition_name.as_deref(),
                                    quality_label: file.quality_label.as_deref(),
                                    container: &file.container,
                                    external_url: file.external_url.as_deref(),
                                    strm_target_kind: file.strm_target_kind.as_deref(),
                                },
                            )
                            .await?
                        } else if let Some(file) = episode_files.get(delta.relative_path.as_str()) {
                            self.apply_manifest_existing_file_in_transaction(
                                &mut transaction,
                                ManifestExistingFileUpdate {
                                    filesystem_entry_id,
                                    library_root_id: batch.library_root_id,
                                    relative_path: &delta.relative_path,
                                    base_fingerprint: expected_fingerprint,
                                    expected_missing,
                                    size,
                                    modified_at,
                                    inode,
                                    fingerprint,
                                    generation: batch.generation,
                                    last_seen_change_kind: None,
                                    source_kind: &file.source_kind,
                                    edition_name: file.edition_name.as_deref(),
                                    quality_label: file.quality_label.as_deref(),
                                    container: &file.container,
                                    external_url: file.external_url.as_deref(),
                                    strm_target_kind: file.strm_target_kind.as_deref(),
                                },
                            )
                            .await?
                        } else if let Some(file) =
                            unresolved_files.get(delta.relative_path.as_str())
                        {
                            self.apply_manifest_existing_file_in_transaction(
                                &mut transaction,
                                ManifestExistingFileUpdate {
                                    filesystem_entry_id,
                                    library_root_id: batch.library_root_id,
                                    relative_path: &delta.relative_path,
                                    base_fingerprint: expected_fingerprint,
                                    expected_missing,
                                    size,
                                    modified_at,
                                    inode,
                                    fingerprint,
                                    generation: batch.generation,
                                    last_seen_change_kind: None,
                                    source_kind: &file.source_kind,
                                    edition_name: None,
                                    quality_label: None,
                                    container: &file.container,
                                    external_url: file.external_url.as_deref(),
                                    strm_target_kind: file.strm_target_kind.as_deref(),
                                },
                            )
                            .await?
                        } else {
                            self.query(
                                "UPDATE filesystem_entries
                                 SET size = ?, modified_at = ?, inode = ?, fingerprint = ?,
                                     last_seen_generation = ?, is_missing = 0,
                                     updated_at = unixepoch()
                                 WHERE id = ? AND library_root_id = ? AND relative_path = ?
                                   AND entry_kind = 'FILE' AND is_missing = ?
                                   AND (fingerprint = ? OR (fingerprint IS NULL AND ? IS NULL))",
                            )
                            .bind(size)
                            .bind(modified_at)
                            .bind(inode)
                            .bind(fingerprint)
                            .bind(batch.generation)
                            .bind(filesystem_entry_id)
                            .bind(batch.library_root_id)
                            .bind(&delta.relative_path)
                            .bind(database_flag(expected_missing))
                            .bind(expected_fingerprint)
                            .bind(expected_fingerprint)
                            .execute(&mut *transaction)
                            .await
                            .map_err(|source| StorageError::Sqlx {
                                path: self.path.clone(),
                                source,
                            })?
                            .rows_affected()
                                == 1
                        };
                        if applied {
                            if movie_files.contains_key(delta.relative_path.as_str())
                                || episode_files.contains_key(delta.relative_path.as_str())
                                || unresolved_files.contains_key(delta.relative_path.as_str())
                            {
                                changed_paths.push(delta.relative_path.clone());
                            } else {
                                changed_sidecar_paths.push(delta.relative_path.clone());
                            }
                        }
                        applied
                    }
                    "REMOVE" => {
                        let Some(filesystem_entry_id) = expected_baseline else {
                            return Err(StorageError::Conflict(
                                "remove delta has no baseline entry".to_owned(),
                            ));
                        };
                        let updated = self
                            .query(
                                "UPDATE filesystem_entries
                                 SET is_missing = 1, updated_at = unixepoch()
                                 WHERE id = ? AND library_root_id = ? AND relative_path = ?
                                   AND entry_kind = 'FILE' AND is_missing = 0
                                   AND (fingerprint = ? OR (fingerprint IS NULL AND ? IS NULL))",
                            )
                            .bind(filesystem_entry_id)
                            .bind(batch.library_root_id)
                            .bind(&delta.relative_path)
                            .bind(expected_fingerprint)
                            .bind(expected_fingerprint)
                            .execute(&mut *transaction)
                            .await
                            .map_err(|source| StorageError::Sqlx {
                                path: self.path.clone(),
                                source,
                            })?
                            .rows_affected()
                            == 1;
                        if updated {
                            if batch.removed_media_paths.contains(&delta.relative_path) {
                                removed_media_paths.push(delta.relative_path.clone());
                                removed_media_entry_ids.push(filesystem_entry_id.to_owned());
                            }
                            if batch.removed_sidecar_paths.contains(&delta.relative_path) {
                                removed_sidecar_paths.push(delta.relative_path.clone());
                            }
                            result.removed_count = result.removed_count.saturating_add(1);
                        }
                        updated
                    }
                    _ => {
                        return Err(StorageError::Conflict(
                            "manifest delta has an unknown kind".to_owned(),
                        ));
                    }
                };
                if applied {
                    result.applied_count = result.applied_count.saturating_add(1);
                } else {
                    state = "CONFLICT";
                    error = Some("filesystem entry changed after manifest diff was computed");
                    result.conflict_count = result.conflict_count.saturating_add(1);
                }
            }

            delta_updates.push((delta.id.as_str(), state, error));
        }

        for chunk in delta_updates.chunks(SCAN_MANIFEST_DELTA_BATCH_SIZE) {
            let state_cases = std::iter::repeat_n("WHEN ? THEN ?", chunk.len())
                .collect::<Vec<_>>()
                .join(" ");
            let error_cases = state_cases.clone();
            let ids = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "UPDATE scan_manifest_deltas
                 SET state = CASE id {state_cases} ELSE state END,
                     error = CASE id {error_cases} ELSE error END,
                     attempt_count = attempt_count + 1,
                     updated_at = unixepoch()
                 WHERE manifest_id = ? AND state = 'PENDING' AND id IN ({ids})"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for (id, state, _) in chunk {
                statement = statement.bind(id).bind(state);
            }
            for (id, _, error) in chunk {
                statement = statement.bind(id).bind(error);
            }
            statement = statement.bind(batch.manifest_id);
            for (id, _, _) in chunk {
                statement = statement.bind(id);
            }
            let updated = statement
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if usize::try_from(updated.rows_affected()).ok() != Some(chunk.len()) {
                return Err(StorageError::Conflict(
                    "manifest delta changed before its batch commit".to_owned(),
                ));
            }
        }

        let manifest_path_chunk_size = super::manifest_path_query_chunk_size(self.backend());
        for paths in new_paths.chunks(manifest_path_chunk_size) {
            result.metadata_targets_changed |= self
                .record_scan_job_targets_in_transaction(
                    &mut transaction,
                    batch.job_id,
                    batch.library_root_id,
                    paths,
                    "NEW",
                )
                .await?;
        }
        for paths in changed_paths.chunks(manifest_path_chunk_size) {
            result.metadata_targets_changed |= self
                .record_scan_job_targets_in_transaction(
                    &mut transaction,
                    batch.job_id,
                    batch.library_root_id,
                    paths,
                    "CHANGED",
                )
                .await?;
        }
        for paths in [&changed_sidecar_paths, &removed_sidecar_paths] {
            let directories = prune_sidecar_directories(
                paths
                    .iter()
                    .filter_map(|path| {
                        Path::new(path)
                            .parent()
                            .and_then(|parent| parent.to_str())
                            .map(|parent| {
                                if parent.is_empty() {
                                    ".".to_owned()
                                } else {
                                    parent.to_owned()
                                }
                            })
                    })
                    .collect(),
            );
            result.metadata_targets_changed |= self
                .record_scan_job_sidecar_targets_in_transaction(
                    &mut transaction,
                    batch.job_id,
                    batch.library_root_id,
                    &directories,
                )
                .await?;
        }
        self.record_scan_job_removed_targets_for_entry_ids_in_transaction(
            &mut transaction,
            batch.job_id,
            &removed_media_entry_ids,
        )
        .await?;
        if !removed_media_paths.is_empty() {
            self.refresh_removed_media_items_in_transaction(
                &mut transaction,
                batch.library_root_id,
            )
            .await?;
        }
        let processed_count = result
            .applied_count
            .saturating_add(result.conflict_count)
            .saturating_add(result.unstable_count);
        if processed_count > 0 {
            let processed_count_i64 = i64::try_from(processed_count).map_err(|_| {
                StorageError::Conflict("manifest progress count overflow".to_owned())
            })?;
            self.query(
                "UPDATE scan_manifests
                 SET applied_delta_count = applied_delta_count + ?, updated_at = unixepoch()
                 WHERE id = ? AND state = 'APPLYING'",
            )
            .bind(
                i64::try_from(result.applied_count).map_err(|_| {
                    StorageError::Conflict("manifest apply count overflow".to_owned())
                })?,
            )
            .bind(batch.manifest_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
            let update = self
                .query(
                    "UPDATE scan_jobs
                     SET processed_count = processed_count + ?, updated_at = unixepoch()
                     WHERE id = ? AND status = 'RUNNING' AND cancel_requested = 0",
                )
                .bind(processed_count_i64)
                .bind(batch.job_id)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if update.rows_affected() != 1 {
                return Err(StorageError::Conflict(
                    "scan job stopped during manifest delta apply".to_owned(),
                ));
            }
        }
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result)
    }

    #[allow(dead_code)] // LUX-267 writes the computed manifest delta set through this boundary.
    pub(crate) async fn insert_scan_manifest_deltas(
        &self,
        manifest_id: &str,
        deltas: &[NewScanManifestDelta<'_>],
    ) -> Result<usize, StorageError> {
        if deltas.is_empty() {
            return Ok(0);
        }

        let mut unique_paths = std::collections::HashSet::with_capacity(deltas.len());
        for delta in deltas {
            if !unique_paths.insert((delta.library_root_id, delta.relative_path)) {
                return Err(StorageError::Conflict(
                    "manifest delta batch contains duplicate root paths".to_owned(),
                ));
            }
            let valid_delta = match delta.delta_kind {
                "ADD" => {
                    delta.observation_sequence.is_some()
                        && delta.base_filesystem_entry_id.is_none()
                        && delta.base_fingerprint.is_none()
                }
                "CHANGE" | "REAPPEARED" => {
                    delta.observation_sequence.is_some() && delta.base_filesystem_entry_id.is_some()
                }
                "REMOVE" => {
                    delta.observation_sequence.is_none() && delta.base_filesystem_entry_id.is_some()
                }
                _ => false,
            };
            if !valid_delta {
                return Err(StorageError::Conflict(
                    "manifest delta payload does not match its kind".to_owned(),
                ));
            }
        }

        let mut inserted_count = 0_usize;
        for transaction_batch in deltas.chunks(SCAN_MANIFEST_DIFF_TRANSACTION_BATCH_SIZE) {
            let mut transaction = self.begin_scan_write_transaction().await?;
            let locked: Option<String> = self
                .query_scalar(
                    "UPDATE scan_manifests SET updated_at = updated_at
                     WHERE id = ? AND state = 'READY_TO_DIFF'
                     RETURNING state",
                )
                .bind(manifest_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if locked.as_deref() != Some("READY_TO_DIFF") {
                return Err(StorageError::Conflict(
                    "manifest is not ready to accept difference rows".to_owned(),
                ));
            }

            let mut batch_counts = [0_i64; 4];
            let mut batch_inserted_count = 0_usize;
            for chunk in transaction_batch.chunks(SCAN_MANIFEST_DELTA_BATCH_SIZE) {
                let tuple = "(?, ?, ?, ?, ?, ?, ?)";
                let values = std::iter::repeat_n(tuple, chunk.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                let verify_query = format!(
                    "WITH incoming (
                     id, library_root_id, relative_path, observation_sequence,
                     delta_kind, base_filesystem_entry_id, base_fingerprint
                 ) AS (VALUES {values})
                 SELECT stored.id
                 FROM incoming
                 JOIN scan_manifest_deltas stored
                   ON stored.manifest_id = ?
                  AND stored.library_root_id = incoming.library_root_id
                  AND stored.relative_path = incoming.relative_path
                 WHERE stored.id <> incoming.id
                    OR NOT (
                        stored.observation_sequence = incoming.observation_sequence
                        OR (stored.observation_sequence IS NULL
                            AND incoming.observation_sequence IS NULL)
                    )
                    OR stored.delta_kind <> incoming.delta_kind
                    OR NOT (
                        stored.base_filesystem_entry_id = incoming.base_filesystem_entry_id
                        OR (stored.base_filesystem_entry_id IS NULL
                            AND incoming.base_filesystem_entry_id IS NULL)
                    )
                    OR NOT (
                        stored.base_fingerprint = incoming.base_fingerprint
                        OR (stored.base_fingerprint IS NULL
                            AND incoming.base_fingerprint IS NULL)
                    )
                 LIMIT 1"
                );
                let mut verify = self.query_scalar(sqlx::AssertSqlSafe(verify_query));
                for delta in chunk {
                    verify = verify
                        .bind(delta.id)
                        .bind(delta.library_root_id)
                        .bind(delta.relative_path)
                        .bind(delta.observation_sequence)
                        .bind(delta.delta_kind)
                        .bind(delta.base_filesystem_entry_id)
                        .bind(delta.base_fingerprint);
                }
                let conflicting_row: Option<String> = verify
                    .bind(manifest_id)
                    .fetch_optional(&mut *transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                if conflicting_row.is_some() {
                    return Err(StorageError::Conflict(
                        "manifest delta retry disagrees with its persisted baseline".to_owned(),
                    ));
                }

                let insert_values = std::iter::repeat_n("(?, ?, ?, ?, ?, ?, ?, ?)", chunk.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                let insert_query = format!(
                    "INSERT INTO scan_manifest_deltas (
                     id, manifest_id, library_root_id, relative_path,
                     observation_sequence, delta_kind, base_filesystem_entry_id, base_fingerprint
                 ) VALUES {insert_values}
                 ON CONFLICT(manifest_id, library_root_id, relative_path) DO NOTHING
                 RETURNING delta_kind"
                );
                let mut insert = self.query(sqlx::AssertSqlSafe(insert_query));
                for delta in chunk {
                    insert = insert
                        .bind(delta.id)
                        .bind(manifest_id)
                        .bind(delta.library_root_id)
                        .bind(delta.relative_path)
                        .bind(delta.observation_sequence)
                        .bind(delta.delta_kind)
                        .bind(delta.base_filesystem_entry_id)
                        .bind(delta.base_fingerprint);
                }
                let inserted_kinds: Vec<String> = insert
                    .fetch_all(&mut *transaction)
                    .await
                    .map(|rows| rows.into_iter().map(|row| row.get("delta_kind")).collect())
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                for kind in &inserted_kinds {
                    let index = match kind.as_str() {
                        "ADD" => 0,
                        "CHANGE" => 1,
                        "REMOVE" => 2,
                        "REAPPEARED" => 3,
                        _ => {
                            return Err(StorageError::Conflict(
                                "database returned an unknown manifest delta kind".to_owned(),
                            ));
                        }
                    };
                    batch_counts[index] += 1;
                }
                batch_inserted_count = batch_inserted_count
                    .checked_add(inserted_kinds.len())
                    .ok_or_else(|| {
                        StorageError::Conflict("manifest delta count overflow".to_owned())
                    })?;
            }
            if batch_inserted_count > 0 {
                self.query(
                    "UPDATE scan_manifests
                 SET add_count = add_count + ?, change_count = change_count + ?,
                     remove_count = remove_count + ?, reappeared_count = reappeared_count + ?,
                     updated_at = unixepoch()
                 WHERE id = ? AND state = 'READY_TO_DIFF'",
                )
                .bind(batch_counts[0])
                .bind(batch_counts[1])
                .bind(batch_counts[2])
                .bind(batch_counts[3])
                .bind(manifest_id)
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
            inserted_count = inserted_count
                .checked_add(batch_inserted_count)
                .ok_or_else(|| {
                    StorageError::Conflict("manifest delta count overflow".to_owned())
                })?;
        }
        Ok(inserted_count)
    }

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
}

#[cfg(test)]
#[path = "jobs/tests.rs"]
mod tests;
