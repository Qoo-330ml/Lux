use super::*;

// Separate EXISTS predicates let SQLite use each stage's partial index even when
// the job retains a large number of completed targets.
const UNFINISHED_TARGETS: &str = "(EXISTS (
    SELECT 1 FROM scan_job_targets t WHERE t.job_id = sj.id AND t.probe_state IN ('PENDING', 'FAILED')
) OR EXISTS (
    SELECT 1 FROM scan_job_targets t WHERE t.job_id = sj.id AND t.metadata_state IN ('PENDING', 'FAILED')
) OR EXISTS (
    SELECT 1 FROM scan_job_targets t WHERE t.job_id = sj.id AND t.thumbnail_state IN ('PENDING', 'FAILED')
))";
const UNREADY_TARGETS: &str = "EXISTS (
    SELECT 1 FROM scan_manifests m WHERE m.job_id = sj.id
      AND m.workflow_version IN (2, 3) AND m.discovery_format_version = 3
      AND m.postprocessing_targets_ready = 0
)";

fn postprocessing_issues_predicate() -> String {
    format!("sj.status = 'COMPLETED' AND sj.job_type = 'RECONCILE_LIBRARY'
        AND sj.scan_phase = 'IDLE'
        AND (COALESCE(sj.error, '') = 'POSTPROCESSING_FAILED' OR {UNFINISHED_TARGETS} OR {UNREADY_TARGETS})")
}

/// Row of `scan_job_retry_states`: one flag column per retry reason, projected as 0/1 integers
/// because `sqlx::Any` binds booleans to the backend's integer representation.
#[derive(sqlx::FromRow)]
struct ScanJobRetryStateRow {
    id: String,
    issues: i64,
    unfinished: i64,
    unready: i64,
    legacy_work: i64,
    active_job: i64,
}

impl ScanJobRetryStateRow {
    /// Splits the decoded row into the map key and the flag struct callers consume.
    fn into_keyed_state(self) -> (String, StoredScanJobRetryState) {
        (
            self.id,
            StoredScanJobRetryState {
                has_postprocessing_issues: self.issues != 0,
                has_unfinished_targets: self.unfinished != 0,
                targets_need_materialization: self.unready != 0,
                has_legacy_work: self.legacy_work != 0,
                has_active_job: self.active_job != 0,
            },
        )
    }
}

/// Row of `list_pending_scan_job_file_changes`.
#[derive(sqlx::FromRow)]
struct ScanJobFileChangeRow {
    library_root_id: String,
    relative_path: String,
    change_kind: String,
    revision: i64,
}

impl From<ScanJobFileChangeRow> for StoredScanJobPath {
    fn from(row: ScanJobFileChangeRow) -> Self {
        Self {
            library_root_id: row.library_root_id,
            relative_path: row.relative_path,
            change_kind: row.change_kind,
            file_change_revision: Some(row.revision),
        }
    }
}

/// Row of `local_metadata_completion_wait_state`: both flags arrive as 0/1 integers.
#[derive(sqlx::FromRow)]
struct LocalMetadataCompletionWaitRow {
    has_pending_targets: i64,
    job_missing_or_cancelled: i64,
}

impl LocalMetadataCompletionWaitRow {
    /// Returns `(has_pending_targets, job_missing_or_cancelled)` exactly as the flags were read.
    fn into_wait_state(self) -> (bool, bool) {
        (
            self.has_pending_targets != 0,
            self.job_missing_or_cancelled != 0,
        )
    }
}

/// Row of `list_pending_local_metadata_item_ids`.
#[derive(sqlx::FromRow)]
struct PendingLocalMetadataItemRow {
    item_id: String,
}

/// Row shared by `list_scan_job_target_sources_page`, `list_scan_job_target_movie_items_page` and
/// `list_scan_job_target_home_video_items_page`: all three select the same preferred-source
/// projection.
#[derive(sqlx::FromRow)]
struct MediaSourcePathRow {
    source_id: String,
    item_id: String,
    probe_status: String,
    root_path: String,
    relative_path: String,
}

impl From<MediaSourcePathRow> for StoredMediaSourcePath {
    fn from(row: MediaSourcePathRow) -> Self {
        Self {
            source_id: row.source_id,
            item_id: row.item_id,
            probe_status: row.probe_status,
            root_path: row.root_path,
            relative_path: row.relative_path,
        }
    }
}

/// Row of `list_scan_job_target_series_items_page`.
#[derive(sqlx::FromRow)]
struct SeriesMetadataSourceRow {
    series_id: String,
    season_id: String,
    episode_id: String,
    season_number: Option<i64>,
    root_path: String,
    relative_path: String,
}

impl From<SeriesMetadataSourceRow> for StoredSeriesMetadataSource {
    fn from(row: SeriesMetadataSourceRow) -> Self {
        Self {
            series_id: row.series_id,
            season_id: row.season_id,
            episode_id: row.episode_id,
            season_number: row.season_number,
            root_path: row.root_path,
            relative_path: row.relative_path,
        }
    }
}

/// Row of `load_scan_job_metadata_page`: the job lifecycle columns are optional because the job
/// row is joined, while the source columns are `COALESCE`d to empty strings and `target_kind`
/// selects the shape of each row.
#[derive(Debug, sqlx::FromRow)]
struct ScanJobMetadataPageRow {
    job_type: Option<String>,
    job_status: Option<String>,
    auto_metadata_match: Option<i64>,
    has_pending: i64,
    target_kind: String,
    source_id: String,
    item_id: String,
    probe_status: String,
    root_path: String,
    relative_path: String,
    series_id: String,
    season_id: String,
    season_number: Option<i64>,
}

impl ScanJobMetadataPageRow {
    /// Projects one movie or home-video row onto the preferred-source shape.
    fn media_source_path(&self) -> StoredMediaSourcePath {
        StoredMediaSourcePath {
            source_id: self.source_id.clone(),
            item_id: self.item_id.clone(),
            probe_status: self.probe_status.clone(),
            root_path: self.root_path.clone(),
            relative_path: self.relative_path.clone(),
        }
    }

    /// Projects one episode row; its `episode_id` is the target `item_id` column.
    fn series_metadata_source(&self) -> StoredSeriesMetadataSource {
        StoredSeriesMetadataSource {
            series_id: self.series_id.clone(),
            season_id: self.season_id.clone(),
            episode_id: self.item_id.clone(),
            season_number: self.season_number,
            root_path: self.root_path.clone(),
            relative_path: self.relative_path.clone(),
        }
    }
}

impl Database {
    pub(crate) async fn scan_job_retry_states(
        &self,
        job_ids: &[String],
    ) -> Result<HashMap<String, StoredScanJobRetryState>, StorageError> {
        let mut states = HashMap::with_capacity(job_ids.len());
        for chunk in job_ids.chunks(SCAN_DML_CHUNK_SIZE) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let sql = format!(
                "SELECT sj.id,
                CASE WHEN {} THEN 1 ELSE 0 END AS issues,
                CASE WHEN {UNFINISHED_TARGETS} THEN 1 ELSE 0 END AS unfinished,
                CASE WHEN {UNREADY_TARGETS} THEN 1 ELSE 0 END AS unready,
                CASE WHEN EXISTS (SELECT 1 FROM reconciliation_scan_entries r
                    WHERE r.job_id = sj.id AND r.status = 'PENDING')
                    THEN 1 ELSE 0 END AS legacy_work,
                CASE WHEN EXISTS (SELECT 1 FROM scan_jobs active
                    WHERE active.library_id = sj.library_id AND active.job_type = sj.job_type
                      AND active.status IN ('PENDING', 'RUNNING'))
                    THEN 1 ELSE 0 END AS active_job
                FROM scan_jobs sj WHERE sj.id IN ({placeholders})",
                postprocessing_issues_predicate()
            );
            // Only fixed predicates and generated placeholders enter SQL; IDs are bound below.
            let mut query = self.query_as::<ScanJobRetryStateRow>(sqlx::AssertSqlSafe(sql));
            for id in chunk {
                query = query.bind(id);
            }
            let rows = query.fetch_all(&self.pool).await.map_err(self.sqlx_err())?;
            for state in rows {
                let (id, state) = state.into_keyed_state();
                states.insert(id, state);
            }
        }
        Ok(states)
    }

    pub(crate) async fn list_admin_scan_jobs(
        &self,
        status: Option<&str>,
        has_postprocessing_issues: Option<bool>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredScanJob>, StorageError> {
        let Some(has_issues) = has_postprocessing_issues else {
            return self.list_scan_jobs(status, offset, limit).await;
        };
        let predicate = postprocessing_issues_predicate();
        let predicate = if has_issues {
            predicate
        } else {
            format!("NOT ({predicate})")
        };
        let sql = format!(
            "SELECT sj.* FROM scan_jobs sj WHERE ({predicate})
            {} ORDER BY sj.created_at DESC, sj.id DESC LIMIT ? OFFSET ?",
            if status.is_some() {
                "AND sj.status = ?"
            } else {
                ""
            }
        );
        // Predicates contain only static SQL; all external values use bind parameters.
        let mut query = self.query(sqlx::AssertSqlSafe(sql));
        if let Some(status) = status {
            query = query.bind(status);
        }
        query
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
            .map(|rows| rows.into_iter().map(stored_scan_job).collect())
            .map_err(self.sqlx_err())
    }
}

pub(in crate::storage::repository::jobs) fn prune_sidecar_directories(
    mut directories: Vec<String>,
) -> Vec<String> {
    directories.sort();
    directories.dedup();
    if directories
        .first()
        .is_some_and(|directory| directory == ".")
    {
        return vec![".".to_owned()];
    }

    let mut retained = Vec::with_capacity(directories.len());
    for directory in directories {
        let covered = retained.iter().any(|parent: &String| {
            directory.starts_with(parent) && directory.as_bytes().get(parent.len()) == Some(&b'/')
        });
        if !covered {
            retained.push(directory);
        }
    }
    retained
}

pub(in crate::storage::repository::jobs) fn sidecar_target_query(values: &str) -> String {
    format!(
        "WITH sidecar_directories(directory) AS (VALUES {values})
         INSERT INTO scan_job_targets (
             job_id, target_type, target_id, item_id, change_kind,
             probe_state, metadata_state, thumbnail_state
         )
         SELECT ?, 'ITEM', ms.item_id, ms.item_id, 'SIDECAR',
                'SKIPPED', 'PENDING', 'PENDING'
         FROM media_sources ms
         JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
         CROSS JOIN sidecar_directories sd
         WHERE fe.library_root_id = ? AND fe.is_missing = 0
           AND fe.relative_path >= sd.directory || '/'
           AND fe.relative_path < sd.directory || '0'
         GROUP BY ms.item_id
         ON CONFLICT(job_id, target_type, target_id) DO UPDATE SET
             change_kind = 'SIDECAR', metadata_state = 'PENDING', error = NULL,
             updated_at = unixepoch()
         WHERE scan_job_targets.change_kind <> 'REMOVED'
           AND (scan_job_targets.change_kind <> 'SIDECAR'
                OR scan_job_targets.metadata_state <> 'PENDING'
                OR scan_job_targets.error IS NOT NULL)"
    )
}

impl Database {
    pub(crate) async fn record_scan_job_file_change(
        &self,
        job_id: &str,
        library_root_id: &str,
        relative_path: &str,
        change_kind: &str,
    ) -> Result<bool, StorageError> {
        let path = Path::new(relative_path);
        if relative_path.trim().is_empty()
            || path.is_absolute()
            || path
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(StorageError::Conflict(
                "invalid scan file change path".to_owned(),
            ));
        }
        if !matches!(change_kind, "ADD" | "CHANGE" | "REMOVE") {
            return Err(StorageError::Conflict(
                "invalid scan file change kind".to_owned(),
            ));
        }
        let result = self
            .query(
                "INSERT INTO scan_job_file_changes (
                     job_id, library_root_id, relative_path, change_kind
                 ) VALUES (?, ?, ?, ?)
                 ON CONFLICT(job_id, library_root_id, relative_path) DO UPDATE SET
                     change_kind = excluded.change_kind,
                     processing_state = 'PENDING', error = NULL,
                     revision = scan_job_file_changes.revision + 1,
                     updated_at = unixepoch()
                 WHERE scan_job_file_changes.change_kind <> excluded.change_kind
                    OR scan_job_file_changes.processing_state <> 'PENDING'
                    OR scan_job_file_changes.error IS NOT NULL",
            )
            .bind(job_id)
            .bind(library_root_id)
            .bind(relative_path)
            .bind(change_kind)
            .execute(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        Ok(result.rows_affected() > 0)
    }

    pub(crate) async fn list_pending_scan_job_file_changes(
        &self,
        job_id: &str,
        after: Option<&StoredScanJobPath>,
        limit: i64,
    ) -> Result<Vec<StoredScanJobPath>, StorageError> {
        let limit = limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE);
        let rows = if let Some(after) = after {
            self.query_as::<ScanJobFileChangeRow>(
                "SELECT library_root_id, relative_path, change_kind, revision
                 FROM scan_job_file_changes
                 WHERE job_id = ? AND processing_state = 'PENDING'
                   AND (relative_path > ?
                        OR (relative_path = ? AND library_root_id > ?))
                 ORDER BY relative_path, library_root_id
                 LIMIT ?",
            )
            .bind(job_id)
            .bind(&after.relative_path)
            .bind(&after.relative_path)
            .bind(&after.library_root_id)
            .bind(limit)
            .fetch_all(&self.pool)
            .await
        } else {
            self.query_as::<ScanJobFileChangeRow>(
                "SELECT library_root_id, relative_path, change_kind, revision
                 FROM scan_job_file_changes
                 WHERE job_id = ? AND processing_state = 'PENDING'
                 ORDER BY relative_path, library_root_id
                 LIMIT ?",
            )
            .bind(job_id)
            .bind(limit)
            .fetch_all(&self.pool)
            .await
        }
        .map_err(self.sqlx_err())?;
        Ok(rows.into_iter().map(StoredScanJobPath::from).collect())
    }

    pub(crate) async fn requeue_running_scan_job_file_changes(
        &self,
        job_id: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE scan_job_file_changes
             SET processing_state = 'PENDING', error = NULL, updated_at = unixepoch()
             WHERE job_id = ? AND processing_state = 'RUNNING'",
        )
        .bind(job_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn set_scan_job_file_change_state(
        &self,
        job_id: &str,
        library_root_id: &str,
        relative_path: &str,
        revision: i64,
        state: &str,
        error: Option<&str>,
    ) -> Result<bool, StorageError> {
        if !matches!(state, "PENDING" | "RUNNING" | "DONE" | "FAILED" | "SKIPPED") {
            return Err(StorageError::Conflict(
                "invalid scan file change state".to_owned(),
            ));
        }
        let result = self
            .query(
                "UPDATE scan_job_file_changes
                 SET processing_state = ?, error = ?, updated_at = unixepoch()
                 WHERE job_id = ? AND library_root_id = ? AND relative_path = ?
                   AND revision = ?
                   AND (processing_state <> ? OR error IS DISTINCT FROM ?)",
            )
            .bind(state)
            .bind(error)
            .bind(job_id)
            .bind(library_root_id)
            .bind(relative_path)
            .bind(revision)
            .bind(state)
            .bind(error)
            .execute(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        Ok(result.rows_affected() > 0)
    }

    pub(crate) async fn record_scan_job_targets(
        &self,
        job_id: &str,
        library_root_id: &str,
        relative_paths: &[String],
        change_kind: &str,
    ) -> Result<bool, StorageError> {
        if relative_paths.is_empty() {
            return Ok(false);
        }
        let mut transaction = self.begin_scan_write_transaction().await?;
        let changed = self
            .record_scan_job_targets_in_transaction(
                &mut transaction,
                job_id,
                library_root_id,
                relative_paths,
                change_kind,
            )
            .await?;
        transaction
            .commit()
            .await
            .map_err(self.sqlx_err())
            .map(|_| changed)
    }

    pub(in crate::storage::repository::jobs::scan) async fn record_scan_job_targets_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        job_id: &str,
        library_root_id: &str,
        relative_paths: &[String],
        change_kind: &str,
    ) -> Result<bool, StorageError> {
        if relative_paths.is_empty() {
            return Ok(false);
        }
        let mut changed = false;
        for paths in relative_paths.chunks(super::manifest_path_query_chunk_size(self.backend())) {
            let placeholders = std::iter::repeat_n("?", paths.len())
                .collect::<Vec<_>>()
                .join(", ");
            let source_query = format!(
                "INSERT INTO scan_job_targets (
                     job_id, target_type, target_id, source_id, item_id, change_kind,
                     probe_state, metadata_state, thumbnail_state
                 )
                 SELECT ?, 'SOURCE', ms.id, ms.id, ms.item_id, ?,
                        'PENDING', 'SKIPPED', 'SKIPPED'
                 FROM media_sources ms
                 JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                 WHERE fe.library_root_id = ? AND fe.is_missing = 0
                   AND fe.relative_path IN ({placeholders})
                 ON CONFLICT(job_id, target_type, target_id) DO NOTHING"
            );
            let mut source_statement = self
                .query(sqlx::AssertSqlSafe(source_query))
                .bind(job_id)
                .bind(change_kind)
                .bind(library_root_id);
            for path in paths {
                source_statement = source_statement.bind(path);
            }
            let source_result = source_statement
                .execute(&mut **transaction)
                .await
                .map_err(self.sqlx_err())?;
            changed |= source_result.rows_affected() > 0;

            let item_query = format!(
                "INSERT INTO scan_job_targets (
                     job_id, target_type, target_id, item_id, change_kind,
                     probe_state, metadata_state, thumbnail_state
                 )
                 SELECT ?, 'ITEM', ms.item_id, ms.item_id, ?,
                        'SKIPPED', 'PENDING', 'PENDING'
                 FROM media_sources ms
                 JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                 WHERE fe.library_root_id = ? AND fe.is_missing = 0
                   AND fe.relative_path IN ({placeholders})
                 ON CONFLICT(job_id, target_type, target_id) DO NOTHING"
            );
            let mut item_statement = self
                .query(sqlx::AssertSqlSafe(item_query))
                .bind(job_id)
                .bind(change_kind)
                .bind(library_root_id);
            for path in paths {
                item_statement = item_statement.bind(path);
            }
            let item_result = item_statement
                .execute(&mut **transaction)
                .await
                .map_err(self.sqlx_err())?;
            changed |= item_result.rows_affected() > 0;
        }
        Ok(changed)
    }

    pub(crate) async fn record_scan_job_sidecar_targets(
        &self,
        job_id: &str,
        library_root_id: &str,
        sidecar_paths: &[String],
    ) -> Result<bool, StorageError> {
        let directories = sidecar_paths
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
            .collect::<Vec<_>>();
        let directories = prune_sidecar_directories(directories);
        if directories.is_empty() {
            return Ok(false);
        }
        let mut transaction = self.begin_scan_write_transaction().await?;
        let changed = self
            .record_scan_job_sidecar_targets_in_transaction(
                &mut transaction,
                job_id,
                library_root_id,
                &directories,
            )
            .await?;
        transaction
            .commit()
            .await
            .map_err(self.sqlx_err())
            .map(|_| changed)
    }

    pub(in crate::storage::repository::jobs::scan) async fn record_scan_job_sidecar_targets_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        job_id: &str,
        library_root_id: &str,
        directories: &[String],
    ) -> Result<bool, StorageError> {
        if directories.iter().any(|directory| directory == ".") {
            let result = self
                .query(
                    "INSERT INTO scan_job_targets (
                     job_id, target_type, target_id, item_id, change_kind,
                     probe_state, metadata_state, thumbnail_state
                 )
                 SELECT ?, 'ITEM', ms.item_id, ms.item_id, 'SIDECAR',
                        'SKIPPED', 'PENDING', 'PENDING'
                 FROM media_sources ms
                 JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                 WHERE fe.library_root_id = ? AND fe.is_missing = 0
                 GROUP BY ms.item_id
                 ON CONFLICT(job_id, target_type, target_id) DO UPDATE SET
                     change_kind = 'SIDECAR', metadata_state = 'PENDING', error = NULL,
                     updated_at = unixepoch()
                 WHERE scan_job_targets.change_kind <> 'REMOVED'
                   AND (scan_job_targets.change_kind <> 'SIDECAR'
                        OR scan_job_targets.metadata_state <> 'PENDING'
                        OR scan_job_targets.error IS NOT NULL)",
                )
                .bind(job_id)
                .bind(library_root_id)
                .execute(&mut **transaction)
                .await
                .map_err(self.sqlx_err())?;
            return Ok(result.rows_affected() > 0);
        }
        let mut changed = false;
        for directory_chunk in directories.chunks(SCAN_DML_CHUNK_SIZE) {
            let values = std::iter::repeat_n("(?)", directory_chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = sidecar_target_query(&values);
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for directory in directory_chunk {
                statement = statement.bind(directory);
            }
            let result = statement
                .bind(job_id)
                .bind(library_root_id)
                .execute(&mut **transaction)
                .await
                .map_err(self.sqlx_err())?;
            changed |= result.rows_affected() > 0;
        }
        Ok(changed)
    }

    pub(in crate::storage::repository::jobs::scan) async fn record_scan_job_removed_targets_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        job_id: &str,
        library_root_id: &str,
        relative_paths: &[String],
    ) -> Result<(), StorageError> {
        if relative_paths.is_empty() {
            return Ok(());
        }
        for paths in relative_paths.chunks(SCAN_DML_CHUNK_SIZE) {
            let placeholders = std::iter::repeat_n("?", paths.len())
                .collect::<Vec<_>>()
                .join(", ");
            let source_query = format!(
                "INSERT INTO scan_job_targets (
                     job_id, target_type, target_id, source_id, item_id, change_kind,
                     probe_state, metadata_state, thumbnail_state
                 )
                 SELECT ?, 'SOURCE', ms.id, ms.id, ms.item_id, 'REMOVED',
                        'SKIPPED', 'SKIPPED', 'SKIPPED'
                 FROM media_sources ms
                 JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                 WHERE fe.library_root_id = ? AND fe.is_missing = 0
                   AND fe.relative_path IN ({placeholders})
                 ON CONFLICT(job_id, target_type, target_id) DO NOTHING"
            );
            let mut source_statement = self
                .query(sqlx::AssertSqlSafe(source_query))
                .bind(job_id)
                .bind(library_root_id);
            for path in paths {
                source_statement = source_statement.bind(path);
            }
            source_statement
                .execute(&mut **transaction)
                .await
                .map_err(self.sqlx_err())?;

            let item_query = format!(
                "INSERT INTO scan_job_targets (
                     job_id, target_type, target_id, item_id, change_kind,
                     probe_state, metadata_state, thumbnail_state
                 )
                 SELECT ?, 'ITEM', ms.item_id, ms.item_id, 'REMOVED',
                        'SKIPPED', 'SKIPPED', 'SKIPPED'
                 FROM media_sources ms
                 JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                 WHERE fe.library_root_id = ? AND fe.is_missing = 0
                   AND fe.relative_path IN ({placeholders})
                 ON CONFLICT(job_id, target_type, target_id) DO NOTHING"
            );
            let mut item_statement = self
                .query(sqlx::AssertSqlSafe(item_query))
                .bind(job_id)
                .bind(library_root_id);
            for path in paths {
                item_statement = item_statement.bind(path);
            }
            item_statement
                .execute(&mut **transaction)
                .await
                .map_err(self.sqlx_err())?;
        }
        Ok(())
    }

    pub(in crate::storage::repository::jobs::scan) async fn record_scan_job_removed_targets_for_entry_ids_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        job_id: &str,
        filesystem_entry_ids: &[String],
    ) -> Result<(), StorageError> {
        if filesystem_entry_ids.is_empty() {
            return Ok(());
        }
        for ids in filesystem_entry_ids.chunks(SCAN_DML_CHUNK_SIZE) {
            let placeholders = std::iter::repeat_n("?", ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            for (columns, values) in [
                (
                    "target_id, source_id, item_id, change_kind, probe_state,
                     metadata_state, thumbnail_state",
                    "?, 'SOURCE', ms.id, ms.id, ms.item_id, 'REMOVED', 'SKIPPED', 'SKIPPED', 'SKIPPED'",
                ),
                (
                    "target_id, item_id, change_kind, probe_state,
                     metadata_state, thumbnail_state",
                    "?, 'ITEM', ms.item_id, ms.item_id, 'REMOVED', 'SKIPPED', 'SKIPPED', 'SKIPPED'",
                ),
            ] {
                let query = format!(
                    "INSERT INTO scan_job_targets (
                         job_id, target_type, {columns}
                     )
                     SELECT {values}
                     FROM media_sources ms
                     WHERE ms.filesystem_entry_id IN ({placeholders})
                     ON CONFLICT(job_id, target_type, target_id) DO NOTHING"
                );
                let mut statement = self.query(sqlx::AssertSqlSafe(query)).bind(job_id);
                for id in ids {
                    statement = statement.bind(id);
                }
                statement
                    .execute(&mut **transaction)
                    .await
                    .map_err(self.sqlx_err())?;
            }
        }
        Ok(())
    }

    pub(crate) async fn list_scan_job_target_sources_page(
        &self,
        job_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<StoredMediaSourcePath>, StorageError> {
        self.query_as::<MediaSourcePathRow>(
            "SELECT ms.id AS source_id, ms.item_id, ms.probe_status,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM scan_job_targets t
             JOIN media_sources ms ON ms.id = t.source_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE t.job_id = ? AND t.target_type = 'SOURCE'
               AND t.probe_state = 'PENDING'
               AND fe.is_missing = 0
             ORDER BY t.target_id
             LIMIT ? OFFSET ?",
        )
        .bind(job_id)
        .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
        .bind(offset.max(0))
        .fetch_all(&self.pool)
        .await
        .map(|rows| rows.into_iter().map(StoredMediaSourcePath::from).collect())
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn load_scan_job_metadata_page(
        &self,
        job_id: &str,
        limit: i64,
    ) -> Result<StoredScanJobMetadataPage, StorageError> {
        let rows = self
            .query_as::<ScanJobMetadataPageRow>(
                "WITH job_lifecycle AS (
                     SELECT id, job_type, status, auto_metadata_match
                     FROM scan_jobs
                     WHERE id = ?
                 ), pending_targets AS (
                     SELECT t.target_id, t.item_id
                     FROM scan_job_targets t
                     JOIN job_lifecycle job ON job.id = t.job_id
                     WHERE t.target_type = 'ITEM'
                       AND t.metadata_state = 'PENDING'
                 ), source_candidates AS (
                     SELECT * FROM (
                     SELECT 0 AS source_order, t.target_id, 'MOVIE' AS target_kind,
                            ms.id AS source_id, ms.item_id, ms.probe_status,
                            lr.canonical_path AS root_path, fe.relative_path,
                            CAST(NULL AS TEXT) AS series_id,
                            CAST(NULL AS TEXT) AS season_id,
                            CAST(NULL AS INTEGER) AS season_number
                     FROM pending_targets t
                     JOIN media_items mi ON mi.id = t.item_id AND mi.item_type = 'MOVIE'
                     JOIN media_sources ms ON ms.id = (
                         SELECT preferred.id FROM media_sources preferred
                         JOIN filesystem_entries preferred_fe
                           ON preferred_fe.id = preferred.filesystem_entry_id
                         WHERE preferred.item_id = t.item_id
                           AND preferred_fe.is_missing = 0
                         ORDER BY preferred.is_default DESC, preferred.id
                         LIMIT 1
                     )
                     JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                     JOIN library_roots lr ON lr.id = fe.library_root_id
                     WHERE fe.is_missing = 0
                     ORDER BY t.target_id
                     LIMIT ?
                     ) movie_page
                     UNION ALL
                     SELECT * FROM (
                     SELECT 1 AS source_order, t.target_id, 'VIDEO' AS target_kind,
                            ms.id AS source_id, ms.item_id, ms.probe_status,
                            lr.canonical_path AS root_path, fe.relative_path,
                            CAST(NULL AS TEXT) AS series_id,
                            CAST(NULL AS TEXT) AS season_id,
                            CAST(NULL AS INTEGER) AS season_number
                     FROM pending_targets t
                     JOIN media_items mi ON mi.id = t.item_id AND mi.item_type = 'VIDEO'
                     JOIN media_sources ms ON ms.id = (
                         SELECT preferred.id FROM media_sources preferred
                         JOIN filesystem_entries preferred_fe
                           ON preferred_fe.id = preferred.filesystem_entry_id
                         WHERE preferred.item_id = t.item_id
                           AND preferred_fe.is_missing = 0
                         ORDER BY preferred.is_default DESC, preferred.id
                         LIMIT 1
                     )
                     JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                     JOIN library_roots lr ON lr.id = fe.library_root_id
                     WHERE fe.is_missing = 0
                     ORDER BY t.target_id
                     LIMIT ?
                     ) home_video_page
                     UNION ALL
                     SELECT * FROM (
                     SELECT 2 AS source_order, t.target_id, 'EPISODE' AS target_kind,
                            ms.id AS source_id, episode.id AS item_id, ms.probe_status,
                            lr.canonical_path AS root_path, fe.relative_path,
                            series.id AS series_id, season.id AS season_id,
                            season.season_number
                     FROM pending_targets t
                     JOIN media_items episode ON episode.id = t.item_id
                       AND episode.item_type = 'EPISODE'
                     JOIN media_items season ON season.id = episode.parent_id
                       AND season.item_type = 'SEASON'
                     JOIN media_items series ON series.id = episode.series_id
                       AND series.item_type = 'SERIES'
                     JOIN media_sources ms ON ms.id = (
                         SELECT preferred.id FROM media_sources preferred
                         JOIN filesystem_entries preferred_fe
                           ON preferred_fe.id = preferred.filesystem_entry_id
                         WHERE preferred.item_id = episode.id
                           AND preferred_fe.is_missing = 0
                         ORDER BY preferred.is_default DESC, preferred.id
                         LIMIT 1
                     )
                     JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                     JOIN library_roots lr ON lr.id = fe.library_root_id
                     WHERE fe.is_missing = 0
                     ORDER BY t.target_id
                     LIMIT ?
                     ) episode_page
                 ), selected_sources AS (
                     SELECT * FROM source_candidates
                     WHERE source_order = (SELECT MIN(source_order) FROM source_candidates)
                     ORDER BY target_id
                     LIMIT ?
                 )
                 SELECT job.job_type AS job_type,
                        job.status AS job_status,
                        job.auto_metadata_match AS auto_metadata_match,
                        CASE WHEN EXISTS(SELECT 1 FROM pending_targets) THEN 1 ELSE 0 END
                            AS has_pending,
                        COALESCE(selected.target_kind, '') AS target_kind,
                        COALESCE(selected.source_id, '') AS source_id,
                        COALESCE(selected.item_id, '') AS item_id,
                        COALESCE(selected.probe_status, '') AS probe_status,
                        COALESCE(selected.root_path, '') AS root_path,
                        COALESCE(selected.relative_path, '') AS relative_path,
                        COALESCE(selected.series_id, '') AS series_id,
                        COALESCE(selected.season_id, '') AS season_id,
                        selected.season_number
                 FROM (SELECT 1 AS singleton) singleton
                 LEFT JOIN job_lifecycle job ON TRUE
                 LEFT JOIN selected_sources selected ON TRUE
                 ORDER BY selected.target_id",
            )
            .bind(job_id)
            .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
            .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
            .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
            .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
            .fetch_all(&self.pool)
            .await
            .map_err(self.sqlx_err())?;

        let (job_type, job_status, auto_metadata_match, has_pending) = rows
            .first()
            .map(|row| {
                (
                    row.job_type.clone(),
                    row.job_status.clone(),
                    row.auto_metadata_match.unwrap_or_default() != 0,
                    row.has_pending != 0,
                )
            })
            .unwrap_or((None, None, false, false));
        let mut movies = Vec::new();
        let mut home_videos = Vec::new();
        let mut episodes = Vec::new();
        for row in rows {
            if row.target_kind.is_empty() {
                continue;
            }
            match row.target_kind.as_str() {
                "MOVIE" => movies.push(row.media_source_path()),
                "VIDEO" => home_videos.push(row.media_source_path()),
                "EPISODE" => episodes.push(row.series_metadata_source()),
                _ => {
                    return Err(StorageError::Conflict(
                        "unknown local metadata target type".to_owned(),
                    ));
                }
            }
        }
        let sources = if !movies.is_empty() {
            StoredScanJobMetadataSources::Movies(movies)
        } else if !home_videos.is_empty() {
            StoredScanJobMetadataSources::HomeVideos(home_videos)
        } else if !episodes.is_empty() {
            StoredScanJobMetadataSources::Episodes(episodes)
        } else {
            StoredScanJobMetadataSources::None
        };
        Ok(StoredScanJobMetadataPage {
            job_type,
            job_status,
            auto_metadata_match,
            has_pending,
            sources,
        })
    }

    pub(crate) async fn list_scan_job_target_movie_items_page(
        &self,
        job_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<StoredMediaSourcePath>, StorageError> {
        self.query_as::<MediaSourcePathRow>(
            "SELECT ms.id AS source_id, ms.item_id, ms.probe_status,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM scan_job_targets t
             JOIN media_items mi ON mi.id = t.item_id
             JOIN media_sources ms ON ms.id = (
                 SELECT preferred.id FROM media_sources preferred
                 JOIN filesystem_entries preferred_fe
                   ON preferred_fe.id = preferred.filesystem_entry_id
                 WHERE preferred.item_id = t.item_id
                   AND preferred_fe.is_missing = 0
                 ORDER BY preferred.is_default DESC, preferred.id
                 LIMIT 1
             )
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE t.job_id = ? AND t.target_type = 'ITEM'
               AND t.metadata_state = 'PENDING'
               AND mi.item_type = 'MOVIE'
               AND fe.is_missing = 0
             ORDER BY t.target_id
             LIMIT ? OFFSET ?",
        )
        .bind(job_id)
        .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
        .bind(offset.max(0))
        .fetch_all(&self.pool)
        .await
        .map(|rows| rows.into_iter().map(StoredMediaSourcePath::from).collect())
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn list_scan_job_target_home_video_items_page(
        &self,
        job_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<StoredMediaSourcePath>, StorageError> {
        self.query_as::<MediaSourcePathRow>(
            "SELECT ms.id AS source_id, ms.item_id, ms.probe_status,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM scan_job_targets t
             JOIN media_items mi ON mi.id = t.item_id
             JOIN media_sources ms ON ms.id = (
                 SELECT preferred.id FROM media_sources preferred
                 JOIN filesystem_entries preferred_fe
                   ON preferred_fe.id = preferred.filesystem_entry_id
                 WHERE preferred.item_id = t.item_id
                   AND preferred_fe.is_missing = 0
                 ORDER BY preferred.is_default DESC, preferred.id
                 LIMIT 1
             )
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE t.job_id = ? AND t.target_type = 'ITEM'
               AND t.metadata_state = 'PENDING'
               AND mi.item_type = 'VIDEO'
               AND fe.is_missing = 0
             ORDER BY t.target_id
             LIMIT ? OFFSET ?",
        )
        .bind(job_id)
        .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
        .bind(offset.max(0))
        .fetch_all(&self.pool)
        .await
        .map(|rows| rows.into_iter().map(StoredMediaSourcePath::from).collect())
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn list_scan_job_target_series_items_page(
        &self,
        job_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<StoredSeriesMetadataSource>, StorageError> {
        self.query_as::<SeriesMetadataSourceRow>(
            "SELECT series.id AS series_id, season.id AS season_id,
                    episode.id AS episode_id, season.season_number,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM scan_job_targets t
             JOIN media_items episode ON episode.id = t.item_id
             JOIN media_items season ON season.id = episode.parent_id
             JOIN media_items series ON series.id = episode.series_id
             JOIN media_sources ms ON ms.id = (
                 SELECT preferred.id FROM media_sources preferred
                 JOIN filesystem_entries preferred_fe
                   ON preferred_fe.id = preferred.filesystem_entry_id
                 WHERE preferred.item_id = episode.id
                   AND preferred_fe.is_missing = 0
                 ORDER BY preferred.is_default DESC, preferred.id
                 LIMIT 1
             )
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE t.job_id = ? AND t.target_type = 'ITEM'
               AND t.metadata_state = 'PENDING'
               AND episode.item_type = 'EPISODE'
               AND season.item_type = 'SEASON'
               AND series.item_type = 'SERIES'
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
                .map(StoredSeriesMetadataSource::from)
                .collect()
        })
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn local_metadata_completion_wait_state(
        &self,
        job_id: &str,
    ) -> Result<(bool, bool), StorageError> {
        // Return (has_pending_targets, job_missing_or_cancelled) from one read.
        let row = self
            .query_as::<LocalMetadataCompletionWaitRow>(
                "WITH completion_state AS (
                    SELECT CASE WHEN EXISTS (
                        SELECT 1 FROM scan_job_targets
                        WHERE job_id = ? AND target_type = 'ITEM'
                          AND metadata_state = 'PENDING'
                    ) THEN 1 ELSE 0 END AS has_pending_targets
                 )
                 SELECT has_pending_targets,
                    CASE WHEN has_pending_targets = 1 AND NOT EXISTS (
                        SELECT 1 FROM scan_jobs
                        WHERE id = ? AND cancel_requested = 0
                    ) THEN 1 ELSE 0 END AS job_missing_or_cancelled
                 FROM completion_state",
            )
            .bind(job_id)
            .bind(job_id)
            .fetch_one(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        Ok(row.into_wait_state())
    }

    pub(crate) async fn mark_pending_scan_job_metadata_targets_failed(
        &self,
        job_id: &str,
        error: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE scan_job_targets
             SET metadata_state = 'FAILED', error = ?, updated_at = unixepoch()
             WHERE job_id = ? AND target_type = 'ITEM'
               AND metadata_state = 'PENDING'",
        )
        .bind(error)
        .bind(job_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn list_pending_local_metadata_item_ids(
        &self,
        item_ids: &[String],
    ) -> Result<HashSet<String>, StorageError> {
        let mut pending = HashSet::new();
        for chunk in item_ids.chunks(SCAN_DML_CHUNK_SIZE) {
            if chunk.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT DISTINCT t.item_id
                 FROM scan_job_targets t
                 JOIN scan_jobs sj ON sj.id = t.job_id
                 WHERE t.target_type = 'ITEM' AND t.metadata_state = 'PENDING'
                   AND sj.job_type = 'RECONCILE_LIBRARY'
                   AND (
                       sj.status IN ('PENDING', 'RUNNING')
                       OR (sj.status = 'COMPLETED' AND sj.scan_phase = 'POSTPROCESSING')
                   )
                   AND t.item_id IN ({placeholders})"
            );
            let mut statement =
                self.query_as::<PendingLocalMetadataItemRow>(sqlx::AssertSqlSafe(query));
            for item_id in chunk {
                statement = statement.bind(item_id);
            }
            let rows = statement
                .fetch_all(&self.pool)
                .await
                .map_err(self.sqlx_err())?;
            pending.extend(rows.into_iter().map(|row| row.item_id));
        }
        Ok(pending)
    }

    pub(crate) async fn mark_scan_job_target_stage(
        &self,
        job_id: &str,
        target_type: &str,
        target_ids: &[String],
        stage: &str,
        state: &str,
    ) -> Result<(), StorageError> {
        if target_ids.is_empty() {
            return Ok(());
        }
        let column = match stage {
            "PROBE" => "probe_state",
            "METADATA" => "metadata_state",
            "THUMBNAIL" => "thumbnail_state",
            _ => {
                return Err(StorageError::Conflict(
                    "invalid scan target stage".to_owned(),
                ));
            }
        };
        for chunk in target_ids.chunks(SCAN_DML_CHUNK_SIZE) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "UPDATE scan_job_targets
                 SET {column} = ?, updated_at = unixepoch()
                 WHERE job_id = ? AND target_type = ? AND target_id IN ({placeholders})
                   AND {column} <> ?"
            );
            let mut statement = self
                .query(sqlx::AssertSqlSafe(query))
                .bind(state)
                .bind(job_id)
                .bind(target_type);
            for target_id in chunk {
                statement = statement.bind(target_id);
            }
            statement = statement.bind(state);
            statement
                .execute(&self.pool)
                .await
                .map_err(self.sqlx_err())?;
        }
        Ok(())
    }

    pub(crate) async fn skip_pending_scan_job_target_stage(
        &self,
        job_id: &str,
        target_type: &str,
        stage: &str,
    ) -> Result<(), StorageError> {
        let column = match stage {
            "PROBE" => "probe_state",
            "METADATA" => "metadata_state",
            "THUMBNAIL" => "thumbnail_state",
            _ => {
                return Err(StorageError::Conflict(
                    "invalid scan target stage".to_owned(),
                ));
            }
        };
        let query = format!(
            "UPDATE scan_job_targets
             SET {column} = 'SKIPPED', updated_at = unixepoch()
             WHERE job_id = ? AND target_type = ? AND {column} = 'PENDING'"
        );
        self.query(sqlx::AssertSqlSafe(query))
            .bind(job_id)
            .bind(target_type)
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(self.sqlx_err())
    }

    pub(crate) async fn ensure_scan_job_thumbnail_targets(
        &self,
        job_id: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO scan_job_targets (
                 job_id, target_type, target_id, item_id, change_kind,
                 probe_state, metadata_state, thumbnail_state
             )
             SELECT sj.id, 'ITEM', mi.id, mi.id, 'CHANGED',
                    'SKIPPED', 'SKIPPED', 'PENDING'
             FROM scan_jobs sj
             JOIN media_items mi ON mi.library_id = sj.library_id
             JOIN media_sources ms ON ms.item_id = mi.id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             WHERE sj.id = ?
               AND mi.removed_at IS NULL
               AND ms.source_kind = 'LOCAL_FILE'
               AND fe.is_missing = 0
             GROUP BY sj.id, mi.id
             ON CONFLICT(job_id, target_type, target_id) DO NOTHING",
        )
        .bind(job_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(self.sqlx_err())
    }

    pub(crate) async fn clear_completed_scan_job_targets(
        &self,
        job_id: &str,
    ) -> Result<bool, StorageError> {
        let result = self
            .query(
                "DELETE FROM scan_job_targets
                 WHERE job_id = ?
                   AND probe_state NOT IN ('PENDING', 'FAILED')
                   AND metadata_state NOT IN ('PENDING', 'FAILED')
                   AND thumbnail_state NOT IN ('PENDING', 'FAILED')",
            )
            .bind(job_id)
            .execute(&self.pool)
            .await
            .map_err(self.sqlx_err())?;
        Ok(result.rows_affected() > 0)
    }

    pub(crate) async fn retry_failed_scan_job_targets(
        &self,
        job_id: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE media_sources
             SET probe_status = 'PENDING', probe_error = NULL, updated_at = unixepoch()
             WHERE source_kind = 'LOCAL_FILE' AND id IN (
                 SELECT source_id FROM scan_job_targets
                 WHERE job_id = ? AND target_type = 'SOURCE'
                   AND probe_state = 'FAILED' AND source_id IS NOT NULL
             )",
        )
        .bind(job_id)
        .execute(&self.pool)
        .await
        .map_err(self.sqlx_err())?;
        self.query(
            "UPDATE scan_job_targets
             SET probe_state = CASE WHEN probe_state = 'FAILED' THEN 'PENDING' ELSE probe_state END,
                 metadata_state = CASE WHEN metadata_state = 'FAILED' THEN 'PENDING' ELSE metadata_state END,
                 thumbnail_state = CASE WHEN thumbnail_state = 'FAILED' THEN 'PENDING' ELSE thumbnail_state END,
                 updated_at = unixepoch()
             WHERE job_id = ?
               AND (probe_state = 'FAILED'
                    OR metadata_state = 'FAILED'
                    OR thumbnail_state = 'FAILED')",
        )
        .bind(job_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(self.sqlx_err())
    }
}

#[cfg(test)]
mod retry_diagnostic_tests {
    use super::*;

    #[tokio::test]
    async fn scan_retry_diagnostics_use_unfinished_stage_indexes()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let database = Database::connect(&crate::config::Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp.path().join("config"),
        })
        .await?;
        let sql =
            format!("EXPLAIN QUERY PLAN SELECT sj.id FROM scan_jobs sj WHERE {UNFINISHED_TARGETS}");
        let rows = database
            .query(sqlx::AssertSqlSafe(sql))
            .fetch_all(database.pool())
            .await?;
        let plan = rows
            .iter()
            .map(|row| row.get::<String, _>("detail"))
            .collect::<Vec<_>>()
            .join("\n");
        for stage in ["probe", "metadata", "thumbnail"] {
            assert!(
                plan.contains(&format!("idx_scan_job_targets_{stage}")),
                "{plan}"
            );
        }
        Ok(())
    }
}

/// Decoding tests for the row structs above: one seed drives every scan-target read so a column
/// rename or reorder shows up as a decode failure here instead of a panic in production.
#[cfg(test)]
mod row_decode_tests {
    use super::*;

    const LIBRARY_ID: &str = "library-scan-targets";
    const ROOT_ID: &str = "root-scan-targets";
    const JOB_ID: &str = "job-scan-targets";
    const MOVIE_ID: &str = "target-movie";
    const VIDEO_ID: &str = "target-video";
    const SERIES_ID: &str = "target-series";
    const SEASON_ID: &str = "target-season";
    const EPISODE_ID: &str = "target-episode";

    async fn test_database() -> Result<(tempfile::TempDir, Database), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let database = Database::connect(&crate::config::Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp.path().join("config"),
        })
        .await?;
        Ok((temp, database))
    }

    /// One library root holding a movie, a home video and an episode, each with a local source and
    /// one scan target, so every target query decodes exactly one row per kind.
    async fn seed_scan_targets(database: &Database) -> Result<(), sqlx::Error> {
        database
            .query("INSERT INTO libraries (id, name, kind) VALUES (?, 'Targets', 'MIXED')")
            .bind(LIBRARY_ID)
            .execute(database.pool())
            .await?;
        database
            .query(
                "INSERT INTO library_roots (
                     id, library_id, canonical_path, display_path, is_available, is_writable
                 ) VALUES (?, ?, '/srv/targets', '/srv/targets', 1, 1)",
            )
            .bind(ROOT_ID)
            .bind(LIBRARY_ID)
            .execute(database.pool())
            .await?;
        for (entry_id, relative_path) in [
            ("fe-movie", "Movies/Movie.2024.mkv"),
            ("fe-video", "Home/Clip.mkv"),
            ("fe-episode", "Shows/Show/S01E01.mkv"),
        ] {
            database
                .query(
                    "INSERT INTO filesystem_entries (
                         id, library_root_id, relative_path, entry_kind, size, modified_at,
                         last_seen_generation
                     ) VALUES (?, ?, ?, 'FILE', 1024, 100, 'generation')",
                )
                .bind(entry_id)
                .bind(ROOT_ID)
                .bind(relative_path)
                .execute(database.pool())
                .await?;
        }
        for (item_id, item_type, parent_id, series_id, season_number, title) in [
            (MOVIE_ID, "MOVIE", None, None, None, "Movie"),
            (VIDEO_ID, "VIDEO", None, None, None, "Clip"),
            (SERIES_ID, "SERIES", None, None, None, "Show"),
            (
                SEASON_ID,
                "SEASON",
                Some(SERIES_ID),
                Some(SERIES_ID),
                Some(1),
                "Season 1",
            ),
            (
                EPISODE_ID,
                "EPISODE",
                Some(SEASON_ID),
                Some(SERIES_ID),
                None,
                "Pilot",
            ),
        ] {
            database
                .query(
                    "INSERT INTO media_items (
                         id, library_id, item_type, parent_id, series_id, season_number,
                         title, sort_title, identification_status
                     ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'LOCAL_CONFIRMED')",
                )
                .bind(item_id)
                .bind(LIBRARY_ID)
                .bind(item_type)
                .bind(parent_id)
                .bind(series_id)
                .bind(season_number)
                .bind(title)
                .bind(title.to_lowercase())
                .execute(database.pool())
                .await?;
        }
        for (source_id, item_id, entry_id, probe_status) in [
            ("source-movie", MOVIE_ID, "fe-movie", "PENDING"),
            ("source-video", VIDEO_ID, "fe-video", "DONE"),
            ("source-episode", EPISODE_ID, "fe-episode", "DONE"),
        ] {
            database
                .query(
                    "INSERT INTO media_sources (
                         id, item_id, source_kind, filesystem_entry_id, is_default, probe_status
                     ) VALUES (?, ?, 'LOCAL_FILE', ?, 1, ?)",
                )
                .bind(source_id)
                .bind(item_id)
                .bind(entry_id)
                .bind(probe_status)
                .execute(database.pool())
                .await?;
        }
        database
            .query(
                "INSERT INTO scan_jobs (id, library_id, job_type, status, generation)
                 VALUES (?, ?, 'RECONCILE_LIBRARY', 'PENDING', 'generation')",
            )
            .bind(JOB_ID)
            .bind(LIBRARY_ID)
            .execute(database.pool())
            .await?;
        database
            .query(
                "INSERT INTO scan_job_targets (
                     job_id, target_type, target_id, source_id, item_id, change_kind,
                     probe_state, metadata_state, thumbnail_state
                 ) VALUES
                     (?, 'SOURCE', 'source-movie', 'source-movie', ?, 'NEW', 'PENDING', 'SKIPPED', 'SKIPPED'),
                     (?, 'ITEM', ?, NULL, ?, 'NEW', 'SKIPPED', 'PENDING', 'SKIPPED'),
                     (?, 'ITEM', ?, NULL, ?, 'NEW', 'SKIPPED', 'PENDING', 'SKIPPED'),
                     (?, 'ITEM', ?, NULL, ?, 'NEW', 'SKIPPED', 'PENDING', 'SKIPPED')",
            )
            .bind(JOB_ID)
            .bind(MOVIE_ID)
            .bind(JOB_ID)
            .bind(MOVIE_ID)
            .bind(MOVIE_ID)
            .bind(JOB_ID)
            .bind(VIDEO_ID)
            .bind(VIDEO_ID)
            .bind(JOB_ID)
            .bind(EPISODE_ID)
            .bind(EPISODE_ID)
            .execute(database.pool())
            .await?;
        Ok(())
    }

    #[tokio::test]
    async fn scan_target_pages_decode_seeded_rows() -> Result<(), Box<dyn std::error::Error>> {
        let (_temp, database) = test_database().await?;
        seed_scan_targets(&database).await?;

        let sources = database
            .list_scan_job_target_sources_page(JOB_ID, 10, 0)
            .await?;
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].source_id, "source-movie");
        assert_eq!(sources[0].item_id, MOVIE_ID);
        assert_eq!(sources[0].probe_status, "PENDING");
        assert_eq!(sources[0].root_path, "/srv/targets");
        assert_eq!(sources[0].relative_path, "Movies/Movie.2024.mkv");

        let movies = database
            .list_scan_job_target_movie_items_page(JOB_ID, 10, 0)
            .await?;
        assert_eq!(movies.len(), 1);
        assert_eq!(movies[0].source_id, "source-movie");
        assert_eq!(movies[0].item_id, MOVIE_ID);
        assert_eq!(movies[0].probe_status, "PENDING");

        let home_videos = database
            .list_scan_job_target_home_video_items_page(JOB_ID, 10, 0)
            .await?;
        assert_eq!(home_videos.len(), 1);
        assert_eq!(home_videos[0].source_id, "source-video");
        assert_eq!(home_videos[0].item_id, VIDEO_ID);
        assert_eq!(home_videos[0].relative_path, "Home/Clip.mkv");

        let episodes = database
            .list_scan_job_target_series_items_page(JOB_ID, 10, 0)
            .await?;
        assert_eq!(episodes.len(), 1);
        assert_eq!(episodes[0].series_id, SERIES_ID);
        assert_eq!(episodes[0].season_id, SEASON_ID);
        assert_eq!(episodes[0].episode_id, EPISODE_ID);
        assert_eq!(episodes[0].season_number, Some(1));
        assert_eq!(episodes[0].root_path, "/srv/targets");
        assert_eq!(episodes[0].relative_path, "Shows/Show/S01E01.mkv");
        Ok(())
    }

    #[tokio::test]
    async fn scan_target_metadata_page_decodes_job_and_source_rows()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp, database) = test_database().await?;
        seed_scan_targets(&database).await?;

        let page = database.load_scan_job_metadata_page(JOB_ID, 8).await?;
        assert_eq!(page.job_type.as_deref(), Some("RECONCILE_LIBRARY"));
        assert_eq!(page.job_status.as_deref(), Some("PENDING"));
        assert!(!page.auto_metadata_match);
        assert!(page.has_pending);
        let StoredScanJobMetadataSources::Movies(movies) = page.sources else {
            return Err("expected the movie source page".into());
        };
        assert_eq!(movies.len(), 1);
        assert_eq!(movies[0].source_id, "source-movie");
        assert_eq!(movies[0].item_id, MOVIE_ID);
        assert_eq!(movies[0].root_path, "/srv/targets");
        assert_eq!(movies[0].relative_path, "Movies/Movie.2024.mkv");
        Ok(())
    }

    #[tokio::test]
    async fn scan_target_flag_rows_decode_seeded_states() -> Result<(), Box<dyn std::error::Error>>
    {
        let (_temp, database) = test_database().await?;
        seed_scan_targets(&database).await?;

        let states = database.scan_job_retry_states(&[JOB_ID.to_owned()]).await?;
        let state = states.get(JOB_ID).ok_or("missing retry state")?;
        assert!(state.has_unfinished_targets);
        assert!(state.has_active_job);
        assert!(!state.has_postprocessing_issues);
        assert!(!state.targets_need_materialization);
        assert!(!state.has_legacy_work);

        let pending = database
            .list_pending_local_metadata_item_ids(&[
                MOVIE_ID.to_owned(),
                VIDEO_ID.to_owned(),
                EPISODE_ID.to_owned(),
                "missing-item".to_owned(),
            ])
            .await?;
        assert_eq!(
            pending,
            [MOVIE_ID, VIDEO_ID, EPISODE_ID]
                .into_iter()
                .map(str::to_owned)
                .collect::<HashSet<_>>()
        );

        assert_eq!(
            database
                .local_metadata_completion_wait_state(JOB_ID)
                .await?,
            (true, false)
        );
        Ok(())
    }

    #[tokio::test]
    async fn metadata_page_row_reports_a_missing_column_instead_of_panicking()
    -> Result<(), Box<dyn std::error::Error>> {
        use sqlx::FromRow;

        let (_temp, database) = test_database().await?;
        let row = sqlx::query("SELECT 1 AS other")
            .fetch_one(database.pool())
            .await?;
        let error = ScanJobMetadataPageRow::from_row(&row).expect_err("must not panic");
        assert!(
            matches!(error, sqlx::Error::ColumnNotFound(_)),
            "unexpected decode error: {error}"
        );
        Ok(())
    }
}
