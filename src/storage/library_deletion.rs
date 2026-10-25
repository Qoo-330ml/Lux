use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LibraryDeletionBatch {
    MediaItems,
    FilesystemEntries,
    StrmProbeJobs,
    ScanManifestDeltas,
    ScanManifestEntries,
    ScanManifestDirectories,
    ScanManifestSeenPaths,
    ScanManifestRoots,
    ScanManifests,
    ScanJobTargets,
    ScanJobPaths,
    ReconciliationEntries,
    ScanJobFileChanges,
    ScanJobProcessedMediaItems,
    ScanJobEvents,
    ScanLocalMetadataBatches,
    ScanJobs,
}

/// Which parent table a set-based child delete is scoped through.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DeletionParent {
    MediaItems,
    MediaSources,
}

/// Deepest-first child deletes for one media-item batch.
///
/// Deleting a library item by item makes the foreign keys run one cascade lookup per
/// parent row. Walking the same tables set-based keeps the work identical but turns it
/// into a fixed number of statements per batch.
///
/// SQLite only. Measured on the 502-rows-per-series fixture: 4.02M rows went from
/// 38.0 s to 25.2 s. PostgreSQL moves the other way (5.69 s to 8.29 s) because its
/// cascade triggers are already a single index probe per row and the extra statements
/// re-scan the batch selection 37 times, so PostgreSQL keeps the cascade path.
///
/// Foreign keys stay enabled either way: a table missing from this list is still
/// cleaned up by the cascade, only more slowly, so the list is an optimization rather
/// than a correctness requirement.
pub(crate) const MEDIA_ITEM_CHILD_DELETES: &[(&str, &str, DeletionParent)] = &[
    // Children of media_sources first: they hold the bulk of the rows (streams and
    // chapter markers per episode) and must go before their parent.
    (
        "chapter_detection_job_items",
        "source_id",
        DeletionParent::MediaSources,
    ),
    (
        "chapter_detection_source_states",
        "source_id",
        DeletionParent::MediaSources,
    ),
    (
        "danmaku_match_job_items",
        "media_source_id",
        DeletionParent::MediaSources,
    ),
    (
        "danmaku_tracks",
        "media_source_id",
        DeletionParent::MediaSources,
    ),
    (
        "media_chapters",
        "media_source_id",
        DeletionParent::MediaSources,
    ),
    (
        "media_info_chapters",
        "media_source_id",
        DeletionParent::MediaSources,
    ),
    (
        "media_streams",
        "media_source_id",
        DeletionParent::MediaSources,
    ),
    // Then media_sources and the remaining children of media_items.
    ("media_sources", "item_id", DeletionParent::MediaItems),
    (
        "chapter_detection_job_items",
        "item_id",
        DeletionParent::MediaItems,
    ),
    (
        "chapter_detection_job_items",
        "season_id",
        DeletionParent::MediaItems,
    ),
    (
        "collection_item_references",
        "item_id",
        DeletionParent::MediaItems,
    ),
    ("collection_items", "item_id", DeletionParent::MediaItems),
    (
        "collection_work_items",
        "work_item_id",
        DeletionParent::MediaItems,
    ),
    (
        "collection_work_sources",
        "source_item_id",
        DeletionParent::MediaItems,
    ),
    ("collections", "item_id", DeletionParent::MediaItems),
    (
        "emby_migration_import_records",
        "lux_item_id",
        DeletionParent::MediaItems,
    ),
    ("item_aliases", "item_id", DeletionParent::MediaItems),
    ("item_images", "item_id", DeletionParent::MediaItems),
    (
        "item_metadata_completeness",
        "item_id",
        DeletionParent::MediaItems,
    ),
    (
        "media_content_restriction_index",
        "item_id",
        DeletionParent::MediaItems,
    ),
    (
        "media_content_restriction_index_queue",
        "item_id",
        DeletionParent::MediaItems,
    ),
    (
        "media_item_provider_ids",
        "media_item_id",
        DeletionParent::MediaItems,
    ),
    (
        "media_merge_records",
        "merged_item_id",
        DeletionParent::MediaItems,
    ),
    (
        "media_merge_records",
        "primary_item_id",
        DeletionParent::MediaItems,
    ),
    ("metadata_candidates", "item_id", DeletionParent::MediaItems),
    (
        "metadata_capability_attempts",
        "item_id",
        DeletionParent::MediaItems,
    ),
    (
        "metadata_image_attempts",
        "item_id",
        DeletionParent::MediaItems,
    ),
    (
        "metadata_reidentify_job_items",
        "item_id",
        DeletionParent::MediaItems,
    ),
    ("person_credits", "item_id", DeletionParent::MediaItems),
    (
        "person_index_item_state",
        "item_id",
        DeletionParent::MediaItems,
    ),
    (
        "person_match_candidates",
        "item_id",
        DeletionParent::MediaItems,
    ),
    (
        "playback_history_events",
        "item_id",
        DeletionParent::MediaItems,
    ),
    ("playback_sessions", "item_id", DeletionParent::MediaItems),
    (
        "recommendation_item_stats",
        "item_id",
        DeletionParent::MediaItems,
    ),
    (
        "thumbnail_scraper_retries",
        "item_id",
        DeletionParent::MediaItems,
    ),
    ("user_item_state", "item_id", DeletionParent::MediaItems),
    (
        "web_playback_sessions",
        "item_id",
        DeletionParent::MediaItems,
    ),
];

fn media_item_child_delete_sql(table: &str, column: &str, parent: DeletionParent) -> String {
    match parent {
        DeletionParent::MediaItems => format!(
            "DELETE FROM {table} WHERE {column} IN \
             (SELECT id FROM media_items WHERE library_id = ? LIMIT ?)"
        ),
        DeletionParent::MediaSources => format!(
            "DELETE FROM {table} WHERE {column} IN (SELECT id FROM media_sources \
             WHERE item_id IN (SELECT id FROM media_items WHERE library_id = ? LIMIT ?))"
        ),
    }
}

fn media_item_child_deletes() -> &'static [String] {
    static STATEMENTS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    STATEMENTS.get_or_init(|| {
        MEDIA_ITEM_CHILD_DELETES
            .iter()
            .map(|(table, column, parent)| media_item_child_delete_sql(table, column, *parent))
            .collect()
    })
}

const LIBRARY_DELETION_JOB_COLUMNS: &str =
    "id, library_id, library_name, status, stage, processed_count, error_code,
     created_at, updated_at, started_at, finished_at";

/// SQLite keeps the original batch size: with the deletion connection at
/// `synchronous = NORMAL` a commit no longer fsyncs, and a longer batch only holds
/// the database write lock for longer. PostgreSQL still pays real per-commit cost,
/// so it batches more rows per transaction.
const SQLITE_MEDIA_ITEM_DELETION_BATCH: i64 = 500;
const POSTGRES_MEDIA_ITEM_DELETION_BATCH: i64 = 5_000;
const SQLITE_FILESYSTEM_ENTRY_DELETION_BATCH: i64 = 500;
const POSTGRES_FILESYSTEM_ENTRY_DELETION_BATCH: i64 = 5_000;
const SCAN_ROW_DELETION_BATCH: i64 = 500;
const JOB_ROW_DELETION_BATCH: i64 = 50;

/// SQLite defaults to `synchronous = FULL`, which fsyncs once per batch. The
/// deletion job is restartable and every batch is idempotent, so the worker may
/// run at `NORMAL`: WAL stays crash-safe and only the tail of an interrupted run
/// can be lost. The pragmas are applied to a checked-out connection and restored
/// before it returns to the pool.
const SQLITE_DELETION_DURABILITY_PRAGMAS: &[&str] = &[
    "PRAGMA synchronous = NORMAL",
    "PRAGMA wal_autocheckpoint = 20000",
];
const SQLITE_DELETION_RESTORE_PRAGMAS: &[&str] = &[
    "PRAGMA synchronous = FULL",
    "PRAGMA wal_autocheckpoint = 1000",
];

/// Batch size for one library-deletion statement. Every statement binds only the
/// library identifier and the limit, so these sizes are bounded by lock hold time
/// rather than by a bind-parameter budget.
pub(crate) fn resolve_library_deletion_batch_size(
    backend: DatabaseBackend,
    batch: LibraryDeletionBatch,
) -> i64 {
    match (batch, backend) {
        (LibraryDeletionBatch::MediaItems, DatabaseBackend::Sqlite) => {
            SQLITE_MEDIA_ITEM_DELETION_BATCH
        }
        (LibraryDeletionBatch::MediaItems, DatabaseBackend::Postgres) => {
            POSTGRES_MEDIA_ITEM_DELETION_BATCH
        }
        (LibraryDeletionBatch::FilesystemEntries, DatabaseBackend::Sqlite) => {
            SQLITE_FILESYSTEM_ENTRY_DELETION_BATCH
        }
        (LibraryDeletionBatch::FilesystemEntries, DatabaseBackend::Postgres) => {
            POSTGRES_FILESYSTEM_ENTRY_DELETION_BATCH
        }
        (LibraryDeletionBatch::ScanJobs, _) => JOB_ROW_DELETION_BATCH,
        (LibraryDeletionBatch::ScanManifests, _) => JOB_ROW_DELETION_BATCH,
        (LibraryDeletionBatch::StrmProbeJobs, _) => JOB_ROW_DELETION_BATCH,
        _ => SCAN_ROW_DELETION_BATCH,
    }
}

fn deletion_durability_pragmas(backend: DatabaseBackend) -> &'static [&'static str] {
    match backend {
        DatabaseBackend::Sqlite => SQLITE_DELETION_DURABILITY_PRAGMAS,
        DatabaseBackend::Postgres => &[],
    }
}

fn deletion_restore_pragmas(backend: DatabaseBackend) -> &'static [&'static str] {
    match backend {
        DatabaseBackend::Sqlite => SQLITE_DELETION_RESTORE_PRAGMAS,
        DatabaseBackend::Postgres => &[],
    }
}

fn stored_library_deletion_job(row: sqlx::any::AnyRow) -> StoredLibraryDeletionJob {
    StoredLibraryDeletionJob {
        id: row.get("id"),
        library_id: row.get("library_id"),
        library_name: row.get("library_name"),
        status: row.get("status"),
        stage: row.get("stage"),
        processed_count: row.get("processed_count"),
        error_code: row.get("error_code"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        started_at: row.get("started_at"),
        finished_at: row.get("finished_at"),
    }
}

impl Database {
    /// Batch size for one library-deletion statement on this database.
    pub(crate) fn library_deletion_batch_size(&self, batch: LibraryDeletionBatch) -> i64 {
        resolve_library_deletion_batch_size(self.backend(), batch)
    }

    pub(crate) async fn request_library_deletion(
        &self,
        library_id: &str,
        job_id: &str,
    ) -> Result<Option<StoredLibraryDeletionJob>, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if self.backend() == DatabaseBackend::Postgres {
            self.query("SET LOCAL lock_timeout = '2s'")
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }

        let active_job = self
            .query(sqlx::AssertSqlSafe(format!(
                "SELECT {LIBRARY_DELETION_JOB_COLUMNS}
                 FROM library_deletion_jobs
                 WHERE library_id = ? AND status IN ('PENDING', 'RUNNING')
                 ORDER BY created_at DESC, id DESC LIMIT 1"
            )))
            .bind(library_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if let Some(row) = active_job {
            transaction
                .commit()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(Some(stored_library_deletion_job(row)));
        }

        let library_name = self
            .query(
                "UPDATE libraries
                 SET deletion_state = 'DELETING', is_enabled = 0,
                     realtime_watch_enabled = 0, updated_at = unixepoch()
                 WHERE id = ? AND system_kind IS NULL AND deletion_state = 'ACTIVE'
                 RETURNING name",
            )
            .bind(library_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .map(|row| row.get::<String, _>("name"));

        let Some(library_name) = library_name else {
            let existing = self
                .query(sqlx::AssertSqlSafe(format!(
                    "SELECT {LIBRARY_DELETION_JOB_COLUMNS}
                     FROM library_deletion_jobs WHERE library_id = ?
                     ORDER BY created_at DESC, id DESC LIMIT 1"
                )))
                .bind(library_id)
                .fetch_optional(&mut *transaction)
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
            return Ok(existing.map(stored_library_deletion_job));
        };

        self.query(
            "INSERT INTO library_deletion_jobs (
                id, library_id, library_name, status, stage
             ) VALUES (?, ?, ?, 'PENDING', 'CANCELLING_SCANS')",
        )
        .bind(job_id)
        .bind(library_id)
        .bind(library_name)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        let row = self
            .query(sqlx::AssertSqlSafe(format!(
                "SELECT {LIBRARY_DELETION_JOB_COLUMNS}
                 FROM library_deletion_jobs WHERE id = ?"
            )))
            .bind(job_id)
            .fetch_one(&mut *transaction)
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
        Ok(Some(stored_library_deletion_job(row)))
    }

    pub(crate) async fn get_library_deletion_job(
        &self,
        job_id: &str,
    ) -> Result<Option<StoredLibraryDeletionJob>, StorageError> {
        self.query(sqlx::AssertSqlSafe(format!(
            "SELECT {LIBRARY_DELETION_JOB_COLUMNS}
             FROM library_deletion_jobs WHERE id = ?"
        )))
        .bind(job_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_library_deletion_job))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_library_deletion_jobs(
        &self,
        offset: i64,
        limit: i64,
        status: Option<&str>,
    ) -> Result<Vec<StoredLibraryDeletionJob>, StorageError> {
        let rows = if let Some(status) = status {
            self.query(sqlx::AssertSqlSafe(format!(
                "SELECT {LIBRARY_DELETION_JOB_COLUMNS}
                 FROM library_deletion_jobs WHERE status = ?
                 ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?"
            )))
            .bind(status)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        } else {
            self.query(sqlx::AssertSqlSafe(format!(
                "SELECT {LIBRARY_DELETION_JOB_COLUMNS}
                 FROM library_deletion_jobs
                 ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?"
            )))
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        }
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        Ok(rows.into_iter().map(stored_library_deletion_job).collect())
    }

    pub(crate) async fn count_library_deletion_jobs(
        &self,
        status: Option<&str>,
    ) -> Result<i64, StorageError> {
        let count = if let Some(status) = status {
            self.query_scalar("SELECT COUNT(*) FROM library_deletion_jobs WHERE status = ?")
                .bind(status)
                .fetch_one(&self.pool)
                .await
        } else {
            self.query_scalar("SELECT COUNT(*) FROM library_deletion_jobs")
                .fetch_one(&self.pool)
                .await
        }
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        Ok(count)
    }

    pub(crate) async fn retry_library_deletion_job(
        &self,
        job_id: &str,
    ) -> Result<Option<StoredLibraryDeletionJob>, StorageError> {
        let result = self
            .query(
                "UPDATE library_deletion_jobs
                 SET status = 'PENDING', error_code = NULL, finished_at = NULL,
                     updated_at = unixepoch()
                 WHERE id = ? AND status IN ('FAILED', 'CANCELLED')
                   AND EXISTS (
                       SELECT 1 FROM libraries
                       WHERE libraries.id = library_deletion_jobs.library_id
                         AND libraries.deletion_state = 'DELETING'
                   )",
            )
            .bind(job_id)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if result.rows_affected() == 0 {
            return self.get_library_deletion_job(job_id).await;
        }
        self.get_library_deletion_job(job_id).await
    }

    pub(crate) async fn claim_library_deletion_job(
        &self,
        job_id: &str,
    ) -> Result<bool, StorageError> {
        let result = self
            .query(
                "UPDATE library_deletion_jobs
                 SET status = 'RUNNING', started_at = COALESCE(started_at, unixepoch()),
                     error_code = NULL, updated_at = unixepoch()
                 WHERE id = ? AND status = 'PENDING'",
            )
            .bind(job_id)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn set_library_deletion_stage(
        &self,
        job_id: &str,
        stage: &str,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE library_deletion_jobs
             SET stage = ?, updated_at = unixepoch()
             WHERE id = ? AND status = 'RUNNING'",
        )
        .bind(stage)
        .bind(job_id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    /// Best-effort WAL trimming between stages. `PASSIVE` never blocks readers, so
    /// it is safe while the server keeps serving requests.
    pub(crate) async fn checkpoint_library_deletion_wal(&self) {
        if self.backend() != DatabaseBackend::Sqlite {
            return;
        }
        let _ = self
            .query("PRAGMA wal_checkpoint(PASSIVE)")
            .fetch_optional(&self.pool)
            .await;
    }

    pub(crate) async fn fail_library_deletion_job(
        &self,
        job_id: &str,
        error_code: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE library_deletion_jobs
             SET status = 'FAILED', error_code = ?, finished_at = unixepoch(),
                 updated_at = unixepoch()
             WHERE id = ? AND status = 'RUNNING'",
        )
        .bind(error_code)
        .bind(job_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn delete_library_deletion_batch(
        &self,
        job_id: &str,
        library_id: &str,
        batch: LibraryDeletionBatch,
        limit: i64,
    ) -> Result<Option<u64>, StorageError> {
        // One checked-out connection per batch: the reduced-durability pragmas must
        // never be inherited by a pooled connection that a later request reuses.
        let mut connection = self
            .pool
            .acquire()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        for &pragma in deletion_durability_pragmas(self.backend()) {
            self.query(pragma)
                .execute(&mut *connection)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }
        let result = self
            .delete_library_deletion_batch_on(&mut connection, job_id, library_id, batch, limit)
            .await;
        for &pragma in deletion_restore_pragmas(self.backend()) {
            // A failed restore must not hide the batch outcome, but it must also not
            // leave reduced durability behind for the next borrower.
            let _ = self.query(pragma).execute(&mut *connection).await;
        }
        result
    }

    async fn delete_library_deletion_batch_on(
        &self,
        connection: &mut sqlx::AnyConnection,
        job_id: &str,
        library_id: &str,
        batch: LibraryDeletionBatch,
        limit: i64,
    ) -> Result<Option<u64>, StorageError> {
        let mut transaction = connection
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if self.backend() == DatabaseBackend::Postgres {
            // Every batch is idempotent and the job restarts from committed data,
            // so the commit fsync is not worth paying for on a multi-hour deletion.
            self.query("SET LOCAL synchronous_commit = off")
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            self.query("SET LOCAL lock_timeout = '5s'")
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }
        let running_job = self
            .query(
                "UPDATE library_deletion_jobs SET updated_at = updated_at
                 WHERE id = ? AND status = 'RUNNING'",
            )
            .bind(job_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if running_job.rows_affected() == 0 {
            transaction
                .rollback()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(None);
        }
        if batch == LibraryDeletionBatch::MediaItems {
            // Watch state and playback history have to be copied out before anything below removes
            // them: the set-based child deletes on SQLite and the foreign-key cascade on PostgreSQL
            // both take both tables with the item, and the user expects the history back when the
            // same title is added to another library.
            self.archive_watch_state_for_library(&mut transaction, library_id)
                .await?;
            self.archive_playback_history_for_library(&mut transaction, library_id)
                .await?;
        }
        if batch == LibraryDeletionBatch::MediaItems && self.backend() == DatabaseBackend::Sqlite {
            // Set-based child deletes: same rows, a fixed number of statements instead
            // of one foreign-key cascade lookup per parent row.
            for statement in media_item_child_deletes() {
                self.query(sqlx::AssertSqlSafe(statement.clone()))
                    .bind(library_id)
                    .bind(limit)
                    .execute(&mut *transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
            }
        }
        let result = match batch {
            LibraryDeletionBatch::MediaItems => {
                self.query(
                    "DELETE FROM media_items
                     WHERE id IN (
                         SELECT id FROM media_items WHERE library_id = ? LIMIT ?
                     )",
                )
                .bind(library_id)
                .bind(limit)
                .execute(&mut *transaction)
                .await
            }
            LibraryDeletionBatch::FilesystemEntries => {
                self.query(
                    "DELETE FROM filesystem_entries
                     WHERE id IN (
                         SELECT entry.id
                         FROM filesystem_entries entry
                         JOIN library_roots root ON root.id = entry.library_root_id
                         WHERE root.library_id = ? LIMIT ?
                     )",
                )
                .bind(library_id)
                .bind(limit)
                .execute(&mut *transaction)
                .await
            }
            LibraryDeletionBatch::StrmProbeJobs => {
                self.query(
                    "DELETE FROM strm_probe_jobs
                     WHERE id IN (
                         SELECT probe.id
                         FROM strm_probe_jobs probe
                         WHERE probe.library_id = ?
                            OR probe.target_scan_job_id IN (
                                SELECT id FROM scan_jobs WHERE library_id = ?
                            )
                         LIMIT ?
                     )",
                )
                .bind(library_id)
                .bind(library_id)
                .bind(limit)
                .execute(&mut *transaction)
                .await
            }
            LibraryDeletionBatch::ScanManifestDeltas => {
                self.query(
                    "DELETE FROM scan_manifest_deltas
                     WHERE id IN (
                         SELECT delta.id
                         FROM scan_manifest_deltas delta
                         JOIN scan_manifests manifest ON manifest.id = delta.manifest_id
                         WHERE manifest.library_id = ? LIMIT ?
                     )",
                )
                .bind(library_id)
                .bind(limit)
                .execute(&mut *transaction)
                .await
            }
            LibraryDeletionBatch::ScanManifestEntries => {
                self.query(
                    "DELETE FROM scan_manifest_entries
                     WHERE (manifest_id, library_root_id, relative_path, observation_sequence) IN (
                         SELECT entry.manifest_id, entry.library_root_id,
                                entry.relative_path, entry.observation_sequence
                         FROM scan_manifest_entries entry
                         JOIN scan_manifests manifest ON manifest.id = entry.manifest_id
                         WHERE manifest.library_id = ? LIMIT ?
                     )",
                )
                .bind(library_id)
                .bind(limit)
                .execute(&mut *transaction)
                .await
            }
            LibraryDeletionBatch::ScanManifestDirectories => {
                self.query(
                    "DELETE FROM scan_manifest_directories
                     WHERE (manifest_id, library_root_id, relative_path) IN (
                         SELECT directory.manifest_id, directory.library_root_id,
                                directory.relative_path
                         FROM scan_manifest_directories directory
                         JOIN scan_manifests manifest ON manifest.id = directory.manifest_id
                         WHERE manifest.library_id = ? LIMIT ?
                     )",
                )
                .bind(library_id)
                .bind(limit)
                .execute(&mut *transaction)
                .await
            }
            LibraryDeletionBatch::ScanManifestSeenPaths => {
                self.query(
                    "DELETE FROM scan_manifest_seen_paths
                     WHERE (manifest_id, library_root_id, relative_path) IN (
                         SELECT seen.manifest_id, seen.library_root_id, seen.relative_path
                         FROM scan_manifest_seen_paths seen
                         JOIN scan_manifests manifest ON manifest.id = seen.manifest_id
                         WHERE manifest.library_id = ? LIMIT ?
                     )",
                )
                .bind(library_id)
                .bind(limit)
                .execute(&mut *transaction)
                .await
            }
            LibraryDeletionBatch::ScanManifestRoots => {
                self.query(
                    "DELETE FROM scan_manifest_roots
                     WHERE (manifest_id, library_root_id) IN (
                         SELECT root.manifest_id, root.library_root_id
                         FROM scan_manifest_roots root
                         JOIN scan_manifests manifest ON manifest.id = root.manifest_id
                         WHERE manifest.library_id = ? LIMIT ?
                     )",
                )
                .bind(library_id)
                .bind(limit)
                .execute(&mut *transaction)
                .await
            }
            LibraryDeletionBatch::ScanManifests => {
                self.query(
                    "DELETE FROM scan_manifests
                     WHERE id IN (
                         SELECT id FROM scan_manifests WHERE library_id = ? LIMIT ?
                     )",
                )
                .bind(library_id)
                .bind(limit)
                .execute(&mut *transaction)
                .await
            }
            LibraryDeletionBatch::ScanJobTargets => {
                self.query(
                    "DELETE FROM scan_job_targets
                     WHERE (job_id, target_type, target_id) IN (
                         SELECT target.job_id, target.target_type, target.target_id
                         FROM scan_job_targets target
                         JOIN scan_jobs job ON job.id = target.job_id
                         WHERE job.library_id = ? LIMIT ?
                     )",
                )
                .bind(library_id)
                .bind(limit)
                .execute(&mut *transaction)
                .await
            }
            LibraryDeletionBatch::ScanJobPaths => {
                self.query(
                    "DELETE FROM scan_job_paths
                     WHERE (job_id, library_root_id, relative_path) IN (
                         SELECT path.job_id, path.library_root_id, path.relative_path
                         FROM scan_job_paths path
                         JOIN scan_jobs job ON job.id = path.job_id
                         WHERE job.library_id = ? LIMIT ?
                     )",
                )
                .bind(library_id)
                .bind(limit)
                .execute(&mut *transaction)
                .await
            }
            LibraryDeletionBatch::ReconciliationEntries => {
                self.query(
                    "DELETE FROM reconciliation_scan_entries
                     WHERE (job_id, entry_type, library_root_id, relative_path) IN (
                         SELECT entry.job_id, entry.entry_type,
                                entry.library_root_id, entry.relative_path
                         FROM reconciliation_scan_entries entry
                         JOIN scan_jobs job ON job.id = entry.job_id
                         WHERE job.library_id = ? LIMIT ?
                     )",
                )
                .bind(library_id)
                .bind(limit)
                .execute(&mut *transaction)
                .await
            }
            LibraryDeletionBatch::ScanJobFileChanges => {
                self.query(
                    "DELETE FROM scan_job_file_changes
                     WHERE (job_id, library_root_id, relative_path) IN (
                         SELECT change.job_id, change.library_root_id, change.relative_path
                         FROM scan_job_file_changes change
                         JOIN scan_jobs job ON job.id = change.job_id
                         WHERE job.library_id = ? LIMIT ?
                     )",
                )
                .bind(library_id)
                .bind(limit)
                .execute(&mut *transaction)
                .await
            }
            LibraryDeletionBatch::ScanJobProcessedMediaItems => {
                self.query(
                    "DELETE FROM scan_job_processed_media_items
                     WHERE (job_id, media_item_id) IN (
                         SELECT processed.job_id, processed.media_item_id
                         FROM scan_job_processed_media_items processed
                         JOIN scan_jobs job ON job.id = processed.job_id
                         WHERE job.library_id = ? LIMIT ?
                     )",
                )
                .bind(library_id)
                .bind(limit)
                .execute(&mut *transaction)
                .await
            }
            LibraryDeletionBatch::ScanJobEvents => {
                self.query(
                    "DELETE FROM scan_job_events
                     WHERE id IN (
                         SELECT event.id
                         FROM scan_job_events event
                         JOIN scan_jobs job ON job.id = event.job_id
                         WHERE job.library_id = ? LIMIT ?
                     )",
                )
                .bind(library_id)
                .bind(limit)
                .execute(&mut *transaction)
                .await
            }
            LibraryDeletionBatch::ScanLocalMetadataBatches => {
                self.query(
                    "DELETE FROM scan_local_metadata_batches
                     WHERE id IN (
                         SELECT batch.id
                         FROM scan_local_metadata_batches batch
                         JOIN library_roots root ON root.id = batch.library_root_id
                         WHERE root.library_id = ? LIMIT ?
                     )",
                )
                .bind(library_id)
                .bind(limit)
                .execute(&mut *transaction)
                .await
            }
            LibraryDeletionBatch::ScanJobs => {
                self.query(
                    "DELETE FROM scan_jobs
                     WHERE id IN (
                         SELECT id FROM scan_jobs WHERE library_id = ? LIMIT ?
                     )",
                )
                .bind(library_id)
                .bind(limit)
                .execute(&mut *transaction)
                .await
            }
        }
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        let deleted = result.rows_affected();
        if deleted > 0 {
            self.query(
                "UPDATE library_deletion_jobs
                 SET processed_count = processed_count + ?, updated_at = unixepoch()
                 WHERE id = ? AND status = 'RUNNING'",
            )
            .bind(i64::try_from(deleted).unwrap_or(i64::MAX))
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
        Ok(Some(deleted))
    }

    pub(crate) async fn complete_library_deletion(
        &self,
        job_id: &str,
        library_id: &str,
    ) -> Result<Option<bool>, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if self.backend() == DatabaseBackend::Postgres {
            self.query("SET LOCAL lock_timeout = '5s'")
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }
        let running_job = self
            .query(
                "UPDATE library_deletion_jobs SET updated_at = updated_at
                 WHERE id = ? AND library_id = ? AND status = 'RUNNING'",
            )
            .bind(job_id)
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if running_job.rows_affected() == 0 {
            transaction
                .rollback()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(None);
        }
        self.query(
            "DELETE FROM scheduled_task_configs
             WHERE owner_type = 'LIBRARY' AND owner_id = ?",
        )
        .bind(library_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        let deleted = self
            .query("DELETE FROM libraries WHERE id = ? AND deletion_state = 'DELETING'")
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .rows_affected()
            == 1;
        if deleted {
            self.query(
                "UPDATE library_deletion_jobs
                 SET status = 'COMPLETED', stage = 'COMPLETED', error_code = NULL,
                     finished_at = unixepoch(), updated_at = unixepoch()
                 WHERE id = ? AND library_id = ? AND status = 'RUNNING'",
            )
            .bind(job_id)
            .bind(library_id)
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
        Ok(Some(deleted))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deletion_batch_sizes_are_backend_specific_and_bounded() {
        let sqlite_media = resolve_library_deletion_batch_size(
            DatabaseBackend::Sqlite,
            LibraryDeletionBatch::MediaItems,
        );
        let postgres_media = resolve_library_deletion_batch_size(
            DatabaseBackend::Postgres,
            LibraryDeletionBatch::MediaItems,
        );
        assert_eq!(sqlite_media, SQLITE_MEDIA_ITEM_DELETION_BATCH);
        assert_eq!(postgres_media, POSTGRES_MEDIA_ITEM_DELETION_BATCH);
        assert!(postgres_media > sqlite_media);
        assert!(
            sqlite_media >= 500,
            "SQLite must keep its bounded write lock"
        );
        assert_eq!(
            resolve_library_deletion_batch_size(
                DatabaseBackend::Sqlite,
                LibraryDeletionBatch::FilesystemEntries
            ),
            SQLITE_FILESYSTEM_ENTRY_DELETION_BATCH
        );
        assert_eq!(
            resolve_library_deletion_batch_size(
                DatabaseBackend::Postgres,
                LibraryDeletionBatch::ScanManifestEntries
            ),
            SCAN_ROW_DELETION_BATCH
        );
        assert_eq!(
            resolve_library_deletion_batch_size(
                DatabaseBackend::Postgres,
                LibraryDeletionBatch::ScanJobs
            ),
            JOB_ROW_DELETION_BATCH
        );
        for batch in [
            LibraryDeletionBatch::MediaItems,
            LibraryDeletionBatch::FilesystemEntries,
            LibraryDeletionBatch::StrmProbeJobs,
            LibraryDeletionBatch::ScanManifestDeltas,
            LibraryDeletionBatch::ScanManifestEntries,
            LibraryDeletionBatch::ScanManifestDirectories,
            LibraryDeletionBatch::ScanManifestSeenPaths,
            LibraryDeletionBatch::ScanManifestRoots,
            LibraryDeletionBatch::ScanManifests,
            LibraryDeletionBatch::ScanJobTargets,
            LibraryDeletionBatch::ScanJobPaths,
            LibraryDeletionBatch::ReconciliationEntries,
            LibraryDeletionBatch::ScanJobFileChanges,
            LibraryDeletionBatch::ScanJobProcessedMediaItems,
            LibraryDeletionBatch::ScanJobEvents,
            LibraryDeletionBatch::ScanLocalMetadataBatches,
            LibraryDeletionBatch::ScanJobs,
        ] {
            for backend in [DatabaseBackend::Sqlite, DatabaseBackend::Postgres] {
                let size = resolve_library_deletion_batch_size(backend, batch);
                assert!(size > 0, "{batch:?} must delete at least one row per batch");
                assert!(
                    size <= POSTGRES_MEDIA_ITEM_DELETION_BATCH,
                    "{batch:?} holds the write lock for too long"
                );
            }
        }
    }

    #[test]
    fn deletion_durability_pragmas_are_sqlite_only_and_restored() {
        assert!(deletion_durability_pragmas(DatabaseBackend::Postgres).is_empty());
        assert!(deletion_restore_pragmas(DatabaseBackend::Postgres).is_empty());
        assert_eq!(
            deletion_durability_pragmas(DatabaseBackend::Sqlite),
            [
                "PRAGMA synchronous = NORMAL",
                "PRAGMA wal_autocheckpoint = 20000"
            ]
        );
        assert_eq!(
            deletion_restore_pragmas(DatabaseBackend::Sqlite),
            [
                "PRAGMA synchronous = FULL",
                "PRAGMA wal_autocheckpoint = 1000"
            ]
        );
    }
}
