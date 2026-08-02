use std::{path::PathBuf, time::Duration};

use sqlx::{
    Row,
    migrate::{MigrateError, Migrator},
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions},
};
use tokio::fs;
use uuid::Uuid;

use crate::config::Config;

static MIGRATOR: Migrator = sqlx::migrate!();

#[derive(Clone)]
pub struct Database {
    pool: SqlitePool,
    path: PathBuf,
    server_id: String,
}

impl Database {
    pub async fn connect(config: &Config) -> Result<Self, StorageError> {
        fs::create_dir_all(&config.config_dir)
            .await
            .map_err(|source| StorageError::Io {
                path: config.config_dir.clone(),
                source,
            })?;

        let path = config.config_dir.join("lux.db");
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: path.clone(),
                source,
            })?;

        if let Err(source) = MIGRATOR.run(&pool).await {
            pool.close().await;
            return Err(StorageError::Migration { path, source });
        }
        let server_id = ensure_server_id(&pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: path.clone(),
                source,
            })?;

        Ok(Self {
            pool,
            path,
            server_id,
        })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub fn server_id(&self) -> &str {
        &self.server_id
    }

    pub(crate) async fn has_users(&self) -> Result<bool, StorageError> {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users LIMIT 1)")
            .fetch_one(&self.pool)
            .await
            .map(|value: i64| value != 0)
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn insert_initial_user(
        &self,
        id: &str,
        username_normalized: &str,
        display_name: &str,
        password_hash: &str,
    ) -> Result<bool, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let inserted = sqlx::query(
            "INSERT INTO users (
                id, username_normalized, display_name, password_hash,
                is_admin, can_manage_server
            )
            SELECT ?, ?, ?, ?, 1, 1
            WHERE NOT EXISTS (SELECT 1 FROM users)",
        )
        .bind(id)
        .bind(username_normalized)
        .bind(display_name)
        .bind(password_hash)
        .execute(&mut *transaction)
        .await
        .map(|result| result.rows_affected() == 1)
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
        Ok(inserted)
    }

    pub(crate) async fn insert_user(
        &self,
        id: &str,
        username_normalized: &str,
        display_name: &str,
        password_hash: &str,
        is_admin: bool,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO users (
                id, username_normalized, display_name, password_hash,
                is_admin, can_manage_server
            ) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(username_normalized)
        .bind(display_name)
        .bind(password_hash)
        .bind(is_admin)
        .bind(is_admin)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_user_by_username(
        &self,
        username_normalized: &str,
    ) -> Result<Option<StoredUser>, StorageError> {
        sqlx::query(
            "SELECT id, username_normalized, display_name, password_hash,
                    is_disabled, is_admin, can_manage_server,
                    can_remote_access, can_download
             FROM users WHERE username_normalized = ?",
        )
        .bind(username_normalized)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredUser {
                id: row.get("id"),
                username_normalized: row.get("username_normalized"),
                display_name: row.get("display_name"),
                password_hash: row.get("password_hash"),
                is_disabled: row.get::<i64, _>("is_disabled") != 0,
                is_admin: row.get::<i64, _>("is_admin") != 0,
                can_manage_server: row.get::<i64, _>("can_manage_server") != 0,
                can_remote_access: row.get::<i64, _>("can_remote_access") != 0,
                can_download: row.get::<i64, _>("can_download") != 0,
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn user_exists(&self, user_id: &str) -> Result<bool, StorageError> {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE id = ?)")
            .bind(user_id)
            .fetch_one(&self.pool)
            .await
            .map(|value: i64| value != 0)
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_users(&self) -> Result<Vec<StoredUser>, StorageError> {
        sqlx::query(
            "SELECT id, username_normalized, display_name, password_hash,
                    is_disabled, is_admin, can_manage_server,
                    can_remote_access, can_download
             FROM users WHERE is_disabled = 0 ORDER BY username_normalized",
        )
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredUser {
                    id: row.get("id"),
                    username_normalized: row.get("username_normalized"),
                    display_name: row.get("display_name"),
                    password_hash: row.get("password_hash"),
                    is_disabled: row.get::<i64, _>("is_disabled") != 0,
                    is_admin: row.get::<i64, _>("is_admin") != 0,
                    can_manage_server: row.get::<i64, _>("can_manage_server") != 0,
                    can_remote_access: row.get::<i64, _>("can_remote_access") != 0,
                    can_download: row.get::<i64, _>("can_download") != 0,
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_user_by_access_token(
        &self,
        token_hash: &[u8],
    ) -> Result<Option<StoredUser>, StorageError> {
        sqlx::query(
            "SELECT u.id, u.username_normalized, u.display_name, u.password_hash,
                    u.is_disabled, u.is_admin, u.can_manage_server,
                    u.can_remote_access, u.can_download
             FROM access_tokens at
             JOIN users u ON u.id = at.user_id
             WHERE at.token_hash = ? AND at.revoked_at IS NULL",
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredUser {
                id: row.get("id"),
                username_normalized: row.get("username_normalized"),
                display_name: row.get("display_name"),
                password_hash: row.get("password_hash"),
                is_disabled: row.get::<i64, _>("is_disabled") != 0,
                is_admin: row.get::<i64, _>("is_admin") != 0,
                can_manage_server: row.get::<i64, _>("can_manage_server") != 0,
                can_remote_access: row.get::<i64, _>("can_remote_access") != 0,
                can_download: row.get::<i64, _>("can_download") != 0,
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn set_user_library_access(
        &self,
        user_id: &str,
        library_id: &str,
        can_view: bool,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO user_library_access (user_id, library_id, can_view)
             VALUES (?, ?, ?)
             ON CONFLICT(user_id, library_id) DO UPDATE SET
                 can_view = excluded.can_view, updated_at = unixepoch()",
        )
        .bind(user_id)
        .bind(library_id)
        .bind(can_view)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn has_user_library_access(
        &self,
        user_id: &str,
        library_id: &str,
    ) -> Result<bool, StorageError> {
        sqlx::query_scalar(
            "SELECT EXISTS(
                SELECT 1 FROM user_library_access
                WHERE user_id = ? AND library_id = ? AND can_view = 1
            )",
        )
        .bind(user_id)
        .bind(library_id)
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_accessible_library_ids(
        &self,
        user_id: &str,
    ) -> Result<Vec<String>, StorageError> {
        sqlx::query_scalar(
            "SELECT ula.library_id
             FROM user_library_access ula
             JOIN libraries l ON l.id = ula.library_id
             WHERE ula.user_id = ? AND ula.can_view = 1 AND l.is_enabled = 1
             ORDER BY l.name, l.id",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_enabled_library_ids(&self) -> Result<Vec<String>, StorageError> {
        sqlx::query_scalar("SELECT id FROM libraries WHERE is_enabled = 1 ORDER BY name, id")
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn find_item_library_id(
        &self,
        item_id: &str,
    ) -> Result<Option<String>, StorageError> {
        sqlx::query_scalar(
            "SELECT mi.library_id
             FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             WHERE mi.id = ? AND mi.removed_at IS NULL",
        )
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn insert_library(&self, library: NewLibrary<'_>) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO libraries (
                id, name, kind, is_enabled, realtime_watch_enabled,
                incremental_schedule, reconciliation_schedule, metadata_schedule,
                scan_concurrency, probe_concurrency
            ) VALUES (?, ?, ?, 1, ?, ?, ?, ?, ?, ?)",
        )
        .bind(library.id)
        .bind(library.name)
        .bind(library.kind)
        .bind(library.realtime_watch_enabled)
        .bind(library.incremental_schedule)
        .bind(library.reconciliation_schedule)
        .bind(library.metadata_schedule)
        .bind(library.scan_concurrency)
        .bind(library.probe_concurrency)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_libraries(&self) -> Result<Vec<StoredLibrary>, StorageError> {
        sqlx::query(
            "SELECT id, name, kind, is_enabled, realtime_watch_enabled,
                    incremental_schedule, reconciliation_schedule, metadata_schedule,
                    scan_concurrency, probe_concurrency, last_scan_at
             FROM libraries ORDER BY name, id",
        )
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredLibrary {
                    id: row.get("id"),
                    name: row.get("name"),
                    kind: row.get("kind"),
                    is_enabled: row.get::<i64, _>("is_enabled") != 0,
                    realtime_watch_enabled: row.get::<i64, _>("realtime_watch_enabled") != 0,
                    incremental_schedule: row.get("incremental_schedule"),
                    reconciliation_schedule: row.get("reconciliation_schedule"),
                    metadata_schedule: row.get("metadata_schedule"),
                    scan_concurrency: row.get("scan_concurrency"),
                    probe_concurrency: row.get("probe_concurrency"),
                    last_scan_at: row.get("last_scan_at"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_library(
        &self,
        id: &str,
    ) -> Result<Option<StoredLibrary>, StorageError> {
        sqlx::query(
            "SELECT id, name, kind, is_enabled, realtime_watch_enabled,
                    incremental_schedule, reconciliation_schedule, metadata_schedule,
                    scan_concurrency, probe_concurrency, last_scan_at
             FROM libraries WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredLibrary {
                id: row.get("id"),
                name: row.get("name"),
                kind: row.get("kind"),
                is_enabled: row.get::<i64, _>("is_enabled") != 0,
                realtime_watch_enabled: row.get::<i64, _>("realtime_watch_enabled") != 0,
                incremental_schedule: row.get("incremental_schedule"),
                reconciliation_schedule: row.get("reconciliation_schedule"),
                metadata_schedule: row.get("metadata_schedule"),
                scan_concurrency: row.get("scan_concurrency"),
                probe_concurrency: row.get("probe_concurrency"),
                last_scan_at: row.get("last_scan_at"),
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn library_exists(&self, library_id: &str) -> Result<bool, StorageError> {
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM libraries WHERE id = ?)")
            .bind(library_id)
            .fetch_one(&self.pool)
            .await
            .map(|value: i64| value != 0)
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn insert_library_root(
        &self,
        root: NewLibraryRoot<'_>,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO library_roots (
                id, library_id, canonical_path, display_path, is_available, is_writable
            ) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(root.id)
        .bind(root.library_id)
        .bind(root.canonical_path)
        .bind(root.display_path)
        .bind(root.is_available)
        .bind(root.is_writable)
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
        sqlx::query(
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

    pub(crate) async fn list_all_library_roots(
        &self,
    ) -> Result<Vec<StoredLibraryRoot>, StorageError> {
        sqlx::query(
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

    pub(crate) async fn create_scan_job(
        &self,
        id: &str,
        library_id: &str,
        job_type: &str,
        generation: &str,
        total_count: i64,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO scan_jobs (id, library_id, job_type, status, generation, total_count)
             VALUES (?, ?, ?, 'PENDING', ?, ?)",
        )
        .bind(id)
        .bind(library_id)
        .bind(job_type)
        .bind(generation)
        .bind(total_count)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_scan_job(
        &self,
        id: &str,
    ) -> Result<Option<StoredScanJob>, StorageError> {
        sqlx::query(
            "SELECT id, library_id, job_type, status, generation, cursor,
                    processed_count, total_count, cancel_requested, error
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

    pub(crate) async fn find_active_scan_job(
        &self,
        library_id: &str,
        job_type: &str,
    ) -> Result<Option<StoredScanJob>, StorageError> {
        sqlx::query(
            "SELECT id, library_id, job_type, status, generation, cursor,
                    processed_count, total_count, cancel_requested, error
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

    pub(crate) async fn claim_scan_job(&self, id: &str) -> Result<bool, StorageError> {
        sqlx::query(
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
        sqlx::query(
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

    pub(crate) async fn scan_job_cancel_requested(&self, id: &str) -> Result<bool, StorageError> {
        sqlx::query_scalar("SELECT cancel_requested FROM scan_jobs WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .map(|value: i64| value != 0)
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn request_scan_job_cancel(&self, id: &str) -> Result<(), StorageError> {
        sqlx::query(
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
        sqlx::query(
            "UPDATE scan_jobs
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

    pub(crate) async fn update_library_last_scan(
        &self,
        library_id: &str,
    ) -> Result<(), StorageError> {
        sqlx::query("UPDATE libraries SET last_scan_at = unixepoch() WHERE id = ?")
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
        sqlx::query("UPDATE library_roots SET scan_cursor = ? WHERE id = ?")
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
        sqlx::query(
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
        sqlx::query(
            "UPDATE library_roots
             SET is_available = ?, last_checked_at = unixepoch(),
                 unavailable_since = CASE
                     WHEN ? = 1 THEN NULL
                     ELSE COALESCE(unavailable_since, unixepoch())
                 END
             WHERE id = ?",
        )
        .bind(is_available)
        .bind(is_available)
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
        sqlx::query(
            "SELECT id, fingerprint
             FROM filesystem_entries
             WHERE library_root_id = ? AND relative_path = ?",
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

    pub(crate) async fn update_filesystem_entry(
        &self,
        id: &str,
        size: i64,
        modified_at: i64,
        fingerprint: &[u8],
        last_seen_generation: &str,
    ) -> Result<(), StorageError> {
        sqlx::query(
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
        sqlx::query(
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
        sqlx::query(
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
        sqlx::query(
            "UPDATE media_sources
             SET size = ?, probe_status = 'PENDING', probe_error = NULL,
                 updated_at = unixepoch()
             WHERE filesystem_entry_id = ?",
        )
        .bind(size)
        .bind(filesystem_entry_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn insert_filesystem_entry(
        &self,
        entry: NewFilesystemEntry<'_>,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO filesystem_entries (
                id, library_root_id, relative_path, entry_kind, size,
                modified_at, fingerprint, last_seen_generation, is_missing
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0)",
        )
        .bind(entry.id)
        .bind(entry.library_root_id)
        .bind(entry.relative_path)
        .bind(entry.entry_kind)
        .bind(entry.size)
        .bind(entry.modified_at)
        .bind(entry.fingerprint)
        .bind(entry.last_seen_generation)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_media_item(
        &self,
        library_id: &str,
        sort_title: &str,
        production_year: Option<i64>,
    ) -> Result<Option<StoredMediaItem>, StorageError> {
        let row = match production_year {
            Some(year) => {
                sqlx::query(
                    "SELECT id
                     FROM media_items
                     WHERE library_id = ? AND sort_title = ? AND production_year = ?
                       AND removed_at IS NULL",
                )
                .bind(library_id)
                .bind(sort_title)
                .bind(year)
                .fetch_optional(&self.pool)
                .await
            }
            None => {
                sqlx::query(
                    "SELECT id
                     FROM media_items
                     WHERE library_id = ? AND sort_title = ? AND production_year IS NULL
                       AND removed_at IS NULL",
                )
                .bind(library_id)
                .bind(sort_title)
                .fetch_optional(&self.pool)
                .await
            }
        };
        row.map(|row| row.map(stored_media_item))
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn count_catalog_items(
        &self,
        library_id: Option<&str>,
    ) -> Result<i64, StorageError> {
        match library_id {
            Some(library_id) => {
                sqlx::query_scalar(
                    "SELECT COUNT(*) FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 WHERE mi.library_id = ? AND mi.removed_at IS NULL",
                )
                .bind(library_id)
                .fetch_one(&self.pool)
                .await
            }
            None => {
                sqlx::query_scalar(
                    "SELECT COUNT(*) FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 WHERE mi.removed_at IS NULL",
                )
                .fetch_one(&self.pool)
                .await
            }
        }
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_catalog_rows(
        &self,
        library_id: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        let (query, binds) = match library_id {
            Some(library_id) => (
                "SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                        mi.title, mi.sort_title, mi.original_title, mi.overview,
                        mi.production_year, mi.runtime_ticks,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                         ORDER BY image_index LIMIT 1) AS poster_image_tag,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                         ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                        ms.id AS source_id, ms.source_kind, ms.container, ms.size,
                        ms.bitrate, ms.duration_ticks, ms.is_default, ms.probe_status,
                        mt.id AS stream_id, mt.stream_index, mt.stream_type,
                        mt.codec, mt.language, mt.title AS stream_title
                 FROM (
                     SELECT mi.id, mi.library_id, mi.item_type, mi.title, mi.sort_title,
                            mi.original_title, mi.overview, mi.production_year, mi.runtime_ticks
                     FROM media_items mi
                     JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                     WHERE mi.library_id = ? AND mi.removed_at IS NULL
                     ORDER BY mi.sort_title, mi.id
                     LIMIT ? OFFSET ?
                 ) mi
                 LEFT JOIN media_sources ms ON ms.item_id = mi.id
                 LEFT JOIN media_streams mt ON mt.media_source_id = ms.id
                 ORDER BY mi.sort_title, mi.id, ms.id, mt.stream_index",
                vec![
                    CatalogBind::Text(library_id),
                    CatalogBind::Integer(limit),
                    CatalogBind::Integer(offset),
                ],
            ),
            None => (
                "SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                        mi.title, mi.sort_title, mi.original_title, mi.overview,
                        mi.production_year, mi.runtime_ticks,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                         ORDER BY image_index LIMIT 1) AS poster_image_tag,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                         ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                        ms.id AS source_id, ms.source_kind, ms.container, ms.size,
                        ms.bitrate, ms.duration_ticks, ms.is_default, ms.probe_status,
                        mt.id AS stream_id, mt.stream_index, mt.stream_type,
                        mt.codec, mt.language, mt.title AS stream_title
                 FROM (
                     SELECT mi.id, mi.library_id, mi.item_type, mi.title, mi.sort_title,
                            mi.original_title, mi.overview, mi.production_year, mi.runtime_ticks
                     FROM media_items mi
                     JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                     WHERE mi.removed_at IS NULL
                     ORDER BY mi.sort_title, mi.id
                     LIMIT ? OFFSET ?
                 ) mi
                 LEFT JOIN media_sources ms ON ms.item_id = mi.id
                 LEFT JOIN media_streams mt ON mt.media_source_id = ms.id
                 ORDER BY mi.sort_title, mi.id, ms.id, mt.stream_index",
                vec![CatalogBind::Integer(limit), CatalogBind::Integer(offset)],
            ),
        };
        self.fetch_catalog_rows(query, &binds).await
    }

    pub(crate) async fn find_catalog_rows(
        &self,
        item_id: &str,
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        self.fetch_catalog_rows(
            "SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                    mi.title, mi.sort_title, mi.original_title, mi.overview,
                    mi.production_year, mi.runtime_ticks,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                     ORDER BY image_index LIMIT 1) AS poster_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                     ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                    ms.id AS source_id, ms.source_kind, ms.container, ms.size,
                    ms.bitrate, ms.duration_ticks, ms.is_default, ms.probe_status,
                    mt.id AS stream_id, mt.stream_index, mt.stream_type,
                    mt.codec, mt.language, mt.title AS stream_title
             FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             LEFT JOIN media_sources ms ON ms.item_id = mi.id
             LEFT JOIN media_streams mt ON mt.media_source_id = ms.id
             WHERE mi.id = ? AND mi.removed_at IS NULL
             ORDER BY mi.sort_title, mi.id, ms.id, mt.stream_index",
            &[CatalogBind::Text(item_id)],
        )
        .await
    }

    async fn fetch_catalog_rows(
        &self,
        query: &'static str,
        binds: &[CatalogBind<'_>],
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        let mut statement = sqlx::query(query);
        for bind in binds {
            statement = match bind {
                CatalogBind::Text(value) => statement.bind(*value),
                CatalogBind::Integer(value) => statement.bind(*value),
            };
        }
        statement
            .fetch_all(&self.pool)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(|row| StoredCatalogRow {
                        item_id: row.get("item_id"),
                        library_id: row.get("library_id"),
                        item_type: row.get("item_type"),
                        title: row.get("title"),
                        sort_title: row.get("sort_title"),
                        original_title: row.get("original_title"),
                        overview: row.get("overview"),
                        production_year: row.get("production_year"),
                        runtime_ticks: row.get("runtime_ticks"),
                        poster_image_tag: row.get("poster_image_tag"),
                        fanart_image_tag: row.get("fanart_image_tag"),
                        source_id: row.get("source_id"),
                        source_kind: row.get("source_kind"),
                        container: row.get("container"),
                        size: row.get("size"),
                        bitrate: row.get("bitrate"),
                        duration_ticks: row.get("duration_ticks"),
                        is_default: row
                            .get::<Option<i64>, _>("is_default")
                            .map(|value| value != 0),
                        probe_status: row.get("probe_status"),
                        stream_id: row.get("stream_id"),
                        stream_index: row.get("stream_index"),
                        stream_type: row.get("stream_type"),
                        codec: row.get("codec"),
                        language: row.get("language"),
                        stream_title: row.get("stream_title"),
                    })
                    .collect()
            })
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn insert_media_item(
        &self,
        item: NewMediaItem<'_>,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title,
                original_title, production_year, identification_status
            ) VALUES (?, ?, 'MOVIE', ?, ?, ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(item.id)
        .bind(item.library_id)
        .bind(item.title)
        .bind(item.sort_title)
        .bind(item.original_title)
        .bind(item.production_year)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn insert_media_source(
        &self,
        source: NewMediaSource<'_>,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO media_sources (
                id, item_id, source_kind, filesystem_entry_id,
                container, size, is_default, probe_status
            ) VALUES (?, ?, 'LOCAL_FILE', ?, ?, ?, ?, 'PENDING')",
        )
        .bind(source.id)
        .bind(source.item_id)
        .bind(source.filesystem_entry_id)
        .bind(source.container)
        .bind(source.size)
        .bind(source.is_default)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_media_sources_for_library(
        &self,
        library_id: &str,
    ) -> Result<Vec<StoredMediaSourcePath>, StorageError> {
        sqlx::query(
            "SELECT ms.id AS source_id, ms.item_id, ms.probe_status,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE mi.library_id = ? AND ms.source_kind = 'LOCAL_FILE'
             ORDER BY ms.item_id, fe.relative_path",
        )
        .bind(library_id)
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

    pub(crate) async fn save_media_probe(
        &self,
        update: MediaProbeUpdate<'_>,
    ) -> Result<(), StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        sqlx::query(
            "UPDATE media_sources
             SET container = ?, duration_ticks = ?, bitrate = ?,
                 probe_status = 'READY', probe_error = NULL,
                 updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(update.container)
        .bind(update.duration_ticks)
        .bind(update.bitrate)
        .bind(update.source_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        sqlx::query("DELETE FROM media_streams WHERE media_source_id = ?")
            .bind(update.source_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        for stream in update.streams {
            sqlx::query(
                "INSERT INTO media_streams (
                    id, media_source_id, stream_index, stream_type,
                    codec, language, title
                ) VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(Uuid::now_v7().to_string())
            .bind(update.source_id)
            .bind(stream.stream_index)
            .bind(stream.stream_type)
            .bind(stream.codec)
            .bind(stream.language)
            .bind(stream.title)
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

    pub(crate) async fn mark_media_probe_failed(
        &self,
        source_id: &str,
        status: &str,
        error: &str,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "UPDATE media_sources
             SET probe_status = ?, probe_error = ?, updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(status)
        .bind(error)
        .bind(source_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn update_media_item_metadata(
        &self,
        item_id: &str,
        title: Option<&str>,
        original_title: Option<&str>,
        overview: Option<&str>,
        production_year: Option<i64>,
        metadata_fingerprint: &[u8],
    ) -> Result<(), StorageError> {
        sqlx::query(
            "UPDATE media_items
             SET title = COALESCE(?, title),
                 original_title = COALESCE(?, original_title),
                 overview = COALESCE(?, overview),
                 production_year = COALESCE(?, production_year),
                 metadata_fingerprint = ?,
                 metadata_provenance_json = '{\"source\":\"LOCAL_NFO\"}'
             WHERE id = ?",
        )
        .bind(title)
        .bind(original_title)
        .bind(overview)
        .bind(production_year)
        .bind(metadata_fingerprint)
        .bind(item_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn media_item_metadata_fingerprint(
        &self,
        item_id: &str,
    ) -> Result<Option<Vec<u8>>, StorageError> {
        sqlx::query_scalar("SELECT metadata_fingerprint FROM media_items WHERE id = ?")
            .bind(item_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn mark_media_item_metadata_checked(
        &self,
        item_id: &str,
        metadata_fingerprint: &[u8],
    ) -> Result<(), StorageError> {
        sqlx::query("UPDATE media_items SET metadata_fingerprint = ? WHERE id = ?")
            .bind(metadata_fingerprint)
            .bind(item_id)
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn insert_item_image(
        &self,
        item_id: &str,
        image_type: &str,
        local_path: &std::path::Path,
        file_size: i64,
    ) -> Result<bool, StorageError> {
        let id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT OR IGNORE INTO item_images (
                id, item_id, image_type, image_index, local_path, file_size, source
            ) VALUES (?, ?, ?, 0, ?, ?, 'LOCAL')",
        )
        .bind(id)
        .bind(item_id)
        .bind(image_type)
        .bind(local_path.to_string_lossy().as_ref())
        .bind(file_size)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_item_image_candidates(
        &self,
        item_id: &str,
        image_type: &str,
        image_index: i64,
    ) -> Result<Vec<StoredItemImageCandidate>, StorageError> {
        sqlx::query(
            "SELECT ii.id, ii.local_path, lr.canonical_path AS root_path
             FROM item_images ii
             JOIN media_items mi ON mi.id = ii.item_id
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             JOIN library_roots lr ON lr.library_id = mi.library_id
             WHERE ii.item_id = ? AND ii.image_type = ? AND ii.image_index = ?
             ORDER BY lr.canonical_path",
        )
        .bind(item_id)
        .bind(image_type)
        .bind(image_index)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredItemImageCandidate {
                    id: row.get("id"),
                    local_path: row.get("local_path"),
                    root_path: row.get("root_path"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn create_access_token(
        &self,
        token: NewAccessToken<'_>,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO access_tokens (
                id, token_hash, user_id, device_id, client_name,
                device_name, client_version
            ) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(token.id)
        .bind(token.token_hash)
        .bind(token.user_id)
        .bind(token.device_id)
        .bind(token.client_name)
        .bind(token.device_name)
        .bind(token.client_version)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn revoke_access_token(&self, token_hash: &[u8]) -> Result<(), StorageError> {
        sqlx::query(
            "UPDATE access_tokens
             SET revoked_at = unixepoch(), updated_at = unixepoch()
             WHERE token_hash = ? AND revoked_at IS NULL",
        )
        .bind(token_hash)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn has_valid_access_token(
        &self,
        token_hash: &[u8],
    ) -> Result<bool, StorageError> {
        sqlx::query_scalar(
            "SELECT EXISTS(
                SELECT 1 FROM access_tokens
                WHERE token_hash = ? AND revoked_at IS NULL
            )",
        )
        .bind(token_hash)
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn create_web_session(
        &self,
        id: &str,
        user_id: &str,
        session_token_hash: &[u8],
        csrf_token_hash: &[u8],
        lifetime_seconds: i64,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO web_sessions (
                id, user_id, session_token_hash, csrf_token_hash, expires_at
            ) VALUES (?, ?, ?, ?, unixepoch() + ?)",
        )
        .bind(id)
        .bind(user_id)
        .bind(session_token_hash)
        .bind(csrf_token_hash)
        .bind(lifetime_seconds)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_web_session(
        &self,
        session_token_hash: &[u8],
    ) -> Result<Option<StoredWebSession>, StorageError> {
        sqlx::query(
            "SELECT ws.csrf_token_hash, u.id AS user_id,
                    u.username_normalized, u.display_name, u.is_disabled,
                    u.is_admin, u.can_manage_server, u.can_remote_access,
                    u.can_download
             FROM web_sessions ws
             JOIN users u ON u.id = ws.user_id
             WHERE ws.session_token_hash = ?
               AND ws.revoked_at IS NULL
               AND ws.expires_at > unixepoch()",
        )
        .bind(session_token_hash)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredWebSession {
                csrf_token_hash: row.get("csrf_token_hash"),
                user_id: row.get("user_id"),
                username_normalized: row.get("username_normalized"),
                display_name: row.get("display_name"),
                is_disabled: row.get::<i64, _>("is_disabled") != 0,
                is_admin: row.get::<i64, _>("is_admin") != 0,
                can_manage_server: row.get::<i64, _>("can_manage_server") != 0,
                can_remote_access: row.get::<i64, _>("can_remote_access") != 0,
                can_download: row.get::<i64, _>("can_download") != 0,
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn revoke_web_session(
        &self,
        session_token_hash: &[u8],
    ) -> Result<(), StorageError> {
        sqlx::query(
            "UPDATE web_sessions
             SET revoked_at = unixepoch(), updated_at = unixepoch()
             WHERE session_token_hash = ? AND revoked_at IS NULL",
        )
        .bind(session_token_hash)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub async fn schema_version(&self) -> Result<i64, StorageError> {
        sqlx::query("SELECT COALESCE(MAX(version), 0) AS version FROM _sqlx_migrations")
            .fetch_one(&self.pool)
            .await
            .map(|row| row.get::<i64, _>("version"))
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub async fn close(self) {
        self.pool.close().await;
    }
}

#[derive(Debug)]
pub(crate) struct StoredUser {
    pub(crate) id: String,
    pub(crate) username_normalized: String,
    pub(crate) display_name: String,
    pub(crate) password_hash: String,
    pub(crate) is_disabled: bool,
    pub(crate) is_admin: bool,
    pub(crate) can_manage_server: bool,
    pub(crate) can_remote_access: bool,
    pub(crate) can_download: bool,
}

#[derive(Debug)]
pub(crate) struct StoredLibrary {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) is_enabled: bool,
    pub(crate) realtime_watch_enabled: bool,
    pub(crate) incremental_schedule: Option<String>,
    pub(crate) reconciliation_schedule: Option<String>,
    pub(crate) metadata_schedule: Option<String>,
    pub(crate) scan_concurrency: i64,
    pub(crate) probe_concurrency: i64,
    pub(crate) last_scan_at: Option<i64>,
}

#[derive(Debug)]
pub(crate) struct StoredLibraryRoot {
    pub(crate) id: String,
    pub(crate) library_id: String,
    pub(crate) canonical_path: String,
    pub(crate) display_path: String,
    pub(crate) is_available: bool,
    pub(crate) is_writable: bool,
    pub(crate) last_checked_at: i64,
    pub(crate) unavailable_since: Option<i64>,
    pub(crate) scan_cursor: Option<String>,
}

#[derive(Debug)]
pub(crate) struct StoredScanJob {
    pub(crate) id: String,
    pub(crate) library_id: String,
    pub(crate) job_type: String,
    pub(crate) status: String,
    pub(crate) generation: String,
    pub(crate) cursor: Option<String>,
    pub(crate) processed_count: i64,
    pub(crate) total_count: i64,
    pub(crate) cancel_requested: bool,
    pub(crate) error: Option<String>,
}

fn stored_scan_job(row: sqlx::sqlite::SqliteRow) -> StoredScanJob {
    StoredScanJob {
        id: row.get("id"),
        library_id: row.get("library_id"),
        job_type: row.get("job_type"),
        status: row.get("status"),
        generation: row.get("generation"),
        cursor: row.get("cursor"),
        processed_count: row.get("processed_count"),
        total_count: row.get("total_count"),
        cancel_requested: row.get::<i64, _>("cancel_requested") != 0,
        error: row.get("error"),
    }
}

fn stored_library_root(row: sqlx::sqlite::SqliteRow) -> StoredLibraryRoot {
    StoredLibraryRoot {
        id: row.get("id"),
        library_id: row.get("library_id"),
        canonical_path: row.get("canonical_path"),
        display_path: row.get("display_path"),
        is_available: row.get::<i64, _>("is_available") != 0,
        is_writable: row.get::<i64, _>("is_writable") != 0,
        last_checked_at: row.get("last_checked_at"),
        unavailable_since: row.get("unavailable_since"),
        scan_cursor: row.get("scan_cursor"),
    }
}

#[derive(Debug)]
pub(crate) struct StoredFilesystemEntry {
    pub(crate) id: String,
    pub(crate) fingerprint: Option<Vec<u8>>,
}

fn stored_filesystem_entry(row: sqlx::sqlite::SqliteRow) -> StoredFilesystemEntry {
    StoredFilesystemEntry {
        id: row.get("id"),
        fingerprint: row.get("fingerprint"),
    }
}

#[derive(Debug)]
pub(crate) struct StoredMediaItem {
    pub(crate) id: String,
}

fn stored_media_item(row: sqlx::sqlite::SqliteRow) -> StoredMediaItem {
    StoredMediaItem { id: row.get("id") }
}

#[derive(Debug)]
pub(crate) struct StoredCatalogRow {
    pub(crate) item_id: String,
    pub(crate) library_id: String,
    pub(crate) item_type: String,
    pub(crate) title: String,
    pub(crate) sort_title: String,
    pub(crate) original_title: Option<String>,
    pub(crate) overview: Option<String>,
    pub(crate) production_year: Option<i64>,
    pub(crate) runtime_ticks: Option<i64>,
    pub(crate) poster_image_tag: Option<String>,
    pub(crate) fanart_image_tag: Option<String>,
    pub(crate) source_id: Option<String>,
    pub(crate) source_kind: Option<String>,
    pub(crate) container: Option<String>,
    pub(crate) size: Option<i64>,
    pub(crate) bitrate: Option<i64>,
    pub(crate) duration_ticks: Option<i64>,
    pub(crate) is_default: Option<bool>,
    pub(crate) probe_status: Option<String>,
    pub(crate) stream_id: Option<String>,
    pub(crate) stream_index: Option<i64>,
    pub(crate) stream_type: Option<String>,
    pub(crate) codec: Option<String>,
    pub(crate) language: Option<String>,
    pub(crate) stream_title: Option<String>,
}

#[derive(Debug)]
pub(crate) struct StoredItemImageCandidate {
    pub(crate) id: String,
    pub(crate) local_path: String,
    pub(crate) root_path: String,
}

enum CatalogBind<'a> {
    Text(&'a str),
    Integer(i64),
}

#[derive(Debug)]
pub(crate) struct StoredMediaSourcePath {
    pub(crate) source_id: String,
    pub(crate) item_id: String,
    pub(crate) probe_status: String,
    pub(crate) root_path: String,
    pub(crate) relative_path: String,
}

#[derive(Debug)]
pub(crate) struct StoredWebSession {
    pub(crate) csrf_token_hash: Vec<u8>,
    pub(crate) user_id: String,
    pub(crate) username_normalized: String,
    pub(crate) display_name: String,
    pub(crate) is_disabled: bool,
    pub(crate) is_admin: bool,
    pub(crate) can_manage_server: bool,
    pub(crate) can_remote_access: bool,
    pub(crate) can_download: bool,
}

pub(crate) struct NewAccessToken<'a> {
    pub(crate) id: &'a str,
    pub(crate) token_hash: &'a [u8],
    pub(crate) user_id: &'a str,
    pub(crate) device_id: &'a str,
    pub(crate) client_name: &'a str,
    pub(crate) device_name: &'a str,
    pub(crate) client_version: &'a str,
}

pub(crate) struct NewLibrary<'a> {
    pub(crate) id: &'a str,
    pub(crate) name: &'a str,
    pub(crate) kind: &'a str,
    pub(crate) realtime_watch_enabled: bool,
    pub(crate) incremental_schedule: Option<&'a str>,
    pub(crate) reconciliation_schedule: Option<&'a str>,
    pub(crate) metadata_schedule: Option<&'a str>,
    pub(crate) scan_concurrency: i64,
    pub(crate) probe_concurrency: i64,
}

pub(crate) struct NewLibraryRoot<'a> {
    pub(crate) id: &'a str,
    pub(crate) library_id: &'a str,
    pub(crate) canonical_path: &'a str,
    pub(crate) display_path: &'a str,
    pub(crate) is_available: bool,
    pub(crate) is_writable: bool,
}

pub(crate) struct NewFilesystemEntry<'a> {
    pub(crate) id: &'a str,
    pub(crate) library_root_id: &'a str,
    pub(crate) relative_path: &'a str,
    pub(crate) entry_kind: &'a str,
    pub(crate) size: i64,
    pub(crate) modified_at: i64,
    pub(crate) fingerprint: &'a [u8],
    pub(crate) last_seen_generation: &'a str,
}

pub(crate) struct NewMediaItem<'a> {
    pub(crate) id: &'a str,
    pub(crate) library_id: &'a str,
    pub(crate) title: &'a str,
    pub(crate) sort_title: &'a str,
    pub(crate) original_title: Option<&'a str>,
    pub(crate) production_year: Option<i64>,
}

pub(crate) struct NewMediaSource<'a> {
    pub(crate) id: &'a str,
    pub(crate) item_id: &'a str,
    pub(crate) filesystem_entry_id: &'a str,
    pub(crate) container: &'a str,
    pub(crate) size: i64,
    pub(crate) is_default: bool,
}

pub(crate) struct MediaProbeUpdate<'a> {
    pub(crate) source_id: &'a str,
    pub(crate) container: Option<&'a str>,
    pub(crate) duration_ticks: Option<i64>,
    pub(crate) bitrate: Option<i64>,
    pub(crate) streams: &'a [MediaStreamUpdate<'a>],
}

pub(crate) struct MediaStreamUpdate<'a> {
    pub(crate) stream_index: i64,
    pub(crate) stream_type: &'a str,
    pub(crate) codec: Option<&'a str>,
    pub(crate) language: Option<&'a str>,
    pub(crate) title: Option<&'a str>,
}

async fn ensure_server_id(pool: &SqlitePool) -> Result<String, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let existing =
        sqlx::query_scalar::<_, String>("SELECT value FROM lux_meta WHERE key = 'server_id'")
            .fetch_optional(&mut *transaction)
            .await?;
    if let Some(server_id) = existing {
        transaction.commit().await?;
        return Ok(server_id);
    }

    let generated = Uuid::now_v7().to_string();
    sqlx::query("INSERT OR IGNORE INTO lux_meta (key, value) VALUES ('server_id', ?)")
        .bind(generated)
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    sqlx::query_scalar("SELECT value FROM lux_meta WHERE key = 'server_id'")
        .fetch_one(pool)
        .await
}

#[derive(Debug)]
pub enum StorageError {
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Sqlx {
        path: PathBuf,
        source: sqlx::Error,
    },
    Migration {
        path: PathBuf,
        source: MigrateError,
    },
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(formatter, "database path '{}': {source}", path.display())
            }
            Self::Sqlx { path, source } => {
                write!(formatter, "database '{}': {source}", path.display())
            }
            Self::Migration { path, source } => {
                write!(
                    formatter,
                    "database migration '{}': {source}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Sqlx { source, .. } => Some(source),
            Self::Migration { source, .. } => Some(source),
        }
    }
}
