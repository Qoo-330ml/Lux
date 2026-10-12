use super::*;

struct ManifestPostprocessingTargetRange<'a> {
    job_id: &'a str,
    library_root_id: &'a str,
    generation: &'a str,
    target_stage: &'a str,
    after_relative_path: Option<&'a str>,
    through_relative_path: Option<&'a str>,
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
}
