use std::{path::PathBuf, time::Duration};

use sqlx::{
    Row,
    migrate::{MigrateError, Migrator},
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions},
};
use tokio::fs;

use crate::config::Config;

static MIGRATOR: Migrator = sqlx::migrate!();

#[derive(Clone)]
pub struct Database {
    pool: SqlitePool,
    path: PathBuf,
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

        Ok(Self { pool, path })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
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
