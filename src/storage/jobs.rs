use super::*;

impl Database {
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
             SET status = 'COMPLETED', current_item = NULL, scan_phase = 'IDLE',
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

    pub(crate) async fn list_unseen_filesystem_entry_paths(
        &self,
        library_root_id: &str,
        generation: &str,
    ) -> Result<Vec<String>, StorageError> {
        self.query_scalar(
            "SELECT relative_path FROM filesystem_entries
             WHERE library_root_id = ? AND last_seen_generation != ? AND is_missing = 0
             ORDER BY relative_path",
        )
        .bind(library_root_id)
        .bind(generation)
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

    pub(crate) async fn create_reconciliation_scan_job(
        &self,
        id: &str,
        library_id: &str,
        generation: &str,
        library_root_ids: &[String],
        auto_metadata_match: bool,
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
            "INSERT INTO scan_jobs (
                id, library_id, job_type, status, generation, total_count,
                discovery_completed, auto_metadata_match
             ) VALUES (?, ?, 'RECONCILE_LIBRARY', 'PENDING', ?, 0, 0, ?)",
        )
        .bind(id)
        .bind(library_id)
        .bind(generation)
        .bind(database_flag(auto_metadata_match))
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        for root_id in library_root_ids {
            self.query(
                "INSERT INTO reconciliation_scan_entries (
                    job_id, library_root_id, relative_path, entry_type
                 ) VALUES (?, ?, '', 'DIRECTORY')",
            )
            .bind(id)
            .bind(root_id)
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

    pub(crate) async fn list_reconciliation_scan_entries(
        &self,
        job_id: &str,
        entry_type: &str,
        limit: i64,
    ) -> Result<Vec<StoredReconciliationScanEntry>, StorageError> {
        self.query(
            "SELECT library_root_id, relative_path
             FROM reconciliation_scan_entries
             WHERE job_id = ? AND entry_type = ?
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

    pub(crate) async fn complete_reconciliation_directory(
        &self,
        job_id: &str,
        library_root_id: &str,
        relative_path: &str,
        child_directories: &[String],
        media_files: &[String],
    ) -> Result<(), StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.insert_reconciliation_directory_entries(
            &mut transaction,
            job_id,
            library_root_id,
            child_directories,
            media_files,
        )
        .await?;
        self.query(
            "DELETE FROM reconciliation_scan_entries
             WHERE job_id = ? AND library_root_id = ?
               AND relative_path = ? AND entry_type = 'DIRECTORY'",
        )
        .bind(job_id)
        .bind(library_root_id)
        .bind(relative_path)
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

    pub(crate) async fn append_reconciliation_directory_entries(
        &self,
        job_id: &str,
        library_root_id: &str,
        child_directories: &[String],
        media_files: &[String],
    ) -> Result<(), StorageError> {
        if child_directories.is_empty() && media_files.is_empty() {
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
        self.insert_reconciliation_directory_entries(
            &mut transaction,
            job_id,
            library_root_id,
            child_directories,
            media_files,
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

    async fn insert_reconciliation_directory_entries(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        job_id: &str,
        library_root_id: &str,
        child_directories: &[String],
        media_files: &[String],
    ) -> Result<(), StorageError> {
        for (entry_type, paths) in [("DIRECTORY", child_directories), ("FILE", media_files)] {
            for chunk in paths.chunks(BATCH_INSERT_CHUNK_SIZE) {
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
                     ON CONFLICT(job_id, library_root_id, entry_type, relative_path) DO NOTHING"
                );
                let mut statement = self.query(sqlx::AssertSqlSafe(query));
                for path in chunk {
                    statement = statement
                        .bind(job_id)
                        .bind(library_root_id)
                        .bind(path)
                        .bind(entry_type);
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

    pub(crate) async fn finish_reconciliation_discovery(
        &self,
        job_id: &str,
    ) -> Result<i64, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let total_count: i64 = self
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

    pub(crate) async fn update_scan_job_discovery_progress(
        &self,
        job_id: &str,
        discovered_count: i64,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE scan_jobs
             SET total_count = ?, updated_at = unixepoch()
             WHERE id = ? AND status = 'RUNNING' AND discovery_completed = 0",
        )
        .bind(discovered_count)
        .bind(job_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn complete_reconciliation_files(
        &self,
        job_id: &str,
        entries: &[StoredReconciliationScanEntry],
        processed_count: i64,
    ) -> Result<(), StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let mut entries_by_root = HashMap::<&str, Vec<&str>>::new();
        for entry in entries {
            entries_by_root
                .entry(entry.library_root_id.as_str())
                .or_default()
                .push(entry.relative_path.as_str());
        }
        for (library_root_id, paths) in entries_by_root {
            for chunk in paths.chunks(BATCH_INSERT_CHUNK_SIZE) {
                let placeholders = std::iter::repeat_n("?", chunk.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                let query = format!(
                    "DELETE FROM reconciliation_scan_entries
                     WHERE job_id = ? AND library_root_id = ?
                       AND entry_type = 'FILE'
                       AND relative_path IN ({placeholders})"
                );
                let mut statement = self
                    .query(sqlx::AssertSqlSafe(query))
                    .bind(job_id)
                    .bind(library_root_id);
                for path in chunk {
                    statement = statement.bind(path);
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
        self.query(
            "UPDATE scan_jobs
             SET cursor = ?, processed_count = ?, updated_at = unixepoch()
             WHERE id = ? AND status = 'RUNNING'",
        )
        .bind(entries.last().map(|entry| entry.relative_path.as_str()))
        .bind(processed_count)
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
             WHERE job_id = ? AND library_root_id = ? AND entry_type = 'FILE'",
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
    ) -> Result<(), StorageError> {
        if relative_paths.is_empty() {
            return Ok(());
        }
        for paths in relative_paths.chunks(BATCH_INSERT_CHUNK_SIZE) {
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
            source_statement
                .execute(&self.pool)
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
            item_statement
                .execute(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }
        Ok(())
    }

    pub(crate) async fn record_scan_job_sidecar_targets(
        &self,
        job_id: &str,
        library_root_id: &str,
        sidecar_paths: &[String],
    ) -> Result<(), StorageError> {
        let mut directories = sidecar_paths
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
        directories.sort();
        directories.dedup();
        for chunk in directories.chunks(BATCH_INSERT_CHUNK_SIZE) {
            if chunk.is_empty() {
                continue;
            }
            let predicates = chunk
                .iter()
                .map(|_| {
                    "(? = '.' OR fe.relative_path = ? OR substr(fe.relative_path, 1, length(?) + 1) = ? || '/')"
                })
                .collect::<Vec<_>>()
                .join(" OR ");
            let query = format!(
                "INSERT INTO scan_job_targets (
                     job_id, target_type, target_id, item_id, change_kind,
                     probe_state, metadata_state, thumbnail_state
                 )
                 SELECT ?, 'ITEM', ms.item_id, ms.item_id, 'SIDECAR',
                        'SKIPPED', 'PENDING', 'PENDING'
                 FROM media_sources ms
                 JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                 WHERE fe.library_root_id = ? AND fe.is_missing = 0
                   AND ({predicates})
                 GROUP BY ms.item_id
                 ON CONFLICT(job_id, target_type, target_id) DO NOTHING"
            );
            let mut statement = self
                .query(sqlx::AssertSqlSafe(query))
                .bind(job_id)
                .bind(library_root_id);
            for directory in chunk {
                statement = statement
                    .bind(directory)
                    .bind(directory)
                    .bind(directory)
                    .bind(directory);
            }
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

    pub(crate) async fn record_scan_job_removed_targets(
        &self,
        job_id: &str,
        library_root_id: &str,
        relative_paths: &[String],
    ) -> Result<(), StorageError> {
        if relative_paths.is_empty() {
            return Ok(());
        }
        for paths in relative_paths.chunks(BATCH_INSERT_CHUNK_SIZE) {
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
                .execute(&self.pool)
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
                .execute(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }
        Ok(())
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
        for chunk in target_ids.chunks(BATCH_INSERT_CHUNK_SIZE) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "UPDATE scan_job_targets
                 SET {column} = ?, updated_at = unixepoch()
                 WHERE job_id = ? AND target_type = ? AND target_id IN ({placeholders})"
            );
            let mut statement = self
                .query(sqlx::AssertSqlSafe(query))
                .bind(state)
                .bind(job_id)
                .bind(target_type);
            for target_id in chunk {
                statement = statement.bind(target_id);
            }
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

    pub(crate) async fn clear_completed_scan_job_targets(
        &self,
        job_id: &str,
    ) -> Result<bool, StorageError> {
        let result = self
            .query(
                "DELETE FROM scan_job_targets
                 WHERE job_id = ?
                   AND NOT EXISTS (
                       SELECT 1 FROM scan_job_targets pending
                       WHERE pending.job_id = scan_job_targets.job_id
                         AND (
                             pending.probe_state IN ('PENDING', 'FAILED')
                             OR pending.metadata_state IN ('PENDING', 'FAILED')
                             OR pending.thumbnail_state IN ('PENDING', 'FAILED')
                         )
                   )",
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
             WHERE job_id = ?",
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
                 SELECT 1 FROM reconciliation_scan_entries WHERE job_id = ?
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
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
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
             SET status = ?, error = ?, current_item = NULL, scan_phase = 'IDLE',
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
             SET status = 'COMPLETED', current_item = NULL, scan_phase = 'POSTPROCESSING',
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
        let result = self
            .query(
                "UPDATE scan_jobs
                 SET status = 'COMPLETED', error = NULL, current_item = NULL,
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

    pub(crate) async fn fail_scan_job_postprocessing(
        &self,
        id: &str,
    ) -> Result<bool, StorageError> {
        let result = self
            .query(
                "UPDATE scan_jobs
                 SET status = 'COMPLETED', error = NULL, current_item = NULL,
                     scan_phase = 'IDLE', finished_at = COALESCE(finished_at, unixepoch()),
                     updated_at = unixepoch()
                 WHERE id = ? AND status IN ('RUNNING', 'COMPLETED')
                   AND scan_phase = 'POSTPROCESSING'
                   AND EXISTS (
                       SELECT 1 FROM scan_job_targets
                       WHERE job_id = ?
                         AND (
                             probe_state IN ('PENDING', 'FAILED')
                             OR metadata_state IN ('PENDING', 'FAILED')
                             OR thumbnail_state IN ('PENDING', 'FAILED')
                         )
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
                   )
                   AND EXISTS (
                       SELECT 1 FROM scan_job_targets WHERE job_id = ?
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
            "SELECT fe.id, fe.relative_path, fe.fingerprint, ms.item_id,
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
                "SELECT fe.id, fe.relative_path, fe.fingerprint, ms.item_id,
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
                "SELECT fe.id, fe.relative_path, fe.fingerprint, ms.item_id,
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
        .execute(&self.pool)
        .await
        .map(|_| ())
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
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
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

    pub(crate) async fn update_filesystem_entry(
        &self,
        id: &str,
        size: i64,
        modified_at: i64,
        fingerprint: &[u8],
        last_seen_generation: &str,
    ) -> Result<(), StorageError> {
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
        .execute(&self.pool)
        .await
        .map(|_| ())
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
        self.query(
            "UPDATE filesystem_entries
             SET last_seen_generation = ?, is_missing = 0, updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(last_seen_generation)
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn mark_missing_filesystem_entries(
        &self,
        library_root_id: &str,
        generation: &str,
    ) -> Result<u64, StorageError> {
        self.query(
            "UPDATE filesystem_entries
             SET is_missing = 1, updated_at = unixepoch()
             WHERE library_root_id = ? AND last_seen_generation != ? AND is_missing = 0",
        )
        .bind(library_root_id)
        .bind(generation)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected())
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

        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
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
                 SET removed_at = unixepoch()
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
                 SET removed_at = unixepoch()
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
