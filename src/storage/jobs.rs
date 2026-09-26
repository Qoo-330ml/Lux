use super::*;
use std::time::Instant;

const SHUTDOWN_JOB_ERROR_CODE: &str = "SERVER_SHUTDOWN";
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

fn prune_sidecar_directories(mut directories: Vec<String>) -> Vec<String> {
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

fn sidecar_target_query(values: &str) -> String {
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

fn valid_scan_manifest_transition(expected: &str, next: &str) -> bool {
    matches!(
        (expected, next),
        ("DISCOVERING", "READY_TO_DIFF" | "FAILED" | "CANCELLED")
            | ("READY_TO_DIFF", "APPLYING" | "FAILED" | "CANCELLED")
            | ("APPLYING", "INDEXED" | "FAILED" | "CANCELLED")
            | ("INDEXED", "POSTPROCESSING" | "COMPLETED" | "FAILED")
            | ("POSTPROCESSING", "COMPLETED" | "FAILED" | "CANCELLED")
    )
}

fn validate_scan_manifest(manifest: &NewScanManifest<'_>) -> Result<i64, StorageError> {
    let root_count = i64::try_from(manifest.roots.len())
        .map_err(|_| StorageError::Conflict("manifest root count overflow".to_owned()))?;
    let mut unique_roots = std::collections::HashSet::with_capacity(manifest.roots.len());
    if manifest
        .roots
        .iter()
        .any(|root| !unique_roots.insert(root.library_root_id))
    {
        return Err(StorageError::Conflict(
            "manifest root list contains duplicates".to_owned(),
        ));
    }
    Ok(root_count)
}

impl Database {
    /// Marks every unfinished persistent background job as cancelled.
    ///
    /// This is deliberately one transaction so startup and shutdown never
    /// leave a mixed set of job tables eligible for automatic recovery.
    pub async fn cancel_incomplete_jobs_for_shutdown(&self) -> Result<u64, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let mut cancelled = 0_u64;

        let legacy_scan_ids: Vec<String> = self
            .query_scalar(
                "SELECT id FROM scan_jobs
                 WHERE job_type = 'RECONCILE_LIBRARY'
                   AND (
                       status IN ('PENDING', 'RUNNING')
                       OR (status = 'COMPLETED' AND scan_phase = 'POSTPROCESSING')
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM scan_manifests WHERE job_id = scan_jobs.id
                   )",
            )
            .fetch_all(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        for job_id in &legacy_scan_ids {
            let result = self
                .query(
                    "UPDATE scan_jobs
                     SET status = 'CANCELLED', cancel_requested = 0, error = ?, cursor = NULL,
                         current_item = NULL, scan_phase = 'IDLE', finished_at = unixepoch(),
                         updated_at = unixepoch()
                     WHERE id = ? AND status IN ('PENDING', 'RUNNING', 'COMPLETED')",
                )
                .bind(Self::LEGACY_SCAN_REQUIRES_NEW_MANIFEST)
                .bind(job_id)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            cancelled = cancelled.saturating_add(result.rows_affected());
            if result.rows_affected() == 1 {
                self.query(
                    "INSERT INTO scan_job_events (
                         id, job_id, level, event_code, message, details_json
                     ) VALUES (?, ?, 'WARN', ?, ?, '{}')",
                )
                .bind(uuid::Uuid::now_v7().to_string())
                .bind(job_id)
                .bind(Self::LEGACY_SCAN_REQUIRES_NEW_MANIFEST)
                .bind("旧版全量扫描没有 Manifest 检查点；请重试以创建新的 Manifest 扫描")
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            }
        }

        for query in [
            "UPDATE scan_jobs
             SET status = 'CANCELLED', cancel_requested = 0, error = ?, cursor = NULL,
                 current_item = NULL, scan_phase = 'IDLE', finished_at = unixepoch(),
                 updated_at = unixepoch()
             WHERE (status IN ('PENDING', 'RUNNING')
                OR (status = 'COMPLETED' AND scan_phase = 'POSTPROCESSING'))",
            "UPDATE strm_probe_jobs
             SET status = 'CANCELLED', cancel_requested = 0, error = ?, finished_at = unixepoch(),
                 updated_at = unixepoch()
             WHERE status IN ('PENDING', 'RUNNING')",
            "UPDATE chapter_detection_jobs
             SET status = 'CANCELLED', cancel_requested = 0, error = ?, finished_at = unixepoch(),
                 updated_at = unixepoch()
             WHERE status IN ('PENDING', 'RUNNING')",
            "UPDATE library_cover_jobs
             SET status = 'CANCELLED', error = ?, finished_at = unixepoch(), updated_at = unixepoch()
             WHERE status IN ('PENDING', 'RUNNING')",
            "UPDATE danmaku_match_jobs
             SET status = 'CANCELLED', cancel_requested = 0, error = ?, finished_at = unixepoch(),
                 updated_at = unixepoch()
             WHERE status IN ('PENDING', 'RUNNING')",
            "UPDATE metadata_reidentify_jobs
             SET status = 'CANCELLED', cancel_requested = 0, error = ?, finished_at = unixepoch(),
                 updated_at = unixepoch()
             WHERE status IN ('QUEUED', 'RUNNING')",
            "UPDATE emby_migration_jobs
             SET status = 'CANCELLED', cancel_requested = 0, error = ?, finished_at = unixepoch(),
                 updated_at = unixepoch()
             WHERE status IN ('PENDING', 'RUNNING')",
            "UPDATE person_index_rebuild_jobs
             SET status = 'CANCELLED', cancel_requested = 0, run_token = NULL, error = ?,
                 finished_at = unixepoch(), updated_at = unixepoch()
             WHERE status IN ('QUEUED', 'RUNNING')",
        ] {
            let result = self
                .query(query)
                .bind(SHUTDOWN_JOB_ERROR_CODE)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            cancelled = cancelled.saturating_add(result.rows_affected());
        }

        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(cancelled)
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

    pub(crate) async fn insert_library_root(
        &self,
        root: NewLibraryRoot<'_>,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO library_roots (
                id, library_id, canonical_path, display_path, is_available, is_writable
            ) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(root.id)
        .bind(root.library_id)
        .bind(root.canonical_path)
        .bind(root.display_path)
        .bind(database_flag(root.is_available))
        .bind(database_flag(root.is_writable))
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_library_roots(
        &self,
        library_id: &str,
    ) -> Result<Vec<StoredLibraryRoot>, StorageError> {
        self.query(
            "SELECT id, library_id, canonical_path, display_path,
                    is_available, is_writable, last_checked_at,
                    unavailable_since, scan_cursor
             FROM library_roots WHERE library_id = ?
             ORDER BY canonical_path, id",
        )
        .bind(library_id)
        .fetch_all(&self.pool)
        .await
        .map(|rows| rows.into_iter().map(stored_library_root).collect())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_library_roots_by_ids(
        &self,
        root_ids: &[String],
    ) -> Result<HashMap<String, StoredLibraryRoot>, StorageError> {
        if root_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let mut roots = HashMap::with_capacity(root_ids.len());
        for root_ids in root_ids.chunks(500) {
            let placeholders = std::iter::repeat_n("?", root_ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT id, library_id, canonical_path, display_path,
                        is_available, is_writable, last_checked_at,
                        unavailable_since, scan_cursor
                 FROM library_roots
                 WHERE id IN ({placeholders})"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for root_id in root_ids {
                statement = statement.bind(root_id);
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
                let root = stored_library_root(row);
                roots.insert(root.id.clone(), root);
            }
        }
        Ok(roots)
    }

    pub(crate) async fn list_library_roots_by_library_ids(
        &self,
        library_ids: &[String],
    ) -> Result<HashMap<String, Vec<StoredLibraryRoot>>, StorageError> {
        if library_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let mut roots = HashMap::<String, Vec<StoredLibraryRoot>>::new();
        for library_ids in library_ids.chunks(500) {
            let placeholders = std::iter::repeat_n("?", library_ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT id, library_id, canonical_path, display_path,
                        is_available, is_writable, last_checked_at,
                        unavailable_since, scan_cursor
                 FROM library_roots
                 WHERE library_id IN ({placeholders})
                 ORDER BY library_id, canonical_path, id"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for library_id in library_ids {
                statement = statement.bind(library_id);
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
                let library_id: String = row.get("library_id");
                roots
                    .entry(library_id)
                    .or_default()
                    .push(stored_library_root(row));
            }
        }
        Ok(roots)
    }

    pub(crate) async fn delete_library_root(
        &self,
        library_id: &str,
        root_id: &str,
    ) -> Result<bool, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let history = self
            .query(
                "INSERT INTO library_root_history (library_id, canonical_path, root_id)
                 SELECT library_id, canonical_path, id
                 FROM library_roots
                 WHERE id = ? AND library_id = ?
                 ON CONFLICT(library_id, canonical_path) DO UPDATE SET
                     root_id = excluded.root_id,
                     deleted_at = unixepoch()",
            )
            .bind(root_id)
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if history.rows_affected() == 0 {
            transaction
                .rollback()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(false);
        }
        self.query("DELETE FROM library_roots WHERE id = ? AND library_id = ?")
            .bind(root_id)
            .bind(library_id)
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

    pub(crate) async fn find_deleted_library_root_id(
        &self,
        library_id: &str,
        canonical_path: &str,
    ) -> Result<Option<String>, StorageError> {
        self.query_scalar(
            "SELECT root_id
             FROM library_root_history
             WHERE library_id = ? AND canonical_path = ?",
        )
        .bind(library_id)
        .bind(canonical_path)
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn delete_library_root_history(
        &self,
        library_id: &str,
        canonical_path: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "DELETE FROM library_root_history
             WHERE library_id = ? AND canonical_path = ?",
        )
        .bind(library_id)
        .bind(canonical_path)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_all_library_roots(
        &self,
    ) -> Result<Vec<StoredLibraryRoot>, StorageError> {
        self.query(
            "SELECT id, library_id, canonical_path, display_path,
                    is_available, is_writable, last_checked_at,
                    unavailable_since, scan_cursor
             FROM library_roots ORDER BY canonical_path, id",
        )
        .fetch_all(&self.pool)
        .await
        .map(|rows| rows.into_iter().map(stored_library_root).collect())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_enabled_library_roots(
        &self,
    ) -> Result<Vec<StoredLibraryRoot>, StorageError> {
        self.query(
            "SELECT lr.id, lr.library_id, lr.canonical_path, lr.display_path,
                    lr.is_available, lr.is_writable, lr.last_checked_at,
                    lr.unavailable_since, lr.scan_cursor
             FROM library_roots lr
             JOIN libraries l ON l.id = lr.library_id
             WHERE l.is_enabled = 1
               AND l.realtime_watch_enabled = 1
             ORDER BY lr.canonical_path, lr.id",
        )
        .fetch_all(&self.pool)
        .await
        .map(|rows| rows.into_iter().map(stored_library_root).collect())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn create_scan_job(
        &self,
        id: &str,
        library_id: &str,
        job_type: &str,
        generation: &str,
        total_count: i64,
        auto_metadata_match: bool,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO scan_jobs (
                id, library_id, job_type, status, generation, total_count, auto_metadata_match
             ) VALUES (?, ?, ?, 'PENDING', ?, ?, ?)",
        )
        .bind(id)
        .bind(library_id)
        .bind(job_type)
        .bind(generation)
        .bind(total_count)
        .bind(database_flag(auto_metadata_match))
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    #[cfg(test)]
    pub(crate) async fn create_scan_manifest(
        &self,
        manifest: &NewScanManifest<'_>,
    ) -> Result<(), StorageError> {
        let root_count = validate_scan_manifest(manifest)?;

        let mut transaction = self.begin_scan_write_transaction().await?;
        let existing: Option<(String, String)> = self
            .query_as("SELECT id, library_id FROM scan_manifests WHERE job_id = ?")
            .bind(manifest.job_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if let Some((existing_id, existing_library_id)) = existing {
            if existing_id != manifest.id || existing_library_id != manifest.library_id {
                return Err(StorageError::Conflict(
                    "scan job is already associated with a different manifest".to_owned(),
                ));
            }
            transaction
                .commit()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(());
        }

        let created = self
            .query(
                "INSERT INTO scan_manifests (id, job_id, library_id, state, root_count)
                 SELECT ?, sj.id, sj.library_id, 'DISCOVERING', ?
                 FROM scan_jobs sj
                 WHERE sj.id = ? AND sj.library_id = ?
                   AND sj.job_type = 'RECONCILE_LIBRARY'
                   AND sj.status IN ('PENDING', 'RUNNING')",
            )
            .bind(manifest.id)
            .bind(root_count)
            .bind(manifest.job_id)
            .bind(manifest.library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if created.rows_affected() != 1 {
            return Err(StorageError::Conflict(
                "manifest must reference an existing full-scan job in the same library".to_owned(),
            ));
        }

        for root in manifest.roots {
            let created_root = self
                .query(
                    "INSERT INTO scan_manifest_roots (
                         manifest_id, library_root_id, state, directory_count
                     )
                     SELECT ?, lr.id, 'PENDING', 1
                     FROM library_roots lr
                     WHERE lr.id = ? AND lr.library_id = ?",
                )
                .bind(manifest.id)
                .bind(root.library_root_id)
                .bind(manifest.library_id)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if created_root.rows_affected() != 1 {
                return Err(StorageError::Conflict(format!(
                    "library root {} does not belong to the manifest library",
                    root.library_root_id
                )));
            }
            self.query(
                "INSERT INTO scan_manifest_directories (
                     manifest_id, library_root_id, relative_path, state
                 ) VALUES (?, ?, '', 'PENDING')",
            )
            .bind(manifest.id)
            .bind(root.library_root_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }

        self.query(
            "UPDATE scan_manifests
             SET discovered_directory_count = ?, updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(root_count)
        .bind(manifest.id)
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

    pub(crate) async fn create_full_scan_manifest_job(
        &self,
        id: &str,
        generation: &str,
        auto_metadata_match: bool,
        manifest: &NewScanManifest<'_>,
        legacy_retry_job_id: Option<&str>,
    ) -> Result<(), StorageError> {
        if manifest.job_id != id {
            return Err(StorageError::Conflict(
                "manifest job id does not match the scan job".to_owned(),
            ));
        }
        let root_count = validate_scan_manifest(manifest)?;
        let mut transaction = self.begin_scan_write_transaction().await?;
        self.query(
            "INSERT INTO scan_jobs (
                id, library_id, job_type, status, generation, total_count,
                discovery_completed, auto_metadata_match
             ) VALUES (?, ?, 'RECONCILE_LIBRARY', 'PENDING', ?, 0, 0, ?)",
        )
        .bind(id)
        .bind(manifest.library_id)
        .bind(generation)
        .bind(database_flag(auto_metadata_match))
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        let created_manifest = self
            .query(
                "INSERT INTO scan_manifests (
                     id, job_id, library_id, state, workflow_version,
                     discovery_format_version, root_count, postprocessing_targets_ready
                 )
                 SELECT ?, sj.id, sj.library_id, 'DISCOVERING', 2, 3, ?, 0
                 FROM scan_jobs sj
                 WHERE sj.id = ? AND sj.library_id = ?
                   AND sj.job_type = 'RECONCILE_LIBRARY' AND sj.status = 'PENDING'",
            )
            .bind(manifest.id)
            .bind(root_count)
            .bind(id)
            .bind(manifest.library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if created_manifest.rows_affected() != 1 {
            return Err(StorageError::Conflict(
                "manifest must reference its new full-scan job".to_owned(),
            ));
        }
        for root in manifest.roots {
            let created_root = self
                .query(
                    "INSERT INTO scan_manifest_roots (
                         manifest_id, library_root_id, state, directory_count,
                         postprocessing_target_stage
                     )
                     SELECT ?, lr.id, 'PENDING', 1, 'NEW'
                     FROM library_roots lr
                     WHERE lr.id = ? AND lr.library_id = ?",
                )
                .bind(manifest.id)
                .bind(root.library_root_id)
                .bind(manifest.library_id)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if created_root.rows_affected() != 1 {
                return Err(StorageError::Conflict(format!(
                    "library root {} does not belong to the manifest library",
                    root.library_root_id
                )));
            }
            self.query(
                "INSERT INTO scan_manifest_directories (
                     manifest_id, library_root_id, relative_path, state
                 ) VALUES (?, ?, '', 'PENDING')",
            )
            .bind(manifest.id)
            .bind(root.library_root_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        self.query(
            "UPDATE scan_manifests
             SET discovered_directory_count = ?, updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(root_count)
        .bind(manifest.id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        if let Some(legacy_job_id) = legacy_retry_job_id {
            self.query(
                "INSERT INTO scan_job_targets (
                     job_id, target_type, target_id, source_id, item_id, change_kind,
                     probe_state, metadata_state, thumbnail_state
                 )
                 SELECT ?, target_type, target_id, source_id, item_id, change_kind,
                        CASE WHEN probe_state IN ('PENDING', 'FAILED')
                             THEN 'PENDING' ELSE probe_state END,
                        CASE WHEN metadata_state IN ('PENDING', 'FAILED')
                             THEN 'PENDING' ELSE metadata_state END,
                        CASE WHEN thumbnail_state IN ('PENDING', 'FAILED')
                             THEN 'PENDING' ELSE thumbnail_state END
                 FROM scan_job_targets
                 WHERE job_id = ?
                   AND (probe_state IN ('PENDING', 'FAILED')
                        OR metadata_state IN ('PENDING', 'FAILED')
                        OR thumbnail_state IN ('PENDING', 'FAILED'))
                 ON CONFLICT(job_id, target_type, target_id) DO NOTHING",
            )
            .bind(id)
            .bind(legacy_job_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
            self.query(
                "INSERT INTO scan_job_targets (
                     job_id, target_type, target_id, source_id, item_id, change_kind,
                     probe_state, metadata_state, thumbnail_state
                 )
                 SELECT ?, 'SOURCE', source.id, source.id, source.item_id, 'CHANGED',
                        'PENDING', 'SKIPPED', 'SKIPPED'
                 FROM reconciliation_scan_entries legacy
                 JOIN filesystem_entries entry
                   ON entry.library_root_id = legacy.library_root_id
                  AND entry.relative_path = legacy.relative_path
                 JOIN media_sources source ON source.filesystem_entry_id = entry.id
                 WHERE legacy.job_id = ? AND legacy.entry_type = 'FILE'
                   AND legacy.status = 'PENDING' AND entry.is_missing = 0
                 ON CONFLICT(job_id, target_type, target_id) DO NOTHING",
            )
            .bind(id)
            .bind(legacy_job_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
            self.query(
                "INSERT INTO scan_job_targets (
                     job_id, target_type, target_id, item_id, change_kind,
                     probe_state, metadata_state, thumbnail_state
                 )
                 SELECT ?, 'ITEM', source.item_id, source.item_id, 'CHANGED',
                        'SKIPPED', 'PENDING', 'PENDING'
                 FROM reconciliation_scan_entries legacy
                 JOIN filesystem_entries entry
                   ON entry.library_root_id = legacy.library_root_id
                  AND entry.relative_path = legacy.relative_path
                 JOIN media_sources source ON source.filesystem_entry_id = entry.id
                 WHERE legacy.job_id = ? AND legacy.entry_type = 'FILE'
                   AND legacy.status = 'PENDING' AND entry.is_missing = 0
                 GROUP BY source.item_id
                 ON CONFLICT(job_id, target_type, target_id) DO NOTHING",
            )
            .bind(id)
            .bind(legacy_job_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
            self.query("DELETE FROM scan_job_targets WHERE job_id = ?")
                .bind(legacy_job_id)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            self.query("DELETE FROM reconciliation_scan_entries WHERE job_id = ?")
                .bind(legacy_job_id)
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

    #[allow(dead_code)] // LUX-266 uses this to resume and report manifest progress.
    pub(crate) async fn get_scan_manifest(
        &self,
        id: &str,
    ) -> Result<Option<StoredScanManifest>, StorageError> {
        self.query(
            "SELECT id, job_id, library_id, state, workflow_version, discovery_format_version,
                    root_count,
                    discovered_directory_count, completed_directory_count,
                    observed_file_count, unchanged_count, add_count, change_count, remove_count,
                    reappeared_count, applied_delta_count, postprocessing_targets_ready
             FROM scan_manifests WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredScanManifest {
                id: row.get("id"),
                job_id: row.get("job_id"),
                library_id: row.get("library_id"),
                state: row.get("state"),
                workflow_version: row.get("workflow_version"),
                discovery_format_version: row.get("discovery_format_version"),
                root_count: row.get("root_count"),
                discovered_directory_count: row.get("discovered_directory_count"),
                completed_directory_count: row.get("completed_directory_count"),
                observed_file_count: row.get("observed_file_count"),
                unchanged_count: row.get("unchanged_count"),
                add_count: row.get("add_count"),
                change_count: row.get("change_count"),
                remove_count: row.get("remove_count"),
                reappeared_count: row.get("reappeared_count"),
                applied_delta_count: row.get("applied_delta_count"),
                postprocessing_targets_ready: row.get::<i64, _>("postprocessing_targets_ready")
                    != 0,
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn get_scan_manifest_by_job(
        &self,
        job_id: &str,
    ) -> Result<Option<StoredScanManifest>, StorageError> {
        self.query(
            "SELECT id, job_id, library_id, state, workflow_version, discovery_format_version,
                    root_count,
                    discovered_directory_count, completed_directory_count,
                    observed_file_count, unchanged_count, add_count, change_count, remove_count,
                    reappeared_count, applied_delta_count, postprocessing_targets_ready
             FROM scan_manifests WHERE job_id = ?",
        )
        .bind(job_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredScanManifest {
                id: row.get("id"),
                job_id: row.get("job_id"),
                library_id: row.get("library_id"),
                state: row.get("state"),
                workflow_version: row.get("workflow_version"),
                discovery_format_version: row.get("discovery_format_version"),
                root_count: row.get("root_count"),
                discovered_directory_count: row.get("discovered_directory_count"),
                completed_directory_count: row.get("completed_directory_count"),
                observed_file_count: row.get("observed_file_count"),
                unchanged_count: row.get("unchanged_count"),
                add_count: row.get("add_count"),
                change_count: row.get("change_count"),
                remove_count: row.get("remove_count"),
                reappeared_count: row.get("reappeared_count"),
                applied_delta_count: row.get("applied_delta_count"),
                postprocessing_targets_ready: row.get::<i64, _>("postprocessing_targets_ready")
                    != 0,
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_scan_manifest_postprocessing_roots(
        &self,
        manifest_id: &str,
    ) -> Result<Vec<StoredScanManifestPostprocessingRoot>, StorageError> {
        self.query(
            "SELECT root.library_root_id, library_root.canonical_path,
                    root.postprocessing_target_stage, root.postprocessing_target_cursor,
                    observed.device, observed.inode,
                    CASE WHEN EXISTS (
                        SELECT 1 FROM filesystem_entries entry
                        WHERE entry.library_root_id = root.library_root_id
                          AND entry.last_seen_generation = job.generation
                          AND entry.entry_kind = 'FILE'
                          AND entry.last_seen_change_kind = root.postprocessing_target_stage
                          AND entry.relative_path > COALESCE(root.postprocessing_target_cursor, '')
                    ) THEN 1 ELSE 0 END AS has_stage_rows,
                    CASE WHEN EXISTS (
                        SELECT 1 FROM filesystem_entries entry
                        JOIN scan_jobs job ON job.id = manifest.job_id
                        WHERE entry.library_root_id = root.library_root_id
                          AND entry.last_seen_generation = job.generation
                          AND entry.entry_kind = 'FILE'
                          AND entry.last_seen_change_kind IN ('NEW', 'CHANGED', 'SIDECAR')
                    ) THEN 1 ELSE 0 END AS has_positive_rows
             FROM scan_manifest_roots root
             JOIN scan_manifests manifest ON manifest.id = root.manifest_id
             JOIN scan_jobs job ON job.id = manifest.job_id
             JOIN library_roots library_root ON library_root.id = root.library_root_id
             LEFT JOIN scan_manifest_entries observed
               ON observed.manifest_id = root.manifest_id
              AND observed.library_root_id = root.library_root_id
              AND observed.relative_path = ''
              AND observed.observation_sequence = (
                  SELECT MAX(latest.observation_sequence)
                  FROM scan_manifest_entries latest
                  WHERE latest.manifest_id = root.manifest_id
                    AND latest.library_root_id = root.library_root_id
                    AND latest.relative_path = ''
              )
             WHERE root.manifest_id = ?
             ORDER BY root.library_root_id",
        )
        .bind(manifest_id)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredScanManifestPostprocessingRoot {
                    library_root_id: row.get("library_root_id"),
                    canonical_path: row.get("canonical_path"),
                    target_stage: row.get("postprocessing_target_stage"),
                    target_cursor: row.get("postprocessing_target_cursor"),
                    expected_device: row.get("device"),
                    expected_inode: row.get("inode"),
                    has_stage_rows: row.get::<i64, _>("has_stage_rows") != 0,
                    has_positive_rows: row.get::<i64, _>("has_positive_rows") != 0,
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

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
            || workflow != 2
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
        let source_result = self
            .query(
                "INSERT INTO scan_job_targets (
                     job_id, target_type, target_id, source_id, item_id, change_kind,
                     probe_state, metadata_state, thumbnail_state
                 )
                 SELECT ?, 'SOURCE', source.id, source.id, source.item_id,
                        entry.last_seen_change_kind, 'PENDING', 'SKIPPED', 'SKIPPED'
                 FROM filesystem_entries entry
                 JOIN media_sources source ON source.filesystem_entry_id = entry.id
                 WHERE entry.library_root_id = ? AND entry.last_seen_generation = ?
                   AND entry.entry_kind = 'FILE'
                   AND entry.last_seen_change_kind = ?
                   AND entry.relative_path > COALESCE(?, '')
                   AND entry.relative_path <= ?
                 ON CONFLICT(job_id, target_type, target_id) DO NOTHING",
            )
            .bind(job_id)
            .bind(library_root_id)
            .bind(generation)
            .bind(target_stage)
            .bind(after_relative_path)
            .bind(through_relative_path)
            .execute(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let item_result = self
            .query(
                "INSERT INTO scan_job_targets (
                     job_id, target_type, target_id, item_id, change_kind,
                     probe_state, metadata_state, thumbnail_state
                 )
                 SELECT ?, 'ITEM', source.item_id, source.item_id,
                        CASE WHEN EXISTS (
                            SELECT 1
                            FROM media_sources new_source
                            JOIN filesystem_entries new_entry
                              ON new_entry.id = new_source.filesystem_entry_id
                            WHERE new_source.item_id = source.item_id
                              AND new_entry.last_seen_generation = ?
                              AND new_entry.last_seen_change_kind = 'NEW'
                        ) THEN 'NEW' ELSE 'CHANGED' END,
                        'SKIPPED', 'PENDING', 'PENDING'
                 FROM filesystem_entries entry
                 JOIN media_sources source ON source.filesystem_entry_id = entry.id
                 WHERE entry.library_root_id = ? AND entry.last_seen_generation = ?
                   AND entry.entry_kind = 'FILE'
                   AND entry.last_seen_change_kind = ?
                   AND entry.relative_path > COALESCE(?, '')
                   AND entry.relative_path <= ?
                 GROUP BY source.item_id
                 ON CONFLICT(job_id, target_type, target_id) DO NOTHING",
            )
            .bind(job_id)
            .bind(generation)
            .bind(library_root_id)
            .bind(generation)
            .bind(target_stage)
            .bind(after_relative_path)
            .bind(through_relative_path)
            .execute(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(source_result.rows_affected() > 0 || item_result.rows_affected() > 0)
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
        let (workflow_version, discovery_format_version): (i64, i64) = self
            .query_as(
                "SELECT workflow_version, discovery_format_version FROM scan_manifests
                 WHERE id = ? AND state = 'DISCOVERING'",
            )
            .bind(chunk.manifest_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;

        record_manifest_storage_stage("manifest_state_check", manifest_state_started, 1, 0, 0);
        let directory_frontier_started = Instant::now();
        let mut inserted_directory_count = 0_u64;
        for paths in child_directories.chunks(SCAN_DML_CHUNK_SIZE) {
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
                         SELECT 1 FROM scan_manifest_seen_paths seen
                         WHERE seen.manifest_id = ? AND seen.library_root_id = ?
                           AND seen.relative_path = incoming.relative_path
                     ) OR EXISTS (
                         SELECT 1 FROM filesystem_entries entry
                         WHERE entry.library_root_id = ?
                           AND entry.relative_path = incoming.relative_path
                           AND entry.last_seen_generation = (
                               SELECT generation FROM scan_jobs WHERE id = ?
                           )
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
                statement = statement
                    .bind(chunk.manifest_id)
                    .bind(chunk.library_root_id)
                    .bind(chunk.library_root_id)
                    .bind(chunk.job_id);
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
        for directories in completed_directories.chunks(SCAN_DML_CHUNK_SIZE) {
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
                .filter(|entry| entry.entry_kind == "DIRECTORY")
                .collect::<Vec<_>>()
        } else {
            entries.clone()
        };
        let observation_count_i64 = if workflow_version == 2 {
            i64::try_from(observation_entries.len()).map_err(|_| {
                StorageError::Conflict("manifest observation count overflow".to_owned())
            })?
        } else {
            0
        };
        let last_sequence = self
            .query_scalar::<i64>(
                "UPDATE scan_manifest_roots
                 SET state = CASE WHEN NOT EXISTS (
                         SELECT 1 FROM scan_manifest_directories
                         WHERE manifest_id = ? AND library_root_id = ? AND state = 'PENDING'
                     ) THEN 'COMPLETE' ELSE 'SCANNING' END,
                     started_at = COALESCE(started_at, unixepoch()),
                     finished_at = CASE WHEN NOT EXISTS (
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
            .bind(chunk.manifest_id)
            .bind(chunk.library_root_id)
            .bind(chunk.manifest_id)
            .bind(chunk.library_root_id)
            .bind(inserted_directory_count_i64)
            .bind(completed_directory_count)
            .bind(inserted_file_count_i64)
            .bind(observation_count_i64)
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
        let observation_sequence_start = if workflow_version == 2 && observation_count_i64 > 0 {
            Some(
                last_sequence
                    .checked_sub(observation_count_i64)
                    .and_then(|sequence| sequence.checked_add(1))
                    .ok_or_else(|| {
                        StorageError::Conflict("manifest observation sequence overflow".to_owned())
                    })?,
            )
        } else {
            None
        };

        let observation_batch_size = if workflow_version == 2 {
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

        if workflow_version != 2 && !positive_indexes.is_empty() {
            return Err(StorageError::Conflict(
                "legacy manifest cannot receive streamed positive indexes".to_owned(),
            ));
        }
        let mut positive_result = ManifestDiscoveryPositiveIndexResult::default();
        let positive_index_started = Instant::now();
        if workflow_version == 2 && !positive_indexes.is_empty() {
            let (library_id, generation): (String, String) = self
                .query_as(
                    "SELECT manifest.library_id, job.generation
                     FROM scan_manifests manifest
                     JOIN scan_jobs job ON job.id = manifest.job_id
                     WHERE manifest.id = ? AND job.id = ?
                       AND job.status = 'RUNNING' AND job.cancel_requested = 0",
                )
                .bind(chunk.manifest_id)
                .bind(chunk.job_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
                .ok_or_else(|| {
                    StorageError::Conflict(
                        "streamed indexing requires an active manifest scan job".to_owned(),
                    )
                })?;
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

        let presence_ledger_started = Instant::now();
        if discovery_format_version == 3 {
            let mut ledger_paths = Vec::new();
            positive_result.indexed_paths.sort_unstable();
            for path in &file_paths {
                if positive_result.indexed_paths.binary_search(path).is_err() {
                    ledger_paths.push(*path);
                }
            }
            for paths in ledger_paths.chunks(super::manifest_path_query_chunk_size(self.backend()))
            {
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
        }
        record_manifest_storage_stage(
            "presence_ledger",
            presence_ledger_started,
            file_paths.len(),
            file_paths.len(),
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
            .bind(if workflow_version == 2 {
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

    pub(crate) async fn mark_scan_manifest_root_unavailable(
        &self,
        manifest_id: &str,
        library_root_id: &str,
    ) -> Result<(), StorageError> {
        let mut transaction = self.begin_scan_write_transaction().await?;
        self.query(
            "UPDATE scan_manifest_roots
             SET state = 'UNAVAILABLE', error = 'filesystem root could not be read',
                 finished_at = unixepoch(), updated_at = unixepoch()
             WHERE manifest_id = ? AND library_root_id = ?
               AND state IN ('PENDING', 'SCANNING', 'COMPLETE', 'INCOMPLETE')",
        )
        .bind(manifest_id)
        .bind(library_root_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query(
            "UPDATE library_roots
             SET is_available = 0, last_checked_at = unixepoch(),
                 unavailable_since = COALESCE(unavailable_since, unixepoch())
             WHERE id = ?",
        )
        .bind(library_root_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query(
            "UPDATE scan_manifest_directories
             SET state = 'FAILED', error = 'filesystem root could not be read',
                 updated_at = unixepoch()
             WHERE manifest_id = ? AND library_root_id = ? AND state = 'PENDING'",
        )
        .bind(manifest_id)
        .bind(library_root_id)
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

    pub(crate) async fn get_scan_manifest_root_identity(
        &self,
        manifest_id: &str,
        library_root_id: &str,
    ) -> Result<Option<(String, Option<i64>, Option<i64>)>, StorageError> {
        self.query_as(
            "SELECT root.state, observed.device, observed.inode
             FROM scan_manifest_roots root
             LEFT JOIN scan_manifest_entries observed
               ON observed.manifest_id = root.manifest_id
              AND observed.library_root_id = root.library_root_id
              AND observed.relative_path = ''
              AND observed.observation_sequence = (
                  SELECT MAX(latest.observation_sequence)
                  FROM scan_manifest_entries latest
                  WHERE latest.manifest_id = root.manifest_id
                    AND latest.library_root_id = root.library_root_id
                    AND latest.relative_path = ''
              )
             WHERE root.manifest_id = ? AND root.library_root_id = ?",
        )
        .bind(manifest_id)
        .bind(library_root_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn finish_scan_manifest_discovery(
        &self,
        manifest_id: &str,
        job_id: &str,
    ) -> Result<i64, StorageError> {
        let mut transaction = self.begin_scan_write_transaction().await?;
        let pending_directories: i64 = self
            .query_scalar(
                "SELECT COUNT(*) FROM scan_manifest_directories
                 WHERE manifest_id = ? AND state = 'PENDING'",
            )
            .bind(manifest_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let pending_roots: i64 = self
            .query_scalar(
                "SELECT COUNT(*) FROM scan_manifest_roots
                 WHERE manifest_id = ? AND state IN ('PENDING', 'SCANNING', 'INCOMPLETE')",
            )
            .bind(manifest_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if pending_directories != 0 || pending_roots != 0 {
            return Err(StorageError::Conflict(
                "manifest discovery cannot complete while frontier or roots remain unresolved"
                    .to_owned(),
            ));
        }
        let discovered_file_count: i64 = self
            .query_scalar("SELECT observed_file_count FROM scan_manifests WHERE id = ?")
            .bind(manifest_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let processed_count: i64 = self
            .query_scalar("SELECT processed_count FROM scan_jobs WHERE id = ?")
            .bind(job_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let total_count = discovered_file_count.max(processed_count);
        let job_update = self
            .query(
                "UPDATE scan_jobs
             SET discovery_completed = 1, total_count = ?, updated_at = unixepoch()
             WHERE id = ? AND status = 'RUNNING' AND cancel_requested = 0",
            )
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
                "scan job is no longer accepting manifest discovery completion".to_owned(),
            ));
        }
        let manifest_update = self
            .query(
                "UPDATE scan_manifests
             SET state = 'READY_TO_DIFF', updated_at = unixepoch()
             WHERE id = ? AND state = 'DISCOVERING'",
            )
            .bind(manifest_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if manifest_update.rows_affected() != 1 {
            return Err(StorageError::Conflict(
                "manifest is no longer accepting discovery completion".to_owned(),
            ));
        }
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(total_count)
    }

    pub(crate) async fn finish_scan_manifest(
        &self,
        job_id: &str,
        next_state: &str,
    ) -> Result<(), StorageError> {
        if !matches!(next_state, "FAILED" | "CANCELLED") {
            return Err(StorageError::Conflict(
                "invalid manifest terminal state".to_owned(),
            ));
        }
        let mut transaction = self.begin_scan_write_transaction().await?;
        self.query(
            "UPDATE scan_manifests
             SET resume_state = state, state = ?, updated_at = unixepoch()
             WHERE job_id = ? AND state IN (
                 'DISCOVERING', 'READY_TO_DIFF', 'APPLYING', 'INDEXED', 'POSTPROCESSING'
             )",
        )
        .bind(next_state)
        .bind(job_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query(
            "UPDATE scan_manifest_roots
             SET state = 'INCOMPLETE', error = 'scan did not complete this root',
                 finished_at = unixepoch(), updated_at = unixepoch()
             WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)
               AND state IN ('PENDING', 'SCANNING')",
        )
        .bind(job_id)
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

    #[allow(dead_code)] // Lifecycle owners use this compare-and-swap in LUX-266 onward.
    pub(crate) async fn transition_scan_manifest_state(
        &self,
        id: &str,
        expected_state: &str,
        next_state: &str,
    ) -> Result<bool, StorageError> {
        if expected_state == next_state {
            return Ok(false);
        }
        if !valid_scan_manifest_transition(expected_state, next_state) {
            return Err(StorageError::Conflict(
                "invalid scan manifest state transition".to_owned(),
            ));
        }
        let mut transaction = self.begin_scan_write_transaction().await?;
        let result = self
            .query(
                "UPDATE scan_manifests
                 SET state = ?, updated_at = unixepoch(),
                     indexed_at = CASE WHEN ? = 'INDEXED'
                         THEN COALESCE(indexed_at, unixepoch()) ELSE indexed_at END,
                     completed_at = CASE WHEN ? = 'COMPLETED'
                         THEN COALESCE(completed_at, unixepoch()) ELSE completed_at END
                 WHERE id = ? AND state = ?",
            )
            .bind(next_state)
            .bind(next_state)
            .bind(next_state)
            .bind(id)
            .bind(expected_state)
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

    pub(crate) async fn list_scan_manifest_removal_candidates(
        &self,
        manifest_id: &str,
        discovery_format_version: i64,
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
               AND NOT EXISTS (
                   SELECT 1 FROM scan_manifest_deltas delta
                   WHERE delta.manifest_id = ?
                     AND delta.library_root_id = fe.library_root_id
                     AND delta.relative_path = fe.relative_path
               )
             ORDER BY fe.library_root_id, fe.relative_path
             LIMIT ?"
        );
        self.query(sqlx::AssertSqlSafe(query))
            .bind(manifest_id)
            .bind(after_library_root_id.unwrap_or_default())
            .bind(after_relative_path.unwrap_or_default())
            .bind(manifest_id)
            .bind(manifest_id)
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
        let (workflow_version, discovery_format_version): (i64, i64) = self
            .query_as(
                "SELECT workflow_version, discovery_format_version
                 FROM scan_manifests WHERE id = ?",
            )
            .bind(manifest_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let remaining_changes: i64 = if workflow_version == 2 && discovery_format_version == 3 {
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
        } else if workflow_version == 2 {
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
        let unchanged_count: i64 = if workflow_version == 2 {
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
            .query_scalar(if workflow_version == 2 {
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
        let processed_count = if workflow_version == 2 {
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

    pub(crate) async fn enable_scan_job_auto_metadata_match(
        &self,
        job_id: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE scan_jobs
             SET auto_metadata_match = 1, updated_at = unixepoch()
             WHERE id = ? AND status IN ('PENDING', 'RUNNING')",
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

    pub(crate) async fn enqueue_incremental_scan_path(
        &self,
        job_id: &str,
        library_root_id: &str,
        relative_path: &str,
        change_kind: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO scan_job_paths (
                job_id, library_root_id, relative_path, change_kind
             ) VALUES (?, ?, ?, ?)
             ON CONFLICT(job_id, library_root_id, relative_path) DO UPDATE SET
                change_kind = excluded.change_kind,
                processed_at = NULL,
                updated_at = unixepoch()",
        )
        .bind(job_id)
        .bind(library_root_id)
        .bind(relative_path)
        .bind(change_kind)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query(
            "UPDATE scan_jobs
             SET total_count = (
                 SELECT COUNT(*) FROM scan_job_paths
                 WHERE job_id = ?
             ), updated_at = unixepoch()
             WHERE id = ? AND status IN ('PENDING', 'RUNNING')",
        )
        .bind(job_id)
        .bind(job_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_pending_scan_job_paths(
        &self,
        job_id: &str,
        limit: i64,
    ) -> Result<Vec<StoredScanJobPath>, StorageError> {
        self.query(
            "SELECT job_id, library_root_id, relative_path, change_kind
             FROM scan_job_paths
             WHERE job_id = ? AND processed_at IS NULL
             ORDER BY created_at, relative_path
             LIMIT ?",
        )
        .bind(job_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map(|rows| rows.into_iter().map(stored_scan_job_path).collect())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn mark_scan_job_path_processed(
        &self,
        job_id: &str,
        library_root_id: &str,
        relative_path: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE scan_job_paths
             SET processed_at = unixepoch(), updated_at = unixepoch()
             WHERE job_id = ? AND library_root_id = ? AND relative_path = ?",
        )
        .bind(job_id)
        .bind(library_root_id)
        .bind(relative_path)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_media_item_ids_for_incremental_scan(
        &self,
        job_id: &str,
    ) -> Result<Vec<String>, StorageError> {
        self.query_scalar(
            "WITH affected_items AS (
                 SELECT DISTINCT ms.item_id
                 FROM scan_job_paths sjp
                 JOIN filesystem_entries fe
                   ON fe.library_root_id = sjp.library_root_id
                  AND (
                        sjp.relative_path = '.'
                        OR
                        fe.relative_path = sjp.relative_path
                        OR substr(fe.relative_path, 1, length(sjp.relative_path) + 1)
                           = sjp.relative_path || '/'
                      )
                 JOIN media_sources ms ON ms.filesystem_entry_id = fe.id
                 JOIN media_items mi ON mi.id = ms.item_id
                 WHERE sjp.job_id = ?
                   AND sjp.processed_at IS NOT NULL
                   AND fe.is_missing = 0
                   AND mi.removed_at IS NULL
             ),
             metadata_targets AS (
                 SELECT item_id
                 FROM affected_items
                 UNION
                 SELECT mi.parent_id
                 FROM media_items mi
                 JOIN affected_items affected ON affected.item_id = mi.id
                 WHERE mi.item_type = 'EPISODE'
                   AND mi.parent_id IS NOT NULL
                 UNION
                 SELECT mi.series_id
                 FROM media_items mi
                 JOIN affected_items affected ON affected.item_id = mi.id
                 WHERE mi.item_type = 'EPISODE'
                   AND mi.series_id IS NOT NULL
             )
             SELECT DISTINCT target.id
             FROM metadata_targets targets
             JOIN media_items target ON target.id = targets.item_id
             WHERE target.removed_at IS NULL
             ORDER BY target.id",
        )
        .bind(job_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn finish_scan_job_if_idle(&self, id: &str) -> Result<bool, StorageError> {
        let result = self
            .query(
                "UPDATE scan_jobs
             SET status = 'COMPLETED', cursor = NULL, current_item = NULL,
                 scan_phase = 'IDLE',
                 finished_at = unixepoch(), updated_at = unixepoch()
             WHERE id = ? AND status IN ('PENDING', 'RUNNING')
               AND NOT EXISTS (
                   SELECT 1 FROM scan_job_paths
                   WHERE job_id = ? AND processed_at IS NULL
               )",
            )
            .bind(id)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn mark_filesystem_entry_missing_by_path(
        &self,
        library_root_id: &str,
        relative_path: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE filesystem_entries
             SET is_missing = 1, updated_at = unixepoch()
             WHERE library_root_id = ? AND relative_path = ?",
        )
        .bind(library_root_id)
        .bind(relative_path)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_reconciliation_missing_filesystem_entry_paths_page(
        &self,
        job_id: &str,
        library_root_id: &str,
        after_relative_path: Option<&str>,
        limit: i64,
    ) -> Result<Vec<String>, StorageError> {
        self.query_scalar(
            "SELECT fe.relative_path
             FROM filesystem_entries fe
             WHERE fe.library_root_id = ? AND fe.is_missing = 0
               AND NOT EXISTS (
                   SELECT 1
                   FROM reconciliation_scan_entries rse
                   WHERE rse.job_id = ?
                     AND rse.library_root_id = fe.library_root_id
                     AND rse.entry_type = 'FILE'
                     AND rse.relative_path = fe.relative_path
               )
               AND fe.relative_path > ?
             ORDER BY fe.relative_path
             LIMIT ?",
        )
        .bind(library_root_id)
        .bind(job_id)
        .bind(after_relative_path.unwrap_or_default())
        .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
        .fetch_all(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn create_strm_probe_job(
        &self,
        job: NewStrmProbeJob<'_>,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO strm_probe_jobs (
                id, operation_id, library_id, status, concurrency,
                include_ready, write_sidecars, media_info_enabled,
                thumbnail_enabled, thumbnail_position_percent, target_scan_job_id,
                total_count
             ) VALUES (?, ?, ?, 'PENDING', ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(job.id)
        .bind(job.operation_id)
        .bind(job.library_id)
        .bind(job.concurrency)
        .bind(database_flag(job.include_ready))
        .bind(database_flag(job.write_sidecars))
        .bind(database_flag(job.media_info_enabled))
        .bind(database_flag(job.thumbnail_enabled))
        .bind(job.thumbnail_position_percent)
        .bind(job.target_scan_job_id)
        .bind(job.total_count)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn has_active_strm_probe_jobs(&self) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(
                SELECT 1 FROM strm_probe_jobs WHERE status IN ('PENDING', 'RUNNING')
            ) THEN 1 ELSE 0 END",
        )
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn has_active_strm_probe_jobs_for_operation(
        &self,
        operation_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(
                SELECT 1 FROM strm_probe_jobs
                WHERE operation_id = ? AND status IN ('PENDING', 'RUNNING')
            ) THEN 1 ELSE 0 END",
        )
        .bind(operation_id)
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_strm_probe_job(
        &self,
        id: &str,
    ) -> Result<Option<StoredStrmProbeJob>, StorageError> {
        self.query(
            "SELECT id, operation_id, library_id, status, concurrency,
                    include_ready, write_sidecars, media_info_enabled,
                    thumbnail_enabled, thumbnail_position_percent, target_scan_job_id,
                    cursor, processed_count,
                    total_count, cancel_requested, error,
                    created_at, started_at, finished_at
             FROM strm_probe_jobs WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_strm_probe_job))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_strm_probe_jobs(
        &self,
        status: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredStrmProbeJob>, StorageError> {
        let rows = if let Some(status) = status {
            self.query(
                "SELECT id, operation_id, library_id, status, concurrency,
                        include_ready, write_sidecars, media_info_enabled,
                        thumbnail_enabled, thumbnail_position_percent, target_scan_job_id,
                        cursor, processed_count,
                        total_count, cancel_requested, error,
                        created_at, started_at, finished_at
                 FROM strm_probe_jobs WHERE status = ?
                 ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
            )
            .bind(status)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        } else {
            self.query(
                "SELECT id, operation_id, library_id, status, concurrency,
                        include_ready, write_sidecars, media_info_enabled,
                        thumbnail_enabled, thumbnail_position_percent, target_scan_job_id,
                        cursor, processed_count,
                        total_count, cancel_requested, error,
                        created_at, started_at, finished_at
                 FROM strm_probe_jobs
                 ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
            )
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        };
        rows.map(|rows| rows.into_iter().map(stored_strm_probe_job).collect())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn clear_scan_job_paths(&self, job_id: &str) -> Result<(), StorageError> {
        self.query("DELETE FROM scan_job_paths WHERE job_id = ?")
            .bind(job_id)
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_reconciliation_scan_entries(
        &self,
        job_id: &str,
        entry_type: &str,
        limit: i64,
    ) -> Result<Vec<StoredReconciliationScanEntry>, StorageError> {
        self.query(
            "SELECT library_root_id, relative_path
             FROM reconciliation_scan_entries
             WHERE job_id = ? AND entry_type = ? AND status = 'PENDING'
             ORDER BY library_root_id, relative_path
             LIMIT ?",
        )
        .bind(job_id)
        .bind(entry_type)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(stored_reconciliation_scan_entry)
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn commit_reconciliation_discovery_chunk(
        &self,
        job_id: &str,
        library_root_id: &str,
        child_directories: &[String],
        media_files: &[String],
        completed_directory: Option<&str>,
    ) -> Result<i64, StorageError> {
        if child_directories.is_empty() && media_files.is_empty() && completed_directory.is_none() {
            return Ok(0);
        }
        let mut transaction = self.begin_scan_write_transaction().await?;
        let inserted_file_count = self
            .insert_reconciliation_directory_entries(
                &mut transaction,
                job_id,
                library_root_id,
                child_directories,
                media_files,
            )
            .await?;
        self.increment_reconciliation_total_count_in_transaction(
            &mut transaction,
            job_id,
            inserted_file_count,
        )
        .await?;
        if let Some(completed_directory) = completed_directory {
            self.query(
                "DELETE FROM reconciliation_scan_entries
                 WHERE job_id = ? AND library_root_id = ?
                   AND relative_path = ? AND entry_type = 'DIRECTORY'",
            )
            .bind(job_id)
            .bind(library_root_id)
            .bind(completed_directory)
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
        i64::try_from(inserted_file_count)
            .map_err(|_| StorageError::Conflict("reconciliation file count overflow".to_owned()))
    }

    async fn insert_reconciliation_directory_entries(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        job_id: &str,
        library_root_id: &str,
        child_directories: &[String],
        media_files: &[String],
    ) -> Result<u64, StorageError> {
        let mut inserted_file_count = 0_u64;
        for (entry_type, paths) in [("DIRECTORY", child_directories), ("FILE", media_files)] {
            for chunk in paths.chunks(SCAN_DML_CHUNK_SIZE) {
                if chunk.is_empty() {
                    continue;
                }
                let values = std::iter::repeat_n("(?, ?, ?, ?)", chunk.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                let query = format!(
                    "INSERT INTO reconciliation_scan_entries (
                         job_id, library_root_id, relative_path, entry_type
                     ) VALUES {values}
                     ON CONFLICT(job_id, entry_type, library_root_id, relative_path) DO NOTHING"
                );
                let mut statement = self.query(sqlx::AssertSqlSafe(query));
                for path in chunk {
                    statement = statement
                        .bind(job_id)
                        .bind(library_root_id)
                        .bind(path)
                        .bind(entry_type);
                }
                let result = statement
                    .execute(&mut **transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                if entry_type == "FILE" {
                    inserted_file_count = inserted_file_count
                        .checked_add(result.rows_affected())
                        .ok_or_else(|| {
                        StorageError::Conflict("reconciliation file count overflow".to_owned())
                    })?;
                }
            }
        }
        Ok(inserted_file_count)
    }

    async fn increment_reconciliation_total_count_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        job_id: &str,
        inserted_file_count: u64,
    ) -> Result<(), StorageError> {
        if inserted_file_count == 0 {
            return Ok(());
        }
        let inserted_file_count = i64::try_from(inserted_file_count)
            .map_err(|_| StorageError::Conflict("reconciliation file count overflow".to_owned()))?;
        self.query(
            "UPDATE scan_jobs
             SET total_count = CASE
                     WHEN total_count < processed_count
                         THEN processed_count + ?
                     ELSE total_count + ?
                 END,
                 updated_at = unixepoch()
             WHERE id = ? AND status = 'RUNNING' AND discovery_completed = 0",
        )
        .bind(inserted_file_count)
        .bind(inserted_file_count)
        .bind(job_id)
        .execute(&mut **transaction)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn finish_reconciliation_discovery(
        &self,
        job_id: &str,
    ) -> Result<i64, StorageError> {
        let mut transaction = self.begin_scan_write_transaction().await?;
        let discovered_file_count: i64 = self
            .query_scalar(
                "SELECT COUNT(*) FROM reconciliation_scan_entries
             WHERE job_id = ? AND entry_type = 'FILE'",
            )
            .bind(job_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let processed_count: i64 = self
            .query_scalar("SELECT processed_count FROM scan_jobs WHERE id = ?")
            .bind(job_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let total_count = discovered_file_count.max(processed_count);
        self.query(
            "UPDATE scan_jobs
             SET discovery_completed = 1, total_count = ?, updated_at = unixepoch()
             WHERE id = ? AND status = 'RUNNING'",
        )
        .bind(total_count)
        .bind(job_id)
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
        Ok(total_count)
    }

    pub(crate) async fn discard_reconciliation_root_entries(
        &self,
        job_id: &str,
        library_root_id: &str,
    ) -> Result<i64, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let file_count: i64 = self
            .query_scalar(
                "SELECT COUNT(*) FROM reconciliation_scan_entries
             WHERE job_id = ? AND library_root_id = ?
               AND entry_type = 'FILE' AND status = 'PENDING'",
            )
            .bind(job_id)
            .bind(library_root_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.query(
            "DELETE FROM reconciliation_scan_entries
             WHERE job_id = ? AND library_root_id = ?",
        )
        .bind(job_id)
        .bind(library_root_id)
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
        Ok(file_count)
    }

    pub(crate) async fn clear_reconciliation_scan_entries(
        &self,
        job_id: &str,
    ) -> Result<(), StorageError> {
        self.query("DELETE FROM reconciliation_scan_entries WHERE job_id = ?")
            .bind(job_id)
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
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
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
            .map(|_| changed)
    }

    async fn record_scan_job_targets_in_transaction(
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
            let source_result =
                source_statement
                    .execute(&mut **transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
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
            let item_result =
                item_statement
                    .execute(&mut **transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
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
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
            .map(|_| changed)
    }

    async fn record_scan_job_sidecar_targets_in_transaction(
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
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
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
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            changed |= result.rows_affected() > 0;
        }
        Ok(changed)
    }

    /// Commits one reconciliation batch atomically.
    ///
    /// The batch is scoped to one library root. The caller must prepare all
    /// filesystem data before this call; all database changes are committed
    /// together or rolled back together. The returned counts describe
    /// `(confirmed_entries, created_items)`.
    pub(crate) async fn commit_reconciliation_batch(
        &self,
        batch: &ReconciliationBatchCommit<'_>,
    ) -> Result<ReconciliationBatchCommitResult, StorageError> {
        if batch.entries.is_empty()
            && batch.movie_files.is_empty()
            && batch.episode_files.is_empty()
            && batch.seen_entry_ids.is_empty()
            && batch.missing_paths.is_empty()
            && batch.new_paths.is_empty()
            && batch.changed_paths.is_empty()
            && batch.sidecar_paths.is_empty()
        {
            return Ok(ReconciliationBatchCommitResult {
                confirmed_entries: 0,
                created_items: 0,
                metadata_targets_changed: false,
            });
        }
        self.commit_reconciliation_batch_in_transaction(batch).await
    }

    async fn commit_reconciliation_batch_in_transaction(
        &self,
        batch: &ReconciliationBatchCommit<'_>,
    ) -> Result<ReconciliationBatchCommitResult, StorageError> {
        if batch
            .entries
            .iter()
            .any(|entry| entry.library_root_id != batch.library_root_id)
        {
            return Err(StorageError::Conflict(
                "reconciliation batch cannot span library roots".to_owned(),
            ));
        }

        let mut transaction = self.begin_scan_write_transaction().await?;
        let job_status: Option<String> = self
            .query_scalar("SELECT status FROM scan_jobs WHERE id = ?")
            .bind(batch.job_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if job_status.as_deref() != Some("RUNNING") {
            return Err(StorageError::Conflict(
                "reconciliation batch requires a running scan job".to_owned(),
            ));
        }

        let pending_paths = self
            .list_pending_reconciliation_paths_in_transaction(&mut transaction, batch)
            .await?;
        let movie_files = batch
            .movie_files
            .iter()
            .filter(|file| pending_paths.contains(&file.relative_path))
            .cloned()
            .collect::<Vec<_>>();
        let episode_files = batch
            .episode_files
            .iter()
            .filter(|file| pending_paths.contains(&file.relative_path))
            .cloned()
            .collect::<Vec<_>>();

        let mut created_items = 0_usize;
        created_items = created_items.saturating_add(
            self.insert_movie_files_batch_in_transaction(
                &mut transaction,
                batch.library_id,
                batch.library_root_id,
                batch.generation,
                &movie_files,
            )
            .await?,
        );
        created_items = created_items.saturating_add(
            self.insert_episode_files_batch_in_transaction(
                &mut transaction,
                batch.library_id,
                batch.library_root_id,
                batch.generation,
                &episode_files,
            )
            .await?,
        );

        let mut metadata_targets_changed = self
            .record_scan_job_targets_in_transaction(
                &mut transaction,
                batch.job_id,
                batch.library_root_id,
                batch.new_paths,
                "NEW",
            )
            .await?;
        metadata_targets_changed |= self
            .record_scan_job_targets_in_transaction(
                &mut transaction,
                batch.job_id,
                batch.library_root_id,
                batch.changed_paths,
                "CHANGED",
            )
            .await?;
        let sidecar_directories = prune_sidecar_directories(
            batch
                .sidecar_paths
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
        metadata_targets_changed |= self
            .record_scan_job_sidecar_targets_in_transaction(
                &mut transaction,
                batch.job_id,
                batch.library_root_id,
                &sidecar_directories,
            )
            .await?;
        self.restore_filesystem_entries_batch_in_transaction(
            &mut transaction,
            batch.seen_entry_ids,
        )
        .await?;

        let missing_count = self
            .discard_reconciliation_file_entries_in_transaction(&mut transaction, batch)
            .await?;
        let confirmed_entries = self
            .confirm_reconciliation_entries_in_transaction(&mut transaction, batch)
            .await?;
        let confirmed_count = i64::try_from(confirmed_entries).map_err(|_| {
            StorageError::Conflict("reconciliation batch confirmation count overflow".to_owned())
        })?;
        let missing_count = i64::try_from(missing_count).map_err(|_| {
            StorageError::Conflict("reconciliation batch confirmation count overflow".to_owned())
        })?;
        let confirmed_count = confirmed_count.checked_add(missing_count).ok_or_else(|| {
            StorageError::Conflict("reconciliation batch confirmation count overflow".to_owned())
        })?;
        let update = self
            .query(
                "UPDATE scan_jobs
                 SET cursor = CASE WHEN ? > 0 THEN ? ELSE cursor END,
                     processed_count = processed_count + ?,
                     total_count = CASE
                         WHEN total_count < processed_count + ?
                             THEN processed_count + ?
                         ELSE total_count
                     END,
                     updated_at = unixepoch()
                 WHERE id = ? AND status = 'RUNNING'",
            )
            .bind(confirmed_count)
            .bind(
                batch
                    .entries
                    .last()
                    .map(|entry| entry.relative_path.as_str()),
            )
            .bind(confirmed_count)
            .bind(confirmed_count)
            .bind(confirmed_count);
        let result = update
            .bind(batch.job_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if result.rows_affected() != 1 {
            return Err(StorageError::Conflict(
                "reconciliation scan job stopped during batch commit".to_owned(),
            ));
        }

        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(ReconciliationBatchCommitResult {
            confirmed_entries: usize::try_from(confirmed_count).map_err(|_| {
                StorageError::Conflict(
                    "reconciliation batch confirmation count overflow".to_owned(),
                )
            })?,
            created_items,
            metadata_targets_changed,
        })
    }

    async fn discard_reconciliation_file_entries_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        batch: &ReconciliationBatchCommit<'_>,
    ) -> Result<u64, StorageError> {
        if batch.missing_paths.is_empty() {
            return Ok(0);
        }
        let mut discarded = 0_u64;
        for paths in batch.missing_paths.chunks(SCAN_DML_CHUNK_SIZE) {
            let placeholders = std::iter::repeat_n("?", paths.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "DELETE FROM reconciliation_scan_entries
                 WHERE job_id = ? AND library_root_id = ?
                   AND entry_type = 'FILE' AND status = 'PENDING'
                   AND relative_path IN ({placeholders})"
            );
            let mut statement = self
                .query(sqlx::AssertSqlSafe(query))
                .bind(batch.job_id)
                .bind(batch.library_root_id);
            for path in paths {
                statement = statement.bind(path);
            }
            discarded = discarded.saturating_add(
                statement
                    .execute(&mut **transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?
                    .rows_affected(),
            );
        }
        Ok(discarded)
    }

    async fn confirm_reconciliation_entries_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        batch: &ReconciliationBatchCommit<'_>,
    ) -> Result<usize, StorageError> {
        let paths = batch
            .entries
            .iter()
            .map(|entry| entry.relative_path.as_str())
            .collect::<Vec<_>>();
        let mut confirmed = 0_u64;
        for chunk in paths.chunks(SCAN_DML_CHUNK_SIZE) {
            if chunk.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "UPDATE reconciliation_scan_entries
                 SET status = 'DONE'
                 WHERE job_id = ? AND library_root_id = ?
                   AND entry_type = 'FILE' AND status = 'PENDING'
                   AND relative_path IN ({placeholders})"
            );
            let mut statement = self
                .query(sqlx::AssertSqlSafe(query))
                .bind(batch.job_id)
                .bind(batch.library_root_id);
            for path in chunk {
                statement = statement.bind(path);
            }
            confirmed = confirmed.saturating_add(
                statement
                    .execute(&mut **transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?
                    .rows_affected(),
            );
        }
        usize::try_from(confirmed).map_err(|_| {
            StorageError::Conflict("reconciliation confirmation count overflow".to_owned())
        })
    }

    async fn list_pending_reconciliation_paths_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        batch: &ReconciliationBatchCommit<'_>,
    ) -> Result<HashSet<String>, StorageError> {
        let paths = batch
            .entries
            .iter()
            .map(|entry| entry.relative_path.as_str())
            .collect::<Vec<_>>();
        if paths.is_empty() {
            return Ok(HashSet::new());
        }
        let mut pending = HashSet::new();
        for chunk in paths.chunks(SCAN_DML_CHUNK_SIZE) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT relative_path
                 FROM reconciliation_scan_entries
                 WHERE job_id = ? AND library_root_id = ?
                   AND entry_type = 'FILE' AND status = 'PENDING'
                   AND relative_path IN ({placeholders})"
            );
            let mut statement = self
                .query(sqlx::AssertSqlSafe(query))
                .bind(batch.job_id)
                .bind(batch.library_root_id);
            for path in chunk {
                statement = statement.bind(path);
            }
            let rows = statement
                .fetch_all(&mut **transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            for row in rows {
                let path = row
                    .try_get::<String, _>("relative_path")
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                pending.insert(path);
            }
        }
        Ok(pending)
    }

    async fn record_scan_job_removed_targets_in_transaction(
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
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;

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
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }
        Ok(())
    }

    pub(crate) async fn prepare_scan_manifest_retry(
        &self,
        job_id: &str,
    ) -> Result<Option<String>, StorageError> {
        let mut transaction = self.begin_scan_write_transaction().await?;
        let manifest: Option<(String, Option<String>)> = self
            .query_as("SELECT state, resume_state FROM scan_manifests WHERE job_id = ?")
            .bind(job_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let Some((state, resume_state)) = manifest else {
            transaction
                .commit()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(None);
        };

        let stage = match state.as_str() {
            "DISCOVERING" | "READY_TO_DIFF" | "APPLYING" | "INDEXED" | "POSTPROCESSING" => {
                state.clone()
            }
            "FAILED" | "CANCELLED" => match resume_state.as_deref() {
                Some(
                    stage @ ("DISCOVERING" | "READY_TO_DIFF" | "APPLYING" | "INDEXED"
                    | "POSTPROCESSING"),
                ) => {
                    let restored = self
                        .query(
                            "UPDATE scan_manifests
                             SET state = ?, resume_state = NULL, error = NULL,
                                 updated_at = unixepoch()
                             WHERE job_id = ? AND state = ? AND resume_state = ?",
                        )
                        .bind(stage)
                        .bind(job_id)
                        .bind(&state)
                        .bind(stage)
                        .execute(&mut *transaction)
                        .await
                        .map_err(|source| StorageError::Sqlx {
                            path: self.path.clone(),
                            source,
                        })?;
                    if restored.rows_affected() != 1 {
                        return Err(StorageError::Conflict(
                            "scan manifest checkpoint changed while preparing retry".to_owned(),
                        ));
                    }
                    stage.to_owned()
                }
                _ => "RESET_REQUIRED".to_owned(),
            },
            "COMPLETED" => "COMPLETED".to_owned(),
            _ => {
                return Err(StorageError::Conflict(
                    "scan manifest has an unsupported retry state".to_owned(),
                ));
            }
        };

        if stage == "DISCOVERING" {
            self.query(
                "UPDATE scan_manifest_directories
                 SET state = 'PENDING', error = NULL, updated_at = unixepoch()
                 WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)
                   AND state IN ('SCANNING', 'FAILED')",
            )
            .bind(job_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
            self.query(
                "UPDATE scan_manifest_roots
                 SET state = CASE WHEN EXISTS (
                         SELECT 1 FROM scan_manifest_directories directory
                         WHERE directory.manifest_id = scan_manifest_roots.manifest_id
                           AND directory.library_root_id = scan_manifest_roots.library_root_id
                           AND directory.state = 'PENDING'
                     ) THEN 'SCANNING' ELSE 'COMPLETE' END,
                     error = NULL, finished_at = NULL, updated_at = unixepoch()
                 WHERE manifest_id = (SELECT id FROM scan_manifests WHERE job_id = ?)
                   AND state IN ('PENDING', 'SCANNING', 'INCOMPLETE')",
            )
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
            })?;
        Ok(Some(stage))
    }

    async fn record_scan_job_removed_targets_for_entry_ids_in_transaction(
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
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
            }
        }
        Ok(())
    }

    pub(crate) async fn finalize_reconciliation_root_page(
        &self,
        job_id: &str,
        library_root_id: &str,
        generation: &str,
        missing_paths: &[String],
        removed_media_paths: &[String],
        removed_sidecar_paths: &[String],
    ) -> Result<u64, StorageError> {
        if missing_paths.is_empty() {
            return Ok(0);
        }
        let mut transaction = self.begin_scan_write_transaction().await?;
        self.record_scan_job_removed_targets_in_transaction(
            &mut transaction,
            job_id,
            library_root_id,
            removed_media_paths,
        )
        .await?;
        let sidecar_directories = prune_sidecar_directories(
            removed_sidecar_paths
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
        let _ = self
            .record_scan_job_sidecar_targets_in_transaction(
                &mut transaction,
                job_id,
                library_root_id,
                &sidecar_directories,
            )
            .await?;
        let missing_entries = self
            .mark_missing_filesystem_entry_paths_in_transaction(
                &mut transaction,
                library_root_id,
                generation,
                missing_paths,
            )
            .await?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(missing_entries)
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

    pub(crate) async fn list_scan_job_target_sources_page(
        &self,
        job_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<StoredMediaSourcePath>, StorageError> {
        self.query(
            "SELECT ms.id AS source_id, ms.item_id, ms.probe_status,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM scan_job_targets t
             JOIN media_sources ms ON ms.id = t.source_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE t.job_id = ? AND t.target_type = 'SOURCE'
               AND t.probe_state = 'PENDING'
               AND ms.probe_status = 'PENDING'
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

    pub(crate) async fn list_scan_job_target_movie_items_page(
        &self,
        job_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<StoredMediaSourcePath>, StorageError> {
        self.query(
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

    pub(crate) async fn list_scan_job_target_series_items_page(
        &self,
        job_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<StoredSeriesMetadataSource>, StorageError> {
        self.query(
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

    pub(crate) async fn has_pending_scan_job_metadata_targets(
        &self,
        job_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(
                 SELECT 1 FROM scan_job_targets
                 WHERE job_id = ? AND target_type = 'ITEM'
                   AND metadata_state = 'PENDING'
             ) THEN 1 ELSE 0 END",
        )
        .bind(job_id)
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
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
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
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
            pending.extend(rows.into_iter().map(|row| row.get("item_id")));
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
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
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
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
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
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
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
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result.rows_affected() > 0)
    }

    pub(crate) async fn retry_failed_scan_job_targets(
        &self,
        job_id: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE media_sources
             SET probe_status = 'PENDING', probe_error = NULL, updated_at = unixepoch()
             WHERE id IN (
                 SELECT source_id FROM scan_job_targets
                 WHERE job_id = ? AND target_type = 'SOURCE'
                   AND probe_state = 'FAILED' AND source_id IS NOT NULL
             )",
        )
        .bind(job_id)
        .execute(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
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
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_active_strm_probe_job_ids(&self) -> Result<Vec<String>, StorageError> {
        self.query_scalar(
            "SELECT id FROM strm_probe_jobs
             WHERE status IN ('PENDING', 'RUNNING')
             ORDER BY created_at, id LIMIT 10000",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn has_reconciliation_scan_entries(
        &self,
        job_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(
                 SELECT 1 FROM reconciliation_scan_entries
                 WHERE job_id = ? AND status = 'PENDING'
             ) THEN 1 ELSE 0 END",
        )
        .bind(job_id)
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn claim_strm_probe_job(&self, id: &str) -> Result<bool, StorageError> {
        self.query(
            "UPDATE strm_probe_jobs
             SET status = 'RUNNING', started_at = COALESCE(started_at, unixepoch()),
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

    pub(crate) async fn update_strm_probe_job_progress(
        &self,
        id: &str,
        cursor: Option<&str>,
        processed_count: i64,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE strm_probe_jobs
             SET cursor = ?, processed_count = ?, updated_at = unixepoch()
             WHERE id = ? AND status = 'RUNNING'",
        )
        .bind(cursor)
        .bind(processed_count)
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn strm_probe_job_cancel_requested(
        &self,
        id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar("SELECT cancel_requested FROM strm_probe_jobs WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .map(|value: i64| value != 0)
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn request_strm_probe_job_cancel(&self, id: &str) -> Result<(), StorageError> {
        self.query(
            "UPDATE strm_probe_jobs SET cancel_requested = 1, updated_at = unixepoch()
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

    pub(crate) async fn finish_strm_probe_job(
        &self,
        id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE strm_probe_jobs
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

    pub(crate) async fn append_scan_job_event(
        &self,
        event: NewScanJobEvent<'_>,
    ) -> Result<(), StorageError> {
        if event.level == "INFO" {
            return Ok(());
        }
        self.query(
            "INSERT INTO scan_job_events
             (id, job_id, level, event_code, message, details_json)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(event.id)
        .bind(event.job_id)
        .bind(event.level)
        .bind(event.event_code)
        .bind(event.message)
        .bind(event.details_json)
        .execute(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        if let Err(error) = self.prune_scan_job_events().await {
            tracing::warn!(job_id = event.job_id, %error, "scan event retention cleanup failed");
        }
        Ok(())
    }

    pub(crate) async fn count_scan_job_events(
        &self,
        job_id: &str,
        level: Option<&str>,
        event_code: Option<&str>,
    ) -> Result<i64, StorageError> {
        let count = match (level, event_code) {
            (Some(_), Some(_)) => {
                self.query_scalar(
                    "SELECT COUNT(*) FROM scan_job_events
                     WHERE job_id = ? AND level = ? AND event_code = ?",
                )
                .bind(job_id)
                .bind(level)
                .bind(event_code)
                .fetch_one(&self.pool)
                .await
            }
            (Some(_), None) => {
                self.query_scalar(
                    "SELECT COUNT(*) FROM scan_job_events
                     WHERE job_id = ? AND level = ?",
                )
                .bind(job_id)
                .bind(level)
                .fetch_one(&self.pool)
                .await
            }
            (None, Some(_)) => {
                self.query_scalar(
                    "SELECT COUNT(*) FROM scan_job_events
                     WHERE job_id = ? AND event_code = ?",
                )
                .bind(job_id)
                .bind(event_code)
                .fetch_one(&self.pool)
                .await
            }
            (None, None) => {
                self.query_scalar("SELECT COUNT(*) FROM scan_job_events WHERE job_id = ?")
                    .bind(job_id)
                    .fetch_one(&self.pool)
                    .await
            }
        };
        count.map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_scan_job_events(
        &self,
        job_id: &str,
        level: Option<&str>,
        event_code: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredScanJobEvent>, StorageError> {
        let rows = match (level, event_code) {
            (Some(_), Some(_)) => {
                self.query(
                    "SELECT id, job_id, level, event_code, message, details_json, created_at
                     FROM scan_job_events
                     WHERE job_id = ? AND level = ? AND event_code = ?
                     ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
                )
                .bind(job_id)
                .bind(level)
                .bind(event_code)
                .bind(limit)
                .bind(offset)
                .fetch_all(&self.pool)
                .await
            }
            (Some(_), None) => {
                self.query(
                    "SELECT id, job_id, level, event_code, message, details_json, created_at
                     FROM scan_job_events
                     WHERE job_id = ? AND level = ?
                     ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
                )
                .bind(job_id)
                .bind(level)
                .bind(limit)
                .bind(offset)
                .fetch_all(&self.pool)
                .await
            }
            (None, Some(_)) => {
                self.query(
                    "SELECT id, job_id, level, event_code, message, details_json, created_at
                     FROM scan_job_events
                     WHERE job_id = ? AND event_code = ?
                     ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
                )
                .bind(job_id)
                .bind(event_code)
                .bind(limit)
                .bind(offset)
                .fetch_all(&self.pool)
                .await
            }
            (None, None) => {
                self.query(
                    "SELECT id, job_id, level, event_code, message, details_json, created_at
                     FROM scan_job_events WHERE job_id = ?
                     ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
                )
                .bind(job_id)
                .bind(limit)
                .bind(offset)
                .fetch_all(&self.pool)
                .await
            }
        };
        rows.map(|rows| rows.into_iter().map(stored_scan_job_event).collect())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn create_metadata_reidentify_job(
        &self,
        job_id: &str,
        item_ids: &[String],
        mode: &str,
    ) -> Result<(), StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        self.query(
            "INSERT INTO metadata_reidentify_jobs (
                id, status, total_count, mode, library_id, job_scope
             ) VALUES (?, 'QUEUED', ?, ?, NULL, 'ITEMS')",
        )
        .bind(job_id)
        .bind(i64::try_from(item_ids.len()).unwrap_or(i64::MAX))
        .bind(mode)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        for chunk in item_ids.chunks(BATCH_INSERT_CHUNK_SIZE) {
            let values = std::iter::repeat_n("(?, ?, 'PENDING')", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "INSERT INTO metadata_reidentify_job_items (job_id, item_id, status)
                 VALUES {values}"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for item_id in chunk {
                statement = statement.bind(job_id).bind(item_id);
            }
            statement
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }
        self.query(
            "UPDATE metadata_reidentify_jobs
             SET library_id = (
                 SELECT CASE
                     WHEN MIN(media_items.library_id) = MAX(media_items.library_id)
                         THEN MIN(media_items.library_id)
                     ELSE NULL
                 END
                 FROM metadata_reidentify_job_items
                 JOIN media_items ON media_items.id = metadata_reidentify_job_items.item_id
                 WHERE metadata_reidentify_job_items.job_id = ?
             )
             WHERE id = ?",
        )
        .bind(job_id)
        .bind(job_id)
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

    pub(crate) async fn create_metadata_reidentify_library_job(
        &self,
        job_id: &str,
        library_id: &str,
        mode: &str,
    ) -> Result<i64, StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        self.query(
            "INSERT INTO metadata_reidentify_jobs (
                id, status, total_count, mode, library_id, job_scope
             )
             SELECT ?, 'CANCELLED', COUNT(*), ?, ?, 'LIBRARY'
             FROM media_items
             WHERE library_id = ? AND removed_at IS NULL
               AND item_type IN ('MOVIE', 'SERIES', 'SEASON', 'EPISODE')",
        )
        .bind(job_id)
        .bind(mode)
        .bind(library_id)
        .bind(library_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        let total_count: i64 = self
            .query_scalar("SELECT total_count FROM metadata_reidentify_jobs WHERE id = ?")
            .bind(job_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if total_count == 0 {
            transaction
                .rollback()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(0);
        }
        self.query(
            "INSERT INTO metadata_reidentify_job_items (job_id, item_id, status)
             SELECT ?, id, 'PENDING'
             FROM media_items
             WHERE library_id = ? AND removed_at IS NULL
               AND item_type IN ('MOVIE', 'SERIES', 'SEASON', 'EPISODE')",
        )
        .bind(job_id)
        .bind(library_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query(
            "UPDATE metadata_reidentify_jobs
             SET status = 'QUEUED', updated_at = unixepoch()
             WHERE id = ? AND status = 'CANCELLED'",
        )
        .bind(job_id)
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
        Ok(total_count)
    }

    pub(crate) async fn find_metadata_reidentify_job(
        &self,
        job_id: &str,
    ) -> Result<Option<StoredMetadataReidentifyJob>, StorageError> {
        self.query(
            "WITH pending_counts AS (
                 SELECT job_items.job_id, COUNT(DISTINCT candidates.item_id) AS pending_count
                 FROM metadata_reidentify_job_items job_items
                 JOIN metadata_candidates candidates
                   ON candidates.item_id = job_items.item_id
                 WHERE job_items.job_id = ?
                   AND candidates.status = 'PENDING'
                 GROUP BY job_items.job_id
             )
             SELECT jobs.id, jobs.status, jobs.processed_count, jobs.total_count,
                    jobs.error, jobs.created_at, jobs.updated_at, jobs.started_at,
                    jobs.finished_at, jobs.mode, jobs.cancel_requested,
                    jobs.library_id, jobs.job_scope,
                    COALESCE(pending_counts.pending_count, 0) AS pending_count
             FROM metadata_reidentify_jobs jobs
             LEFT JOIN pending_counts ON pending_counts.job_id = jobs.id
             WHERE jobs.id = ?",
        )
        .bind(job_id)
        .bind(job_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_metadata_reidentify_job))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_metadata_reidentify_jobs(
        &self,
        status: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredMetadataReidentifyJob>, StorageError> {
        let rows = if let Some(status) = status {
            self.query(
                "WITH selected_jobs AS (
                     SELECT id, status, processed_count, total_count, error,
                            created_at, updated_at, started_at, finished_at, mode,
                            cancel_requested, library_id, job_scope
                     FROM metadata_reidentify_jobs
                     WHERE status = ?
                     ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?
                 ), pending_counts AS (
                     SELECT job_items.job_id, COUNT(DISTINCT candidates.item_id) AS pending_count
                     FROM metadata_reidentify_job_items job_items
                     JOIN selected_jobs ON selected_jobs.id = job_items.job_id
                     JOIN metadata_candidates candidates
                       ON candidates.item_id = job_items.item_id
                      AND candidates.status = 'PENDING'
                     GROUP BY job_items.job_id
                 )
                 SELECT selected_jobs.id, selected_jobs.status,
                        selected_jobs.processed_count, selected_jobs.total_count,
                        selected_jobs.error, selected_jobs.created_at,
                        selected_jobs.updated_at, selected_jobs.started_at,
                        selected_jobs.finished_at, selected_jobs.mode,
                        selected_jobs.cancel_requested, selected_jobs.library_id,
                        selected_jobs.job_scope,
                        COALESCE(pending_counts.pending_count, 0) AS pending_count
                 FROM selected_jobs
                 LEFT JOIN pending_counts ON pending_counts.job_id = selected_jobs.id
                 ORDER BY selected_jobs.created_at DESC, selected_jobs.id DESC",
            )
            .bind(status)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        } else {
            self.query(
                "WITH selected_jobs AS (
                     SELECT id, status, processed_count, total_count, error,
                            created_at, updated_at, started_at, finished_at, mode,
                            cancel_requested, library_id, job_scope
                     FROM metadata_reidentify_jobs
                     ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?
                 ), pending_counts AS (
                     SELECT job_items.job_id, COUNT(DISTINCT candidates.item_id) AS pending_count
                     FROM metadata_reidentify_job_items job_items
                     JOIN selected_jobs ON selected_jobs.id = job_items.job_id
                     JOIN metadata_candidates candidates
                       ON candidates.item_id = job_items.item_id
                      AND candidates.status = 'PENDING'
                     GROUP BY job_items.job_id
                 )
                 SELECT selected_jobs.id, selected_jobs.status,
                        selected_jobs.processed_count, selected_jobs.total_count,
                        selected_jobs.error, selected_jobs.created_at,
                        selected_jobs.updated_at, selected_jobs.started_at,
                        selected_jobs.finished_at, selected_jobs.mode,
                        selected_jobs.cancel_requested, selected_jobs.library_id,
                        selected_jobs.job_scope,
                        COALESCE(pending_counts.pending_count, 0) AS pending_count
                 FROM selected_jobs
                 LEFT JOIN pending_counts ON pending_counts.job_id = selected_jobs.id
                 ORDER BY selected_jobs.created_at DESC, selected_jobs.id DESC",
            )
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        };
        rows.map(|rows| {
            rows.into_iter()
                .map(stored_metadata_reidentify_job)
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_current_metadata_reidentify_items(
        &self,
        job_ids: &[String],
    ) -> Result<Vec<(String, StoredJobActivityItem)>, StorageError> {
        if job_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat_n("?", job_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "WITH ranked AS (
                 SELECT job_items.job_id, items.item_type, items.season_number,
                        items.episode_number, items.title, series.title AS series_title,
                        ROW_NUMBER() OVER (
                            PARTITION BY job_items.job_id
                            ORDER BY CASE WHEN job_items.status = 'RUNNING' THEN 0 ELSE 1 END,
                                     job_items.updated_at DESC, job_items.item_id
                        ) AS activity_rank
                 FROM metadata_reidentify_job_items job_items
                 JOIN media_items items ON items.id = job_items.item_id
                 LEFT JOIN media_items series ON series.id = items.series_id
                 WHERE job_items.job_id IN ({placeholders})
                   AND job_items.status IN ('PENDING', 'RUNNING')
             )
             SELECT job_id, item_type, season_number, episode_number, title, series_title
             FROM ranked WHERE activity_rank = 1"
        );
        let mut statement = self.query(sqlx::AssertSqlSafe(query));
        for job_id in job_ids {
            statement = statement.bind(job_id);
        }
        statement
            .fetch_all(&self.pool)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(|row| {
                        (
                            row.get("job_id"),
                            StoredJobActivityItem {
                                item_type: row.get("item_type"),
                                season_number: row.get("season_number"),
                                episode_number: row.get("episode_number"),
                                title: row.get("title"),
                                series_title: row.get("series_title"),
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

    pub(crate) async fn list_current_chapter_detection_items(
        &self,
        job_ids: &[String],
    ) -> Result<Vec<(String, StoredJobActivityItem)>, StorageError> {
        if job_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat_n("?", job_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "WITH ranked AS (
                 SELECT job_items.job_id, items.item_type, items.season_number,
                        items.episode_number, items.title, series.title AS series_title,
                        ROW_NUMBER() OVER (
                            PARTITION BY job_items.job_id
                            ORDER BY CASE WHEN job_items.status = 'RUNNING' THEN 0 ELSE 1 END,
                                     job_items.updated_at DESC, job_items.source_id
                        ) AS activity_rank
                 FROM chapter_detection_job_items job_items
                 JOIN media_items items ON items.id = job_items.item_id
                 LEFT JOIN media_items series ON series.id = items.series_id
                 WHERE job_items.job_id IN ({placeholders})
                   AND job_items.status IN ('PENDING', 'RUNNING')
             )
             SELECT job_id, item_type, season_number, episode_number, title, series_title
             FROM ranked WHERE activity_rank = 1"
        );
        let mut statement = self.query(sqlx::AssertSqlSafe(query));
        for job_id in job_ids {
            statement = statement.bind(job_id);
        }
        statement
            .fetch_all(&self.pool)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(|row| {
                        (
                            row.get("job_id"),
                            StoredJobActivityItem {
                                item_type: row.get("item_type"),
                                season_number: row.get("season_number"),
                                episode_number: row.get("episode_number"),
                                title: row.get("title"),
                                series_title: row.get("series_title"),
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

    pub(crate) async fn list_current_danmaku_match_items(
        &self,
        job_ids: &[String],
    ) -> Result<Vec<(String, StoredJobActivityItem)>, StorageError> {
        if job_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat_n("?", job_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "WITH ranked AS (
                 SELECT job_items.job_id, items.item_type, items.season_number,
                        items.episode_number, items.title, series.title AS series_title,
                        ROW_NUMBER() OVER (
                            PARTITION BY job_items.job_id
                            ORDER BY CASE WHEN job_items.status = 'RUNNING' THEN 0 ELSE 1 END,
                                     job_items.updated_at DESC, job_items.id
                        ) AS activity_rank
                 FROM danmaku_match_job_items job_items
                 JOIN media_sources sources ON sources.id = job_items.media_source_id
                 JOIN media_items items ON items.id = sources.item_id
                 LEFT JOIN media_items series ON series.id = items.series_id
                 WHERE job_items.job_id IN ({placeholders})
                   AND job_items.status IN ('PENDING', 'RUNNING')
             )
             SELECT job_id, item_type, season_number, episode_number, title, series_title
             FROM ranked WHERE activity_rank = 1"
        );
        let mut statement = self.query(sqlx::AssertSqlSafe(query));
        for job_id in job_ids {
            statement = statement.bind(job_id);
        }
        statement
            .fetch_all(&self.pool)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(|row| {
                        (
                            row.get("job_id"),
                            StoredJobActivityItem {
                                item_type: row.get("item_type"),
                                season_number: row.get("season_number"),
                                episode_number: row.get("episode_number"),
                                title: row.get("title"),
                                series_title: row.get("series_title"),
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

    pub(crate) async fn active_library_metadata_reidentify_job_id(
        &self,
    ) -> Result<Option<String>, StorageError> {
        self.query_scalar(
            "SELECT id
             FROM metadata_reidentify_jobs
             WHERE job_scope = 'LIBRARY'
               AND status IN ('QUEUED', 'RUNNING')
             ORDER BY created_at DESC, id DESC
             LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn claim_metadata_reidentify_job(
        &self,
        job_id: &str,
    ) -> Result<bool, StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let result = self
            .query(
                "UPDATE metadata_reidentify_jobs
             SET status = 'RUNNING', started_at = COALESCE(started_at, unixepoch()),
                 updated_at = unixepoch()
             WHERE id = ? AND status = 'QUEUED' AND cancel_requested = 0",
            )
            .bind(job_id)
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

    #[cfg(test)]
    pub(crate) async fn next_metadata_reidentify_item(
        &self,
        job_id: &str,
    ) -> Result<Option<String>, StorageError> {
        self.query_scalar(
            "WITH prioritized AS (
                 SELECT job_items.item_id, job_items.status,
                        CASE
                            WHEN items.item_type IN ('MOVIE', 'SERIES') THEN 0
                            WHEN items.item_type = 'SEASON' THEN 1
                            WHEN items.item_type = 'EPISODE' THEN 2
                            ELSE 3
                        END AS priority
                 FROM metadata_reidentify_job_items job_items
                 JOIN media_items items ON items.id = job_items.item_id
                 WHERE job_items.job_id = ?
             )
             SELECT item_id
             FROM prioritized
             WHERE status = 'PENDING'
               AND priority = (
                   SELECT MIN(priority)
                   FROM prioritized
                   WHERE status IN ('PENDING', 'RUNNING')
               )
             ORDER BY item_id
             LIMIT 1",
        )
        .bind(job_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    /// Claims up to `limit` metadata items in one write transaction.
    ///
    /// Keeping the priority selection and status updates in the same
    /// transaction avoids one read, one write transaction, and one commit per
    /// worker slot while preserving the existing series/season/episode order.
    pub(crate) async fn claim_next_metadata_reidentify_items(
        &self,
        job_id: &str,
        limit: usize,
    ) -> Result<Vec<String>, StorageError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let mut claimed = self
            .query_scalar::<String>(
                "WITH prioritized AS (
                     SELECT job_items.item_id, job_items.status,
                            CASE
                                WHEN items.item_type IN ('MOVIE', 'SERIES') THEN 0
                                WHEN items.item_type = 'SEASON' THEN 1
                                WHEN items.item_type = 'EPISODE' THEN 2
                                ELSE 3
                            END AS priority
                     FROM metadata_reidentify_job_items job_items
                     JOIN media_items items ON items.id = job_items.item_id
                     WHERE job_items.job_id = ?
                 ), eligible AS (
                     SELECT item_id
                     FROM prioritized
                     WHERE status = 'PENDING'
                       AND priority = (
                           SELECT MIN(priority)
                           FROM prioritized
                           WHERE status IN ('PENDING', 'RUNNING')
                       )
                     ORDER BY item_id
                     LIMIT ?
                 )
                 UPDATE metadata_reidentify_job_items
                 SET status = 'RUNNING', updated_at = unixepoch()
                 WHERE job_id = ? AND status = 'PENDING'
                   AND item_id IN (SELECT item_id FROM eligible)
                   AND EXISTS (
                       SELECT 1 FROM metadata_reidentify_jobs
                       WHERE id = ? AND status IN ('QUEUED', 'RUNNING')
                         AND cancel_requested = 0
                   )
                 RETURNING item_id",
            )
            .bind(job_id)
            .bind(i64::try_from(limit).unwrap_or(i64::MAX))
            .bind(job_id)
            .bind(job_id)
            .fetch_all(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        claimed.sort_unstable();
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(claimed)
    }

    pub(crate) async fn finish_metadata_reidentify_item(
        &self,
        job_id: &str,
        item_id: &str,
        status: &str,
        candidate_count: i64,
        error: Option<&str>,
    ) -> Result<(), StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        self.query(
            "UPDATE metadata_reidentify_job_items
             SET status = ?, candidate_count = ?, error = ?, updated_at = unixepoch()
             WHERE job_id = ? AND item_id = ? AND status = 'RUNNING'",
        )
        .bind(status)
        .bind(candidate_count)
        .bind(error)
        .bind(job_id)
        .bind(item_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query(
            "UPDATE metadata_reidentify_jobs
             SET processed_count = processed_count + 1, updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(job_id)
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

    pub(crate) async fn fail_running_metadata_reidentify_items(
        &self,
        job_id: &str,
        error: &str,
    ) -> Result<i64, StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let result = self
            .query(
                "UPDATE metadata_reidentify_job_items
                 SET status = 'FAILED', candidate_count = 0, error = ?, updated_at = unixepoch()
                 WHERE job_id = ? AND status = 'RUNNING'",
            )
            .bind(error)
            .bind(job_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let affected = i64::try_from(result.rows_affected()).unwrap_or(i64::MAX);
        if affected > 0 {
            self.query(
                "UPDATE metadata_reidentify_jobs
                 SET processed_count = processed_count + ?, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(affected)
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
            })?;
        Ok(affected)
    }

    pub(crate) async fn requeue_running_metadata_reidentify_items(
        &self,
        job_id: &str,
    ) -> Result<u64, StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let result = self
            .query(
                "UPDATE metadata_reidentify_job_items
             SET status = 'PENDING', error = NULL, updated_at = unixepoch()
             WHERE job_id = ? AND status = 'RUNNING'",
            )
            .bind(job_id)
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
        Ok(result.rows_affected())
    }

    pub(crate) async fn finish_metadata_reidentify_job(
        &self,
        job_id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<(), StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        self.query(
            "UPDATE metadata_reidentify_jobs
             SET status = CASE WHEN cancel_requested = 1 THEN 'CANCELLED' ELSE ? END,
                 error = CASE WHEN cancel_requested = 1 THEN NULL ELSE ? END,
                 finished_at = unixepoch(), updated_at = unixepoch()
             WHERE id = ? AND status IN ('QUEUED', 'RUNNING')",
        )
        .bind(status)
        .bind(error)
        .bind(job_id)
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

    pub(crate) async fn metadata_reidentify_job_cancel_requested(
        &self,
        job_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar("SELECT cancel_requested FROM metadata_reidentify_jobs WHERE id = ?")
            .bind(job_id)
            .fetch_one(&self.pool)
            .await
            .map(|value: i64| value != 0)
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn request_metadata_reidentify_job_cancel(
        &self,
        job_id: &str,
    ) -> Result<bool, StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let result = self
            .query(
                "UPDATE metadata_reidentify_jobs
             SET cancel_requested = 1, updated_at = unixepoch()
             WHERE id = ? AND status IN ('QUEUED', 'RUNNING')",
            )
            .bind(job_id)
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

    pub(crate) async fn retry_metadata_reidentify_job(
        &self,
        job_id: &str,
    ) -> Result<bool, StorageError> {
        let _write_guard = self.acquire_metadata_write_lock().await;
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let result = self
            .query(
                "UPDATE metadata_reidentify_jobs
             SET status = 'QUEUED',
                 processed_count = (
                     SELECT COUNT(*) FROM metadata_reidentify_job_items
                     WHERE job_id = ? AND status = 'COMPLETED'
                 ),
                 cancel_requested = 0, error = NULL, started_at = NULL, finished_at = NULL,
                 updated_at = unixepoch()
             WHERE id = ? AND status IN ('FAILED', 'CANCELLED', 'COMPLETED_WITH_ISSUES', 'DEFERRED')",
            )
            .bind(job_id)
            .bind(job_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if result.rows_affected() == 1 {
            self.query(
                "UPDATE metadata_reidentify_job_items
                 SET status = 'PENDING', candidate_count = 0, error = NULL,
                     updated_at = unixepoch()
                 WHERE job_id = ? AND status IN ('FAILED', 'RUNNING', 'PENDING')",
            )
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
            })?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn list_metadata_reidentify_items(
        &self,
        job_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredMetadataReidentifyItem>, StorageError> {
        self.query(
            "SELECT job_id, item_id, status, candidate_count, error, updated_at
             FROM metadata_reidentify_job_items
             WHERE job_id = ? ORDER BY item_id LIMIT ? OFFSET ?",
        )
        .bind(job_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(stored_metadata_reidentify_item)
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_scan_job(
        &self,
        id: &str,
    ) -> Result<Option<StoredScanJob>, StorageError> {
        self.query(
            "SELECT id, library_id, job_type, status, generation, cursor,
                    processed_count, total_count, cancel_requested, error,
                    created_at, started_at, finished_at,
                    discovery_completed, auto_metadata_match,
                    current_item, scan_phase
             FROM scan_jobs WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_scan_job))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_scan_jobs(
        &self,
        status: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredScanJob>, StorageError> {
        let rows = if let Some(status) = status {
            self.query(
                "SELECT id, library_id, job_type, status, generation, cursor,
                        processed_count, total_count, cancel_requested, error,
                        created_at, started_at, finished_at,
                        discovery_completed, auto_metadata_match,
                        current_item, scan_phase
                 FROM scan_jobs WHERE status = ?
                 ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
            )
            .bind(status)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        } else {
            self.query(
                "SELECT id, library_id, job_type, status, generation, cursor,
                        processed_count, total_count, cancel_requested, error,
                        created_at, started_at, finished_at,
                        discovery_completed, auto_metadata_match,
                        current_item, scan_phase
                 FROM scan_jobs
                 ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
            )
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        };
        rows.map(|rows| rows.into_iter().map(stored_scan_job).collect())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn count_scan_jobs_by_status(
        &self,
    ) -> Result<StoredScanJobCounts, StorageError> {
        self.query(
            "SELECT
                SUM(CASE WHEN status IN ('PENDING', 'RUNNING') THEN 1 ELSE 0 END) AS running,
                SUM(CASE WHEN status = 'FAILED' THEN 1 ELSE 0 END) AS failed
             FROM scan_jobs",
        )
        .fetch_one(&self.pool)
        .await
        .map(|row| StoredScanJobCounts {
            running: row.get::<Option<i64>, _>("running").unwrap_or(0),
            failed: row.get::<Option<i64>, _>("failed").unwrap_or(0),
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_scan_jobs_for_activity(
        &self,
        limit: i64,
    ) -> Result<Vec<StoredScanJob>, StorageError> {
        self.query(
            "SELECT id, library_id, job_type, status, generation, cursor,
                    processed_count, total_count, cancel_requested, error,
                    created_at, started_at, finished_at,
                    discovery_completed, auto_metadata_match,
                    current_item, scan_phase
             FROM scan_jobs
             WHERE status IN ('PENDING', 'RUNNING')
                OR (status = 'COMPLETED' AND scan_phase = 'POSTPROCESSING')
             ORDER BY created_at DESC, id DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map(|rows| rows.into_iter().map(stored_scan_job).collect())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_scan_job_ids_needing_resume(
        &self,
    ) -> Result<Vec<String>, StorageError> {
        self.query_scalar(
            "SELECT id FROM scan_jobs
             WHERE status IN ('PENDING', 'RUNNING')
                OR (status = 'COMPLETED' AND scan_phase = 'POSTPROCESSING')
             ORDER BY created_at, id LIMIT 10000",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn metadata_reidentify_job_has_failed_items(
        &self,
        job_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(
                 SELECT 1 FROM metadata_reidentify_job_items
                 WHERE job_id = ? AND status = 'FAILED'
             ) THEN 1 ELSE 0 END",
        )
        .bind(job_id)
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn metadata_reidentify_job_has_item_error(
        &self,
        job_id: &str,
        error: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(
                 SELECT 1 FROM metadata_reidentify_job_items
                 WHERE job_id = ? AND error = ?
             ) THEN 1 ELSE 0 END",
        )
        .bind(job_id)
        .bind(error)
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_active_metadata_reidentify_job_ids(
        &self,
    ) -> Result<Vec<String>, StorageError> {
        self.query_scalar(
            "SELECT id FROM metadata_reidentify_jobs
             WHERE status IN ('QUEUED', 'RUNNING')
             ORDER BY created_at, id LIMIT 10000",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_active_scan_job_for_library(
        &self,
        library_id: &str,
    ) -> Result<Option<StoredScanJob>, StorageError> {
        self.query(
            "SELECT id, library_id, job_type, status, generation, cursor,
                    processed_count, total_count, cancel_requested, error,
                    created_at, started_at, finished_at,
                    discovery_completed, auto_metadata_match,
                    current_item, scan_phase
             FROM scan_jobs
             WHERE library_id = ? AND status IN ('PENDING', 'RUNNING')
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(library_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_scan_job))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_active_scan_job(
        &self,
        library_id: &str,
        job_type: &str,
    ) -> Result<Option<StoredScanJob>, StorageError> {
        self.query(
            "SELECT id, library_id, job_type, status, generation, cursor,
                    processed_count, total_count, cancel_requested, error,
                    created_at, started_at, finished_at,
                    discovery_completed, auto_metadata_match,
                    current_item, scan_phase
             FROM scan_jobs
             WHERE library_id = ? AND job_type = ? AND status IN ('PENDING', 'RUNNING')
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(library_id)
        .bind(job_type)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_scan_job))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn has_active_scan_job_type(
        &self,
        job_type: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(
                 SELECT 1 FROM scan_jobs
                 WHERE job_type = ? AND status IN ('PENDING', 'RUNNING')
             ) THEN 1 ELSE 0 END",
        )
        .bind(job_type)
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn has_running_scan_job_type(
        &self,
        job_type: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS (
                 SELECT 1 FROM scan_jobs WHERE job_type = ? AND status = 'RUNNING'
             ) THEN 1 ELSE 0 END",
        )
        .bind(job_type)
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn has_unready_manifest_target_materialization_for_library(
        &self,
        library_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS (
                 SELECT 1
                 FROM scan_jobs job
                 JOIN scan_manifests manifest ON manifest.job_id = job.id
                 WHERE job.library_id = ? AND job.job_type = 'RECONCILE_LIBRARY'
                   AND job.status = 'COMPLETED' AND job.scan_phase = 'POSTPROCESSING'
                   AND manifest.workflow_version = 2
                   AND manifest.discovery_format_version = 3
                   AND manifest.postprocessing_targets_ready = 0
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

    pub(crate) async fn claim_scan_job(&self, id: &str) -> Result<bool, StorageError> {
        self.query(
            "UPDATE scan_jobs
             SET status = 'RUNNING', started_at = COALESCE(started_at, unixepoch()),
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

    pub(crate) async fn update_scan_job_progress(
        &self,
        id: &str,
        cursor: Option<&str>,
        processed_count: i64,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE scan_jobs
             SET cursor = ?, processed_count = ?, updated_at = unixepoch()
             WHERE id = ? AND status = 'RUNNING'",
        )
        .bind(cursor)
        .bind(processed_count)
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn update_scan_job_activity(
        &self,
        id: &str,
        current_item: Option<&str>,
        scan_phase: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE scan_jobs
             SET current_item = ?, scan_phase = ?, updated_at = unixepoch()
             WHERE id = ? AND (status IN ('PENDING', 'RUNNING')
                OR (status = 'COMPLETED' AND scan_phase = 'POSTPROCESSING'))",
        )
        .bind(current_item)
        .bind(scan_phase)
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn scan_job_cancel_requested(&self, id: &str) -> Result<bool, StorageError> {
        self.query_scalar("SELECT cancel_requested FROM scan_jobs WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .map(|value: i64| value != 0)
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
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
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    #[allow(dead_code)]
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
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn update_external_subtitle(
        &self,
        update: ExternalSubtitleUpdate<'_>,
    ) -> Result<bool, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
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
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
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
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
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

    pub(crate) async fn request_scan_job_cancel(&self, id: &str) -> Result<(), StorageError> {
        self.query(
            "UPDATE scan_jobs SET cancel_requested = 1, updated_at = unixepoch()
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

    pub(crate) async fn finish_scan_job(
        &self,
        id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE scan_jobs
             SET status = ?, error = ?, cursor = NULL, current_item = NULL,
                 scan_phase = 'IDLE',
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

    pub(crate) async fn mark_scan_job_postprocessing(&self, id: &str) -> Result<(), StorageError> {
        self.query(
            "UPDATE scan_jobs
             SET status = 'COMPLETED', cursor = NULL, current_item = NULL,
                 scan_phase = 'POSTPROCESSING',
                 error = NULL, finished_at = COALESCE(finished_at, unixepoch()),
                 updated_at = unixepoch()
             WHERE id = ? AND status = 'RUNNING'",
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

    pub(crate) async fn complete_scan_job_postprocessing(
        &self,
        id: &str,
    ) -> Result<bool, StorageError> {
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
                "UPDATE scan_jobs
                 SET status = 'COMPLETED', error = NULL, cursor = NULL,
                     current_item = NULL, cancel_requested = 0,
                     scan_phase = 'IDLE', finished_at = COALESCE(finished_at, unixepoch()),
                     updated_at = unixepoch()
                 WHERE id = ? AND status IN ('RUNNING', 'COMPLETED')
                   AND scan_phase = 'POSTPROCESSING'
                   AND NOT EXISTS (
                       SELECT 1 FROM scan_job_targets
                       WHERE job_id = ?
                         AND (
                             probe_state IN ('PENDING', 'FAILED')
                             OR metadata_state IN ('PENDING', 'FAILED')
                             OR thumbnail_state IN ('PENDING', 'FAILED')
                         )
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM scan_manifests
                       WHERE job_id = ? AND workflow_version = 2
                         AND discovery_format_version = 3
                         AND postprocessing_targets_ready = 0
                   )",
            )
            .bind(id)
            .bind(id)
            .bind(id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if result.rows_affected() == 1 {
            self.query(
                "UPDATE scan_manifests
                 SET state = 'COMPLETED', resume_state = NULL,
                     completed_at = COALESCE(completed_at, unixepoch()),
                     updated_at = unixepoch()
                 WHERE job_id = ? AND state = 'POSTPROCESSING'",
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

    pub(crate) async fn fail_scan_job_postprocessing(
        &self,
        id: &str,
    ) -> Result<bool, StorageError> {
        let result = self
            .query(
                "UPDATE scan_jobs
                 SET status = 'COMPLETED', error = NULL, cursor = NULL,
                     current_item = NULL, cancel_requested = 0,
                     scan_phase = 'IDLE', finished_at = COALESCE(finished_at, unixepoch()),
                     updated_at = unixepoch()
                 WHERE id = ? AND status IN ('RUNNING', 'COMPLETED')
                   AND scan_phase = 'POSTPROCESSING'
                   AND (
                     EXISTS (
                       SELECT 1 FROM scan_job_targets
                       WHERE job_id = ?
                         AND (
                             probe_state IN ('PENDING', 'FAILED')
                             OR metadata_state IN ('PENDING', 'FAILED')
                             OR thumbnail_state IN ('PENDING', 'FAILED')
                         )
                     ) OR EXISTS (
                       SELECT 1 FROM scan_manifests
                       WHERE job_id = ? AND workflow_version = 2
                         AND discovery_format_version = 3
                         AND postprocessing_targets_ready = 0
                     )
                   )",
            )
            .bind(id)
            .bind(id)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn has_scan_job_targets(&self, job_id: &str) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(
                 SELECT 1 FROM scan_job_targets WHERE job_id = ?
             ) THEN 1 ELSE 0 END",
        )
        .bind(job_id)
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn has_unready_scan_manifest_postprocessing_targets(
        &self,
        job_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(
                 SELECT 1 FROM scan_manifests
                 WHERE job_id = ? AND workflow_version = 2
                   AND discovery_format_version = 3
                   AND postprocessing_targets_ready = 0
             ) THEN 1 ELSE 0 END",
        )
        .bind(job_id)
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn retry_scan_job_postprocessing(
        &self,
        id: &str,
    ) -> Result<bool, StorageError> {
        let result = self
            .query(
                "UPDATE scan_jobs
                 SET status = CASE WHEN status = 'COMPLETED' THEN 'COMPLETED' ELSE 'RUNNING' END,
                     cancel_requested = 0, error = NULL,
                     current_item = NULL, scan_phase = 'POSTPROCESSING',
                     started_at = COALESCE(started_at, unixepoch()),
                     finished_at = CASE WHEN status = 'COMPLETED' THEN finished_at ELSE NULL END,
                     updated_at = unixepoch()
                 WHERE id = ? AND status IN ('COMPLETED', 'FAILED', 'CANCELLED')
                   AND job_type = 'RECONCILE_LIBRARY'
                   AND scan_phase = 'IDLE'
                   AND NOT EXISTS (
                       SELECT 1 FROM reconciliation_scan_entries
                       WHERE job_id = ?
                   )",
            )
            .bind(id)
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn retry_scan_job(&self, id: &str) -> Result<bool, StorageError> {
        self.query(
            "UPDATE scan_jobs
             SET status = 'PENDING', cancel_requested = 0, error = NULL,
                 current_item = NULL, scan_phase = 'IDLE',
                 started_at = NULL, finished_at = NULL, updated_at = unixepoch()
             WHERE id = ? AND status IN ('FAILED', 'CANCELLED')",
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

    pub(crate) async fn update_library_last_scan(
        &self,
        library_id: &str,
    ) -> Result<(), StorageError> {
        self.query("UPDATE libraries SET last_scan_at = unixepoch() WHERE id = ?")
            .bind(library_id)
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn update_root_scan_cursor(
        &self,
        root_id: &str,
        cursor: Option<&str>,
    ) -> Result<(), StorageError> {
        self.query("UPDATE library_roots SET scan_cursor = ? WHERE id = ?")
            .bind(cursor)
            .bind(root_id)
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn find_library_root(
        &self,
        id: &str,
    ) -> Result<Option<StoredLibraryRoot>, StorageError> {
        self.query(
            "SELECT id, library_id, canonical_path, display_path,
                    is_available, is_writable, last_checked_at,
                    unavailable_since, scan_cursor
             FROM library_roots WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_library_root))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn update_library_root_availability(
        &self,
        root_id: &str,
        is_available: bool,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE library_roots
             SET is_available = ?, last_checked_at = unixepoch(),
                 unavailable_since = CASE
                     WHEN ? = 1 THEN NULL
                     ELSE COALESCE(unavailable_since, unixepoch())
                 END
             WHERE id = ?",
        )
        .bind(database_flag(is_available))
        .bind(database_flag(is_available))
        .bind(root_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_filesystem_entry(
        &self,
        library_root_id: &str,
        relative_path: &str,
    ) -> Result<Option<StoredFilesystemEntry>, StorageError> {
        self.query(
            "SELECT fe.id, fe.relative_path, fe.fingerprint, fe.last_seen_generation, ms.item_id,
                    CASE WHEN parent.removed_at IS NULL THEN parent.identity_key END
                        AS parent_identity_key,
                    item.item_type AS item_type,
                    CASE WHEN item.removed_at IS NULL THEN item.identity_key END
                        AS item_identity_key,
                    series.provider_ids_json AS series_provider_ids_json
             FROM filesystem_entries fe
             LEFT JOIN media_sources ms ON ms.filesystem_entry_id = fe.id
             LEFT JOIN media_items item ON item.id = ms.item_id
             LEFT JOIN media_items parent ON parent.id = item.parent_id
             LEFT JOIN media_items series ON series.id = item.series_id
             WHERE fe.library_root_id = ? AND fe.relative_path = ?",
        )
        .bind(library_root_id)
        .bind(relative_path)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_filesystem_entry))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_filesystem_entries_for_paths(
        &self,
        library_root_id: &str,
        relative_paths: &[String],
    ) -> Result<HashMap<String, StoredFilesystemEntry>, StorageError> {
        let mut entries = HashMap::new();
        for chunk in relative_paths.chunks(500) {
            if chunk.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT fe.id, fe.relative_path, fe.fingerprint, fe.last_seen_generation, ms.item_id,
                        CASE WHEN parent.removed_at IS NULL THEN parent.identity_key END
                            AS parent_identity_key,
                        item.item_type AS item_type,
                        CASE WHEN item.removed_at IS NULL THEN item.identity_key END
                            AS item_identity_key,
                        series.provider_ids_json AS series_provider_ids_json
                 FROM filesystem_entries fe
                 LEFT JOIN media_sources ms ON ms.filesystem_entry_id = fe.id
                 LEFT JOIN media_items item ON item.id = ms.item_id
                 LEFT JOIN media_items parent ON parent.id = item.parent_id
                 LEFT JOIN media_items series ON series.id = item.series_id
                 WHERE fe.library_root_id = ? AND fe.relative_path IN ({placeholders})"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query)).bind(library_root_id);
            for relative_path in chunk {
                statement = statement.bind(relative_path);
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
                let entry = stored_filesystem_entry(row);
                entries.insert(entry.relative_path.clone(), entry);
            }
        }
        Ok(entries)
    }

    pub(crate) async fn has_filesystem_entries_for_root(
        &self,
        library_root_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar::<i64>(
            "SELECT CASE WHEN EXISTS (
                 SELECT 1 FROM filesystem_entries WHERE library_root_id = ?
             ) THEN 1 ELSE 0 END",
        )
        .bind(library_root_id)
        .fetch_one(&self.pool)
        .await
        .map(|value| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_filesystem_entry_by_inode(
        &self,
        library_id: &str,
        target_root_id: &str,
        inode: i64,
        relative_path: &str,
    ) -> Result<Option<StoredFilesystemEntry>, StorageError> {
        let rows = self
            .query(
                "SELECT fe.id, fe.relative_path, fe.fingerprint, fe.last_seen_generation, ms.item_id,
                        CASE WHEN parent.removed_at IS NULL THEN parent.identity_key END
                            AS parent_identity_key,
                        item.item_type AS item_type,
                        CASE WHEN item.removed_at IS NULL THEN item.identity_key END
                            AS item_identity_key,
                        series.provider_ids_json AS series_provider_ids_json
                 FROM filesystem_entries fe
                 JOIN library_roots lr ON lr.id = fe.library_root_id
                 LEFT JOIN media_sources ms ON ms.filesystem_entry_id = fe.id
                 LEFT JOIN media_items item ON item.id = ms.item_id
                 LEFT JOIN media_items parent ON parent.id = item.parent_id
                 LEFT JOIN media_items series ON series.id = item.series_id
                 WHERE lr.library_id = ? AND fe.inode = ?
                   AND NOT (fe.library_root_id = ? AND fe.relative_path = ?)
                 LIMIT 2",
            )
            .bind(library_id)
            .bind(inode)
            .bind(target_root_id)
            .bind(relative_path)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if rows.len() != 1 {
            return Ok(None);
        }
        Ok(rows.into_iter().next().map(stored_filesystem_entry))
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

    pub(crate) async fn move_filesystem_entry(
        &self,
        entry: FilesystemEntryMove<'_>,
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
            "UPDATE filesystem_entries
             SET library_root_id = ?, relative_path = ?, size = ?, modified_at = ?, inode = ?,
                 fingerprint = ?, last_seen_generation = ?, is_missing = 0,
                 updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(entry.library_root_id)
        .bind(entry.relative_path)
        .bind(entry.size)
        .bind(entry.modified_at)
        .bind(entry.inode)
        .bind(entry.fingerprint)
        .bind(entry.generation)
        .bind(entry.entry_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.restore_media_items_for_filesystem_entries(
            &mut transaction,
            &[entry.entry_id.to_owned()],
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

    pub(crate) async fn update_filesystem_entry_inode(
        &self,
        entry_id: &str,
        inode: Option<i64>,
    ) -> Result<(), StorageError> {
        self.query("UPDATE filesystem_entries SET inode = ?, updated_at = unixepoch() WHERE id = ?")
            .bind(inode)
            .bind(entry_id)
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn mark_filesystem_entries_seen_batch(
        &self,
        entry_ids: &[String],
        last_seen_generation: &str,
    ) -> Result<(), StorageError> {
        if entry_ids.is_empty() {
            return Ok(());
        }
        let mut transaction = self.begin_scan_write_transaction().await?;
        self.mark_filesystem_entries_seen_batch_in_transaction(
            &mut transaction,
            entry_ids,
            last_seen_generation,
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

    async fn restore_filesystem_entries_batch_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        entry_ids: &[String],
    ) -> Result<(), StorageError> {
        if entry_ids.is_empty() {
            return Ok(());
        }
        for chunk in entry_ids.chunks(500) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "UPDATE filesystem_entries
                 SET is_missing = 0, updated_at = unixepoch()
                 WHERE is_missing = 1 AND id IN ({placeholders})"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for entry_id in chunk {
                statement = statement.bind(entry_id);
            }
            let restored = statement
                .execute(&mut **transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
                .rows_affected();
            if restored > 0 {
                self.restore_media_items_for_filesystem_entries(transaction, chunk)
                    .await?;
            }
        }
        Ok(())
    }

    async fn mark_filesystem_entries_seen_batch_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        entry_ids: &[String],
        last_seen_generation: &str,
    ) -> Result<(), StorageError> {
        if entry_ids.is_empty() {
            return Ok(());
        }
        for chunk in entry_ids.chunks(500) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "UPDATE filesystem_entries
                 SET last_seen_generation = ?, is_missing = 0, updated_at = unixepoch()
                 WHERE id IN ({placeholders})"
            );
            let mut statement = self
                .query(sqlx::AssertSqlSafe(query))
                .bind(last_seen_generation);
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
            self.restore_media_items_for_filesystem_entries(transaction, chunk)
                .await?;
        }
        Ok(())
    }

    pub(crate) async fn update_filesystem_entry(
        &self,
        id: &str,
        size: i64,
        modified_at: i64,
        fingerprint: &[u8],
        last_seen_generation: &str,
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
            "UPDATE filesystem_entries
             SET size = ?, modified_at = ?, fingerprint = ?, last_seen_generation = ?,
                 is_missing = 0, updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(size)
        .bind(modified_at)
        .bind(fingerprint)
        .bind(last_seen_generation)
        .bind(id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.restore_media_items_for_filesystem_entries(&mut transaction, &[id.to_owned()])
            .await?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn mark_filesystem_entry_seen(
        &self,
        id: &str,
        last_seen_generation: &str,
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
            "UPDATE filesystem_entries
             SET last_seen_generation = ?, is_missing = 0, updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(last_seen_generation)
        .bind(id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.restore_media_items_for_filesystem_entries(&mut transaction, &[id.to_owned()])
            .await?;
        transaction
            .commit()
            .await
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

    pub(crate) async fn mark_missing_filesystem_entries(
        &self,
        library_root_id: &str,
        generation: &str,
    ) -> Result<u64, StorageError> {
        let mut transaction = self.begin_scan_write_transaction().await?;
        let missing_entries = self
            .mark_missing_filesystem_entries_in_transaction(
                &mut transaction,
                library_root_id,
                generation,
            )
            .await?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(missing_entries)
    }

    async fn mark_missing_filesystem_entries_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        library_root_id: &str,
        generation: &str,
    ) -> Result<u64, StorageError> {
        let missing_entries = self
            .query(
                "UPDATE filesystem_entries
             SET is_missing = 1, updated_at = unixepoch()
             WHERE library_root_id = ? AND last_seen_generation != ? AND is_missing = 0",
            )
            .bind(library_root_id)
            .bind(generation)
            .execute(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .rows_affected();
        self.refresh_removed_media_items_in_transaction(transaction, library_root_id)
            .await?;
        Ok(missing_entries)
    }

    async fn mark_missing_filesystem_entry_paths_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        library_root_id: &str,
        generation: &str,
        relative_paths: &[String],
    ) -> Result<u64, StorageError> {
        let mut missing_entries = 0_u64;
        for paths in relative_paths.chunks(SCAN_DML_CHUNK_SIZE) {
            if paths.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", paths.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "UPDATE filesystem_entries
                 SET is_missing = 1, updated_at = unixepoch()
                 WHERE library_root_id = ? AND last_seen_generation != ? AND is_missing = 0
                   AND relative_path IN ({placeholders})"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            statement = statement.bind(library_root_id).bind(generation);
            for path in paths {
                statement = statement.bind(path);
            }
            missing_entries = missing_entries.saturating_add(
                statement
                    .execute(&mut **transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?
                    .rows_affected(),
            );
        }
        Ok(missing_entries)
    }

    async fn refresh_removed_media_items_in_transaction(
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
                   AND item_type IN ('MOVIE', 'EPISODE', 'UNRESOLVED')
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
        self.query(
            "UPDATE media_sources
             SET size = ?, probe_status = 'PENDING', probe_error = NULL,
                 updated_at = unixepoch()
             WHERE filesystem_entry_id = ?",
        )
        .bind(size)
        .bind(filesystem_entry_id)
        .execute(&mut *transaction)
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

    pub(crate) async fn delete_media_source(
        &self,
        item_id: &str,
        source_id: &str,
    ) -> Result<bool, StorageError> {
        let Some((old_item_id, parent_id, series_id)) = self
            .query_as::<(String, Option<String>, Option<String>)>(
                "SELECT ms.item_id, old_item.parent_id, old_item.series_id
                 FROM media_sources ms
                 JOIN media_items old_item ON old_item.id = ms.item_id
                 WHERE ms.id = ? AND ms.item_id = ?",
            )
            .bind(source_id)
            .bind(item_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
        else {
            return Ok(false);
        };
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.query("DELETE FROM media_sources WHERE id = ? AND item_id = ?")
            .bind(source_id)
            .bind(&old_item_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        for related_item_id in [Some(old_item_id), parent_id, series_id]
            .into_iter()
            .flatten()
        {
            self.query(
                "UPDATE media_items
                 SET removed_at = unixepoch(), updated_at = unixepoch()
                 WHERE id = ? AND removed_at IS NULL
                   AND NOT EXISTS (
                       SELECT 1 FROM media_sources WHERE item_id = media_items.id
                   )
                   AND NOT EXISTS (
                       SELECT 1 FROM media_items child
                       WHERE child.parent_id = media_items.id
                         AND child.removed_at IS NULL
                   )",
            )
            .bind(related_item_id)
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
}

#[cfg(test)]
mod tests {
    use super::{
        Database, NewScanManifest, NewScanManifestDelta, NewScanManifestDiscoveryChunk,
        NewScanManifestEntry, NewScanManifestRoot, prune_sidecar_directories, sidecar_target_query,
    };
    use crate::config::Config;

    #[tokio::test]
    async fn scan_manifest_creation_is_atomic_and_idempotent()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        })
        .await?;
        database
            .query("INSERT INTO libraries (id, name, kind) VALUES ('lib', 'Library', 'MOVIE')")
            .execute(database.pool())
            .await?;
        for (id, available) in [("root-available", 1_i64), ("root-unavailable", 0_i64)] {
            database
                .query(
                    "INSERT INTO library_roots (
                         id, library_id, canonical_path, display_path, is_available, is_writable
                     ) VALUES (?, 'lib', ?, ?, ?, 0)",
                )
                .bind(id)
                .bind(format!("/{id}"))
                .bind(format!("/{id}"))
                .bind(available)
                .execute(database.pool())
                .await?;
        }
        database
            .query(
                "WITH RECURSIVE root_numbers(n) AS (
                     SELECT 1 UNION ALL SELECT n + 1 FROM root_numbers WHERE n < 250
                 )
                 INSERT INTO library_roots (
                     id, library_id, canonical_path, display_path, is_available, is_writable
                 )
                 SELECT 'bulk-root-' || n, 'lib', '/bulk/' || n, '/bulk/' || n, 1, 0
                 FROM root_numbers",
            )
            .execute(database.pool())
            .await?;
        database
            .create_scan_job("job", "lib", "RECONCILE_LIBRARY", "generation", 0, false)
            .await?;

        let bulk_root_ids: Vec<String> = (1..=250)
            .map(|number| format!("bulk-root-{number}"))
            .collect();
        let mut roots = vec![
            NewScanManifestRoot {
                library_root_id: "root-available",
            },
            NewScanManifestRoot {
                library_root_id: "root-unavailable",
            },
        ];
        roots.extend(
            bulk_root_ids
                .iter()
                .map(|library_root_id| NewScanManifestRoot { library_root_id }),
        );
        let manifest = NewScanManifest {
            id: "manifest",
            job_id: "job",
            library_id: "lib",
            roots: &roots,
        };
        database.create_scan_manifest(&manifest).await?;
        database.create_scan_manifest(&manifest).await?;

        let stored = database
            .get_scan_manifest("manifest")
            .await?
            .ok_or("manifest was not stored")?;
        assert_eq!(stored.id, "manifest");
        assert_eq!(stored.job_id, "job");
        assert_eq!(stored.library_id, "lib");
        assert_eq!(stored.state, "DISCOVERING");
        assert_eq!(stored.root_count, 252);
        assert_eq!(stored.discovered_directory_count, 252);
        assert_eq!(stored.completed_directory_count, 0);
        assert_eq!(stored.observed_file_count, 0);
        assert_eq!(stored.remove_count, 0);
        assert_eq!(stored.applied_delta_count, 0);

        let root_states: Vec<(String, String)> = database
            .query_as(
                "SELECT library_root_id, state FROM scan_manifest_roots
                 WHERE manifest_id = ? AND library_root_id IN ('root-available', 'root-unavailable')
                 ORDER BY library_root_id",
            )
            .bind("manifest")
            .fetch_all(database.pool())
            .await?;
        assert_eq!(
            root_states,
            vec![
                ("root-available".to_owned(), "PENDING".to_owned()),
                ("root-unavailable".to_owned(), "PENDING".to_owned()),
            ]
        );
        let initial_frontiers: i64 = database
            .query_scalar(
                "SELECT COUNT(*) FROM scan_manifest_directories
                 WHERE manifest_id = 'manifest' AND state = 'PENDING'",
            )
            .fetch_one(database.pool())
            .await?;
        assert_eq!(initial_frontiers, 252);

        assert!(
            database
                .transition_scan_manifest_state("manifest", "DISCOVERING", "READY_TO_DIFF")
                .await?
        );
        let delta_ids: Vec<String> = (1..=250).map(|number| format!("delta-{number}")).collect();
        let delta_paths: Vec<String> = (1..=250)
            .map(|number| format!("missing-{number}.mkv"))
            .collect();
        let baseline_fingerprint = [1_u8, 2, 3];
        let deltas: Vec<NewScanManifestDelta<'_>> = delta_ids
            .iter()
            .zip(&delta_paths)
            .map(|(id, relative_path)| NewScanManifestDelta {
                id,
                library_root_id: "root-available",
                relative_path,
                observation_sequence: None,
                delta_kind: "REMOVE",
                base_filesystem_entry_id: Some("baseline-entry"),
                base_fingerprint: Some(&baseline_fingerprint),
            })
            .collect();
        assert_eq!(
            database
                .insert_scan_manifest_deltas("manifest", &deltas)
                .await?,
            250
        );
        assert_eq!(
            database
                .insert_scan_manifest_deltas("manifest", &deltas)
                .await?,
            0
        );
        let conflicting_retry = [NewScanManifestDelta {
            id: "different-delta-id",
            library_root_id: "root-available",
            relative_path: &delta_paths[0],
            observation_sequence: None,
            delta_kind: "REMOVE",
            base_filesystem_entry_id: Some("different-baseline-entry"),
            base_fingerprint: Some(&baseline_fingerprint),
        }];
        assert!(
            database
                .insert_scan_manifest_deltas("manifest", &conflicting_retry)
                .await
                .is_err()
        );
        database
            .query(
                "INSERT INTO scan_manifest_entries (
                     manifest_id, library_root_id, relative_path, observation_sequence,
                     entry_kind, size, modified_at
                 ) VALUES ('manifest', 'root-available', 'odd-add.mkv', 1, 'FILE', 10, 20)",
            )
            .execute(database.pool())
            .await?;
        let malformed_add = [NewScanManifestDelta {
            id: "malformed-add",
            library_root_id: "root-available",
            relative_path: "odd-add.mkv",
            observation_sequence: Some(1),
            delta_kind: "ADD",
            base_filesystem_entry_id: None,
            base_fingerprint: Some(&baseline_fingerprint),
        }];
        assert!(
            database
                .insert_scan_manifest_deltas("manifest", &malformed_add)
                .await
                .is_err()
        );
        let stored = database
            .get_scan_manifest("manifest")
            .await?
            .ok_or("manifest disappeared after delta writes")?;
        assert_eq!(stored.remove_count, 250);
        assert!(
            database
                .transition_scan_manifest_state("manifest", "READY_TO_DIFF", "APPLYING")
                .await?
        );
        assert!(
            database
                .transition_scan_manifest_state("manifest", "DISCOVERING", "APPLYING")
                .await
                .is_err()
        );
        database.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn manifest_discovery_chunk_does_not_complete_directory_after_cancel_request()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        })
        .await?;
        database
            .query("INSERT INTO libraries (id, name, kind) VALUES ('lib', 'Library', 'MOVIE')")
            .execute(database.pool())
            .await?;
        database
            .query(
                "INSERT INTO library_roots (
                     id, library_id, canonical_path, display_path, is_available, is_writable
                 ) VALUES ('root', 'lib', '/root', '/root', 1, 0)",
            )
            .execute(database.pool())
            .await?;
        let roots = [NewScanManifestRoot {
            library_root_id: "root",
        }];
        let manifest = NewScanManifest {
            id: "manifest",
            job_id: "job",
            library_id: "lib",
            roots: &roots,
        };
        database
            .create_full_scan_manifest_job("job", "generation", false, &manifest, None)
            .await?;
        assert!(database.claim_scan_job("job").await?);
        database
            .query("UPDATE scan_jobs SET cancel_requested = 1 WHERE id = 'job'")
            .execute(database.pool())
            .await?;
        let entries = [NewScanManifestEntry {
            relative_path: String::new(),
            entry_kind: "DIRECTORY".to_owned(),
            size: 0,
            modified_at: 0,
            device: None,
            inode: None,
            fingerprint: Vec::new(),
        }];
        let chunk = NewScanManifestDiscoveryChunk {
            manifest_id: "manifest",
            job_id: "job",
            library_root_id: "root",
            child_directories: &[],
            entries: &entries,
            positive_indexes: &[],
            unchanged_paths: &[],
            completed_directory: Some(""),
        };

        assert!(
            database
                .commit_scan_manifest_discovery_chunk(&chunk)
                .await
                .is_err()
        );
        let directory_state: String = database
            .query_scalar(
                "SELECT state FROM scan_manifest_directories
                 WHERE manifest_id = 'manifest' AND relative_path = ''",
            )
            .fetch_one(database.pool())
            .await?;
        assert_eq!(directory_state, "PENDING");
        let root_state: String = database
            .query_scalar(
                "SELECT state FROM scan_manifest_roots
                 WHERE manifest_id = 'manifest' AND library_root_id = 'root'",
            )
            .fetch_one(database.pool())
            .await?;
        assert_eq!(root_state, "PENDING");
        let observations: i64 = database
            .query_scalar("SELECT COUNT(*) FROM scan_manifest_entries")
            .fetch_one(database.pool())
            .await?;
        assert_eq!(observations, 0);
        database.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn manifest_discovery_finalize_does_not_ignore_cancel_request()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        })
        .await?;
        database
            .query("INSERT INTO libraries (id, name, kind) VALUES ('lib', 'Library', 'MOVIE')")
            .execute(database.pool())
            .await?;
        database
            .query(
                "INSERT INTO library_roots (
                     id, library_id, canonical_path, display_path, is_available, is_writable
                 ) VALUES ('root', 'lib', '/root', '/root', 1, 0)",
            )
            .execute(database.pool())
            .await?;
        let roots = [NewScanManifestRoot {
            library_root_id: "root",
        }];
        let manifest = NewScanManifest {
            id: "manifest",
            job_id: "job",
            library_id: "lib",
            roots: &roots,
        };
        database
            .create_full_scan_manifest_job("job", "generation", false, &manifest, None)
            .await?;
        assert!(database.claim_scan_job("job").await?);
        let entries = [NewScanManifestEntry {
            relative_path: String::new(),
            entry_kind: "DIRECTORY".to_owned(),
            size: 0,
            modified_at: 0,
            device: None,
            inode: None,
            fingerprint: Vec::new(),
        }];
        let chunk = NewScanManifestDiscoveryChunk {
            manifest_id: "manifest",
            job_id: "job",
            library_root_id: "root",
            child_directories: &[],
            entries: &entries,
            positive_indexes: &[],
            unchanged_paths: &[],
            completed_directory: Some(""),
        };
        database
            .commit_scan_manifest_discovery_chunk(&chunk)
            .await?;
        database
            .query("UPDATE scan_jobs SET cancel_requested = 1 WHERE id = 'job'")
            .execute(database.pool())
            .await?;

        assert!(
            database
                .finish_scan_manifest_discovery("manifest", "job")
                .await
                .is_err()
        );
        let job_state: (i64, String) = database
            .query_as(
                "SELECT discovery_completed,
                        (SELECT state FROM scan_manifests WHERE id = 'manifest')
                 FROM scan_jobs WHERE id = 'job'",
            )
            .fetch_one(database.pool())
            .await?;
        assert_eq!(job_state, (0, "DISCOVERING".to_owned()));
        database.close().await;
        Ok(())
    }

    #[test]
    fn sidecar_target_query_uses_indexable_directory_ranges() {
        let query = sidecar_target_query("(?)");
        assert!(query.contains("fe.relative_path >= sd.directory || '/'"));
        assert!(query.contains("fe.relative_path < sd.directory || '0'"));
        assert!(!query.contains("substr("));
    }

    #[test]
    fn nested_sidecar_directories_are_covered_by_their_ancestor() {
        let directories = prune_sidecar_directories(vec![
            "Show/Season 01".to_owned(),
            "Show".to_owned(),
            "Show2".to_owned(),
            "Show/Extras".to_owned(),
            "Show".to_owned(),
        ]);

        assert_eq!(directories, vec!["Show".to_owned(), "Show2".to_owned()]);
        assert_eq!(
            prune_sidecar_directories(vec!["Show/Season 01".to_owned(), ".".to_owned()]),
            vec![".".to_owned()]
        );
    }
}
