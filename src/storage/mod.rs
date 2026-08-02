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

    pub(crate) async fn find_filesystem_entry(
        &self,
        library_root_id: &str,
        relative_path: &str,
    ) -> Result<Option<StoredFilesystemEntry>, StorageError> {
        sqlx::query(
            "SELECT id
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

    pub(crate) async fn insert_filesystem_entry(
        &self,
        entry: NewFilesystemEntry<'_>,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO filesystem_entries (
                id, library_root_id, relative_path, entry_kind, size,
                modified_at, last_seen_generation, is_missing
            ) VALUES (?, ?, ?, ?, ?, ?, ?, 0)",
        )
        .bind(entry.id)
        .bind(entry.library_root_id)
        .bind(entry.relative_path)
        .bind(entry.entry_kind)
        .bind(entry.size)
        .bind(entry.modified_at)
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
pub(crate) struct StoredFilesystemEntry {}

fn stored_filesystem_entry(row: sqlx::sqlite::SqliteRow) -> StoredFilesystemEntry {
    let _id: String = row.get("id");
    StoredFilesystemEntry {}
}

#[derive(Debug)]
pub(crate) struct StoredMediaItem {
    pub(crate) id: String,
}

fn stored_media_item(row: sqlx::sqlite::SqliteRow) -> StoredMediaItem {
    StoredMediaItem { id: row.get("id") }
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
