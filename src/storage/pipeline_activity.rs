use super::*;

pub(crate) const MAX_LIBRARY_PIPELINE_ACTIVITY_PAGE_SIZE: i64 = 50;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StoredPipelineStageKind {
    Index,
    LocalMetadata,
    Probe,
    Thumbnail,
    AutomaticMetadata,
}

#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct StoredPipelineStatusCounts {
    pub(crate) queued: i64,
    pub(crate) running: i64,
    pub(crate) completed: i64,
    pub(crate) failed: i64,
    pub(crate) cancelled: i64,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct StoredPipelineStageActivity {
    pub(crate) kind: StoredPipelineStageKind,
    pub(crate) planned: bool,
    pub(crate) counts: StoredPipelineStatusCounts,
    pub(crate) recent_activity_at: Option<i64>,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct StoredLibraryPipelineActivity {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) stages: Vec<StoredPipelineStageActivity>,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct StoredLibraryPipelineActivityPage {
    pub(crate) page: i64,
    pub(crate) page_size: i64,
    pub(crate) total_libraries: i64,
    pub(crate) libraries: Vec<StoredLibraryPipelineActivity>,
}

impl Database {
    pub(crate) async fn list_library_pipeline_activity(
        &self,
        page: i64,
        page_size: i64,
    ) -> Result<StoredLibraryPipelineActivityPage, StorageError> {
        if page < 1 || !(1..=MAX_LIBRARY_PIPELINE_ACTIVITY_PAGE_SIZE).contains(&page_size) {
            return Err(StorageError::Conflict(
                "invalid library pipeline activity page".to_owned(),
            ));
        }
        let offset = page
            .checked_sub(1)
            .and_then(|page_index| page_index.checked_mul(page_size))
            .ok_or_else(|| {
                StorageError::Conflict("library pipeline activity page overflow".to_owned())
            })?;

        let total_libraries = self
            .query_scalar("SELECT COUNT(*) FROM libraries WHERE is_enabled = 1")
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;

        let rows = self
            .query(
                "WITH selected_libraries AS (
                     SELECT id, name
                     FROM libraries
                     WHERE is_enabled = 1
                     ORDER BY lower(name), id
                     LIMIT ? OFFSET ?
                 ),
                 stage_work AS (
                     SELECT job.library_id, 'INDEX' AS stage,
                            CASE job.status
                                WHEN 'PENDING' THEN 'QUEUED'
                                WHEN 'RUNNING' THEN 'RUNNING'
                                WHEN 'COMPLETED' THEN 'COMPLETED'
                                WHEN 'FAILED' THEN 'FAILED'
                                WHEN 'CANCELLED' THEN 'CANCELLED'
                            END AS state,
                            1 AS units, job.updated_at
                     FROM scan_jobs job
                     JOIN selected_libraries library ON library.id = job.library_id

                     UNION ALL

                     SELECT root.library_id, 'LOCAL_METADATA',
                            CASE batch.status WHEN 'PENDING' THEN 'QUEUED' ELSE batch.status END,
                            batch.source_count, batch.updated_at
                     FROM scan_local_metadata_batches batch
                     JOIN library_roots root ON root.id = batch.library_root_id
                     JOIN selected_libraries library ON library.id = root.library_id

                     UNION ALL

                     SELECT root.library_id, 'LOCAL_METADATA',
                            CASE backfill.status WHEN 'PENDING' THEN 'QUEUED' ELSE backfill.status END,
                            1, backfill.updated_at
                     FROM scan_local_metadata_backfills backfill
                     JOIN library_roots root ON root.id = backfill.library_root_id
                     JOIN selected_libraries library ON library.id = root.library_id

                     UNION ALL

                     SELECT job.library_id, 'LOCAL_METADATA',
                            CASE
                                WHEN job.status = 'CANCELLED' AND target.metadata_state = 'PENDING'
                                    THEN 'CANCELLED'
                                WHEN target.metadata_state = 'PENDING' THEN 'QUEUED'
                                WHEN target.metadata_state = 'DONE' THEN 'COMPLETED'
                                WHEN target.metadata_state = 'FAILED' THEN 'FAILED'
                            END,
                            1, target.updated_at
                     FROM scan_job_targets target
                     JOIN scan_jobs job ON job.id = target.job_id
                     JOIN selected_libraries library ON library.id = job.library_id
                     WHERE target.metadata_state IN ('PENDING', 'DONE', 'FAILED')
                       AND NOT EXISTS (
                           SELECT 1 FROM scan_local_metadata_batches batch
                           WHERE batch.job_id = job.id
                       )

                     UNION ALL

                     SELECT job.library_id, 'LOCAL_METADATA', 'RUNNING',
                            1, job.updated_at
                     FROM scan_jobs job
                     JOIN selected_libraries library ON library.id = job.library_id
                     WHERE job.scan_phase = 'POSTPROCESSING'
                       AND job.current_item = '本地元数据'
                       AND job.status IN ('RUNNING', 'COMPLETED')
                       AND NOT EXISTS (
                           SELECT 1 FROM scan_local_metadata_batches batch
                           WHERE batch.job_id = job.id AND batch.status = 'RUNNING'
                       )

                     UNION ALL

                     SELECT job.library_id, 'PROBE',
                            CASE
                                WHEN job.status = 'CANCELLED' AND target.probe_state = 'PENDING'
                                    THEN 'CANCELLED'
                                WHEN target.probe_state = 'PENDING' THEN 'QUEUED'
                                WHEN target.probe_state = 'DONE' THEN 'COMPLETED'
                                WHEN target.probe_state = 'FAILED' THEN 'FAILED'
                            END,
                            1, target.updated_at
                     FROM scan_job_targets target
                     JOIN scan_jobs job ON job.id = target.job_id
                     JOIN selected_libraries library ON library.id = job.library_id
                     WHERE target.target_type = 'SOURCE'
                       AND target.probe_state IN ('PENDING', 'DONE', 'FAILED')

                     UNION ALL

                     SELECT job.library_id, 'PROBE',
                            CASE job.status
                                WHEN 'PENDING' THEN 'QUEUED'
                                WHEN 'RUNNING' THEN 'RUNNING'
                                WHEN 'COMPLETED' THEN 'COMPLETED'
                                WHEN 'FAILED' THEN 'FAILED'
                                WHEN 'CANCELLED' THEN 'CANCELLED'
                            END,
                            1, job.updated_at
                     FROM strm_probe_jobs job
                     JOIN selected_libraries library ON library.id = job.library_id

                     UNION ALL

                     SELECT job.library_id, 'PROBE', 'RUNNING', 1, job.updated_at
                     FROM scan_jobs job
                     JOIN selected_libraries library ON library.id = job.library_id
                     WHERE job.scan_phase = 'POSTPROCESSING'
                       AND job.current_item = '媒体探测'
                       AND job.status IN ('RUNNING', 'COMPLETED')

                     UNION ALL

                     SELECT job.library_id, 'THUMBNAIL',
                            CASE
                                WHEN job.status = 'CANCELLED' AND target.thumbnail_state = 'PENDING'
                                    THEN 'CANCELLED'
                                WHEN target.thumbnail_state = 'PENDING' THEN 'QUEUED'
                                WHEN target.thumbnail_state = 'DONE' THEN 'COMPLETED'
                                WHEN target.thumbnail_state = 'FAILED' THEN 'FAILED'
                            END,
                            1, target.updated_at
                     FROM scan_job_targets target
                     JOIN scan_jobs job ON job.id = target.job_id
                     JOIN selected_libraries library ON library.id = job.library_id
                     WHERE target.target_type = 'ITEM'
                       AND target.thumbnail_state IN ('PENDING', 'DONE', 'FAILED')

                     UNION ALL

                     SELECT job.library_id, 'THUMBNAIL', 'RUNNING', 1, job.updated_at
                     FROM scan_jobs job
                     JOIN selected_libraries library ON library.id = job.library_id
                     WHERE job.scan_phase = 'POSTPROCESSING'
                       AND job.current_item = '视频缩略图'
                       AND job.status IN ('RUNNING', 'COMPLETED')

                     UNION ALL

                     SELECT job.library_id, 'AUTO_METADATA',
                            CASE job.status
                                WHEN 'QUEUED' THEN 'QUEUED'
                                WHEN 'DEFERRED' THEN 'QUEUED'
                                WHEN 'RUNNING' THEN 'RUNNING'
                                WHEN 'COMPLETED' THEN 'COMPLETED'
                                WHEN 'COMPLETED_WITH_ISSUES' THEN 'COMPLETED'
                                WHEN 'FAILED' THEN 'FAILED'
                                WHEN 'CANCELLED' THEN 'CANCELLED'
                            END,
                            1, job.updated_at
                     FROM metadata_reidentify_jobs job
                     JOIN selected_libraries library ON library.id = job.library_id
                     WHERE job.mode = 'FILL_MISSING'
                       AND job.dispatch_source = 'AUTOMATIC'
                 ),
                 stage_kinds AS (
                     SELECT 'INDEX' AS stage, 1 AS stage_order
                     UNION ALL SELECT 'LOCAL_METADATA', 2
                     UNION ALL SELECT 'PROBE', 3
                     UNION ALL SELECT 'THUMBNAIL', 4
                     UNION ALL SELECT 'AUTO_METADATA', 5
                 ),
                 stage_counts AS (
                     SELECT library_id, stage,
                            CAST(SUM(CASE WHEN state = 'QUEUED' THEN units ELSE 0 END) AS BIGINT) AS queued,
                            CAST(SUM(CASE WHEN state = 'RUNNING' THEN units ELSE 0 END) AS BIGINT) AS running,
                            CAST(SUM(CASE WHEN state = 'COMPLETED' THEN units ELSE 0 END) AS BIGINT) AS completed,
                            CAST(SUM(CASE WHEN state = 'FAILED' THEN units ELSE 0 END) AS BIGINT) AS failed,
                            CAST(SUM(CASE WHEN state = 'CANCELLED' THEN units ELSE 0 END) AS BIGINT) AS cancelled,
                            MAX(updated_at) AS recent_activity_at,
                            COUNT(*) AS work_count
                     FROM stage_work
                     GROUP BY library_id, stage
                 )
                 SELECT library.id AS library_id, library.name AS library_name,
                        kinds.stage,
                        CAST(CASE WHEN counts.work_count IS NULL THEN 0 ELSE 1 END AS BIGINT) AS planned,
                        CAST(COALESCE(counts.queued, 0) AS BIGINT) AS queued,
                        CAST(COALESCE(counts.running, 0) AS BIGINT) AS running,
                        CAST(COALESCE(counts.completed, 0) AS BIGINT) AS completed,
                        CAST(COALESCE(counts.failed, 0) AS BIGINT) AS failed,
                        CAST(COALESCE(counts.cancelled, 0) AS BIGINT) AS cancelled,
                        counts.recent_activity_at
                 FROM selected_libraries library
                 CROSS JOIN stage_kinds kinds
                 LEFT JOIN stage_counts counts
                   ON counts.library_id = library.id AND counts.stage = kinds.stage
                 ORDER BY lower(library.name), library.id, kinds.stage_order",
            )
            .bind(page_size)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;

        let mut libraries = Vec::<StoredLibraryPipelineActivity>::new();
        for row in rows {
            let library_id: String = row.get("library_id");
            if libraries
                .last()
                .is_none_or(|library| library.id != library_id)
            {
                libraries.push(StoredLibraryPipelineActivity {
                    id: library_id,
                    name: row.get("library_name"),
                    stages: Vec::with_capacity(5),
                });
            }
            let kind = match row.get::<String, _>("stage").as_str() {
                "INDEX" => StoredPipelineStageKind::Index,
                "LOCAL_METADATA" => StoredPipelineStageKind::LocalMetadata,
                "PROBE" => StoredPipelineStageKind::Probe,
                "THUMBNAIL" => StoredPipelineStageKind::Thumbnail,
                "AUTO_METADATA" => StoredPipelineStageKind::AutomaticMetadata,
                _ => continue,
            };
            let stage = StoredPipelineStageActivity {
                kind,
                planned: row.get::<i64, _>("planned") != 0,
                counts: StoredPipelineStatusCounts {
                    queued: row.get("queued"),
                    running: row.get("running"),
                    completed: row.get("completed"),
                    failed: row.get("failed"),
                    cancelled: row.get("cancelled"),
                },
                recent_activity_at: row.get("recent_activity_at"),
            };
            if let Some(library) = libraries.last_mut() {
                library.stages.push(stage);
            }
        }

        Ok(StoredLibraryPipelineActivityPage {
            page,
            page_size,
            total_libraries,
            libraries,
        })
    }
}
