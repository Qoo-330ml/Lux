use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use sqlx::{
    Acquire, Any, AnyPool, Executor, Row,
    any::{AnyConnectOptions, AnyPoolOptions},
    migrate::{MigrateError, Migrator},
};
use tokio::fs;
use uuid::Uuid;

use crate::config::{Config, DatabaseBackend, DatabaseConfiguration, DatabaseConfigurationError};

static SQLITE_MIGRATOR: Migrator = sqlx::migrate!();
static POSTGRES_MIGRATOR: Migrator = sqlx::migrate!("./migrations-postgres");

pub(crate) const PLAYBACK_SESSION_STALE_AFTER_SECONDS: i64 = 90;
pub(crate) const DEFAULT_PLAYED_PERCENT: i64 = 95;
const MAX_BACKGROUND_PAGE_SIZE: i64 = 500;
const BATCH_INSERT_CHUNK_SIZE: usize = 100;

fn database_flag(value: bool) -> i64 {
    i64::from(value)
}

fn current_unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default()
}

fn normalize_person_name(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

fn playback_reached_played_threshold(
    position_ticks: i64,
    duration_ticks: i64,
    played_percent: i64,
) -> bool {
    position_ticks > 0
        && duration_ticks > 0
        && i128::from(position_ticks) * 100
            >= i128::from(duration_ticks) * i128::from(played_percent.clamp(1, 100))
}

#[derive(Clone)]
pub struct Database {
    pool: AnyPool,
    path: PathBuf,
    server_id: String,
    backend: DatabaseBackend,
}

impl Database {
    pub async fn connect(config: &Config) -> Result<Self, StorageError> {
        Self::connect_with_configuration(config, &DatabaseConfiguration::Sqlite).await
    }

    pub async fn connect_with_configuration(
        config: &Config,
        configuration: &DatabaseConfiguration,
    ) -> Result<Self, StorageError> {
        configuration
            .validate()
            .map_err(StorageError::Configuration)?;
        let backend = configuration.backend();
        fs::create_dir_all(&config.config_dir)
            .await
            .map_err(|source| StorageError::Io {
                path: config.config_dir.clone(),
                source,
            })?;

        let path = match backend {
            DatabaseBackend::Sqlite => config.config_dir.join("lux.db"),
            DatabaseBackend::Postgres => PathBuf::from("external PostgreSQL"),
        };
        sqlx::any::install_default_drivers();
        let database_url = match configuration {
            DatabaseConfiguration::Sqlite => format!("sqlite://{}?mode=rwc", path.display()),
            DatabaseConfiguration::Postgres(_) => configuration
                .postgres_url()
                .map_err(StorageError::Configuration)?
                .ok_or_else(|| {
                    StorageError::Configuration(DatabaseConfigurationError::Invalid(
                        "PostgreSQL 连接配置缺失".to_owned(),
                    ))
                })?,
        };
        let options =
            AnyConnectOptions::from_str(&database_url).map_err(|source| StorageError::Sqlx {
                path: path.clone(),
                source,
            })?;
        let after_connect_sql = match backend {
            DatabaseBackend::Sqlite => {
                "PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;"
            }
            DatabaseBackend::Postgres => "SET TIME ZONE 'UTC'",
        };
        let pool = AnyPoolOptions::new()
            .max_connections(5)
            .after_connect(move |connection, _| {
                Box::pin(async move {
                    connection.execute(after_connect_sql).await?;
                    if backend == DatabaseBackend::Postgres {
                        connection.execute("SET application_name = 'lux'").await?;
                    }
                    Ok(())
                })
            })
            .connect_with(options)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: path.clone(),
                source,
            })?;

        if backend == DatabaseBackend::Postgres
            && let Err(error) = validate_postgres_schema(&pool).await
        {
            pool.close().await;
            return Err(error);
        }

        let migrator = match backend {
            DatabaseBackend::Sqlite => &SQLITE_MIGRATOR,
            DatabaseBackend::Postgres => &POSTGRES_MIGRATOR,
        };
        if let Err(source) = migrator.run(&pool).await {
            pool.close().await;
            return Err(StorageError::Migration { path, source });
        }
        if backend == DatabaseBackend::Sqlite {
            remove_sqlite_title_year_unique(&pool, &path).await?;
        }
        let server_id =
            ensure_server_id(&pool, backend)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: path.clone(),
                    source,
                })?;

        Ok(Self {
            pool,
            path,
            server_id,
            backend,
        })
    }

    pub async fn test_configuration(
        configuration: &DatabaseConfiguration,
    ) -> Result<(), StorageError> {
        configuration
            .validate()
            .map_err(StorageError::Configuration)?;
        if configuration.backend() == DatabaseBackend::Sqlite {
            return Ok(());
        }

        sqlx::any::install_default_drivers();
        let database_url = configuration
            .postgres_url()
            .map_err(StorageError::Configuration)?
            .ok_or_else(|| {
                StorageError::Configuration(DatabaseConfigurationError::Invalid(
                    "PostgreSQL 连接配置缺失".to_owned(),
                ))
            })?;
        let options =
            AnyConnectOptions::from_str(&database_url).map_err(|source| StorageError::Sqlx {
                path: PathBuf::from("external PostgreSQL"),
                source,
            })?;
        let pool = AnyPoolOptions::new()
            .max_connections(1)
            .after_connect(|connection, _| {
                Box::pin(async move {
                    connection.execute("SET TIME ZONE 'UTC'").await?;
                    connection.execute("SET application_name = 'lux'").await?;
                    Ok(())
                })
            })
            .connect_with(options)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: PathBuf::from("external PostgreSQL"),
                source,
            })?;
        validate_postgres_schema(&pool).await?;
        sqlx::query_scalar::<_, i64>("SELECT 1")
            .fetch_one(&pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: PathBuf::from("external PostgreSQL"),
                source,
            })?;
        pool.close().await;
        Ok(())
    }

    pub fn pool(&self) -> &AnyPool {
        &self.pool
    }

    pub fn server_id(&self) -> &str {
        &self.server_id
    }

    pub fn backend(&self) -> DatabaseBackend {
        self.backend
    }

    fn scalar_max_function(&self) -> &'static str {
        match self.backend {
            DatabaseBackend::Sqlite => "MAX",
            DatabaseBackend::Postgres => "GREATEST",
        }
    }

    fn scalar_min_function(&self) -> &'static str {
        match self.backend {
            DatabaseBackend::Sqlite => "MIN",
            DatabaseBackend::Postgres => "LEAST",
        }
    }

    pub(crate) async fn has_users(&self) -> Result<bool, StorageError> {
        self.query_scalar("SELECT CASE WHEN EXISTS(SELECT 1 FROM users LIMIT 1) THEN 1 ELSE 0 END")
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
        let inserted = self
            .query(
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
        self.query(
            "INSERT INTO users (
                id, username_normalized, display_name, password_hash,
                is_admin, can_manage_server
            ) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(username_normalized)
        .bind(display_name)
        .bind(password_hash)
        .bind(database_flag(is_admin))
        .bind(database_flag(is_admin))
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
        self.query(
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
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(SELECT 1 FROM users WHERE id = ?) THEN 1 ELSE 0 END",
        )
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
        self.query(
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

    pub(crate) async fn find_user_by_id(
        &self,
        user_id: &str,
    ) -> Result<Option<StoredUser>, StorageError> {
        self.query(
            "SELECT id, username_normalized, display_name, password_hash,
                    is_disabled, is_admin, can_manage_server,
                    can_remote_access, can_download
             FROM users WHERE id = ?",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_user))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn update_user(
        &self,
        user_id: &str,
        update: UpdateUser<'_>,
    ) -> Result<Option<StoredUser>, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let Some(current) = self
            .query(
                "SELECT is_disabled, can_manage_server
             FROM users WHERE id = ?",
            )
            .bind(user_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
        else {
            return Ok(None);
        };
        let current_disabled = current.get::<i64, _>("is_disabled") != 0;
        let current_can_manage = current.get::<i64, _>("can_manage_server") != 0;
        let next_disabled = update.is_disabled.unwrap_or(current_disabled);
        let next_can_manage = update.can_manage_server.unwrap_or(current_can_manage);
        let is_disabled = update.is_disabled.map(database_flag);
        let is_admin = update.is_admin.map(database_flag);
        let can_manage_server = update.can_manage_server.map(database_flag);
        let can_remote_access = update.can_remote_access.map(database_flag);
        let can_download = update.can_download.map(database_flag);
        if current_can_manage && (!next_can_manage || next_disabled) {
            let remaining: i64 = self
                .query_scalar(
                    "SELECT COUNT(*) FROM users
                 WHERE can_manage_server = 1 AND is_disabled = 0 AND id != ?",
                )
                .bind(user_id)
                .fetch_one(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if remaining == 0 {
                return Err(StorageError::LastManager);
            }
        }
        self.query(
            "UPDATE users
             SET display_name = COALESCE(?, display_name),
                 password_hash = COALESCE(?, password_hash),
                 is_disabled = COALESCE(?, is_disabled),
                 is_admin = COALESCE(?, is_admin),
                 can_manage_server = COALESCE(?, can_manage_server),
                 can_remote_access = COALESCE(?, can_remote_access),
                 can_download = COALESCE(?, can_download),
                 updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(update.display_name)
        .bind(update.password_hash)
        .bind(is_disabled)
        .bind(is_admin)
        .bind(can_manage_server)
        .bind(can_remote_access)
        .bind(can_download)
        .bind(user_id)
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
        self.find_user_by_id(user_id).await
    }

    pub(crate) async fn insert_audit_event(
        &self,
        event: NewAuditEvent<'_>,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO audit_events (
                id, actor_user_id, event_type, target_type, target_id, metadata_json
            ) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(event.actor_user_id)
        .bind(event.event_type)
        .bind(event.target_type)
        .bind(event.target_id)
        .bind(event.metadata_json)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_audit_events(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredAuditEvent>, StorageError> {
        self.query(
            "SELECT ae.id, ae.actor_user_id, u.username_normalized AS actor_username,
                    ae.event_type, ae.target_type, ae.target_id,
                    ae.metadata_json, ae.created_at
             FROM audit_events ae
             LEFT JOIN users u ON u.id = ae.actor_user_id
             ORDER BY ae.created_at DESC, ae.id DESC
             LIMIT ? OFFSET ?",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredAuditEvent {
                    id: row.get("id"),
                    actor_user_id: row.get("actor_user_id"),
                    actor_username: row.get("actor_username"),
                    event_type: row.get("event_type"),
                    target_type: row.get("target_type"),
                    target_id: row.get("target_id"),
                    metadata_json: row.get("metadata_json"),
                    created_at: row.get("created_at"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_activity_events(
        &self,
        limit: i64,
    ) -> Result<Vec<StoredActivityEvent>, StorageError> {
        self.query(
            "SELECT ae.id, ae.actor_user_id, u.username_normalized AS actor_username,
                    ae.event_type, ae.target_type, ae.target_id,
                    mi.title AS target_title, ae.metadata_json, ae.created_at
             FROM audit_events ae
             LEFT JOIN users u ON u.id = ae.actor_user_id
             LEFT JOIN media_items mi ON mi.id = ae.target_id
             WHERE ae.event_type IN (
                 'AUTH_LOGIN', 'PLAYBACK_STARTED', 'PLAYBACK_PAUSED', 'PLAYBACK_STOPPED'
             )
             ORDER BY ae.created_at DESC, ae.id DESC
             LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredActivityEvent {
                    id: row.get("id"),
                    actor_user_id: row.get("actor_user_id"),
                    actor_username: row.get("actor_username"),
                    event_type: row.get("event_type"),
                    target_type: row.get("target_type"),
                    target_id: row.get("target_id"),
                    target_title: row.get("target_title"),
                    metadata_json: row.get("metadata_json"),
                    created_at: row.get("created_at"),
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
        self.query(
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

    pub(crate) async fn find_access_token_device(
        &self,
        token_hash: &[u8],
    ) -> Result<Option<StoredAccessTokenDevice>, StorageError> {
        self.query(
            "SELECT device_id, client_name, device_name, client_version
             FROM access_tokens
             WHERE token_hash = ? AND revoked_at IS NULL",
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredAccessTokenDevice {
                device_id: row.get("device_id"),
                client_name: row.get("client_name"),
                device_name: row.get("device_name"),
                client_version: row.get("client_version"),
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
        self.query(
            "INSERT INTO user_library_access (user_id, library_id, can_view)
             VALUES (?, ?, ?)
             ON CONFLICT(user_id, library_id) DO UPDATE SET
                 can_view = excluded.can_view, updated_at = unixepoch()",
        )
        .bind(user_id)
        .bind(library_id)
        .bind(database_flag(can_view))
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
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(
                SELECT 1 FROM user_library_access
                WHERE user_id = ? AND library_id = ? AND can_view = 1
            ) THEN 1 ELSE 0 END",
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
        self.query_scalar(
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
        self.query_scalar("SELECT id FROM libraries WHERE is_enabled = 1 ORDER BY name, id")
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn sync_person_index_rebuild_jobs(
        &self,
        schema_version: i64,
    ) -> Result<Vec<StoredPersonIndexRebuildJob>, StorageError> {
        for library_id in self.list_enabled_library_ids().await? {
            self.query(
                "INSERT INTO person_index_rebuild_jobs (library_id, schema_version)
                 VALUES (?, ?)
                 ON CONFLICT(library_id) DO UPDATE SET
                    schema_version = excluded.schema_version,
                    status = CASE
                        WHEN person_index_rebuild_jobs.schema_version <> excluded.schema_version
                            THEN 'QUEUED'
                        WHEN person_index_rebuild_jobs.status = 'RUNNING'
                            AND person_index_rebuild_jobs.updated_at < unixepoch() - 60
                            THEN 'QUEUED'
                        ELSE person_index_rebuild_jobs.status
                    END,
                    cursor_id = CASE
                        WHEN person_index_rebuild_jobs.schema_version <> excluded.schema_version
                            THEN NULL
                        WHEN person_index_rebuild_jobs.status = 'RUNNING'
                            AND person_index_rebuild_jobs.updated_at < unixepoch() - 60
                            THEN person_index_rebuild_jobs.cursor_id
                        ELSE person_index_rebuild_jobs.cursor_id
                    END,
                    processed_count = CASE
                        WHEN person_index_rebuild_jobs.schema_version <> excluded.schema_version
                            THEN 0
                        ELSE person_index_rebuild_jobs.processed_count
                    END,
                    total_count = CASE
                        WHEN person_index_rebuild_jobs.schema_version <> excluded.schema_version
                            THEN 0
                        ELSE person_index_rebuild_jobs.total_count
                    END,
                    cancel_requested = CASE
                        WHEN person_index_rebuild_jobs.schema_version <> excluded.schema_version
                            THEN 0
                        WHEN person_index_rebuild_jobs.status = 'RUNNING'
                            AND person_index_rebuild_jobs.updated_at < unixepoch() - 60
                            THEN 0
                        ELSE person_index_rebuild_jobs.cancel_requested
                    END,
                    run_token = CASE
                        WHEN person_index_rebuild_jobs.schema_version <> excluded.schema_version
                            THEN NULL
                        WHEN person_index_rebuild_jobs.status = 'RUNNING'
                            AND person_index_rebuild_jobs.updated_at < unixepoch() - 60
                            THEN NULL
                        ELSE person_index_rebuild_jobs.run_token
                    END,
                    error = CASE
                        WHEN person_index_rebuild_jobs.schema_version <> excluded.schema_version
                            THEN NULL
                        ELSE person_index_rebuild_jobs.error
                    END,
                    updated_at = unixepoch()",
            )
            .bind(&library_id)
            .bind(schema_version)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        self.list_person_index_rebuild_jobs(0, 500).await
    }

    pub(crate) async fn list_person_index_rebuild_jobs(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredPersonIndexRebuildJob>, StorageError> {
        self.query(
            "SELECT library_id, status, cursor_id, processed_count, total_count,
                    cancel_requested
             FROM person_index_rebuild_jobs
             ORDER BY library_id
             LIMIT ? OFFSET ?",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(stored_person_index_rebuild_job)
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn get_person_index_rebuild_job(
        &self,
        library_id: &str,
    ) -> Result<Option<StoredPersonIndexRebuildJob>, StorageError> {
        self.query(
            "SELECT library_id, status, cursor_id, processed_count, total_count,
                    cancel_requested
             FROM person_index_rebuild_jobs
             WHERE library_id = ?",
        )
        .bind(library_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_person_index_rebuild_job))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn count_person_index_rebuild_jobs(&self) -> Result<i64, StorageError> {
        self.query_scalar("SELECT COUNT(*) FROM person_index_rebuild_jobs")
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn request_person_index_rebuild_job(
        &self,
        library_id: &str,
        schema_version: i64,
    ) -> Result<bool, StorageError> {
        let enabled = self
            .query_scalar::<i64>("SELECT 1 FROM libraries WHERE id = ? AND is_enabled = 1 LIMIT 1")
            .bind(library_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .is_some();
        if !enabled {
            return Ok(false);
        }
        self.query(
            "INSERT INTO person_index_rebuild_jobs (
                library_id, status, cursor_id, processed_count, total_count,
                cancel_requested, schema_version, run_token, error,
                created_at, updated_at, started_at, finished_at
             ) VALUES (?, 'QUEUED', NULL, 0, 0, 0, ?, NULL, NULL,
                       unixepoch(), unixepoch(), NULL, NULL)
             ON CONFLICT(library_id) DO UPDATE SET
                status = 'QUEUED', cursor_id = NULL, processed_count = 0,
                total_count = 0, cancel_requested = 0, schema_version = excluded.schema_version,
                run_token = NULL, error = NULL, updated_at = unixepoch(),
                started_at = NULL, finished_at = NULL",
        )
        .bind(library_id)
        .bind(schema_version)
        .execute(&self.pool)
        .await
        .map(|_| true)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn claim_person_index_rebuild_job(
        &self,
        library_id: &str,
        run_token: &str,
    ) -> Result<bool, StorageError> {
        let result = self
            .query(
                "UPDATE person_index_rebuild_jobs
                 SET status = 'RUNNING', run_token = ?, started_at = unixepoch(),
                     updated_at = unixepoch()
                 WHERE library_id = ? AND status = 'QUEUED' AND cancel_requested = 0",
            )
            .bind(run_token)
            .bind(library_id)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn request_person_index_rebuild_job_cancel(
        &self,
        library_id: &str,
    ) -> Result<bool, StorageError> {
        let result = self
            .query(
                "UPDATE person_index_rebuild_jobs
                 SET status = CASE WHEN status = 'QUEUED' THEN 'CANCELLED' ELSE status END,
                     cancel_requested = CASE WHEN status = 'QUEUED' THEN 0 ELSE 1 END,
                     run_token = CASE WHEN status = 'QUEUED' THEN NULL ELSE run_token END,
                     finished_at = CASE WHEN status = 'QUEUED' THEN unixepoch() ELSE finished_at END,
                     updated_at = unixepoch()
                 WHERE library_id = ? AND status IN ('QUEUED', 'RUNNING')",
            )
            .bind(library_id)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn person_index_rebuild_job_cancel_requested(
        &self,
        library_id: &str,
        run_token: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE
                        WHEN status = 'RUNNING' AND run_token = ? AND cancel_requested = 0
                            THEN 0
                        ELSE 1
                    END
             FROM person_index_rebuild_jobs
             WHERE library_id = ?",
        )
        .bind(run_token)
        .bind(library_id)
        .fetch_optional(&self.pool)
        .await
        .map(|value: Option<i64>| value != Some(0))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn update_person_index_rebuild_progress(
        &self,
        library_id: &str,
        run_token: &str,
        cursor_id: &str,
        processed_count: i64,
        total_count: i64,
    ) -> Result<Option<()>, StorageError> {
        let result = self
            .query(
                "UPDATE person_index_rebuild_jobs
                 SET cursor_id = ?, processed_count = ?, total_count = ?,
                     updated_at = unixepoch()
                 WHERE library_id = ? AND status = 'RUNNING' AND run_token = ?",
            )
            .bind(cursor_id)
            .bind(processed_count)
            .bind(total_count)
            .bind(library_id)
            .bind(run_token)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok((result.rows_affected() == 1).then_some(()))
    }

    pub(crate) async fn finish_person_index_rebuild_job(
        &self,
        library_id: &str,
        run_token: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<bool, StorageError> {
        if !matches!(status, "COMPLETED" | "CANCELLED" | "FAILED") {
            return Err(StorageError::Conflict(
                "invalid person index rebuild status".to_owned(),
            ));
        }
        let result = self
            .query(
                "UPDATE person_index_rebuild_jobs
                 SET status = CASE WHEN cancel_requested = 1 THEN 'CANCELLED' ELSE ? END,
                     error = CASE WHEN cancel_requested = 1 THEN NULL ELSE ? END,
                     run_token = NULL, finished_at = unixepoch(), updated_at = unixepoch()
                 WHERE library_id = ? AND status = 'RUNNING' AND run_token = ?",
            )
            .bind(status)
            .bind(error)
            .bind(library_id)
            .bind(run_token)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn create_notification_destination(
        &self,
        destination: NewNotificationDestination<'_>,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO notification_destinations (
                id, name, url, enabled, allow_private_network, event_types_json, payload_format,
                provider_plugin_id, provider_config_json
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(destination.id)
        .bind(destination.name)
        .bind(destination.url)
        .bind(database_flag(destination.enabled))
        .bind(database_flag(destination.allow_private_network))
        .bind(destination.event_types_json)
        .bind(destination.payload_format)
        .bind(destination.provider_plugin_id)
        .bind(destination.provider_config_json)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_notification_destination(
        &self,
        id: &str,
    ) -> Result<Option<StoredNotificationDestination>, StorageError> {
        self.query(
            "SELECT id, name, url, enabled, allow_private_network, event_types_json, payload_format,
                    provider_plugin_id, provider_config_json, created_at, updated_at
             FROM notification_destinations WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_notification_destination))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_notification_destinations(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredNotificationDestination>, StorageError> {
        self.query(
            "SELECT id, name, url, enabled, allow_private_network, event_types_json, payload_format,
                    provider_plugin_id, provider_config_json, created_at, updated_at
             FROM notification_destinations
             ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
        )
        .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
        .bind(offset.max(0))
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(stored_notification_destination)
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_enabled_notification_destinations(
        &self,
    ) -> Result<Vec<StoredNotificationDestination>, StorageError> {
        self.query(
            "SELECT id, name, url, enabled, allow_private_network, event_types_json, payload_format,
                    provider_plugin_id, provider_config_json, created_at, updated_at
             FROM notification_destinations
             WHERE enabled = 1
             ORDER BY id LIMIT 1000",
        )
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(stored_notification_destination)
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn update_notification_destination(
        &self,
        id: &str,
        update: UpdateNotificationDestination<'_>,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE notification_destinations
             SET name = COALESCE(?, name),
                 url = COALESCE(?, url),
                 enabled = COALESCE(?, enabled),
                 allow_private_network = COALESCE(?, allow_private_network),
                 event_types_json = COALESCE(?, event_types_json),
                 payload_format = COALESCE(?, payload_format),
                 provider_plugin_id = COALESCE(?, provider_plugin_id),
                 provider_config_json = COALESCE(?, provider_config_json),
                 updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(update.name)
        .bind(update.url)
        .bind(update.enabled.map(database_flag))
        .bind(update.allow_private_network.map(database_flag))
        .bind(update.event_types_json)
        .bind(update.payload_format)
        .bind(update.provider_plugin_id)
        .bind(update.provider_config_json)
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn delete_notification_destination(
        &self,
        id: &str,
    ) -> Result<bool, StorageError> {
        self.query("DELETE FROM notification_destinations WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected() == 1)
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn insert_notification_event_with_deliveries(
        &self,
        event: NewNotificationEvent<'_>,
        destination_ids: &[String],
    ) -> Result<bool, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let inserted = self
            .query(
                "INSERT INTO notification_events (
                    id, event_type, schema_version, occurred_at, dedupe_key, payload_json
                 ) VALUES (?, ?, ?, ?, ?, ?)
                 ON CONFLICT(dedupe_key) DO NOTHING",
            )
            .bind(event.id)
            .bind(event.event_type)
            .bind(event.schema_version)
            .bind(event.occurred_at)
            .bind(event.dedupe_key)
            .bind(event.payload_json)
            .execute(&mut *transaction)
            .await
            .map(|result| result.rows_affected() == 1)
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if inserted {
            for destination_id in destination_ids {
                self.query(
                    "INSERT INTO notification_deliveries (
                        id, event_id, destination_id, status
                     ) VALUES (?, ?, ?, 'PENDING')
                     ON CONFLICT(event_id, destination_id) DO NOTHING",
                )
                .bind(Uuid::now_v7().to_string())
                .bind(event.id)
                .bind(destination_id)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            }
        }
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(inserted)
    }

    pub(crate) async fn list_ready_notification_deliveries(
        &self,
        limit: i64,
    ) -> Result<Vec<StoredNotificationDelivery>, StorageError> {
        self.query(
            "SELECT d.id, d.event_id, d.destination_id, d.status, d.attempt_count,
                    d.next_attempt_at, d.claimed_until, d.last_http_status, d.last_error,
                    d.delivered_at, d.created_at, d.updated_at,
                    e.event_type, e.schema_version, e.occurred_at, e.payload_json,
                    n.name, n.url, n.allow_private_network,
                    n.provider_plugin_id, n.provider_config_json
             FROM notification_deliveries d
             JOIN notification_events e ON e.id = d.event_id
             JOIN notification_destinations n ON n.id = d.destination_id
             WHERE n.enabled = 1
               AND d.next_attempt_at <= unixepoch()
               AND (d.status = 'PENDING'
                    OR (d.status = 'RUNNING' AND d.claimed_until <= unixepoch()))
             ORDER BY d.next_attempt_at, d.created_at, d.id
             LIMIT ?",
        )
        .bind(limit.clamp(1, 100))
        .fetch_all(&self.pool)
        .await
        .map(|rows| rows.into_iter().map(stored_notification_delivery).collect())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_notification_deliveries(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredNotificationDelivery>, StorageError> {
        self.query(
            "SELECT d.id, d.event_id, d.destination_id, d.status, d.attempt_count,
                    d.next_attempt_at, d.claimed_until, d.last_http_status, d.last_error,
                    d.delivered_at, d.created_at, d.updated_at,
                    e.event_type, e.schema_version, e.occurred_at, e.payload_json,
                    n.name, n.url, n.allow_private_network,
                    n.provider_plugin_id, n.provider_config_json
             FROM notification_deliveries d
             JOIN notification_events e ON e.id = d.event_id
             JOIN notification_destinations n ON n.id = d.destination_id
             ORDER BY d.created_at DESC, d.id DESC
             LIMIT ? OFFSET ?",
        )
        .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
        .bind(offset.max(0))
        .fetch_all(&self.pool)
        .await
        .map(|rows| rows.into_iter().map(stored_notification_delivery).collect())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn claim_notification_delivery(
        &self,
        id: &str,
        lease_seconds: i64,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE notification_deliveries
             SET status = 'RUNNING',
                 attempt_count = attempt_count + 1,
                 claimed_until = unixepoch() + ?,
                 updated_at = unixepoch()
             WHERE id = ? AND (
                 status = 'PENDING'
                 OR (status = 'RUNNING' AND claimed_until <= unixepoch())
             )",
        )
        .bind(lease_seconds.max(1))
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn mark_notification_delivered(
        &self,
        id: &str,
        http_status: i64,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE notification_deliveries
             SET status = 'DELIVERED', claimed_until = NULL, last_http_status = ?,
                 last_error = NULL, delivered_at = unixepoch(), updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(http_status)
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn mark_notification_retry(
        &self,
        id: &str,
        status: &str,
        http_status: Option<i64>,
        error: &str,
        next_attempt_at: i64,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE notification_deliveries
             SET status = ?, claimed_until = NULL, last_http_status = ?, last_error = ?,
                 next_attempt_at = ?, updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(status)
        .bind(http_status)
        .bind(error)
        .bind(next_attempt_at)
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn retry_notification_delivery(&self, id: &str) -> Result<bool, StorageError> {
        self.query(
            "UPDATE notification_deliveries
             SET status = 'PENDING', next_attempt_at = unixepoch(), claimed_until = NULL,
                 last_error = NULL, updated_at = unixepoch()
             WHERE id = ? AND status IN ('FAILED', 'DELIVERED')",
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

    pub(crate) async fn find_item_library_id(
        &self,
        item_id: &str,
    ) -> Result<Option<String>, StorageError> {
        self.query_scalar(
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

    pub(crate) async fn find_item_scan_source_path(
        &self,
        item_id: &str,
    ) -> Result<Option<StoredItemScanPath>, StorageError> {
        self.query(
            "SELECT source_item.library_id, fe.library_root_id, fe.relative_path
             FROM media_items source_item
             JOIN media_sources ms ON ms.item_id = source_item.id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             WHERE source_item.removed_at IS NULL
               AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
               AND fe.is_missing = 0
               AND (
                    source_item.id = ?
                    OR (
                        source_item.item_type = 'EPISODE'
                        AND (source_item.series_id = ? OR source_item.parent_id = ?)
                    )
               )
             ORDER BY CASE WHEN source_item.id = ? THEN 0 ELSE 1 END,
                      ms.is_default DESC, ms.id
             LIMIT 1",
        )
        .bind(item_id)
        .bind(item_id)
        .bind(item_id)
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredItemScanPath {
                library_id: row.get("library_id"),
                library_root_id: row.get("library_root_id"),
                relative_path: row.get("relative_path"),
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_item_source_locator(
        &self,
        item_id: &str,
    ) -> Result<Option<StoredItemSourceLocator>, StorageError> {
        self.query(
            "SELECT ms.item_id, lr.canonical_path, fe.relative_path,
                    fe.fingerprint, fe.size, fe.modified_at,
                    mi.title, mi.production_year
             FROM media_sources ms
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             JOIN media_items mi ON mi.id = ms.item_id
             WHERE ms.item_id = ? AND mi.removed_at IS NULL AND fe.is_missing = 0
             ORDER BY ms.is_default DESC, ms.id
             LIMIT 1",
        )
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredItemSourceLocator {
                item_id: row.get("item_id"),
                root_path: row.get("canonical_path"),
                relative_path: row.get("relative_path"),
                fingerprint: row.get("fingerprint"),
                size: row.get("size"),
                modified_at: row.get("modified_at"),
                title: row.get("title"),
                production_year: row.get("production_year"),
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_item_by_source_locator(
        &self,
        root_path: &str,
        relative_path: &str,
    ) -> Result<Option<StoredItemSourceLocator>, StorageError> {
        self.query(
            "SELECT ms.item_id, lr.canonical_path, fe.relative_path,
                    fe.fingerprint, fe.size, fe.modified_at,
                    mi.title, mi.production_year
             FROM media_sources ms
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             JOIN media_items mi ON mi.id = ms.item_id
             WHERE lr.canonical_path = ? AND fe.relative_path = ?
               AND mi.removed_at IS NULL AND fe.is_missing = 0
             ORDER BY ms.is_default DESC, ms.id
             LIMIT 1",
        )
        .bind(root_path)
        .bind(relative_path)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredItemSourceLocator {
                item_id: row.get("item_id"),
                root_path: row.get("canonical_path"),
                relative_path: row.get("relative_path"),
                fingerprint: row.get("fingerprint"),
                size: row.get("size"),
                modified_at: row.get("modified_at"),
                title: row.get("title"),
                production_year: row.get("production_year"),
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_items_by_source_fingerprint(
        &self,
        fingerprint: &[u8],
    ) -> Result<Vec<StoredItemSourceLocator>, StorageError> {
        self.query(
            "SELECT ms.item_id, lr.canonical_path, fe.relative_path,
                    fe.fingerprint, fe.size, fe.modified_at,
                    mi.title, mi.production_year
             FROM media_sources ms
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             JOIN media_items mi ON mi.id = ms.item_id
             WHERE fe.fingerprint = ?
               AND mi.removed_at IS NULL AND fe.is_missing = 0
             ORDER BY ms.is_default DESC, ms.id",
        )
        .bind(fingerprint)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredItemSourceLocator {
                    item_id: row.get("item_id"),
                    root_path: row.get("canonical_path"),
                    relative_path: row.get("relative_path"),
                    fingerprint: row.get("fingerprint"),
                    size: row.get("size"),
                    modified_at: row.get("modified_at"),
                    title: row.get("title"),
                    production_year: row.get("production_year"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_item_scraper_id(
        &self,
        item_id: &str,
    ) -> Result<Option<String>, StorageError> {
        let value = self
            .query_scalar::<String>(
                "SELECT COALESCE(l.scraper_id, '')
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
            })?;
        Ok(value.filter(|value| !value.trim().is_empty()))
    }

    #[cfg(test)]
    pub(crate) async fn replace_person_credits(
        &self,
        item_id: &str,
        credits: &[NewPersonCredit],
    ) -> Result<(), StorageError> {
        self.replace_person_credits_with_fingerprint(item_id, credits, None)
            .await
    }

    pub(crate) async fn replace_person_credits_with_fingerprint(
        &self,
        item_id: &str,
        credits: &[NewPersonCredit],
        source_fingerprint: Option<&str>,
    ) -> Result<(), StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.query("DELETE FROM person_credits WHERE item_id = ?")
            .bind(item_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        for credit in credits {
            let provider_ids_json = serde_json::to_string(&credit.provider_ids)
                .map_err(|source| StorageError::Serialization(source.to_string()))?;
            let genres_json = serde_json::to_string(&credit.genres)
                .map_err(|source| StorageError::Serialization(source.to_string()))?;
            let tags_json = serde_json::to_string(&credit.tags)
                .map_err(|source| StorageError::Serialization(source.to_string()))?;
            let production_locations_json = serde_json::to_string(&credit.production_locations)
                .map_err(|source| StorageError::Serialization(source.to_string()))?;
            let taglines_json = serde_json::to_string(&credit.taglines)
                .map_err(|source| StorageError::Serialization(source.to_string()))?;
            self.query(
                "INSERT INTO person_credits (
                    item_id, person_id, person_type, person_name, provider, role,
                    sort_order, biography, birthday, deathday, known_for_department,
                    place_of_birth, provider_ids_json, genres_json, tags_json,
                    production_locations_json, premiere_date, production_year, taglines_json
                    , lux_person_id
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(item_id)
            .bind(&credit.person_id)
            .bind(&credit.person_type)
            .bind(&credit.person_name)
            .bind(&credit.provider)
            .bind(&credit.role)
            .bind(credit.sort_order)
            .bind(&credit.biography)
            .bind(&credit.birthday)
            .bind(&credit.deathday)
            .bind(&credit.known_for_department)
            .bind(&credit.place_of_birth)
            .bind(provider_ids_json)
            .bind(genres_json)
            .bind(tags_json)
            .bind(production_locations_json)
            .bind(&credit.premiere_date)
            .bind(credit.production_year)
            .bind(taglines_json)
            .bind(&credit.lux_person_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        self.query(
            "INSERT INTO person_index_item_state (
                item_id, source_fingerprint, relation_schema_version, updated_at
             ) VALUES (?, ?, 2, unixepoch())
             ON CONFLICT(item_id) DO UPDATE SET
                source_fingerprint = excluded.source_fingerprint,
                relation_schema_version = excluded.relation_schema_version,
                updated_at = excluded.updated_at",
        )
        .bind(item_id)
        .bind(source_fingerprint)
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
        Ok(())
    }

    pub(crate) async fn clear_person_credits(&self, item_id: &str) -> Result<u64, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let result = self
            .query("DELETE FROM person_credits WHERE item_id = ?")
            .bind(item_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.query("DELETE FROM person_index_item_state WHERE item_id = ?")
            .bind(item_id)
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

    pub(crate) async fn resolve_or_create_canonical_person(
        &self,
        display_name: &str,
        provider: &str,
        provider_id: &str,
        match_method: &str,
        confidence: Option<f64>,
        evidence_json: &str,
    ) -> Result<StoredCanonicalPerson, StorageError> {
        let now = current_unix_timestamp();
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;

        if let Some(person) = self
            .query(
                "SELECT p.id
                 FROM people p
                 JOIN person_identities pi ON pi.person_id = p.id
                 WHERE pi.provider = ? AND pi.provider_id = ?",
            )
            .bind(provider)
            .bind(provider_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
        {
            let stored = stored_canonical_person(person);
            transaction
                .commit()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(stored);
        }

        let person_id = loop {
            let sequence: i64 = self
                .query_scalar("INSERT INTO person_id_sequence DEFAULT VALUES RETURNING id")
                .fetch_one(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            let candidate = format!("lux-{sequence:06}");
            let exists: Option<i64> = self
                .query_scalar("SELECT 1 FROM people WHERE id = ?")
                .bind(&candidate)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if exists.is_none() {
                break candidate;
            }
        };
        self.query(
            "INSERT INTO people (
                id, display_name, directory_name, normalized_name, status, created_at, updated_at
             ) VALUES (?, ?, ?, ?, 'ACTIVE', ?, ?)",
        )
        .bind(&person_id)
        .bind(display_name)
        .bind(display_name)
        .bind(normalize_person_name(display_name))
        .bind(now)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query(
            "INSERT INTO person_identities (
                person_id, provider, provider_id, match_method, confidence,
                evidence_json, created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(provider, provider_id) DO NOTHING",
        )
        .bind(&person_id)
        .bind(provider)
        .bind(provider_id)
        .bind(match_method)
        .bind(confidence)
        .bind(evidence_json)
        .bind(now)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;

        let person = self
            .query(
                "SELECT p.id
                 FROM people p
                 JOIN person_identities pi ON pi.person_id = p.id
                 WHERE pi.provider = ? AND pi.provider_id = ?",
            )
            .bind(provider)
            .bind(provider_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let stored = stored_canonical_person(person);
        if stored.id != person_id {
            self.query("DELETE FROM people WHERE id = ?")
                .bind(&person_id)
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
        Ok(stored)
    }

    pub(crate) async fn find_canonical_person_by_identity(
        &self,
        provider: &str,
        provider_id: &str,
    ) -> Result<Option<StoredCanonicalPerson>, StorageError> {
        self.query(
            "SELECT p.id
             FROM people p
             JOIN person_identities pi ON pi.person_id = p.id
             WHERE pi.provider = ? AND pi.provider_id = ?",
        )
        .bind(provider)
        .bind(provider_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_canonical_person))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_canonical_person_display_name(
        &self,
        person_id: &str,
    ) -> Result<Option<String>, StorageError> {
        self.query_scalar::<String>("SELECT display_name FROM people WHERE id = ?")
            .bind(person_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_canonical_people_by_normalized_name(
        &self,
        normalized_name: &str,
    ) -> Result<Vec<StoredCanonicalPersonMatch>, StorageError> {
        let rows = self
            .query(
                "SELECT p.id, p.display_name, pc.birthday
                 FROM people p
                 LEFT JOIN person_credits pc
                   ON pc.lux_person_id = p.id AND pc.person_type = 'Actor'
                 WHERE p.status = 'ACTIVE'
                 ORDER BY p.id, pc.birthday",
            )
            .bind(normalized_name)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let mut matches = Vec::<StoredCanonicalPersonMatch>::new();
        for row in rows {
            let id: String = row.get("id");
            let display_name: String = row.get("display_name");
            if normalize_person_name(&display_name) != normalized_name {
                continue;
            }
            let birthday: Option<String> = row.try_get("birthday").ok();
            if let Some(existing) = matches.iter_mut().find(|candidate| candidate.id == id) {
                if let Some(birthday) = birthday.filter(|value| !value.trim().is_empty())
                    && !existing.birthdays.iter().any(|value| value == &birthday)
                {
                    existing.birthdays.push(birthday);
                }
            } else {
                matches.push(StoredCanonicalPersonMatch {
                    id,
                    birthdays: birthday
                        .filter(|value| !value.trim().is_empty())
                        .into_iter()
                        .collect(),
                });
            }
        }
        Ok(matches)
    }

    pub(crate) async fn enqueue_person_match_candidate(
        &self,
        item_id: &str,
        provider: &str,
        provider_id: &str,
        candidate_person_ids_json: &str,
        score: Option<f64>,
        evidence_json: &str,
    ) -> Result<String, StorageError> {
        let now = current_unix_timestamp();
        let candidate_id = Uuid::now_v7().to_string();
        self.query(
            "INSERT INTO person_match_candidates (
                id, item_id, provider, provider_id, candidate_person_ids_json,
                status, score, evidence_json, created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, 'PENDING', ?, ?, ?, ?)
             ON CONFLICT(item_id, provider, provider_id) DO UPDATE SET
                candidate_person_ids_json = excluded.candidate_person_ids_json,
                status = CASE
                    WHEN person_match_candidates.status IN ('CONFIRMED', 'REJECTED')
                        THEN person_match_candidates.status
                    ELSE excluded.status
                END,
                score = excluded.score,
                evidence_json = excluded.evidence_json,
                updated_at = excluded.updated_at",
        )
        .bind(candidate_id)
        .bind(item_id)
        .bind(provider)
        .bind(provider_id)
        .bind(candidate_person_ids_json)
        .bind(score)
        .bind(evidence_json)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query_scalar::<String>(
            "SELECT id FROM person_match_candidates
             WHERE item_id = ? AND provider = ? AND provider_id = ?",
        )
        .bind(item_id)
        .bind(provider)
        .bind(provider_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn restore_person_match_candidate(
        &self,
        restore: &PersonMatchCandidateRestore<'_>,
    ) -> Result<String, StorageError> {
        if !matches!(restore.status, "PENDING" | "CONFIRMED" | "REJECTED") {
            return Err(StorageError::Serialization(
                "invalid person match candidate status".to_owned(),
            ));
        }
        self.query(
            "INSERT INTO person_match_candidates (
                id, item_id, provider, provider_id, candidate_person_ids_json,
                status, score, evidence_json, target_person_id, previous_person_id,
                created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(item_id, provider, provider_id) DO UPDATE SET
                candidate_person_ids_json = excluded.candidate_person_ids_json,
                status = CASE
                    WHEN person_match_candidates.status IN ('CONFIRMED', 'REJECTED')
                        AND excluded.status = 'PENDING'
                        THEN person_match_candidates.status
                    ELSE excluded.status
                END,
                score = excluded.score,
                evidence_json = excluded.evidence_json,
                target_person_id = COALESCE(excluded.target_person_id, person_match_candidates.target_person_id),
                previous_person_id = COALESCE(excluded.previous_person_id, person_match_candidates.previous_person_id),
                updated_at = excluded.updated_at",
        )
        .bind(restore.candidate_id)
        .bind(restore.item_id)
        .bind(restore.provider)
        .bind(restore.provider_id)
        .bind(restore.candidate_person_ids_json)
        .bind(restore.status)
        .bind(restore.score)
        .bind(restore.evidence_json)
        .bind(restore.target_person_id)
        .bind(restore.previous_person_id)
        .bind(restore.created_at)
        .bind(restore.updated_at)
        .execute(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query_scalar::<String>(
            "SELECT id FROM person_match_candidates
             WHERE item_id = ? AND provider = ? AND provider_id = ?",
        )
        .bind(restore.item_id)
        .bind(restore.provider)
        .bind(restore.provider_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn count_pending_person_match_candidates(&self) -> Result<i64, StorageError> {
        self.query_scalar("SELECT COUNT(*) FROM person_match_candidates WHERE status = 'PENDING'")
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_pending_person_match_candidates(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredPersonMatchCandidate>, StorageError> {
        self.query(
            "SELECT id, item_id, provider, provider_id,
                    candidate_person_ids_json, status, score,
                    evidence_json, target_person_id, previous_person_id,
                    created_at, updated_at
             FROM person_match_candidates
             WHERE status = 'PENDING'
             ORDER BY updated_at DESC, id DESC
             LIMIT ? OFFSET ?",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(stored_person_match_candidate)
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_person_match_candidate(
        &self,
        candidate_id: &str,
    ) -> Result<Option<StoredPersonMatchCandidate>, StorageError> {
        self.query(
            "SELECT id, item_id, provider, provider_id,
                    candidate_person_ids_json, status, score,
                    evidence_json, target_person_id, previous_person_id,
                    created_at, updated_at
             FROM person_match_candidates WHERE id = ?",
        )
        .bind(candidate_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_person_match_candidate))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn reject_person_match_candidate(
        &self,
        candidate_id: &str,
        evidence_json: &str,
    ) -> Result<(), StorageError> {
        let result = self
            .query(
                "UPDATE person_match_candidates
                 SET status = 'REJECTED', evidence_json = ?, updated_at = ?
                 WHERE id = ? AND status = 'PENDING'",
            )
            .bind(evidence_json)
            .bind(current_unix_timestamp())
            .bind(candidate_id)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if result.rows_affected() == 0 {
            return Err(StorageError::Conflict(format!(
                "person match candidate '{candidate_id}' is missing or not pending"
            )));
        }
        Ok(())
    }

    pub(crate) async fn confirm_person_match_candidate(
        &self,
        candidate_id: &str,
        target_person_id: &str,
        evidence_json: &str,
    ) -> Result<StoredPersonIdentityMove, StorageError> {
        let now = current_unix_timestamp();
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let candidate = self
            .query(
                "SELECT id, item_id, provider, provider_id,
                        candidate_person_ids_json, status, score,
                        evidence_json, target_person_id, previous_person_id,
                        created_at, updated_at
                 FROM person_match_candidates WHERE id = ?",
            )
            .bind(candidate_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .map(stored_person_match_candidate)
            .ok_or_else(|| {
                StorageError::Conflict(format!("person match candidate '{candidate_id}' not found"))
            })?;
        if candidate.status != "PENDING" {
            return Err(StorageError::Conflict(format!(
                "person match candidate '{candidate_id}' is {}",
                candidate.status
            )));
        }
        let candidate_person_ids =
            serde_json::from_str::<Vec<String>>(&candidate.candidate_person_ids_json)
                .map_err(|source| StorageError::Serialization(source.to_string()))?;
        if !candidate_person_ids
            .iter()
            .any(|person_id| person_id == target_person_id)
        {
            return Err(StorageError::Conflict(
                "selected person is not one of the candidate matches".to_owned(),
            ));
        }
        let target_exists = self
            .query_scalar::<String>("SELECT id FROM people WHERE id = ?")
            .bind(target_person_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .is_some();
        if !target_exists {
            return Err(StorageError::Conflict(format!(
                "canonical person '{target_person_id}' does not exist"
            )));
        }
        let previous_person_id = self
            .query_scalar::<String>(
                "SELECT person_id FROM person_identities
                 WHERE provider = ? AND provider_id = ?",
            )
            .bind(&candidate.provider)
            .bind(&candidate.provider_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if previous_person_id.as_deref() != Some(target_person_id) {
            self.query(
                "DELETE FROM person_identities
                 WHERE provider = ? AND provider_id = ?",
            )
            .bind(&candidate.provider)
            .bind(&candidate.provider_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
            self.query(
                "INSERT INTO person_identities (
                    person_id, provider, provider_id, match_method, confidence,
                    evidence_json, created_at, updated_at
                 ) VALUES (?, ?, ?, 'MANUAL_CONFIRM', ?, ?, ?, ?)",
            )
            .bind(target_person_id)
            .bind(&candidate.provider)
            .bind(&candidate.provider_id)
            .bind(Some(1.0_f64))
            .bind(evidence_json)
            .bind(now)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
            self.query(
                "UPDATE person_credits SET lux_person_id = ?
                 WHERE provider = ? AND person_id = ?",
            )
            .bind(target_person_id)
            .bind(&candidate.provider)
            .bind(&candidate.provider_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        self.query(
            "UPDATE person_match_candidates
             SET status = 'CONFIRMED', evidence_json = ?,
                 target_person_id = ?, previous_person_id = ?, updated_at = ?
             WHERE id = ?",
        )
        .bind(evidence_json)
        .bind(target_person_id)
        .bind(previous_person_id.as_deref())
        .bind(now)
        .bind(candidate_id)
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
        Ok(StoredPersonIdentityMove { previous_person_id })
    }

    pub(crate) async fn undo_person_match_candidate(
        &self,
        candidate_id: &str,
        evidence_json: &str,
    ) -> Result<StoredPersonIdentityMove, StorageError> {
        let now = current_unix_timestamp();
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let candidate = self
            .query(
                "SELECT id, item_id, provider, provider_id,
                        candidate_person_ids_json, status, score,
                        evidence_json, target_person_id, previous_person_id,
                        created_at, updated_at
                 FROM person_match_candidates WHERE id = ?",
            )
            .bind(candidate_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .map(stored_person_match_candidate)
            .ok_or_else(|| {
                StorageError::Conflict(format!("person match candidate '{candidate_id}' not found"))
            })?;
        if candidate.status != "CONFIRMED" {
            return Err(StorageError::Conflict(format!(
                "person match candidate '{candidate_id}' is {}",
                candidate.status
            )));
        }
        let target_person_id = candidate.target_person_id.ok_or_else(|| {
            StorageError::Conflict(
                "confirmed person match has no recorded target identity".to_owned(),
            )
        })?;
        let current_owner = self
            .query_scalar::<String>(
                "SELECT person_id FROM person_identities
                 WHERE provider = ? AND provider_id = ?",
            )
            .bind(&candidate.provider)
            .bind(&candidate.provider_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if current_owner.as_deref() != Some(target_person_id.as_str()) {
            return Err(StorageError::Conflict(
                "provider identity no longer belongs to the confirmed target".to_owned(),
            ));
        }
        let previous_person_id = candidate.previous_person_id;
        if let Some(previous_person_id) = previous_person_id.as_deref() {
            let previous_exists = self
                .query_scalar::<String>("SELECT id FROM people WHERE id = ?")
                .bind(previous_person_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
                .is_some();
            if !previous_exists {
                return Err(StorageError::Conflict(
                    "previous canonical person no longer exists".to_owned(),
                ));
            }
        }
        self.query(
            "DELETE FROM person_identities
             WHERE provider = ? AND provider_id = ?",
        )
        .bind(&candidate.provider)
        .bind(&candidate.provider_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        if let Some(previous_person_id) = previous_person_id.as_deref() {
            self.query(
                "INSERT INTO person_identities (
                    person_id, provider, provider_id, match_method, confidence,
                    evidence_json, created_at, updated_at
                 ) VALUES (?, ?, ?, 'MANUAL_UNDO', ?, ?, ?, ?)",
            )
            .bind(previous_person_id)
            .bind(&candidate.provider)
            .bind(&candidate.provider_id)
            .bind(Some(1.0_f64))
            .bind(evidence_json)
            .bind(now)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        self.query(
            "UPDATE person_credits SET lux_person_id = ?
             WHERE provider = ? AND person_id = ?",
        )
        .bind(previous_person_id.as_deref())
        .bind(&candidate.provider)
        .bind(&candidate.provider_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query(
            "UPDATE person_match_candidates
             SET status = 'REJECTED', evidence_json = ?, updated_at = ?
             WHERE id = ? AND status = 'CONFIRMED'",
        )
        .bind(evidence_json)
        .bind(now)
        .bind(candidate_id)
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
        Ok(StoredPersonIdentityMove { previous_person_id })
    }

    pub(crate) async fn split_canonical_person_identity(
        &self,
        source_person_id: &str,
        provider: &str,
        provider_id: &str,
        display_name: &str,
        evidence_json: &str,
    ) -> Result<StoredCanonicalPerson, StorageError> {
        let now = current_unix_timestamp();
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let owner = self
            .query_scalar::<String>(
                "SELECT person_id FROM person_identities
                 WHERE provider = ? AND provider_id = ?",
            )
            .bind(provider)
            .bind(provider_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .ok_or_else(|| {
                StorageError::Conflict(format!(
                    "provider identity '{provider}:{provider_id}' does not exist"
                ))
            })?;
        if owner != source_person_id {
            return Err(StorageError::Conflict(format!(
                "provider identity '{provider}:{provider_id}' belongs to '{owner}'"
            )));
        }
        let new_person_id = loop {
            let sequence: i64 = self
                .query_scalar("INSERT INTO person_id_sequence DEFAULT VALUES RETURNING id")
                .fetch_one(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            let candidate = format!("lux-{sequence:06}");
            let exists: Option<i64> = self
                .query_scalar("SELECT 1 FROM people WHERE id = ?")
                .bind(&candidate)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if exists.is_none() {
                break candidate;
            }
        };
        self.query(
            "INSERT INTO people (
                id, display_name, directory_name, normalized_name, status, created_at, updated_at
             ) VALUES (?, ?, ?, ?, 'ACTIVE', ?, ?)",
        )
        .bind(&new_person_id)
        .bind(display_name)
        .bind(display_name)
        .bind(normalize_person_name(display_name))
        .bind(now)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query("DELETE FROM person_identities WHERE provider = ? AND provider_id = ?")
            .bind(provider)
            .bind(provider_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.query(
            "INSERT INTO person_identities (
                person_id, provider, provider_id, match_method, confidence,
                evidence_json, created_at, updated_at
             ) VALUES (?, ?, ?, 'MANUAL_SPLIT', ?, ?, ?, ?)",
        )
        .bind(&new_person_id)
        .bind(provider)
        .bind(provider_id)
        .bind(Some(1.0_f64))
        .bind(evidence_json)
        .bind(now)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query(
            "UPDATE person_credits SET lux_person_id = ?
             WHERE provider = ? AND person_id = ?",
        )
        .bind(&new_person_id)
        .bind(provider)
        .bind(provider_id)
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
        Ok(StoredCanonicalPerson { id: new_person_id })
    }

    pub(crate) async fn restore_canonical_person(
        &self,
        person_id: &str,
        display_name: &str,
        identities: &[(&str, &str)],
    ) -> Result<StoredCanonicalPerson, StorageError> {
        let now = current_unix_timestamp();
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        for (provider, provider_id) in identities {
            let owner = self
                .query_scalar::<String>(
                    "SELECT person_id FROM person_identities
                     WHERE provider = ? AND provider_id = ?",
                )
                .bind(provider)
                .bind(provider_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if let Some(owner) = owner
                && owner != person_id
            {
                return Err(StorageError::Conflict(format!(
                    "provider identity '{provider}:{provider_id}' belongs to '{owner}'"
                )));
            }
        }
        self.query(
            "INSERT INTO people (
                id, display_name, directory_name, normalized_name, status, created_at, updated_at
             ) VALUES (?, ?, ?, ?, 'ACTIVE', ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                display_name = excluded.display_name,
                directory_name = excluded.directory_name,
                normalized_name = excluded.normalized_name,
                updated_at = excluded.updated_at",
        )
        .bind(person_id)
        .bind(display_name)
        .bind(display_name)
        .bind(normalize_person_name(display_name))
        .bind(now)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        if let Some(sequence) = person_id
            .strip_prefix("lux-")
            .and_then(|value| value.parse::<i64>().ok())
        {
            self.query(
                "INSERT INTO person_id_sequence (id) VALUES (?)
                 ON CONFLICT(id) DO NOTHING",
            )
            .bind(sequence)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
            if self.backend == DatabaseBackend::Postgres {
                self.query(
                    "SELECT setval(
                        pg_get_serial_sequence('person_id_sequence', 'id'),
                        COALESCE((SELECT MAX(id) FROM person_id_sequence), 1),
                        true
                    )",
                )
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            }
        }
        for (provider, provider_id) in identities {
            self.query(
                "INSERT INTO person_identities (
                    person_id, provider, provider_id, match_method, confidence,
                    evidence_json, created_at, updated_at
                 ) VALUES (?, ?, ?, 'RECOVERED_MANIFEST', ?, ?, ?, ?)
                 ON CONFLICT(provider, provider_id) DO NOTHING",
            )
            .bind(person_id)
            .bind(provider)
            .bind(provider_id)
            .bind(Some(1.0_f64))
            .bind(r#"{"method":"person-manifest"}"#)
            .bind(now)
            .bind(now)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        let row = self
            .query("SELECT id FROM people WHERE id = ?")
            .bind(person_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let stored = stored_canonical_person(row);
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(stored)
    }

    pub(crate) async fn attach_canonical_person_identity(
        &self,
        person_id: &str,
        provider: &str,
        provider_id: &str,
        match_method: &str,
        confidence: Option<f64>,
        evidence_json: &str,
    ) -> Result<StoredCanonicalPerson, StorageError> {
        let now = current_unix_timestamp();
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let person = self
            .query(
                "SELECT id
                 FROM people WHERE id = ?",
            )
            .bind(person_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .ok_or_else(|| {
                StorageError::Conflict(format!("canonical person '{person_id}' does not exist"))
            })?;

        self.query(
            "INSERT INTO person_identities (
                person_id, provider, provider_id, match_method, confidence,
                evidence_json, created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(provider, provider_id) DO NOTHING",
        )
        .bind(person_id)
        .bind(provider)
        .bind(provider_id)
        .bind(match_method)
        .bind(confidence)
        .bind(evidence_json)
        .bind(now)
        .bind(now)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;

        let owner_id: String = self
            .query_scalar(
                "SELECT person_id FROM person_identities
                 WHERE provider = ? AND provider_id = ?",
            )
            .bind(provider)
            .bind(provider_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if owner_id != person_id {
            return Err(StorageError::Conflict(format!(
                "provider identity '{provider}:{provider_id}' belongs to '{owner_id}'"
            )));
        }
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(stored_canonical_person(person))
    }

    pub(crate) async fn list_person_credit_item_ids(
        &self,
        library_ids: &[String],
        person_type: &str,
        person_id: &str,
    ) -> Result<Vec<String>, StorageError> {
        if library_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat_n("?", library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "SELECT DISTINCT pc.item_id
             FROM person_credits pc
             JOIN media_items mi ON mi.id = pc.item_id
             LEFT JOIN person_identities pi
               ON pi.provider = pc.provider
              AND pi.provider_id = pc.person_id
             WHERE mi.library_id IN ({placeholders})
               AND mi.removed_at IS NULL
               AND pc.person_type = ?
               AND (
                   pc.person_id = ?
                   OR pc.lux_person_id = ?
                   OR pi.person_id = ?
               )
             ORDER BY pc.item_id"
        );
        let mut statement = self.query(sqlx::AssertSqlSafe(query));
        for library_id in library_ids {
            statement = statement.bind(library_id);
        }
        let rows = statement
            .bind(person_type)
            .bind(person_id)
            .bind(person_id)
            .bind(person_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(rows.into_iter().map(|row| row.get("item_id")).collect())
    }

    pub(crate) async fn list_person_credits_for_library(
        &self,
        library_id: &str,
        person_type: &str,
        options: PersonListOptions,
    ) -> Result<(Vec<StoredPersonCredit>, i64), StorageError> {
        self.list_person_credits_for_libraries(&[library_id.to_owned()], person_type, options)
            .await
    }

    pub(crate) async fn list_person_credits_for_libraries(
        &self,
        library_ids: &[String],
        person_type: &str,
        options: PersonListOptions,
    ) -> Result<(Vec<StoredPersonCredit>, i64), StorageError> {
        if library_ids.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let placeholders = std::iter::repeat_n("?", library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let person_group = "COALESCE(
            NULLIF(pc.lux_person_id, ''),
            NULLIF(pi.person_id, ''),
            pc.provider || ':' || pc.person_id
        )";
        let person_sort_order = person_sort_order(options.sort_by, options.descending);
        let recursive_clause = if options.recursive {
            String::new()
        } else {
            format!(" AND (mi.parent_id IS NULL OR mi.parent_id IN ({placeholders}))")
        };
        let count_query = format!(
            "SELECT COUNT(*) FROM (
                 SELECT {person_group}
                 FROM person_credits pc
                 JOIN media_items mi ON mi.id = pc.item_id
                 LEFT JOIN person_identities pi
                   ON pi.provider = pc.provider
                  AND pi.provider_id = pc.person_id
                 WHERE mi.library_id IN ({placeholders})
                   AND mi.removed_at IS NULL
                   {recursive_clause}
                   AND pc.person_type = ?
                 GROUP BY {person_group}
             )"
        );
        let mut count_statement = self.query_scalar::<i64>(sqlx::AssertSqlSafe(count_query));
        for library_id in library_ids {
            count_statement = count_statement.bind(library_id);
        }
        if !options.recursive {
            for library_id in library_ids {
                count_statement = count_statement.bind(library_id);
            }
        }
        let total: i64 = count_statement
            .bind(person_type)
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let list_query = format!(
            "SELECT MIN(pc.item_id) AS item_id,
                    MIN(pc.person_id) AS person_id,
                    MIN(pc.lux_person_id) AS lux_person_id,
                    MIN(pc.provider) AS provider,
                    MIN(pc.person_name) AS person_name,
                    MIN(pc.role) AS role,
                    MIN(mi.added_at) AS date_created,
                    MIN(pc.biography) AS biography,
                    MIN(pc.birthday) AS birthday,
                    MIN(pc.deathday) AS deathday,
                    MIN(pc.known_for_department) AS known_for_department,
                    MIN(pc.place_of_birth) AS place_of_birth,
                    MIN(pc.provider_ids_json) AS provider_ids_json,
                    MIN(pc.genres_json) AS genres_json,
                    MIN(pc.tags_json) AS tags_json,
                    MIN(pc.production_locations_json) AS production_locations_json,
                    MIN(pc.premiere_date) AS premiere_date,
                    MIN(pc.production_year) AS production_year,
                    MIN(pc.taglines_json) AS taglines_json
             FROM person_credits pc
             JOIN media_items mi ON mi.id = pc.item_id
             LEFT JOIN person_identities pi
               ON pi.provider = pc.provider
              AND pi.provider_id = pc.person_id
             WHERE mi.library_id IN ({placeholders})
               AND mi.removed_at IS NULL
               {recursive_clause}
               AND pc.person_type = ?
             GROUP BY {person_group}
             ORDER BY {person_sort_order}
             LIMIT ? OFFSET ?"
        );
        let mut list_statement = self.query(sqlx::AssertSqlSafe(list_query));
        for library_id in library_ids {
            list_statement = list_statement.bind(library_id);
        }
        if !options.recursive {
            for library_id in library_ids {
                list_statement = list_statement.bind(library_id);
            }
        }
        let rows = list_statement
            .bind(person_type)
            .bind(options.limit)
            .bind(options.offset)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .into_iter()
            .map(stored_person_credit)
            .collect();
        Ok((rows, total))
    }

    pub(crate) async fn find_person_credits_for_libraries(
        &self,
        library_ids: &[String],
        person_type: &str,
        person_id: &str,
    ) -> Result<Vec<StoredPersonCredit>, StorageError> {
        if library_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat_n("?", library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let person_group = "COALESCE(
            NULLIF(pc.lux_person_id, ''),
            NULLIF(pi.person_id, ''),
            pc.provider || ':' || pc.person_id
        )";
        let query = format!(
            "SELECT MIN(pc.item_id) AS item_id,
                    MIN(pc.person_id) AS person_id,
                    MIN(pc.lux_person_id) AS lux_person_id,
                    MIN(pc.provider) AS provider,
                    MIN(pc.person_name) AS person_name,
                    MIN(pc.role) AS role,
                    MIN(mi.added_at) AS date_created,
                    MIN(pc.biography) AS biography,
                    MIN(pc.birthday) AS birthday,
                    MIN(pc.deathday) AS deathday,
                    MIN(pc.known_for_department) AS known_for_department,
                    MIN(pc.place_of_birth) AS place_of_birth,
                    MIN(pc.provider_ids_json) AS provider_ids_json,
                    MIN(pc.genres_json) AS genres_json,
                    MIN(pc.tags_json) AS tags_json,
                    MIN(pc.production_locations_json) AS production_locations_json,
                    MIN(pc.premiere_date) AS premiere_date,
                    MIN(pc.production_year) AS production_year,
                    MIN(pc.taglines_json) AS taglines_json
             FROM person_credits pc
             JOIN media_items mi ON mi.id = pc.item_id
             LEFT JOIN person_identities pi
               ON pi.provider = pc.provider
              AND pi.provider_id = pc.person_id
             WHERE mi.library_id IN ({placeholders})
               AND mi.removed_at IS NULL
               AND pc.person_type = ?
               AND (
                   pc.person_id = ?
                   OR pc.lux_person_id = ?
                   OR pi.person_id = ?
               )
             GROUP BY {person_group}
             ORDER BY CASE WHEN MIN(pc.provider) = '' THEN 1 ELSE 0 END,
                      MIN(pc.provider) ASC,
                      MIN(pc.person_id) ASC"
        );
        let mut statement = self.query(sqlx::AssertSqlSafe(query));
        for library_id in library_ids {
            statement = statement.bind(library_id);
        }
        let rows = statement
            .bind(person_type)
            .bind(person_id)
            .bind(person_id)
            .bind(person_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .into_iter()
            .map(stored_person_credit)
            .collect();
        Ok(rows)
    }

    pub(crate) async fn find_person_credits_for_libraries_by_name(
        &self,
        library_ids: &[String],
        person_type: &str,
        person_name: &str,
    ) -> Result<Vec<StoredPersonCredit>, StorageError> {
        if library_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat_n("?", library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let person_group = "COALESCE(
            NULLIF(pc.lux_person_id, ''),
            NULLIF(pi.person_id, ''),
            pc.provider || ':' || pc.person_id
        )";
        let query = format!(
            "SELECT MIN(pc.item_id) AS item_id,
                    MIN(pc.person_id) AS person_id,
                    MIN(pc.lux_person_id) AS lux_person_id,
                    MIN(pc.provider) AS provider,
                    MIN(pc.person_name) AS person_name,
                    MIN(pc.role) AS role,
                    MIN(mi.added_at) AS date_created,
                    MIN(pc.biography) AS biography,
                    MIN(pc.birthday) AS birthday,
                    MIN(pc.deathday) AS deathday,
                    MIN(pc.known_for_department) AS known_for_department,
                    MIN(pc.place_of_birth) AS place_of_birth,
                    MIN(pc.provider_ids_json) AS provider_ids_json,
                    MIN(pc.genres_json) AS genres_json,
                    MIN(pc.tags_json) AS tags_json,
                    MIN(pc.production_locations_json) AS production_locations_json,
                    MIN(pc.premiere_date) AS premiere_date,
                    MIN(pc.production_year) AS production_year,
                    MIN(pc.taglines_json) AS taglines_json
             FROM person_credits pc
             JOIN media_items mi ON mi.id = pc.item_id
             LEFT JOIN person_identities pi
               ON pi.provider = pc.provider
              AND pi.provider_id = pc.person_id
             WHERE mi.library_id IN ({placeholders})
               AND mi.removed_at IS NULL
               AND pc.person_type = ?
               AND pc.person_name = ?
             GROUP BY {person_group}
             ORDER BY CASE WHEN MIN(pc.provider) = '' THEN 1 ELSE 0 END,
                      MIN(pc.provider) ASC,
                      MIN(pc.person_id) ASC"
        );
        let mut statement = self.query(sqlx::AssertSqlSafe(query));
        for library_id in library_ids {
            statement = statement.bind(library_id);
        }
        let rows = statement
            .bind(person_type)
            .bind(person_name)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .into_iter()
            .map(stored_person_credit)
            .collect();
        Ok(rows)
    }

    pub(crate) async fn list_media_item_ids_for_library(
        &self,
        library_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<String>, StorageError> {
        self.query_scalar(
            "SELECT id FROM media_items
             WHERE library_id = ? AND removed_at IS NULL
               AND item_type IN ('MOVIE', 'SERIES', 'SEASON', 'EPISODE')
             ORDER BY CASE item_type
                          WHEN 'SERIES' THEN 0
                          WHEN 'SEASON' THEN 1
                          WHEN 'EPISODE' THEN 2
                          ELSE 3
                      END, id
             LIMIT ? OFFSET ?",
        )
        .bind(library_id)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn insert_library(&self, library: NewLibrary<'_>) -> Result<(), StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.query(
            "INSERT INTO libraries (
                id, name, kind, is_enabled, realtime_watch_enabled,
                realtime_metadata_auto_match_enabled,
                reconciliation_schedule, metadata_schedule,
                scan_concurrency, probe_concurrency, scraper_id, chapter_source_id
            ) VALUES (?, ?, ?, 1, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(library.id)
        .bind(library.name)
        .bind(library.kind)
        .bind(database_flag(library.realtime_watch_enabled))
        .bind(database_flag(library.realtime_metadata_auto_match_enabled))
        .bind(library.reconciliation_schedule)
        .bind(library.metadata_schedule)
        .bind(library.scan_concurrency)
        .bind(library.probe_concurrency)
        .bind(library.scraper_id)
        .bind(library.chapter_source_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;

        let registrations = [
            (
                "RECONCILIATION_SCAN",
                "全量校验媒体库",
                "按计划校验媒体库索引与文件系统的一致性。",
                "SYSTEM",
                None,
                library.reconciliation_schedule,
            ),
            (
                "METADATA_PARSE",
                "元数据刮削",
                "解析本地元数据，并在已配置时调用刮削插件补全内容。",
                if library.scraper_id.is_some() {
                    "PLUGIN"
                } else {
                    "SYSTEM"
                },
                library.scraper_id,
                library.metadata_schedule,
            ),
        ];
        for (task_type, task_name, task_description, source_type, plugin_id, schedule) in
            registrations
        {
            self.query(
                "INSERT INTO scheduled_task_configs (
                    owner_type, owner_id, task_type, task_name, task_description,
                    source_type, plugin_id, cron_or_interval, is_enabled, resource_limit_json
                ) VALUES ('LIBRARY', ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(library.id)
            .bind(task_type)
            .bind(task_name)
            .bind(task_description)
            .bind(source_type)
            .bind(plugin_id)
            .bind(schedule)
            .bind(database_flag(schedule.is_some()))
            .bind(if task_type == "RECONCILIATION_SCAN" {
                format!(
                    "{{\"scanConcurrency\":{},\"probeConcurrency\":{}}}",
                    library.scan_concurrency, library.probe_concurrency
                )
            } else {
                "{}".to_owned()
            })
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

    pub(crate) async fn list_person_index_item_ids(
        &self,
        library_id: &str,
        after_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<String>, StorageError> {
        let sql = if after_id.is_some() {
            "SELECT id FROM media_items
             WHERE library_id = ? AND removed_at IS NULL
               AND item_type IN ('MOVIE', 'SERIES', 'SEASON', 'EPISODE')
               AND id > ?
             ORDER BY id LIMIT ?"
        } else {
            "SELECT id FROM media_items
             WHERE library_id = ? AND removed_at IS NULL
               AND item_type IN ('MOVIE', 'SERIES', 'SEASON', 'EPISODE')
             ORDER BY id LIMIT ?"
        };
        let mut query = self.query_scalar::<String>(sql).bind(library_id);
        if let Some(after_id) = after_id {
            query = query.bind(after_id);
        }
        query
            .bind(limit)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn count_person_index_items(
        &self,
        library_id: &str,
    ) -> Result<i64, StorageError> {
        self.query_scalar(
            "SELECT COUNT(*) FROM media_items
             WHERE library_id = ? AND removed_at IS NULL
               AND item_type IN ('MOVIE', 'SERIES', 'SEASON', 'EPISODE')",
        )
        .bind(library_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn person_index_item_state_is_current(
        &self,
        item_id: &str,
        source_fingerprint: Option<&str>,
    ) -> Result<bool, StorageError> {
        let Some(source_fingerprint) = source_fingerprint else {
            return Ok(false);
        };
        let row = self
            .query(
                "SELECT source_fingerprint, relation_schema_version
                 FROM person_index_item_state WHERE item_id = ?",
            )
            .bind(item_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(row.is_some_and(|row| {
            row.get::<Option<String>, _>("source_fingerprint")
                .as_deref()
                == Some(source_fingerprint)
                && row.get::<i64, _>("relation_schema_version") == 2
        }))
    }

    pub(crate) async fn list_libraries(&self) -> Result<Vec<StoredLibrary>, StorageError> {
        self.query(
            "SELECT id, name, kind, is_enabled, realtime_watch_enabled,
                    realtime_metadata_auto_match_enabled,
                    incremental_schedule, reconciliation_schedule, metadata_schedule,
                    scan_concurrency, probe_concurrency, last_scan_at, scraper_id, chapter_source_id,
                    cover_image_path, cover_image_content_type, cover_image_size, cover_image_tag,
                    media_strategy_json
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
                    realtime_metadata_auto_match_enabled: row
                        .get::<i64, _>("realtime_metadata_auto_match_enabled")
                        != 0,
                    incremental_schedule: row.get("incremental_schedule"),
                    reconciliation_schedule: row.get("reconciliation_schedule"),
                    metadata_schedule: row.get("metadata_schedule"),
                    scan_concurrency: row.get("scan_concurrency"),
                    probe_concurrency: row.get("probe_concurrency"),
                    last_scan_at: row.get("last_scan_at"),
                    scraper_id: row.get("scraper_id"),
                    chapter_source_id: row.get("chapter_source_id"),
                    cover_image_path: row.get("cover_image_path"),
                    cover_image_content_type: row.get("cover_image_content_type"),
                    cover_image_size: row.get("cover_image_size"),
                    cover_image_tag: row.get("cover_image_tag"),
                    media_strategy_json: row.get("media_strategy_json"),
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
        self.query(
            "SELECT id, name, kind, is_enabled, realtime_watch_enabled,
                    realtime_metadata_auto_match_enabled,
                    incremental_schedule, reconciliation_schedule, metadata_schedule,
                    scan_concurrency, probe_concurrency, last_scan_at, scraper_id, chapter_source_id,
                    cover_image_path, cover_image_content_type, cover_image_size, cover_image_tag,
                    media_strategy_json
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
                realtime_metadata_auto_match_enabled: row
                    .get::<i64, _>("realtime_metadata_auto_match_enabled")
                    != 0,
                incremental_schedule: row.get("incremental_schedule"),
                reconciliation_schedule: row.get("reconciliation_schedule"),
                metadata_schedule: row.get("metadata_schedule"),
                scan_concurrency: row.get("scan_concurrency"),
                probe_concurrency: row.get("probe_concurrency"),
                last_scan_at: row.get("last_scan_at"),
                scraper_id: row.get("scraper_id"),
                chapter_source_id: row.get("chapter_source_id"),
                cover_image_path: row.get("cover_image_path"),
                cover_image_content_type: row.get("cover_image_content_type"),
                cover_image_size: row.get("cover_image_size"),
                cover_image_tag: row.get("cover_image_tag"),
                media_strategy_json: row.get("media_strategy_json"),
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn register_auto_library_cover_task(
        &self,
        library_id: &str,
    ) -> Result<bool, StorageError> {
        self.query(
            "INSERT INTO scheduled_task_configs (
                owner_type, owner_id, task_type, task_name, task_description,
                source_type, plugin_id, cron_or_interval, is_enabled, resource_limit_json
             ) VALUES (
                'LIBRARY', ?, 'AUTO_LIBRARY_COVER',
                '自动生成媒体库封面',
                '首次达到至少 9 张海报后，随机选择 9 张海报生成带媒体库名称的旋转堆叠封面；管理员可手动执行或按计划重跑。',
                'SYSTEM', NULL, NULL, 0, '{}'
             ) ON CONFLICT(owner_type, owner_id, task_type) DO UPDATE SET
                task_name = excluded.task_name,
                task_description = excluded.task_description,
                source_type = excluded.source_type,
                resource_limit_json = excluded.resource_limit_json,
                updated_at = unixepoch()
             ",
        )
        .bind(library_id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn create_library_cover_job(
        &self,
        id: &str,
        library_id: &str,
        is_manual: bool,
    ) -> Result<bool, StorageError> {
        self.query(
            "INSERT INTO library_cover_jobs (id, library_id, is_manual, status)
             VALUES (?, ?, ?, 'PENDING')
             ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .bind(library_id)
        .bind(database_flag(is_manual))
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_library_cover_job(
        &self,
        id: &str,
    ) -> Result<Option<StoredLibraryCoverJob>, StorageError> {
        self.query(
            "SELECT id, library_id, is_manual, status, processed_count, total_count,
                    error, created_at, updated_at, started_at, finished_at
             FROM library_cover_jobs WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_library_cover_job))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_library_cover_jobs(
        &self,
        status: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredLibraryCoverJob>, StorageError> {
        let limit = limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE);
        let query = if status.is_some() {
            self.query(
                "SELECT id, library_id, is_manual, status, processed_count, total_count,
                        error, created_at, updated_at, started_at, finished_at
                 FROM library_cover_jobs WHERE status = ?
                 ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
            )
            .bind(status)
            .bind(limit)
            .bind(offset.max(0))
        } else {
            self.query(
                "SELECT id, library_id, is_manual, status, processed_count, total_count,
                        error, created_at, updated_at, started_at, finished_at
                 FROM library_cover_jobs
                 ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
            )
            .bind(limit)
            .bind(offset.max(0))
        };
        query
            .fetch_all(&self.pool)
            .await
            .map(|rows| rows.into_iter().map(stored_library_cover_job).collect())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_active_library_cover_job_ids(
        &self,
    ) -> Result<Vec<String>, StorageError> {
        self.query_scalar(
            "SELECT id FROM library_cover_jobs
             WHERE status IN ('PENDING', 'RUNNING') ORDER BY created_at, id LIMIT 10000",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn has_active_library_cover_job(
        &self,
        library_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(
                SELECT 1 FROM library_cover_jobs
                WHERE library_id = ? AND status IN ('PENDING', 'RUNNING')
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

    pub(crate) async fn claim_library_cover_job(&self, id: &str) -> Result<bool, StorageError> {
        self.query(
            "UPDATE library_cover_jobs
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

    pub(crate) async fn update_library_cover_job_progress(
        &self,
        id: &str,
        processed_count: i64,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE library_cover_jobs
             SET processed_count = ?, updated_at = unixepoch()
             WHERE id = ? AND status = 'RUNNING'",
        )
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

    pub(crate) async fn finish_library_cover_job(
        &self,
        id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE library_cover_jobs
             SET status = ?, error = ?, processed_count = CASE
                    WHEN ? = 'COMPLETED' THEN total_count ELSE processed_count END,
                 finished_at = unixepoch(), updated_at = unixepoch()
             WHERE id = ? AND status IN ('PENDING', 'RUNNING')",
        )
        .bind(status)
        .bind(error)
        .bind(status)
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn delete_library(&self, id: &str) -> Result<bool, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.query(
            "DELETE FROM scheduled_task_configs
             WHERE owner_type = 'LIBRARY' AND owner_id = ?",
        )
        .bind(id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        let deleted = self
            .query("DELETE FROM libraries WHERE id = ?")
            .bind(id)
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
        Ok(deleted)
    }

    pub(crate) async fn update_library_settings(
        &self,
        library_id: &str,
        settings: LibrarySettingsUpdate<'_>,
    ) -> Result<bool, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;

        let exists: i64 = self
            .query_scalar(
                "SELECT CASE WHEN EXISTS(SELECT 1 FROM libraries WHERE id = ?) THEN 1 ELSE 0 END",
            )
            .bind(library_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if exists == 0 {
            return Ok(false);
        }

        if let Some(value) = settings.name {
            self.query(
                "UPDATE libraries
                 SET name = ?, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(value)
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        if let Some(value) = settings.kind {
            self.query(
                "UPDATE libraries
                 SET kind = ?, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(value)
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        if let Some(value) = settings.is_enabled {
            self.query(
                "UPDATE libraries
                 SET is_enabled = ?, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(database_flag(value))
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        if let Some(value) = settings.realtime_watch_enabled {
            self.query(
                "UPDATE libraries
                 SET realtime_watch_enabled = ?, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(database_flag(value))
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        if let Some(value) = settings.realtime_metadata_auto_match_enabled {
            self.query(
                "UPDATE libraries
                 SET realtime_metadata_auto_match_enabled = ?, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(database_flag(value))
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        if let Some(value) = settings.reconciliation_schedule {
            self.query(
                "UPDATE libraries
                 SET reconciliation_schedule = ?, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(value)
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        if let Some(value) = settings.metadata_schedule {
            self.query(
                "UPDATE libraries
                 SET metadata_schedule = ?, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(value)
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        if let Some(value) = settings.scraper_id {
            self.query(
                "UPDATE libraries
                 SET scraper_id = ?, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(value)
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        if let Some(value) = settings.chapter_source_id {
            self.query(
                "UPDATE libraries
                 SET chapter_source_id = ?, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(value)
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        if let Some(value) = settings.media_strategy_json {
            self.query(
                "UPDATE libraries
                 SET media_strategy_json = ?, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(value)
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        if let Some(value) = settings.scan_concurrency {
            self.query(
                "UPDATE libraries
                 SET scan_concurrency = ?, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(value)
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        if let Some(value) = settings.probe_concurrency {
            self.query(
                "UPDATE libraries
                 SET probe_concurrency = ?, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(value)
            .bind(library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }

        let current: (Option<String>, Option<String>, i64, i64, Option<String>) = self
            .query_as(
                "SELECT reconciliation_schedule, metadata_schedule,
                    scan_concurrency, probe_concurrency, scraper_id
             FROM libraries WHERE id = ?",
            )
            .bind(library_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;

        let resources = format!(
            "{{\"scanConcurrency\":{},\"probeConcurrency\":{}}}",
            current.2, current.3
        );
        let task_configs = [
            (
                "RECONCILIATION_SCAN",
                current.0.as_deref(),
                resources.as_str(),
            ),
            ("METADATA_PARSE", current.1.as_deref(), "{}"),
        ];
        for (task_type, schedule, resource_limit_json) in task_configs {
            self.query(
                "UPDATE scheduled_task_configs
                 SET cron_or_interval = ?,
                     is_enabled = ?,
                     resource_limit_json = ?,
                     source_type = CASE
                         WHEN task_type = 'METADATA_PARSE' AND ? IS NOT NULL THEN 'PLUGIN'
                         ELSE 'SYSTEM'
                     END,
                     plugin_id = CASE
                         WHEN task_type = 'METADATA_PARSE' THEN ?
                         ELSE NULL
                     END,
                     updated_at = unixepoch()
                 WHERE owner_type = 'LIBRARY' AND owner_id = ? AND task_type = ?",
            )
            .bind(schedule)
            .bind(database_flag(schedule.is_some()))
            .bind(resource_limit_json)
            .bind(current.4.as_deref())
            .bind(current.4.as_deref())
            .bind(library_id)
            .bind(task_type)
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

    pub(crate) async fn list_scheduled_task_configs(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<StoredScheduledTaskConfig>, i64), StorageError> {
        let total = self
            .query_scalar::<i64>("SELECT COUNT(*) FROM scheduled_task_configs")
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let rows = self
            .query(
                "SELECT s.owner_type, s.owner_id, s.task_type, s.task_name,
                    s.task_description, s.source_type, s.plugin_id,
                    s.cron_or_interval, s.is_enabled, s.resource_limit_json,
                    s.created_at, s.updated_at,
                    l.name AS library_name
             FROM scheduled_task_configs s
             LEFT JOIN libraries l
               ON s.owner_type = 'LIBRARY' AND l.id = s.owner_id
             ORDER BY s.updated_at DESC, s.owner_type, s.owner_id, s.task_type
             LIMIT ? OFFSET ?",
            )
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok((rows.into_iter().map(stored_scheduled_task).collect(), total))
    }

    pub(crate) async fn upsert_scheduled_task_config(
        &self,
        owner_type: &str,
        owner_id: &str,
        task_type: &str,
        schedule: Option<&str>,
        is_enabled: bool,
    ) -> Result<Option<StoredScheduledTaskConfig>, StorageError> {
        let result = self
            .query(
                "UPDATE scheduled_task_configs
             SET cron_or_interval = ?, is_enabled = ?, updated_at = unixepoch()
             WHERE owner_type = ? AND owner_id = ? AND task_type = ?",
            )
            .bind(schedule)
            .bind(database_flag(is_enabled))
            .bind(owner_type)
            .bind(owner_id)
            .bind(task_type)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if result.rows_affected() != 1 {
            return Ok(None);
        }
        self.find_scheduled_task_config(owner_type, owner_id, task_type)
            .await
    }

    pub(crate) async fn upsert_strm_media_info_task(
        &self,
        schedule: &str,
        is_enabled: bool,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO scheduled_task_configs (
                owner_type, owner_id, task_type, task_name, task_description,
                source_type, plugin_id, cron_or_interval, is_enabled, resource_limit_json
             ) VALUES (
                'GLOBAL', 'global', 'STRM_MEDIA_INFO', 'STRM 媒体信息扫描',
                '按插件配置周期扫描选定媒体库的 STRM 外部媒体信息并写入 JSON 旁车。',
                'PLUGIN', 'org.lux.strm-media-info', ?, ?, '{}'
             )
             ON CONFLICT(owner_type, owner_id, task_type) DO UPDATE SET
                task_name = excluded.task_name,
                task_description = excluded.task_description,
                source_type = excluded.source_type,
                plugin_id = excluded.plugin_id,
                cron_or_interval = excluded.cron_or_interval,
                is_enabled = excluded.is_enabled,
                updated_at = unixepoch()",
        )
        .bind(schedule)
        .bind(database_flag(is_enabled))
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn disable_strm_media_info_task(&self) -> Result<(), StorageError> {
        self.query(
            "UPDATE scheduled_task_configs
             SET is_enabled = 0, cron_or_interval = NULL, updated_at = unixepoch()
             WHERE owner_type = 'GLOBAL' AND owner_id = 'global'
               AND task_type = 'STRM_MEDIA_INFO'",
        )
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn disable_chapter_detection_tasks(&self) -> Result<(), StorageError> {
        self.query(
            "UPDATE scheduled_task_configs
             SET is_enabled = 0, cron_or_interval = NULL, updated_at = unixepoch()
             WHERE task_type = 'CHAPTER_DETECTION'",
        )
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn upsert_chapter_detection_task(
        &self,
        library_id: &str,
        plugin_id: &str,
        schedule: &str,
        is_enabled: bool,
        concurrency: i64,
        intro_window_seconds: i64,
        credits_window_seconds: i64,
        match_threshold: u32,
    ) -> Result<(), StorageError> {
        let resource_limit_json = format!(
            "{{\"concurrency\":{concurrency},\"introWindowSeconds\":{intro_window_seconds},\"creditsWindowSeconds\":{credits_window_seconds},\"matchThreshold\":{match_threshold}}}"
        );
        self.query(
            "INSERT INTO scheduled_task_configs (
                owner_type, owner_id, task_type, task_name, task_description,
                source_type, plugin_id, cron_or_interval, is_enabled, resource_limit_json
             ) VALUES (
                'LIBRARY', ?, 'CHAPTER_DETECTION', '片头片尾检测',
                '按插件配置比较同季度分集并保存片头片尾特殊章节。',
                'PLUGIN', ?, ?, ?, ?
             )
             ON CONFLICT(owner_type, owner_id, task_type) DO UPDATE SET
                task_name = excluded.task_name,
                task_description = excluded.task_description,
                source_type = excluded.source_type,
                plugin_id = excluded.plugin_id,
                cron_or_interval = excluded.cron_or_interval,
                is_enabled = excluded.is_enabled,
                resource_limit_json = excluded.resource_limit_json,
                updated_at = unixepoch()",
        )
        .bind(library_id)
        .bind(plugin_id)
        .bind(schedule)
        .bind(database_flag(is_enabled))
        .bind(resource_limit_json)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_scheduled_task_config(
        &self,
        owner_type: &str,
        owner_id: &str,
        task_type: &str,
    ) -> Result<Option<StoredScheduledTaskConfig>, StorageError> {
        self.query(
            "SELECT s.owner_type, s.owner_id, s.task_type, s.task_name,
                    s.task_description, s.source_type, s.plugin_id,
                    s.cron_or_interval, s.is_enabled, s.resource_limit_json,
                    s.created_at, s.updated_at,
                    l.name AS library_name
             FROM scheduled_task_configs s
             LEFT JOIN libraries l
               ON s.owner_type = 'LIBRARY' AND l.id = s.owner_id
             WHERE s.owner_type = ? AND s.owner_id = ? AND s.task_type = ?",
        )
        .bind(owner_type)
        .bind(owner_id)
        .bind(task_type)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_scheduled_task))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn library_exists(&self, library_id: &str) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(SELECT 1 FROM libraries WHERE id = ?) THEN 1 ELSE 0 END",
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

    pub(crate) async fn update_library_cover(
        &self,
        library_id: &str,
        path: &str,
        content_type: &str,
        size: i64,
        tag: &str,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE libraries
             SET cover_image_path = ?,
                 cover_image_content_type = ?,
                 cover_image_size = ?,
                 cover_image_tag = ?,
                 updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(path)
        .bind(content_type)
        .bind(size)
        .bind(tag)
        .bind(library_id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn update_library_cover_if_missing(
        &self,
        library_id: &str,
        path: &str,
        content_type: &str,
        size: i64,
        tag: &str,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE libraries
             SET cover_image_path = ?,
                 cover_image_content_type = ?,
                 cover_image_size = ?,
                 cover_image_tag = ?,
                 updated_at = unixepoch()
             WHERE id = ? AND cover_image_path IS NULL",
        )
        .bind(path)
        .bind(content_type)
        .bind(size)
        .bind(tag)
        .bind(library_id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn update_library_cover_if_auto(
        &self,
        library_id: &str,
        path: &str,
        content_type: &str,
        size: i64,
        tag: &str,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE libraries
             SET cover_image_path = ?,
                 cover_image_content_type = ?,
                 cover_image_size = ?,
                 cover_image_tag = ?,
                 updated_at = unixepoch()
             WHERE id = ? AND (cover_image_path IS NULL OR cover_image_path = ?)",
        )
        .bind(path)
        .bind(content_type)
        .bind(size)
        .bind(tag)
        .bind(library_id)
        .bind(path)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_user_item_state(
        &self,
        user_id: &str,
        item_id: &str,
    ) -> Result<Option<StoredUserItemState>, StorageError> {
        self.query(
            "SELECT position_ticks, is_played, is_favorite, play_count,
                    last_played_at, version
             FROM user_item_state WHERE user_id = ? AND item_id = ?",
        )
        .bind(user_id)
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredUserItemState {
                position_ticks: row.get("position_ticks"),
                is_played: row.get::<i64, _>("is_played") != 0,
                is_favorite: row.get::<i64, _>("is_favorite") != 0,
                play_count: row.get("play_count"),
                last_played_at: row.get("last_played_at"),
                version: row.get("version"),
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn plugin_installation_status(
        &self,
        plugin_id: &str,
    ) -> Result<Option<bool>, StorageError> {
        self.query_scalar("SELECT is_enabled FROM installed_plugins WHERE plugin_id = ?")
            .bind(plugin_id)
            .fetch_optional(&self.pool)
            .await
            .map(|value: Option<i64>| value.map(|value| value != 0))
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn is_plugin_installed(&self, plugin_id: &str) -> Result<bool, StorageError> {
        self.plugin_installation_status(plugin_id)
            .await
            .map(|status| status == Some(true))
    }

    pub(crate) async fn has_plugin_installation(
        &self,
        plugin_id: &str,
    ) -> Result<bool, StorageError> {
        self.plugin_installation_status(plugin_id)
            .await
            .map(|status| status.is_some())
    }

    pub(crate) async fn install_plugin(&self, plugin_id: &str) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO installed_plugins (plugin_id, is_enabled)
             VALUES (?, 1)
             ON CONFLICT(plugin_id) DO UPDATE SET
                is_enabled = 1,
                updated_at = unixepoch()",
        )
        .bind(plugin_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn set_plugin_enabled(
        &self,
        plugin_id: &str,
        enabled: bool,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE installed_plugins
             SET is_enabled = ?, updated_at = unixepoch()
             WHERE plugin_id = ?",
        )
        .bind(database_flag(enabled))
        .bind(plugin_id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_user_item_states(
        &self,
        user_id: &str,
        item_ids: &[String],
    ) -> Result<HashMap<String, StoredUserItemState>, StorageError> {
        let mut states = HashMap::with_capacity(item_ids.len());
        for chunk in item_ids.chunks(500) {
            if chunk.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT item_id, position_ticks, is_played, is_favorite, play_count,
                        last_played_at, version
                 FROM user_item_state WHERE user_id = ? AND item_id IN ({placeholders})"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query)).bind(user_id);
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
            for row in rows {
                states.insert(
                    row.get("item_id"),
                    StoredUserItemState {
                        position_ticks: row.get("position_ticks"),
                        is_played: row.get::<i64, _>("is_played") != 0,
                        is_favorite: row.get::<i64, _>("is_favorite") != 0,
                        play_count: row.get("play_count"),
                        last_played_at: row.get("last_played_at"),
                        version: row.get("version"),
                    },
                );
            }
        }
        Ok(states)
    }

    pub(crate) async fn resume_settings(&self) -> Result<(i64, i64), StorageError> {
        let values: Vec<(String, String)> = self
            .query_as(
                "SELECT key, value FROM server_settings
             WHERE key IN ('resume_played_percent', 'resume_min_ticks')",
            )
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let percent = values
            .iter()
            .find(|(key, _)| key == "resume_played_percent")
            .and_then(|(_, value)| value.parse().ok())
            .unwrap_or(90)
            .clamp(1, 100);
        let min_ticks = values
            .iter()
            .find(|(key, _)| key == "resume_min_ticks")
            .and_then(|(_, value)| value.parse().ok())
            .unwrap_or(1_200_000_000)
            .max(0);
        Ok((percent, min_ticks))
    }

    pub(crate) async fn user_played_percent(&self, user_id: &str) -> Result<i64, StorageError> {
        self.query_scalar(
            "SELECT played_percent FROM user_playback_settings
             WHERE user_id = ?",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map(|value: Option<i64>| value.unwrap_or(DEFAULT_PLAYED_PERCENT).clamp(1, 100))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn set_user_played_percent(
        &self,
        user_id: &str,
        played_percent: i64,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO user_playback_settings (user_id, played_percent)
             VALUES (?, ?)
             ON CONFLICT(user_id) DO UPDATE SET
                 played_percent = excluded.played_percent,
                 updated_at = unixepoch()",
        )
        .bind(user_id)
        .bind(played_percent)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn user_library_order(
        &self,
        user_id: &str,
    ) -> Result<Vec<String>, StorageError> {
        self.query_scalar(
            "SELECT library_id FROM user_library_order
             WHERE user_id = ?
             ORDER BY position, library_id",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn replace_user_library_order(
        &self,
        user_id: &str,
        library_ids: &[String],
    ) -> Result<(), StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.query("DELETE FROM user_library_order WHERE user_id = ?")
            .bind(user_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        for (position, library_id) in library_ids.iter().enumerate() {
            self.query(
                "INSERT INTO user_library_order (user_id, library_id, position)
                 VALUES (?, ?, ?)",
            )
            .bind(user_id)
            .bind(library_id)
            .bind(
                i64::try_from(position).map_err(|_| {
                    StorageError::Serialization("媒体库排序位置超出范围".to_owned())
                })?,
            )
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

    pub(crate) async fn set_server_settings(
        &self,
        percent: i64,
        min_ticks: i64,
        media_strategy: &str,
    ) -> Result<(), StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        for (key, value) in [
            ("resume_played_percent", percent.to_string()),
            ("resume_min_ticks", min_ticks.to_string()),
            ("media_strategy", media_strategy.to_owned()),
        ] {
            self.query(
                "INSERT INTO server_settings (key, value)
                 VALUES (?, ?)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = unixepoch()",
            )
            .bind(key)
            .bind(value)
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

    pub(crate) async fn uninstall_plugin(&self, plugin_id: &str) -> Result<(), StorageError> {
        self.query(
            "UPDATE libraries
             SET scraper_id = CASE WHEN scraper_id = ? THEN NULL ELSE scraper_id END,
                 chapter_source_id = CASE WHEN chapter_source_id = ? THEN NULL ELSE chapter_source_id END,
                 updated_at = unixepoch()
             WHERE scraper_id = ? OR chapter_source_id = ?",
        )
        .bind(plugin_id)
        .bind(plugin_id)
        .bind(plugin_id)
        .bind(plugin_id)
        .execute(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query("DELETE FROM installed_plugins WHERE plugin_id = ?")
            .bind(plugin_id)
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn server_name(&self) -> Result<Option<String>, StorageError> {
        self.query_scalar(
            "SELECT value FROM server_settings
             WHERE key = 'server_name'",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn set_server_name(&self, name: &str) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO server_settings (key, value)
             VALUES ('server_name', ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = unixepoch()",
        )
        .bind(name)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn media_strategy_settings(&self) -> Result<Option<String>, StorageError> {
        self.query_scalar(
            "SELECT value FROM server_settings
             WHERE key = 'media_strategy'",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn set_user_item_played(
        &self,
        user_id: &str,
        item_id: &str,
        played: bool,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO user_item_state (user_id, item_id, is_played, play_count, last_played_at)
             VALUES (?, ?, ?, CASE WHEN ? = 1 THEN 1 ELSE 0 END,
                     CASE WHEN ? = 1 THEN unixepoch() ELSE NULL END)
             ON CONFLICT(user_id, item_id) DO UPDATE SET
                 is_played = excluded.is_played,
                 play_count = CASE
                     WHEN excluded.is_played = 1 AND user_item_state.is_played = 0
                     THEN user_item_state.play_count + 1 ELSE user_item_state.play_count END,
                 last_played_at = CASE
                     WHEN excluded.is_played = 1 THEN unixepoch()
                     ELSE user_item_state.last_played_at END,
                 version = user_item_state.version + CASE
                     WHEN excluded.is_played != user_item_state.is_played THEN 1 ELSE 0 END",
        )
        .bind(user_id)
        .bind(item_id)
        .bind(database_flag(played))
        .bind(database_flag(played))
        .bind(database_flag(played))
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn set_user_item_favorite(
        &self,
        user_id: &str,
        item_id: &str,
        favorite: bool,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO user_item_state (user_id, item_id, is_favorite)
             VALUES (?, ?, ?)
             ON CONFLICT(user_id, item_id) DO UPDATE SET
                 is_favorite = excluded.is_favorite,
                 version = user_item_state.version + CASE
                     WHEN excluded.is_favorite != user_item_state.is_favorite THEN 1 ELSE 0 END",
        )
        .bind(user_id)
        .bind(item_id)
        .bind(database_flag(favorite))
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn record_playback_event(
        &self,
        event: NewPlaybackEvent<'_>,
    ) -> Result<(), StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let max_function = self.scalar_max_function();
        let auto_played = event.duration_ticks.is_some_and(|duration_ticks| {
            playback_reached_played_threshold(
                event.position_ticks,
                duration_ticks,
                event.played_percent,
            )
        });
        let playback_session_query = format!(
            "INSERT INTO playback_sessions (
                id, user_id, item_id, media_source_id, play_session_id,
                device_id, client, device_name, client_version, device_type,
                remote_ip, state,
                position_ticks, duration_ticks, is_paused
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(user_id, play_session_id) DO UPDATE SET
                item_id = excluded.item_id,
                media_source_id = excluded.media_source_id,
                device_id = CASE
                    WHEN excluded.device_id = 'unknown' THEN playback_sessions.device_id
                    ELSE excluded.device_id END,
                client = COALESCE(excluded.client, playback_sessions.client),
                device_name = COALESCE(excluded.device_name, playback_sessions.device_name),
                client_version = COALESCE(excluded.client_version, playback_sessions.client_version),
                device_type = COALESCE(excluded.device_type, playback_sessions.device_type),
                remote_ip = COALESCE(excluded.remote_ip, playback_sessions.remote_ip),
                state = excluded.state,
                position_ticks = {max_function}(playback_sessions.position_ticks, excluded.position_ticks),
                duration_ticks = COALESCE(excluded.duration_ticks, playback_sessions.duration_ticks),
                is_paused = excluded.is_paused,
                last_event_at = unixepoch()"
        );
        self.query(sqlx::AssertSqlSafe(playback_session_query))
            .bind(Uuid::now_v7().to_string())
            .bind(event.user_id)
            .bind(event.item_id)
            .bind(event.media_source_id)
            .bind(event.play_session_id)
            .bind(event.device_id)
            .bind(event.client)
            .bind(event.device_name)
            .bind(event.client_version)
            .bind(event.device_type)
            .bind(event.remote_ip)
            .bind(event.state)
            .bind(event.position_ticks)
            .bind(event.duration_ticks)
            .bind(database_flag(event.is_paused))
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let user_item_state_query = format!(
            "INSERT INTO user_item_state (user_id, item_id, position_ticks, last_played_at)
             VALUES (?, ?, ?, unixepoch())
             ON CONFLICT(user_id, item_id) DO UPDATE SET
                 position_ticks = {max_function}(user_item_state.position_ticks, excluded.position_ticks),
                 last_played_at = CASE
                     WHEN excluded.position_ticks > user_item_state.position_ticks
                     THEN excluded.last_played_at ELSE user_item_state.last_played_at END,
                 version = user_item_state.version + CASE
                     WHEN excluded.position_ticks > user_item_state.position_ticks THEN 1 ELSE 0 END"
        );
        self.query(sqlx::AssertSqlSafe(user_item_state_query))
            .bind(event.user_id)
            .bind(event.item_id)
            .bind(event.position_ticks)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if auto_played {
            self.query(
                "UPDATE user_item_state
                 SET is_played = 1,
                     play_count = CASE WHEN is_played = 0 THEN play_count + 1 ELSE play_count END,
                     last_played_at = unixepoch(),
                     version = version + CASE WHEN is_played = 0 THEN 1 ELSE 0 END
                 WHERE user_id = ? AND item_id = ?",
            )
            .bind(event.user_id)
            .bind(event.item_id)
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

    pub(crate) async fn sync_played_container_states(
        &self,
        user_id: &str,
        item_id: &str,
    ) -> Result<(), StorageError> {
        let parent_ids: Vec<String> = self
            .query(
                "SELECT parent_id FROM media_items
                 WHERE id = ? AND item_type = 'EPISODE' AND parent_id IS NOT NULL
                 UNION
                 SELECT series_id FROM media_items
                 WHERE id = ? AND item_type = 'EPISODE' AND series_id IS NOT NULL",
            )
            .bind(item_id)
            .bind(item_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
            .into_iter()
            .map(|row| row.get::<String, _>(0))
            .collect();
        if parent_ids.is_empty() {
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
        for parent_id in parent_ids {
            let is_played: i64 = self
                .query_scalar(
                    "WITH eligible AS (
                         SELECT episode.id
                         FROM media_items episode
                         JOIN media_items parent ON parent.id = ?
                         WHERE episode.item_type = 'EPISODE'
                           AND episode.removed_at IS NULL
                           AND episode.has_available_source = 1
                           AND ((parent.item_type = 'SEASON' AND episode.parent_id = parent.id)
                             OR (parent.item_type = 'SERIES' AND episode.series_id = parent.id))
                     )
                     SELECT CASE WHEN EXISTS (SELECT 1 FROM eligible)
                                      AND NOT EXISTS (
                                          SELECT 1
                                          FROM eligible
                                          LEFT JOIN user_item_state state
                                            ON state.user_id = ? AND state.item_id = eligible.id
                                          WHERE COALESCE(state.is_played, 0) = 0
                                      )
                                 THEN 1 ELSE 0 END",
                )
                .bind(&parent_id)
                .bind(user_id)
                .fetch_one(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            self.query(
                "INSERT INTO user_item_state (user_id, item_id, is_played, play_count, last_played_at)
                 VALUES (?, ?, ?, CASE WHEN ? = 1 THEN 1 ELSE 0 END,
                         CASE WHEN ? = 1 THEN unixepoch() ELSE NULL END)
                 ON CONFLICT(user_id, item_id) DO UPDATE SET
                     is_played = excluded.is_played,
                     play_count = CASE
                         WHEN excluded.is_played = 1 AND user_item_state.is_played = 0
                         THEN user_item_state.play_count + 1 ELSE user_item_state.play_count END,
                     last_played_at = CASE
                         WHEN excluded.is_played = 1 THEN unixepoch()
                         ELSE user_item_state.last_played_at END,
                     version = user_item_state.version + CASE
                         WHEN excluded.is_played != user_item_state.is_played THEN 1 ELSE 0 END",
            )
            .bind(user_id)
            .bind(&parent_id)
            .bind(is_played)
            .bind(is_played)
            .bind(is_played)
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

    pub(crate) async fn list_playback_sessions(
        &self,
        user_id: Option<&str>,
    ) -> Result<Vec<StoredPlaybackSession>, StorageError> {
        let (query, bind) = if user_id.is_some() {
            (
                "SELECT id, user_id, item_id, media_source_id, play_session_id,
                        device_id, client, device_name, client_version, device_type,
                        remote_ip, state,
                        position_ticks, duration_ticks, is_paused, started_at,
                        last_event_at
                 FROM playback_sessions
                 WHERE user_id = ?
                   AND state != 'STOPPED'
                   AND last_event_at > unixepoch() - ?
                 ORDER BY last_event_at DESC, id",
                user_id,
            )
        } else {
            (
                "SELECT id, user_id, item_id, media_source_id, play_session_id,
                        device_id, client, device_name, client_version, device_type,
                        remote_ip, state,
                        position_ticks, duration_ticks, is_paused, started_at,
                        last_event_at
                 FROM playback_sessions
                 WHERE state != 'STOPPED'
                   AND last_event_at > unixepoch() - ?
                 ORDER BY last_event_at DESC, id",
                None,
            )
        };
        let mut statement = self.query(query);
        if let Some(user_id) = bind {
            statement = statement.bind(user_id);
        }
        statement = statement.bind(PLAYBACK_SESSION_STALE_AFTER_SECONDS);
        statement
            .fetch_all(&self.pool)
            .await
            .map(|rows| rows.into_iter().map(stored_playback_session).collect())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn find_playback_session(
        &self,
        user_id: &str,
        play_session_id: &str,
    ) -> Result<Option<StoredPlaybackSession>, StorageError> {
        self.query(
            "SELECT id, user_id, item_id, media_source_id, play_session_id,
                    device_id, client, device_name, client_version, device_type,
                    remote_ip, state,
                    position_ticks, duration_ticks, is_paused, started_at,
                    last_event_at
             FROM playback_sessions
             WHERE user_id = ? AND play_session_id = ?",
        )
        .bind(user_id)
        .bind(play_session_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_playback_session))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_active_playback_session(
        &self,
        user_id: &str,
        item_id: &str,
    ) -> Result<Option<StoredPlaybackSession>, StorageError> {
        self.query(
            "SELECT id, user_id, item_id, media_source_id, play_session_id,
                    device_id, client, device_name, client_version, device_type,
                    remote_ip, state,
                    position_ticks, duration_ticks, is_paused, started_at,
                    last_event_at
             FROM playback_sessions
             WHERE user_id = ?
               AND item_id = ?
               AND state != 'STOPPED'
               AND last_event_at > unixepoch() - ?
             ORDER BY last_event_at DESC, id
             LIMIT 1",
        )
        .bind(user_id)
        .bind(item_id)
        .bind(PLAYBACK_SESSION_STALE_AFTER_SECONDS)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_playback_session))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn media_source_belongs_to_item(
        &self,
        source_id: &str,
        item_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(
                SELECT 1 FROM media_sources WHERE id = ? AND item_id = ?
            ) THEN 1 ELSE 0 END",
        )
        .bind(source_id)
        .bind(item_id)
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
            "SELECT DISTINCT ms.item_id
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
             ORDER BY ms.item_id",
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
                    total_count, cancel_requested, error
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
                        total_count, cancel_requested, error
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
                        total_count, cancel_requested, error
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
        for (entry_type, paths) in [("DIRECTORY", child_directories), ("FILE", media_files)] {
            for path in paths {
                self.query(
                    "INSERT INTO reconciliation_scan_entries (
                        job_id, library_root_id, relative_path, entry_type
                     ) VALUES (?, ?, ?, ?)
                     ON CONFLICT(job_id, library_root_id, entry_type, relative_path) DO NOTHING",
                )
                .bind(job_id)
                .bind(library_root_id)
                .bind(path)
                .bind(entry_type)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            }
        }
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
        for entry in entries {
            self.query(
                "DELETE FROM reconciliation_scan_entries
                 WHERE job_id = ? AND library_root_id = ?
                   AND relative_path = ? AND entry_type = 'FILE'",
            )
            .bind(job_id)
            .bind(&entry.library_root_id)
            .bind(&entry.relative_path)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
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
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
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
        for item_id in item_ids {
            self.query(
                "INSERT INTO metadata_reidentify_job_items (job_id, item_id, status)
                 VALUES (?, ?, 'PENDING')",
            )
            .bind(job_id)
            .bind(item_id)
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
        item_ids: &[String],
        mode: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO metadata_reidentify_jobs (
                id, status, total_count, mode, library_id, job_scope
             ) VALUES (?, 'CANCELLED', ?, ?, ?, 'LIBRARY')",
        )
        .bind(job_id)
        .bind(i64::try_from(item_ids.len()).unwrap_or(i64::MAX))
        .bind(mode)
        .bind(library_id)
        .execute(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;

        for chunk in item_ids.chunks(500) {
            let mut transaction = self
                .pool
                .begin()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            for item_id in chunk {
                self.query(
                    "INSERT INTO metadata_reidentify_job_items (job_id, item_id, status)
                     VALUES (?, ?, 'PENDING')",
                )
                .bind(job_id)
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
        }

        self.query(
            "UPDATE metadata_reidentify_jobs
             SET status = 'QUEUED', updated_at = unixepoch()
             WHERE id = ? AND status = 'CANCELLED'",
        )
        .bind(job_id)
        .execute(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        Ok(())
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
        self.query(
            "UPDATE metadata_reidentify_jobs
             SET status = 'RUNNING', started_at = COALESCE(started_at, unixepoch()),
                 updated_at = unixepoch()
             WHERE id = ? AND status = 'QUEUED' AND cancel_requested = 0",
        )
        .bind(job_id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

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

    pub(crate) async fn claim_metadata_reidentify_item(
        &self,
        job_id: &str,
        item_id: &str,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE metadata_reidentify_job_items
             SET status = 'RUNNING', updated_at = unixepoch()
             WHERE job_id = ? AND item_id = ? AND status = 'PENDING'
               AND EXISTS (
                   SELECT 1 FROM metadata_reidentify_jobs
                   WHERE id = ? AND status IN ('QUEUED', 'RUNNING')
                     AND cancel_requested = 0
               )",
        )
        .bind(job_id)
        .bind(item_id)
        .bind(job_id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn finish_metadata_reidentify_item(
        &self,
        job_id: &str,
        item_id: &str,
        status: &str,
        candidate_count: i64,
        error: Option<&str>,
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
        self.query(
            "UPDATE metadata_reidentify_job_items
             SET status = 'PENDING', error = NULL, updated_at = unixepoch()
             WHERE job_id = ? AND status = 'RUNNING'",
        )
        .bind(job_id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn finish_metadata_reidentify_job(
        &self,
        job_id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<(), StorageError> {
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
        .execute(&self.pool)
        .await
        .map(|_| ())
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
        self.query(
            "UPDATE metadata_reidentify_jobs
             SET cancel_requested = 1, updated_at = unixepoch()
             WHERE id = ? AND status IN ('QUEUED', 'RUNNING')",
        )
        .bind(job_id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn retry_metadata_reidentify_job(
        &self,
        job_id: &str,
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
                    finished_at,
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
                        finished_at,
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
                        finished_at,
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
                    finished_at,
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
                    finished_at,
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
             WHERE id = ? AND status IN ('PENDING', 'RUNNING')",
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
            "SELECT fe.id, fe.relative_path, fe.fingerprint, ms.item_id
             FROM filesystem_entries fe
             LEFT JOIN media_sources ms ON ms.filesystem_entry_id = fe.id
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
                "SELECT fe.id, fe.relative_path, fe.fingerprint, ms.item_id
                 FROM filesystem_entries fe
                 LEFT JOIN media_sources ms ON ms.filesystem_entry_id = fe.id
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

    pub(crate) async fn find_filesystem_entry_by_inode(
        &self,
        library_id: &str,
        target_root_id: &str,
        inode: i64,
        relative_path: &str,
    ) -> Result<Option<StoredFilesystemEntry>, StorageError> {
        let rows = self
            .query(
                "SELECT fe.id, fe.relative_path, fe.fingerprint, ms.item_id
                 FROM filesystem_entries fe
                 JOIN library_roots lr ON lr.id = fe.library_root_id
                 LEFT JOIN media_sources ms ON ms.filesystem_entry_id = fe.id
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

    pub(crate) async fn list_existing_filesystem_paths(
        &self,
        library_root_id: &str,
        relative_paths: &[String],
    ) -> Result<Vec<String>, StorageError> {
        let mut existing_paths = Vec::new();
        for chunk in relative_paths.chunks(500) {
            if chunk.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT relative_path FROM filesystem_entries
                 WHERE library_root_id = ? AND relative_path IN ({placeholders})"
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
            existing_paths.extend(rows.into_iter().map(|row| row.get("relative_path")));
        }
        Ok(existing_paths)
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

    pub(crate) async fn insert_filesystem_entry(
        &self,
        entry: NewFilesystemEntry<'_>,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO filesystem_entries (
                id, library_root_id, relative_path, entry_kind, size,
                modified_at, inode, fingerprint, last_seen_generation, is_missing
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 0)",
        )
        .bind(entry.id)
        .bind(entry.library_root_id)
        .bind(entry.relative_path)
        .bind(entry.entry_kind)
        .bind(entry.size)
        .bind(entry.modified_at)
        .bind(entry.inode)
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

    async fn ensure_movie_parent_folder_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        library_id: &str,
        library_root_id: &str,
        relative_path: &str,
    ) -> Result<Option<String>, StorageError> {
        let directory = relative_path
            .rsplit_once('/')
            .map(|(directory, _)| directory)
            .or_else(|| {
                relative_path
                    .rsplit_once('\\')
                    .map(|(directory, _)| directory)
            })
            .unwrap_or_default();
        let mut parent_folder_id = None;
        let mut parent_id = library_id.to_owned();
        let mut directory_key = String::new();
        for component in directory.split(['/', '\\']) {
            if component.is_empty() || component == "." {
                continue;
            }
            if !directory_key.is_empty() {
                directory_key.push('/');
            }
            directory_key.push_str(component);
            let identity_key = format!("folder:{library_root_id}:{directory_key}");
            let folder_id = self
                .query_scalar::<String>("SELECT id FROM media_items WHERE identity_key = ? LIMIT 1")
                .bind(&identity_key)
                .fetch_optional(&mut **transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            let folder_id = if let Some(folder_id) = folder_id {
                self.query(
                    "UPDATE media_items
                     SET library_id = ?, item_type = 'FOLDER', parent_id = ?,
                         title = ?, sort_title = ?, original_title = ?,
                         identification_status = 'LOCAL_CONFIRMED', removed_at = NULL
                     WHERE id = ?",
                )
                .bind(library_id)
                .bind(&parent_id)
                .bind(component)
                .bind(component.to_ascii_lowercase())
                .bind(component)
                .bind(&folder_id)
                .execute(&mut **transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
                folder_id
            } else {
                let folder_id = Uuid::now_v7().to_string();
                self.query(
                    "INSERT INTO media_items (
                        id, library_id, item_type, parent_id, title, sort_title,
                        original_title, identification_status, identity_key
                    ) VALUES (?, ?, 'FOLDER', ?, ?, ?, ?, 'LOCAL_CONFIRMED', ?)",
                )
                .bind(&folder_id)
                .bind(library_id)
                .bind(&parent_id)
                .bind(component)
                .bind(component.to_ascii_lowercase())
                .bind(component)
                .bind(&identity_key)
                .execute(&mut **transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
                folder_id
            };
            parent_id = folder_id.clone();
            parent_folder_id = Some(folder_id);
        }
        Ok(parent_folder_id)
    }

    async fn prefetch_movie_items_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        library_id: &str,
        files: &[NewMovieFile],
    ) -> Result<HashMap<(String, Option<i64>), String>, StorageError> {
        let mut sort_titles = files
            .iter()
            .map(|file| file.sort_title.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        sort_titles.sort_unstable();
        let mut movie_items = HashMap::new();
        for chunk in sort_titles.chunks(BATCH_INSERT_CHUNK_SIZE) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT id, sort_title, production_year
                 FROM media_items
                 WHERE library_id = ? AND item_type = 'MOVIE'
                   AND removed_at IS NULL AND sort_title IN ({placeholders})"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query)).bind(library_id);
            for sort_title in chunk {
                statement = statement.bind(sort_title);
            }
            let rows = statement
                .fetch_all(&mut **transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            for row in rows {
                let id = row
                    .try_get::<String, _>("id")
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                let sort_title = row.try_get::<String, _>("sort_title").map_err(|source| {
                    StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    }
                })?;
                let production_year =
                    row.try_get::<Option<i64>, _>("production_year")
                        .map_err(|source| StorageError::Sqlx {
                            path: self.path.clone(),
                            source,
                        })?;
                movie_items.insert((sort_title, production_year), id);
            }
        }
        Ok(movie_items)
    }

    async fn prefetch_movie_folders_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        library_root_id: &str,
        files: &[NewMovieFile],
    ) -> Result<HashMap<String, String>, StorageError> {
        let mut identity_keys = HashSet::new();
        for file in files {
            let mut directory_key = String::new();
            let directory = file
                .relative_path
                .rsplit_once('/')
                .map(|(directory, _)| directory)
                .or_else(|| {
                    file.relative_path
                        .rsplit_once('\\')
                        .map(|(directory, _)| directory)
                })
                .unwrap_or_default();
            for component in directory.split(['/', '\\']) {
                if component.is_empty() || component == "." {
                    continue;
                }
                if !directory_key.is_empty() {
                    directory_key.push('/');
                }
                directory_key.push_str(component);
                identity_keys.insert(format!("folder:{library_root_id}:{directory_key}"));
            }
        }
        let mut identity_keys = identity_keys.into_iter().collect::<Vec<_>>();
        identity_keys.sort_unstable();
        let mut folders = HashMap::new();
        for chunk in identity_keys.chunks(BATCH_INSERT_CHUNK_SIZE) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT id, identity_key
                 FROM media_items
                 WHERE item_type = 'FOLDER' AND identity_key IN ({placeholders})"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for identity_key in chunk {
                statement = statement.bind(identity_key);
            }
            let rows = statement
                .fetch_all(&mut **transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            for row in rows {
                let id = row
                    .try_get::<String, _>("id")
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                let identity_key = row.try_get::<String, _>("identity_key").map_err(|source| {
                    StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    }
                })?;
                folders.insert(identity_key, id);
            }
        }
        Ok(folders)
    }

    async fn ensure_movie_parent_folder_cached(
        &self,
        transaction: &mut sqlx::Transaction<'_, Any>,
        library_id: &str,
        library_root_id: &str,
        relative_path: &str,
        folder_cache: &mut HashMap<String, String>,
        touched_folders: &mut HashSet<String>,
    ) -> Result<Option<String>, StorageError> {
        let directory = relative_path
            .rsplit_once('/')
            .map(|(directory, _)| directory)
            .or_else(|| {
                relative_path
                    .rsplit_once('\\')
                    .map(|(directory, _)| directory)
            })
            .unwrap_or_default();
        let mut parent_folder_id = None;
        let mut parent_id = library_id.to_owned();
        let mut directory_key = String::new();
        for component in directory.split(['/', '\\']) {
            if component.is_empty() || component == "." {
                continue;
            }
            if !directory_key.is_empty() {
                directory_key.push('/');
            }
            directory_key.push_str(component);
            let identity_key = format!("folder:{library_root_id}:{directory_key}");
            let folder_id = if let Some(folder_id) = folder_cache.get(&identity_key) {
                let folder_id = folder_id.clone();
                if touched_folders.insert(identity_key.clone()) {
                    self.query(
                        "UPDATE media_items
                         SET library_id = ?, item_type = 'FOLDER', parent_id = ?,
                             title = ?, sort_title = ?, original_title = ?,
                             identification_status = 'LOCAL_CONFIRMED', removed_at = NULL
                         WHERE id = ?",
                    )
                    .bind(library_id)
                    .bind(&parent_id)
                    .bind(component)
                    .bind(component.to_ascii_lowercase())
                    .bind(component)
                    .bind(&folder_id)
                    .execute(&mut **transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                }
                folder_id
            } else {
                let folder_id = Uuid::now_v7().to_string();
                self.query(
                    "INSERT INTO media_items (
                        id, library_id, item_type, parent_id, title, sort_title,
                        original_title, identification_status, identity_key
                    ) VALUES (?, ?, 'FOLDER', ?, ?, ?, ?, 'LOCAL_CONFIRMED', ?)",
                )
                .bind(&folder_id)
                .bind(library_id)
                .bind(&parent_id)
                .bind(component)
                .bind(component.to_ascii_lowercase())
                .bind(component)
                .bind(&identity_key)
                .execute(&mut **transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
                folder_cache.insert(identity_key.clone(), folder_id.clone());
                touched_folders.insert(identity_key);
                folder_id
            };
            parent_id = folder_id.clone();
            parent_folder_id = Some(folder_id);
        }
        Ok(parent_folder_id)
    }

    pub(crate) async fn repair_movie_parent_folder(
        &self,
        library_id: &str,
        library_root_id: &str,
        relative_path: &str,
        item_id: &str,
    ) -> Result<(), StorageError> {
        let expected_identity_key = movie_parent_folder_identity(library_root_id, relative_path);
        let parent_is_current = if let Some(expected_identity_key) = expected_identity_key {
            self.query_scalar::<i64>(
                "SELECT CASE WHEN EXISTS (
                     SELECT 1
                     FROM media_items movie
                     JOIN media_items parent ON parent.id = movie.parent_id
                     WHERE movie.id = ? AND movie.item_type = 'MOVIE'
                       AND parent.item_type = 'FOLDER'
                       AND parent.identity_key = ? AND parent.removed_at IS NULL
                 ) THEN 1 ELSE 0 END",
            )
            .bind(item_id)
            .bind(expected_identity_key)
            .fetch_one(&self.pool)
            .await
            .map(|value| value != 0)
        } else {
            self.query_scalar::<i64>(
                "SELECT CASE WHEN EXISTS (
                     SELECT 1 FROM media_items
                     WHERE id = ? AND item_type = 'MOVIE' AND parent_id IS NULL
                 ) THEN 1 ELSE 0 END",
            )
            .bind(item_id)
            .fetch_one(&self.pool)
            .await
            .map(|value| value != 0)
        }
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        if parent_is_current {
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
        let parent_folder_id = self
            .ensure_movie_parent_folder_in_transaction(
                &mut transaction,
                library_id,
                library_root_id,
                relative_path,
            )
            .await?;
        self.query(
            "UPDATE media_items SET parent_id = ?
             WHERE id = ? AND item_type = 'MOVIE'",
        )
        .bind(parent_folder_id.as_deref())
        .bind(item_id)
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

    pub(crate) async fn insert_movie_files_batch(
        &self,
        library_id: &str,
        library_root_id: &str,
        generation: &str,
        files: &[NewMovieFile],
    ) -> Result<usize, StorageError> {
        if files.is_empty() {
            return Ok(0);
        }
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let mut folder_cache = self
            .prefetch_movie_folders_in_transaction(&mut transaction, library_root_id, files)
            .await?;
        let mut touched_folders = HashSet::new();
        let mut movie_cache = self
            .prefetch_movie_items_in_transaction(&mut transaction, library_id, files)
            .await?;

        for chunk in files.chunks(BATCH_INSERT_CHUNK_SIZE) {
            let values = std::iter::repeat_n("(?, ?, ?, 'FILE', ?, ?, ?, ?, ?, 0)", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "INSERT INTO filesystem_entries (
                    id, library_root_id, relative_path, entry_kind, size,
                    modified_at, inode, fingerprint, last_seen_generation, is_missing
                ) VALUES {values}"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for file in chunk {
                statement = statement
                    .bind(&file.filesystem_entry_id)
                    .bind(library_root_id)
                    .bind(&file.relative_path)
                    .bind(file.size)
                    .bind(file.modified_at)
                    .bind(Option::<i64>::None)
                    .bind(&file.fingerprint)
                    .bind(generation);
            }
            statement
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }

        let mut new_items = Vec::new();
        let mut new_item_ids = HashSet::new();
        let mut parent_updates = HashMap::new();
        let mut provider_updates = HashMap::new();
        let mut source_rows = Vec::with_capacity(files.len());
        for (index, file) in files.iter().enumerate() {
            let parent_folder_id = self
                .ensure_movie_parent_folder_cached(
                    &mut transaction,
                    library_id,
                    library_root_id,
                    &file.relative_path,
                    &mut folder_cache,
                    &mut touched_folders,
                )
                .await?;
            let identity = (file.sort_title.clone(), file.production_year);
            let (item_id, is_new_item) = if let Some(item_id) = movie_cache.get(&identity) {
                (item_id.clone(), false)
            } else {
                let item_id = Uuid::now_v7().to_string();
                movie_cache.insert(identity, item_id.clone());
                new_items.push((item_id.clone(), index));
                new_item_ids.insert(item_id.clone());
                (item_id, true)
            };
            parent_updates.insert(item_id.clone(), parent_folder_id);
            if let Some(provider_ids_json) = file.provider_ids_json.as_deref() {
                provider_updates
                    .entry(item_id.clone())
                    .or_insert_with(|| provider_ids_json.to_owned());
            }
            source_rows.push((index, item_id, is_new_item));
        }

        for chunk in new_items.chunks(BATCH_INSERT_CHUNK_SIZE) {
            let values = std::iter::repeat_n(
                "(?, ?, 'MOVIE', ?, ?, ?, ?, ?, ?, 'LOCAL_CONFIRMED')",
                chunk.len(),
            )
            .collect::<Vec<_>>()
            .join(", ");
            let query = format!(
                "INSERT INTO media_items (
                    id, library_id, item_type, parent_id, title, sort_title,
                    original_title, production_year, provider_ids_json, identification_status
                ) VALUES {values}"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for (item_id, index) in chunk {
                let file = &files[*index];
                statement = statement
                    .bind(item_id)
                    .bind(library_id)
                    .bind(parent_updates.get(item_id).and_then(Option::as_deref))
                    .bind(&file.title)
                    .bind(&file.sort_title)
                    .bind(&file.original_title)
                    .bind(file.production_year)
                    .bind(file.provider_ids_json.as_deref());
            }
            statement
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }

        for (item_id, parent_id) in &parent_updates {
            if !new_item_ids.contains(item_id) {
                self.query(
                    "UPDATE media_items SET parent_id = ?
                     WHERE id = ? AND item_type = 'MOVIE'",
                )
                .bind(parent_id.as_deref())
                .bind(item_id)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            }
        }
        for (item_id, provider_ids_json) in provider_updates {
            self.query(
                "UPDATE media_items
                 SET provider_ids_json = ?
                 WHERE id = ? AND (provider_ids_json IS NULL OR provider_ids_json = '{}')",
            )
            .bind(provider_ids_json)
            .bind(item_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }

        for chunk in source_rows.chunks(BATCH_INSERT_CHUNK_SIZE) {
            let values =
                std::iter::repeat_n("(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'PENDING')", chunk.len())
                    .collect::<Vec<_>>()
                    .join(", ");
            let query = format!(
                "INSERT INTO media_sources (
                    id, item_id, source_kind, filesystem_entry_id,
                    edition_name, quality_label, container, size,
                    external_url, strm_target_kind, is_default, probe_status
                ) VALUES {values}"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for (index, item_id, is_new_item) in chunk {
                let file = &files[*index];
                statement = statement
                    .bind(&file.source_id)
                    .bind(item_id)
                    .bind(&file.source_kind)
                    .bind(&file.filesystem_entry_id)
                    .bind(file.edition_name.as_deref())
                    .bind(file.quality_label.as_deref())
                    .bind(&file.container)
                    .bind(file.size)
                    .bind(file.external_url.as_deref())
                    .bind(file.strm_target_kind.as_deref())
                    .bind(database_flag(*is_new_item));
            }
            statement
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }
        let strm_item_ids = source_rows
            .iter()
            .filter(|(index, _, _)| files[*index].source_kind == "STRM_URL")
            .map(|(_, item_id, _)| item_id)
            .collect::<HashSet<_>>();
        for item_id in strm_item_ids {
            self.query(
                "UPDATE media_items
                 SET poster_fallback_required = 1
                 WHERE id = ?
                   AND NOT EXISTS (
                       SELECT 1 FROM item_images
                       WHERE item_id = media_items.id
                         AND image_type IN ('POSTER', 'THUMB')
                         AND image_index = 0
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
        Ok(new_items.len())
    }

    pub(crate) async fn find_media_item(
        &self,
        library_id: &str,
        sort_title: &str,
        production_year: Option<i64>,
    ) -> Result<Option<StoredMediaItem>, StorageError> {
        let row = match production_year {
            Some(year) => {
                self.query(
                    "SELECT id
                     FROM media_items
                     WHERE library_id = ? AND item_type = 'MOVIE'
                       AND sort_title = ? AND production_year = ?
                       AND removed_at IS NULL",
                )
                .bind(library_id)
                .bind(sort_title)
                .bind(year)
                .fetch_optional(&self.pool)
                .await
            }
            None => {
                self.query(
                    "SELECT id
                     FROM media_items
                     WHERE library_id = ? AND item_type = 'MOVIE'
                       AND sort_title = ? AND production_year IS NULL
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

    pub(crate) async fn movie_metadata_identity_conflicts(
        &self,
        item_id: &str,
        sort_title: &str,
        production_year: i64,
    ) -> Result<bool, StorageError> {
        self.query_scalar::<i64>(
            "SELECT CASE WHEN EXISTS (
                 SELECT 1
                 FROM media_items current_item
                 JOIN media_items conflicting_item
                   ON conflicting_item.library_id = current_item.library_id
                  AND conflicting_item.id <> current_item.id
                  AND conflicting_item.item_type = 'MOVIE'
                  AND conflicting_item.sort_title = ?
                  AND conflicting_item.production_year = ?
                  AND conflicting_item.removed_at IS NULL
                 WHERE current_item.id = ?
                   AND current_item.item_type = 'MOVIE'
                   AND current_item.removed_at IS NULL
             ) THEN 1 ELSE 0 END",
        )
        .bind(sort_title)
        .bind(production_year)
        .bind(item_id)
        .fetch_one(&self.pool)
        .await
        .map(|value| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_media_item_by_identity(
        &self,
        identity_key: &str,
    ) -> Result<Option<StoredMediaItem>, StorageError> {
        self.query("SELECT id FROM media_items WHERE identity_key = ? AND removed_at IS NULL")
            .bind(identity_key)
            .fetch_optional(&self.pool)
            .await
            .map(|row| row.map(stored_media_item))
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn adopt_media_item_identity(
        &self,
        item_id: &str,
        identity_key: &str,
    ) -> Result<bool, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let occupied = self
            .query_scalar::<i64>(
                "SELECT COUNT(*) FROM media_items
                 WHERE identity_key = ? AND id <> ?",
            )
            .bind(identity_key)
            .bind(item_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if occupied != 0 {
            transaction
                .rollback()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(false);
        }
        self.query(
            "UPDATE media_items
             SET identity_key = ?, removed_at = NULL
             WHERE id = ?",
        )
        .bind(identity_key)
        .bind(item_id)
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

    pub(crate) async fn repair_episode_hierarchy_identities(
        &self,
        episode_id: &str,
        series_identity: &str,
        season_identity: &str,
        episode_identity: &str,
    ) -> Result<bool, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let hierarchy = self
            .query_as::<(String, String, String)>(
                "SELECT episode.id, season.id, series.id
                 FROM media_items episode
                 JOIN media_items season
                   ON season.id = episode.parent_id AND season.item_type = 'SEASON'
                 JOIN media_items series
                   ON series.id = episode.series_id AND series.item_type = 'SERIES'
                 WHERE episode.id = ? AND episode.item_type = 'EPISODE'",
            )
            .bind(episode_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let Some((episode_id, season_id, series_id)) = hierarchy else {
            transaction
                .rollback()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(false);
        };

        let conflicts = self
            .query_scalar::<i64>(
                "SELECT COUNT(*)
                 FROM media_items
                 WHERE identity_key IN (?, ?, ?)
                   AND id NOT IN (?, ?, ?)",
            )
            .bind(series_identity)
            .bind(season_identity)
            .bind(episode_identity)
            .bind(&series_id)
            .bind(&season_id)
            .bind(&episode_id)
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if conflicts != 0 {
            transaction
                .rollback()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(false);
        }

        for (item_id, identity_key) in [
            (&series_id, series_identity),
            (&season_id, season_identity),
            (&episode_id, episode_identity),
        ] {
            self.query("UPDATE media_items SET identity_key = ?, removed_at = NULL WHERE id = ?")
                .bind(identity_key)
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

    pub(crate) async fn find_media_item_metadata(
        &self,
        item_id: &str,
    ) -> Result<Option<StoredMediaMetadata>, StorageError> {
        self.query(
            "SELECT mi.item_type, mi.title, mi.original_title, mi.overview, mi.production_year,
                    mi.premiere_date, mi.last_air_date, mi.status, mi.original_language, mi.rating,
                    mi.provider_ids_json, mi.metadata_provenance_json, mi.locked_fields_json,
                    mi.series_id, mi.season_number, mi.episode_number,
                    series.title AS series_title,
                    series.production_year AS series_production_year,
                    series.provider_ids_json AS series_provider_ids_json,
                    libraries.scraper_id AS scraper_id
             FROM media_items mi
             LEFT JOIN media_items series ON series.id = mi.series_id
             LEFT JOIN libraries ON libraries.id = mi.library_id
             WHERE mi.id = ?",
        )
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| {
                let scraper_id = row.get::<Option<String>, _>("scraper_id");
                let series_provider = first_provider_id(
                    row.get("series_provider_ids_json"),
                    None,
                    scraper_id.as_deref(),
                );
                StoredMediaMetadata {
                    item_type: row.get("item_type"),
                    title: row.get("title"),
                    original_title: row.get("original_title"),
                    overview: row.get("overview"),
                    production_year: row.get("production_year"),
                    premiere_date: row.get("premiere_date"),
                    last_air_date: row.get("last_air_date"),
                    status: row.get("status"),
                    original_language: row.get("original_language"),
                    rating: row.get("rating"),
                    provider_ids_json: row.get("provider_ids_json"),
                    scraper_id,
                    provenance_json: row.get("metadata_provenance_json"),
                    locked_fields_json: row.get("locked_fields_json"),
                    series_item_id: row.get("series_id"),
                    series_title: row.get("series_title"),
                    series_production_year: row.get("series_production_year"),
                    series_provider_name: series_provider.as_ref().map(|(name, _)| name.clone()),
                    series_provider_id: series_provider.map(|(_, id)| id),
                    season_number: row.get("season_number"),
                    episode_number: row.get("episode_number"),
                }
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_metadata_refresh_item_ids(
        &self,
        item_id: &str,
    ) -> Result<Vec<String>, StorageError> {
        let item_type = self
            .query_scalar::<String>(
                "SELECT item_type FROM media_items
             WHERE id = ? AND removed_at IS NULL",
            )
            .bind(item_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let Some(item_type) = item_type else {
            return Ok(Vec::new());
        };
        let query = match item_type.as_str() {
            "SERIES" => {
                "SELECT id FROM media_items
                 WHERE removed_at IS NULL AND (id = ? OR series_id = ?)
                 ORDER BY CASE item_type WHEN 'SERIES' THEN 0 WHEN 'SEASON' THEN 1 ELSE 2 END,
                          season_number, episode_number, id"
            }
            "SEASON" => {
                "SELECT id FROM media_items
                 WHERE removed_at IS NULL AND (id = ? OR parent_id = ?)
                 ORDER BY CASE item_type WHEN 'SEASON' THEN 0 ELSE 1 END,
                          episode_number, id"
            }
            _ => "SELECT id FROM media_items WHERE id = ? AND removed_at IS NULL",
        };
        let mut query = self.query_scalar::<String>(query).bind(item_id);
        if matches!(item_type.as_str(), "SERIES" | "SEASON") {
            query = query.bind(item_id);
        }
        query
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn find_media_item_image_identity(
        &self,
        item_id: &str,
    ) -> Result<Option<StoredImageIdentity>, StorageError> {
        self.query(
            "SELECT mi.item_type, mi.provider_ids_json,
                    series.provider_ids_json AS series_provider_ids_json,
                    mi.season_number, mi.episode_number,
                    l.scraper_id
             FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id
             LEFT JOIN media_items series
               ON series.id = COALESCE(mi.series_id, mi.parent_id)
             WHERE mi.id = ? AND mi.removed_at IS NULL",
        )
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| {
                let provider = first_provider_id(
                    row.get("provider_ids_json"),
                    row.get("series_provider_ids_json"),
                    row.get::<Option<String>, _>("scraper_id").as_deref(),
                );
                StoredImageIdentity {
                    item_type: row.get("item_type"),
                    provider_name: provider.as_ref().map(|(name, _)| name.clone()),
                    provider_id: provider.map(|(_, id)| id),
                    season_number: row.get("season_number"),
                    episode_number: row.get("episode_number"),
                }
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_movie_identity(
        &self,
        item_id: &str,
    ) -> Result<Option<StoredMovieIdentity>, StorageError> {
        self.query(
            "SELECT mi.library_id, mi.provider_ids_json, l.scraper_id
             FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id
             WHERE mi.id = ? AND mi.item_type = 'MOVIE' AND mi.removed_at IS NULL",
        )
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.and_then(|row| {
                let scraper_id = row.get::<Option<String>, _>("scraper_id");
                let provider =
                    first_provider_id(row.get("provider_ids_json"), None, scraper_id.as_deref())?;
                Some(StoredMovieIdentity {
                    library_id: row.get("library_id"),
                    provider_name: provider.0,
                    provider_id: provider.1,
                })
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn upsert_collection(
        &self,
        collection: NewCollection<'_>,
    ) -> Result<StoredCollectionRefresh, StorageError> {
        let NewCollection {
            library_id,
            provider,
            provider_id,
            title,
            overview,
            poster_path,
            backdrop_path,
            member_provider_ids,
        } = collection;
        let provider_name = provider.to_ascii_uppercase();
        let provider_key = provider.to_ascii_lowercase();
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let existing = self
            .query(
                "SELECT id, item_id
             FROM collections
             WHERE library_id = ? AND lower(provider) = lower(?) AND provider_id = ?",
            )
            .bind(library_id)
            .bind(provider)
            .bind(provider_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let (collection_id, item_id) = if let Some(row) = existing {
            (row.get::<String, _>("id"), row.get::<String, _>("item_id"))
        } else {
            let collection_id = Uuid::now_v7().to_string();
            let item_id = Uuid::now_v7().to_string();
            let identity_key = format!("collection:{provider_key}:{library_id}:{provider_id}");
            let provider_ids_json = serde_json::json!({
                format!("{provider_key}Collection"): provider_id
            })
            .to_string();
            self.query(
                "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title, original_title,
                    overview, provider_ids_json, identification_status, identity_key
                ) VALUES (?, ?, 'BOX_SET', ?, ?, ?, ?, ?, 'ONLINE_CONFIRMED', ?)",
            )
            .bind(&item_id)
            .bind(library_id)
            .bind(title)
            .bind(title.to_ascii_lowercase())
            .bind(title)
            .bind(overview)
            .bind(provider_ids_json)
            .bind(identity_key)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
            self.query(
                "INSERT INTO collections (
                    id, item_id, library_id, provider, provider_id,
                    title, overview, poster_path, backdrop_path
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&collection_id)
            .bind(&item_id)
            .bind(library_id)
            .bind(&provider_name)
            .bind(provider_id)
            .bind(title)
            .bind(overview)
            .bind(poster_path)
            .bind(backdrop_path)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
            (collection_id, item_id)
        };
        self.query(
            "UPDATE collections
             SET title = ?, overview = ?, poster_path = ?, backdrop_path = ?,
                 updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(title)
        .bind(overview)
        .bind(poster_path)
        .bind(backdrop_path)
        .bind(&collection_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query(
            "UPDATE media_items
             SET title = ?, sort_title = ?, original_title = ?, overview = ?
             WHERE id = ?",
        )
        .bind(title)
        .bind(title.to_ascii_lowercase())
        .bind(title)
        .bind(overview)
        .bind(&item_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query("DELETE FROM collection_items WHERE collection_id = ?")
            .bind(&collection_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let mut member_count = 0_usize;
        for (member_provider, member_provider_id, sort_order) in member_provider_ids {
            let Some(member_item_id) = self
                .query_scalar::<String>(
                    "SELECT mi.id
                 FROM media_items mi
                 JOIN json_each(
                    CASE WHEN json_valid(mi.provider_ids_json)
                         THEN mi.provider_ids_json ELSE '{}' END
                 ) provider_id ON 1 = 1
                 WHERE mi.library_id = ? AND mi.item_type = 'MOVIE'
                   AND mi.removed_at IS NULL
                   AND lower(provider_id.key) = lower(?)
                   AND provider_id.value = ?
                 LIMIT 1",
                )
                .bind(library_id)
                .bind(member_provider)
                .bind(member_provider_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
            else {
                continue;
            };
            self.query(
                "INSERT INTO collection_items (collection_id, item_id, sort_order)
                 VALUES (?, ?, ?)",
            )
            .bind(&collection_id)
            .bind(member_item_id)
            .bind(*sort_order)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
            member_count += 1;
        }
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(StoredCollectionRefresh {
            collection_item_id: item_id,
            member_count,
        })
    }

    pub(crate) async fn list_collection_member_ids_page(
        &self,
        collection_item_id: &str,
        library_ids: Option<&[String]>,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<String>, i64), StorageError> {
        if library_ids.is_some_and(|library_ids| library_ids.is_empty()) {
            return Ok((Vec::new(), 0));
        }
        let library_filter = library_ids
            .map(|library_ids| {
                let placeholders = std::iter::repeat_n("?", library_ids.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(" AND mi.library_id IN ({placeholders})")
            })
            .unwrap_or_default();
        let from_where = format!(
            "FROM collection_items ci
             JOIN collections c ON c.id = ci.collection_id
             JOIN media_items mi ON mi.id = ci.item_id
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             WHERE c.item_id = ? AND mi.removed_at IS NULL
               {CATALOG_VISIBLE_PREDICATE}{library_filter}"
        );
        let mut count_statement = self
            .query_scalar::<i64>(sqlx::AssertSqlSafe(format!("SELECT COUNT(*) {from_where}")))
            .bind(collection_item_id);
        if let Some(library_ids) = library_ids {
            for library_id in library_ids {
                count_statement = count_statement.bind(library_id);
            }
        }
        let total = count_statement
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;

        let mut list_statement = self
            .query(sqlx::AssertSqlSafe(format!(
                "SELECT ci.item_id {from_where}
                 ORDER BY ci.sort_order, ci.item_id
                 LIMIT ? OFFSET ?"
            )))
            .bind(collection_item_id);
        if let Some(library_ids) = library_ids {
            for library_id in library_ids {
                list_statement = list_statement.bind(library_id);
            }
        }
        let rows = list_statement
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok((
            rows.into_iter().map(|row| row.get("item_id")).collect(),
            total,
        ))
    }

    pub(crate) async fn count_pending_metadata_candidates(&self) -> Result<i64, StorageError> {
        self.query_scalar("SELECT COUNT(*) FROM metadata_candidates WHERE status = 'PENDING'")
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_pending_metadata_item_ids(
        &self,
        item_ids: &[String],
    ) -> Result<HashSet<String>, StorageError> {
        let mut pending = HashSet::new();
        for chunk in item_ids.chunks(500) {
            if chunk.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT DISTINCT item_id FROM metadata_candidates
                 WHERE status = 'PENDING' AND item_id IN ({placeholders})"
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

    pub(crate) async fn insert_metadata_candidate(
        &self,
        candidate: NewMetadataCandidate<'_>,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO metadata_candidates (
                id, item_id, provider, provider_id, candidate_json, score, status, expires_at
            ) VALUES (?, ?, ?, ?, ?, ?, 'PENDING', ?)",
        )
        .bind(candidate.id)
        .bind(candidate.item_id)
        .bind(candidate.provider)
        .bind(candidate.provider_id)
        .bind(candidate.candidate_json)
        .bind(candidate.score)
        .bind(candidate.expires_at)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_pending_metadata_candidates(
        &self,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredMetadataCandidate>, StorageError> {
        self.query(
            "SELECT mc.id, mc.item_id, mc.provider, mc.provider_id,
                    mc.candidate_json, mc.score, mc.status, mc.expires_at,
                    mi.title AS item_title
             FROM metadata_candidates mc
             JOIN media_items mi ON mi.id = mc.item_id
             WHERE mc.status = 'PENDING' AND mi.removed_at IS NULL
             ORDER BY mc.created_at, mc.id
             LIMIT ? OFFSET ?",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await
        .map(|rows| rows.into_iter().map(stored_metadata_candidate).collect())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn count_pending_metadata_candidates_for_item(
        &self,
        item_id: &str,
        search: Option<&str>,
    ) -> Result<i64, StorageError> {
        let count = if let Some(search) = search.map(str::trim).filter(|value| !value.is_empty()) {
            let pattern = format!("%{search}%");
            self.query_scalar::<i64>(
                "SELECT COUNT(*) FROM metadata_candidates
                 WHERE item_id = ? AND status = 'PENDING'
                   AND (provider_id LIKE ? OR candidate_json LIKE ?)",
            )
            .bind(item_id)
            .bind(&pattern)
            .bind(&pattern)
            .fetch_one(&self.pool)
            .await
        } else {
            self.query_scalar::<i64>(
                "SELECT COUNT(*) FROM metadata_candidates
                 WHERE item_id = ? AND status = 'PENDING'",
            )
            .bind(item_id)
            .fetch_one(&self.pool)
            .await
        };
        count.map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_pending_metadata_candidates_for_item(
        &self,
        item_id: &str,
        search: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredMetadataCandidate>, StorageError> {
        let rows = if let Some(search) = search.map(str::trim).filter(|value| !value.is_empty()) {
            let pattern = format!("%{search}%");
            self.query(
                "SELECT mc.id, mc.item_id, mc.provider, mc.provider_id,
                        mc.candidate_json, mc.score, mc.status, mc.expires_at,
                        mi.title AS item_title
                 FROM metadata_candidates mc
                 JOIN media_items mi ON mi.id = mc.item_id
                 WHERE mc.item_id = ? AND mc.status = 'PENDING' AND mi.removed_at IS NULL
                   AND (mc.provider_id LIKE ? OR mc.candidate_json LIKE ?)
                 ORDER BY mc.created_at, mc.id LIMIT ? OFFSET ?",
            )
            .bind(item_id)
            .bind(&pattern)
            .bind(&pattern)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        } else {
            self.query(
                "SELECT mc.id, mc.item_id, mc.provider, mc.provider_id,
                        mc.candidate_json, mc.score, mc.status, mc.expires_at,
                        mi.title AS item_title
                 FROM metadata_candidates mc
                 JOIN media_items mi ON mi.id = mc.item_id
                 WHERE mc.item_id = ? AND mc.status = 'PENDING' AND mi.removed_at IS NULL
                 ORDER BY mc.created_at, mc.id LIMIT ? OFFSET ?",
            )
            .bind(item_id)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        };
        rows.map(|rows| rows.into_iter().map(stored_metadata_candidate).collect())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn find_metadata_candidate(
        &self,
        item_id: &str,
        candidate_id: &str,
    ) -> Result<Option<StoredMetadataCandidate>, StorageError> {
        self.query(
            "SELECT mc.id, mc.item_id, mc.provider, mc.provider_id,
                    mc.candidate_json, mc.score, mc.status, mc.expires_at,
                    mi.title AS item_title
             FROM metadata_candidates mc
             JOIN media_items mi ON mi.id = mc.item_id
             WHERE mc.id = ? AND mc.item_id = ?
               AND mi.removed_at IS NULL
             LIMIT 1",
        )
        .bind(candidate_id)
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_metadata_candidate))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_best_pending_metadata_candidate(
        &self,
        item_id: &str,
    ) -> Result<Option<StoredMetadataCandidate>, StorageError> {
        self.query(
            "SELECT mc.id, mc.item_id, mc.provider, mc.provider_id,
                    mc.candidate_json, mc.score, mc.status, mc.expires_at,
                    mi.title AS item_title
             FROM metadata_candidates mc
             JOIN media_items mi ON mi.id = mc.item_id
             WHERE mc.item_id = ? AND mc.status = 'PENDING'
               AND mi.removed_at IS NULL
             ORDER BY mc.score DESC, mc.created_at, mc.id
             LIMIT 1",
        )
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_metadata_candidate))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn select_metadata_candidate(
        &self,
        update: SelectedMetadataUpdate<'_>,
    ) -> Result<bool, StorageError> {
        let sort_title = update.title.to_lowercase();
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.query(
            "UPDATE media_items
             SET title = ?, sort_title = ?, original_title = ?, overview = ?, production_year = ?,
                 premiere_date = COALESCE(?, premiere_date),
                 last_air_date = COALESCE(?, last_air_date),
                 status = COALESCE(?, status),
                 original_language = COALESCE(?, original_language),
                 rating = COALESCE(?, rating),
                 rating_source = CASE WHEN ? IS NULL THEN rating_source ELSE ? END,
                 provider_ids_json = ?,
                 identification_status = CASE WHEN ? = 1 THEN 'PENDING' ELSE 'ONLINE_CONFIRMED' END,
                 metadata_fingerprint = ?, metadata_provenance_json = ?, locked_fields_json = ?,
                 poster_fallback_required = ?
             WHERE id = ? AND removed_at IS NULL",
        )
        .bind(update.title)
        .bind(sort_title)
        .bind(update.original_title)
        .bind(update.overview)
        .bind(update.production_year)
        .bind(update.premiere_date)
        .bind(update.last_air_date)
        .bind(update.status)
        .bind(update.original_language)
        .bind(update.rating)
        .bind(update.rating_source)
        .bind(update.rating_source)
        .bind(update.provider_ids_json)
        .bind(database_flag(update.keep_pending))
        .bind(update.metadata_fingerprint)
        .bind(update.provenance_json)
        .bind(update.locked_fields_json)
        .bind(database_flag(update.poster_fallback_required))
        .bind(update.item_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        let selected = self
            .query(
                "UPDATE metadata_candidates
             SET status = CASE WHEN ? = 1 THEN 'PENDING' ELSE 'SELECTED' END,
                 updated_at = unixepoch()
             WHERE id = ? AND item_id = ? AND status = 'PENDING'",
            )
            .bind(database_flag(update.keep_pending))
            .bind(update.candidate_id)
            .bind(update.item_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if selected.rows_affected() != 1 {
            transaction
                .rollback()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(false);
        }
        self.query(
            "UPDATE metadata_candidates
             SET status = 'REJECTED', updated_at = unixepoch()
             WHERE item_id = ? AND status = 'PENDING' AND id <> ?",
        )
        .bind(update.item_id)
        .bind(update.candidate_id)
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

    pub(crate) async fn count_catalog_items(
        &self,
        library_id: Option<&str>,
    ) -> Result<i64, StorageError> {
        let query = match library_id {
            Some(_) => format!(
                "SELECT COUNT(*) FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 WHERE mi.library_id = ? AND mi.item_type <> 'FOLDER'
                   AND mi.removed_at IS NULL{CATALOG_VISIBLE_PREDICATE}"
            ),
            None => format!(
                "SELECT COUNT(*) FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 WHERE mi.item_type <> 'FOLDER'
                   AND mi.removed_at IS NULL{CATALOG_VISIBLE_PREDICATE}"
            ),
        };
        let mut statement = self.query_scalar::<i64>(sqlx::AssertSqlSafe(query));
        if let Some(library_id) = library_id {
            statement = statement.bind(library_id);
        }
        statement
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn count_catalog_item_types(
        &self,
        library_ids: &[String],
        user_id: &str,
        is_favorite: Option<bool>,
    ) -> Result<StoredCatalogItemCounts, StorageError> {
        if library_ids.is_empty() {
            return Ok(StoredCatalogItemCounts::default());
        }

        let library_placeholders = std::iter::repeat_n("?", library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let favorite_filter = is_favorite.map_or(String::new(), |_| {
            " AND COALESCE(
                (SELECT state_filter.is_favorite
                 FROM user_item_state state_filter
                 WHERE state_filter.user_id = ? AND state_filter.item_id = mi.id),
                0
            ) = ?"
                .to_owned()
        });
        let query = format!(
            "SELECT
                COUNT(CASE WHEN mi.item_type = 'MOVIE' THEN 1 END) AS movie_count,
                COUNT(CASE WHEN mi.item_type = 'SERIES' THEN 1 END) AS series_count,
                COUNT(CASE WHEN mi.item_type = 'EPISODE' THEN 1 END) AS episode_count,
                COUNT(CASE WHEN mi.item_type = 'BOX_SET' THEN 1 END) AS box_set_count,
                COUNT(*) AS item_count
             FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             WHERE mi.removed_at IS NULL
               AND mi.library_id IN ({library_placeholders})
               AND mi.item_type <> 'FOLDER'
               {CATALOG_VISIBLE_PREDICATE}
               {favorite_filter}"
        );
        let mut statement = self.query(sqlx::AssertSqlSafe(query));
        for library_id in library_ids {
            statement = statement.bind(library_id);
        }
        if let Some(is_favorite) = is_favorite {
            statement = statement.bind(user_id).bind(database_flag(is_favorite));
        }
        statement
            .fetch_one(&self.pool)
            .await
            .map(|row| StoredCatalogItemCounts {
                movie_count: row.get("movie_count"),
                series_count: row.get("series_count"),
                episode_count: row.get("episode_count"),
                box_set_count: row.get("box_set_count"),
                item_count: row.get("item_count"),
            })
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn dashboard_stats(&self) -> Result<DashboardStats, StorageError> {
        self.query(
            "SELECT
                COUNT(CASE WHEN mi.item_type = 'MOVIE' THEN 1 END) AS movie_count,
                COUNT(CASE WHEN mi.item_type = 'SERIES' THEN 1 END) AS series_count,
                (SELECT COUNT(*) FROM users WHERE is_disabled = 0) AS user_count
             FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             WHERE mi.removed_at IS NULL",
        )
        .fetch_one(&self.pool)
        .await
        .map(|row| DashboardStats {
            movie_count: row.get("movie_count"),
            series_count: row.get("series_count"),
            user_count: row.get("user_count"),
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn search_catalog_item_ids(
        &self,
        query: &str,
        like_query: &str,
        library_ids: Option<&[String]>,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<String>, i64), StorageError> {
        if self.backend == DatabaseBackend::Postgres {
            return self
                .search_catalog_item_ids_postgres(like_query, library_ids, offset, limit)
                .await;
        }

        if let Some(library_ids) = library_ids
            && library_ids.is_empty()
        {
            return Ok((Vec::new(), 0));
        }
        let library_filter = library_ids.map(|ids| {
            format!(
                " AND mi.library_id IN ({})",
                std::iter::repeat_n("?", ids.len())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        });
        let fts_query = format!(
            "SELECT mi.id FROM media_search
             JOIN media_items mi ON mi.id = media_search.item_id
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             WHERE media_search MATCH ? AND mi.removed_at IS NULL
               AND mi.item_type <> 'FOLDER'{CATALOG_VISIBLE_PREDICATE}{}",
            library_filter.as_deref().unwrap_or_default()
        );
        // Complete-token searches are served by FTS alone. The LIKE branch remains
        // available for partial searches, but is avoided when FTS already has a page.
        let fts_page_query = format!(
            "SELECT matches.id, COUNT(*) OVER() AS total FROM ({fts_query}) matches
             JOIN media_items mi ON mi.id = matches.id
             ORDER BY mi.sort_title, mi.id LIMIT ? OFFSET ?"
        );
        let fts_page = self
            .fetch_catalog_search_page(
                &fts_page_query,
                Some(query),
                None,
                library_ids,
                offset,
                limit,
            )
            .await?;
        if !fts_page.0.is_empty() {
            return Ok(fts_page);
        }

        let like_query_sql = format!(
            "SELECT mi.id FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             WHERE (mi.title LIKE ? OR COALESCE(mi.original_title, '') LIKE ?
                    OR EXISTS (SELECT 1 FROM item_aliases ia
                               WHERE ia.item_id = mi.id AND ia.alias LIKE ?))
               AND mi.removed_at IS NULL
               AND mi.item_type <> 'FOLDER'{CATALOG_VISIBLE_PREDICATE}{}",
            library_filter.as_deref().unwrap_or_default()
        );
        let like_page_query = format!(
            "SELECT matches.id, COUNT(*) OVER() AS total FROM ({like_query_sql}) matches
             JOIN media_items mi ON mi.id = matches.id
             ORDER BY mi.sort_title, mi.id LIMIT ? OFFSET ?"
        );
        if offset == 0 {
            return self
                .fetch_catalog_search_page(
                    &like_page_query,
                    None,
                    Some(like_query),
                    library_ids,
                    offset,
                    limit,
                )
                .await;
        }

        let union_query = format!("{fts_query} UNION {like_query_sql}");
        let union_page_query = format!(
            "SELECT matches.id, COUNT(*) OVER() AS total FROM ({union_query}) matches
             JOIN media_items mi ON mi.id = matches.id
             ORDER BY mi.sort_title, mi.id LIMIT ? OFFSET ?"
        );
        self.fetch_catalog_search_page(
            &union_page_query,
            Some(query),
            Some(like_query),
            library_ids,
            offset,
            limit,
        )
        .await
    }

    async fn search_catalog_item_ids_postgres(
        &self,
        like_query: &str,
        library_ids: Option<&[String]>,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<String>, i64), StorageError> {
        if let Some(library_ids) = library_ids
            && library_ids.is_empty()
        {
            return Ok((Vec::new(), 0));
        }
        let library_filter = library_ids.map(|ids| {
            format!(
                " AND mi.library_id IN ({})",
                std::iter::repeat_n("?", ids.len())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        });
        let like_query_sql = format!(
            "SELECT mi.id FROM media_search ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             WHERE (ms.title ILIKE ? ESCAPE '\\' OR ms.original_title ILIKE ? ESCAPE '\\'
                    OR ms.aliases ILIKE ? ESCAPE '\\')
               AND mi.removed_at IS NULL
               AND mi.item_type <> 'FOLDER'{CATALOG_VISIBLE_PREDICATE}{}",
            library_filter.as_deref().unwrap_or_default()
        );
        let page_query = format!(
            "SELECT matches.id, COUNT(*) OVER() AS total FROM ({like_query_sql}) matches
             JOIN media_items mi ON mi.id = matches.id
             ORDER BY mi.sort_title, mi.id LIMIT ? OFFSET ?"
        );
        self.fetch_catalog_search_page(
            &page_query,
            None,
            Some(like_query),
            library_ids,
            offset,
            limit,
        )
        .await
    }

    async fn fetch_catalog_search_page(
        &self,
        query: &str,
        fts_query: Option<&str>,
        like_query: Option<&str>,
        library_ids: Option<&[String]>,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<String>, i64), StorageError> {
        let mut statement = self.query(sqlx::AssertSqlSafe(query));
        if let Some(fts_query) = fts_query {
            statement = statement.bind(fts_query);
            if let Some(library_ids) = library_ids {
                for library_id in library_ids {
                    statement = statement.bind(library_id);
                }
            }
        }
        if let Some(like_query) = like_query {
            statement = statement.bind(like_query).bind(like_query).bind(like_query);
            if let Some(library_ids) = library_ids {
                for library_id in library_ids {
                    statement = statement.bind(library_id);
                }
            }
        }
        let rows = statement
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let total = rows.first().map(|row| row.get("total")).unwrap_or(0);
        Ok((rows.into_iter().map(|row| row.get("id")).collect(), total))
    }

    pub(crate) async fn list_recent_catalog_item_ids(
        &self,
        library_ids: &[String],
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<String>, i64), StorageError> {
        if library_ids.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let placeholders = std::iter::repeat_n("?", library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let count_query = format!(
            "SELECT CAST(COALESCE(SUM(item_count), 0) AS BIGINT)
             FROM (
                 SELECT COUNT(*) AS item_count
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 WHERE mi.removed_at IS NULL
                   AND mi.item_type <> 'FOLDER'
                   AND mi.has_available_source = 1
                   AND mi.library_id IN ({placeholders})
                 UNION ALL
                 SELECT COUNT(*) AS item_count
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 WHERE mi.removed_at IS NULL
                   AND mi.item_type IN ('SERIES', 'SEASON', 'BOX_SET')
                   AND mi.has_available_source = 0
                   AND mi.library_id IN ({placeholders})
                   AND (
                       EXISTS (
                           SELECT 1
                           FROM media_items visible_child
                           WHERE visible_child.removed_at IS NULL
                             AND visible_child.has_available_source = 1
                             AND (visible_child.parent_id = mi.id OR visible_child.series_id = mi.id)
                       )
                       OR EXISTS (
                           SELECT 1
                           FROM collection_items visible_collection_item
                           JOIN collections visible_collection
                             ON visible_collection.id = visible_collection_item.collection_id
                           JOIN media_items visible_child
                             ON visible_child.id = visible_collection_item.item_id
                           WHERE visible_collection.item_id = mi.id
                             AND visible_child.removed_at IS NULL
                             AND visible_child.has_available_source = 1
                       )
                   )
             ) visible_catalog"
        );
        let mut count_statement = self.query_scalar::<i64>(sqlx::AssertSqlSafe(count_query));
        for library_id in library_ids {
            count_statement = count_statement.bind(library_id);
        }
        for library_id in library_ids {
            count_statement = count_statement.bind(library_id);
        }
        let list_query = format!(
            "WITH visible_catalog AS (
                 SELECT mi.id, mi.library_id, mi.added_at, mi.sort_title
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 WHERE mi.removed_at IS NULL
                   AND mi.item_type <> 'FOLDER'
                   AND mi.has_available_source = 1
                   AND mi.library_id IN ({placeholders})
                 UNION ALL
                 SELECT mi.id, mi.library_id, mi.added_at, mi.sort_title
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 WHERE mi.removed_at IS NULL
                   AND mi.item_type IN ('SERIES', 'SEASON', 'BOX_SET')
                   AND mi.has_available_source = 0
                   AND mi.library_id IN ({placeholders})
                   AND (
                       EXISTS (
                           SELECT 1
                           FROM media_items visible_child
                           WHERE visible_child.removed_at IS NULL
                             AND visible_child.has_available_source = 1
                             AND (visible_child.parent_id = mi.id OR visible_child.series_id = mi.id)
                       )
                       OR EXISTS (
                           SELECT 1
                           FROM collection_items visible_collection_item
                           JOIN collections visible_collection
                             ON visible_collection.id = visible_collection_item.collection_id
                           JOIN media_items visible_child
                             ON visible_child.id = visible_collection_item.item_id
                           WHERE visible_collection.item_id = mi.id
                             AND visible_child.removed_at IS NULL
                             AND visible_child.has_available_source = 1
                       )
                   )
             )
             SELECT id
             FROM visible_catalog
             ORDER BY added_at DESC, sort_title, id
             LIMIT ? OFFSET ?"
        );
        let mut list_statement = self.query(sqlx::AssertSqlSafe(list_query));
        for library_id in library_ids {
            list_statement = list_statement.bind(library_id);
        }
        for library_id in library_ids {
            list_statement = list_statement.bind(library_id);
        }
        let count_future = async {
            count_statement
                .fetch_one(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })
        };
        let list_future = async {
            list_statement
                .bind(limit)
                .bind(offset)
                .fetch_all(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })
        };
        let (total, rows) = tokio::try_join!(count_future, list_future)?;
        Ok((rows.into_iter().map(|row| row.get("id")).collect(), total))
    }

    pub(crate) async fn list_recent_catalog_rows_by_library(
        &self,
        library_ids: &[String],
        limit: i64,
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        if library_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut rows = Vec::new();
        for library_id in library_ids {
            let query =
                "SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                    mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                    mi.title, mi.sort_title, mi.original_title, mi.overview,
                    mi.production_year, mi.rating, mi.rating_source, mi.runtime_ticks,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                     ORDER BY image_index LIMIT 1) AS poster_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                     ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'THUMB'
                     ORDER BY image_index LIMIT 1) AS thumb_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'LOGO'
                     ORDER BY image_index LIMIT 1) AS logo_image_tag,
                    ms.id AS source_id, ms.source_kind, ms.container, ms.size, ms.external_url,
                    ms.edition_name, ms.quality_label,
                    ms.bitrate, ms.duration_ticks, ms.is_default, ms.probe_status,
                    mt.id AS stream_id, mt.stream_index, mt.stream_type,
                    mt.codec, mt.language, mt.title AS stream_title,
                    mt.details_json AS stream_details_json,
                    mt.is_external AS stream_is_external,
                    mt.is_default AS stream_is_default,
                    mt.is_forced AS stream_is_forced
             FROM (
                 WITH visible_catalog AS (
                     SELECT mi.id, mi.library_id, mi.added_at, mi.sort_title
                     FROM media_items mi
                     JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                     WHERE mi.library_id = ?
                       AND mi.item_type IN ('MOVIE', 'SERIES')
                       AND mi.removed_at IS NULL
                       AND mi.has_available_source = 1
                     UNION ALL
                     SELECT mi.id, mi.library_id, mi.added_at, mi.sort_title
                     FROM media_items mi
                     JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                     WHERE mi.library_id = ?
                       AND mi.item_type = 'SERIES'
                       AND mi.removed_at IS NULL
                       AND mi.has_available_source = 0
                       AND (
                           EXISTS (
                               SELECT 1
                               FROM media_items visible_child
                               WHERE visible_child.removed_at IS NULL
                                 AND visible_child.has_available_source = 1
                                 AND (visible_child.parent_id = mi.id OR visible_child.series_id = mi.id)
                           )
                           OR EXISTS (
                               SELECT 1
                               FROM collection_items visible_collection_item
                               JOIN collections visible_collection
                                 ON visible_collection.id = visible_collection_item.collection_id
                               JOIN media_items visible_child
                                 ON visible_child.id = visible_collection_item.item_id
                               WHERE visible_collection.item_id = mi.id
                                 AND visible_child.removed_at IS NULL
                                 AND visible_child.has_available_source = 1
                           )
                       )
                 )
                 SELECT id, library_id
                 FROM visible_catalog
                 ORDER BY added_at DESC, sort_title ASC, id ASC
                 LIMIT ?
             ) ranked
             JOIN media_items mi ON mi.id = ranked.id
             LEFT JOIN media_sources ms
               ON ms.item_id = mi.id
              AND EXISTS (
                  SELECT 1 FROM filesystem_entries fe
                  WHERE fe.id = ms.filesystem_entry_id AND fe.is_missing = 0
              )
             LEFT JOIN media_streams mt ON mt.media_source_id = ms.id
             ORDER BY mi.added_at DESC, mi.sort_title ASC, mi.id ASC,
                      ms.id, mt.stream_index";
            let library_rows = self
                .fetch_catalog_rows(
                    query,
                    &[
                        CatalogBind::Text(library_id),
                        CatalogBind::Text(library_id),
                        CatalogBind::Integer(limit),
                    ],
                )
                .await?;
            rows.extend(library_rows);
        }
        Ok(rows)
    }

    pub(crate) async fn list_recommended_catalog_rows(
        &self,
        user_id: &str,
        library_ids: &[String],
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        if library_ids.is_empty() {
            return Ok(Vec::new());
        }
        let max_function = self.scalar_max_function();
        let min_function = self.scalar_min_function();
        let placeholders = std::iter::repeat_n("?", library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "WITH ranked AS (
                 SELECT mi.id,
                        (
                            CASE WHEN COALESCE(us.is_favorite, 0) = 1 THEN 100 ELSE 0 END
                            + CASE WHEN us.item_id IS NULL THEN 35 ELSE 0 END
                            + CASE WHEN COALESCE(us.position_ticks, 0) > 0
                                      AND COALESCE(us.is_played, 0) = 0 THEN 55 ELSE 0 END
                            + CASE WHEN us.item_id IS NOT NULL
                                      AND COALESCE(us.is_played, 0) = 0 THEN 20 ELSE 0 END
                            + CASE WHEN COALESCE(us.is_played, 0) = 1 THEN -35 ELSE 0 END
                            + {min_function}(30, {max_function}(0, 30 - CAST((unixepoch() - mi.added_at) / 86400 AS INTEGER)))
                            + CASE WHEN us.last_played_at IS NULL THEN 0 ELSE
                                {min_function}(30, {max_function}(0, 30 - CAST((unixepoch() - us.last_played_at) / 86400 AS INTEGER)))
                              END
                        ) AS recommendation_score,
                        mi.added_at,
                        mi.sort_title
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 LEFT JOIN user_item_state us
                   ON us.item_id = mi.id AND us.user_id = ?
                 WHERE mi.removed_at IS NULL{CATALOG_VISIBLE_PREDICATE}
                   AND mi.item_type IN ('MOVIE', 'SERIES')
                   AND mi.library_id IN ({placeholders})
                 ORDER BY recommendation_score DESC, mi.added_at DESC,
                          mi.sort_title, mi.id
                 LIMIT ? OFFSET ?
             )
             SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                    mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                    mi.title, mi.sort_title, mi.original_title, mi.overview,
                    mi.production_year, mi.rating, mi.rating_source, mi.runtime_ticks,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                     ORDER BY image_index LIMIT 1) AS poster_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                     ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'THUMB'
                     ORDER BY image_index LIMIT 1) AS thumb_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'LOGO'
                     ORDER BY image_index LIMIT 1) AS logo_image_tag,
                    ms.id AS source_id, ms.source_kind, ms.container, ms.size, ms.external_url,
                    ms.edition_name, ms.quality_label,
                    ms.bitrate, ms.duration_ticks, ms.is_default, ms.probe_status,
                    mt.id AS stream_id, mt.stream_index, mt.stream_type,
                    mt.codec, mt.language, mt.title AS stream_title,
                    mt.details_json AS stream_details_json,
                    mt.is_external AS stream_is_external,
                    mt.is_default AS stream_is_default,
                    mt.is_forced AS stream_is_forced
             FROM ranked
             JOIN media_items mi ON mi.id = ranked.id
             LEFT JOIN media_sources ms
               ON ms.item_id = mi.id
              AND EXISTS (
                  SELECT 1 FROM filesystem_entries fe
                  WHERE fe.id = ms.filesystem_entry_id AND fe.is_missing = 0
              )
             LEFT JOIN media_streams mt ON mt.media_source_id = ms.id
             ORDER BY ranked.recommendation_score DESC, mi.added_at DESC,
                      mi.sort_title, mi.id, ms.id, mt.stream_index"
        );
        let mut binds = Vec::with_capacity(library_ids.len() + 3);
        binds.push(CatalogBind::Text(user_id));
        binds.extend(library_ids.iter().map(|value| CatalogBind::Text(value)));
        binds.push(CatalogBind::Integer(limit));
        binds.push(CatalogBind::Integer(offset));
        self.fetch_catalog_rows(&query, &binds).await
    }

    pub(crate) async fn count_catalog_children(
        &self,
        parent_id: &str,
        item_type: &str,
    ) -> Result<i64, StorageError> {
        let query = format!(
            "SELECT COUNT(*) FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             WHERE mi.parent_id = ? AND mi.item_type = ? AND mi.removed_at IS NULL
               {CATALOG_VISIBLE_PREDICATE}"
        );
        self.query_scalar::<i64>(sqlx::AssertSqlSafe(query))
            .bind(parent_id)
            .bind(item_type)
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_episode_counts(
        &self,
        item_ids: &[String],
    ) -> Result<HashMap<String, i64>, StorageError> {
        if item_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = std::iter::repeat_n("?", item_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "SELECT parent.id,
                    COUNT(DISTINCT CASE
                        WHEN parent.item_type = 'SERIES' THEN
                            COALESCE(CAST(child.season_number AS TEXT), '') || ':' ||
                            COALESCE(CAST(child.episode_number AS TEXT), child.id)
                        ELSE COALESCE(CAST(child.episode_number AS TEXT), child.id)
                    END) AS episode_count
             FROM media_items parent
             JOIN libraries l ON l.id = parent.library_id AND l.is_enabled = 1
             LEFT JOIN media_items child
               ON child.item_type = 'EPISODE' AND child.removed_at IS NULL
              AND ((parent.item_type = 'SERIES' AND child.series_id = parent.id)
                OR (parent.item_type = 'SEASON' AND child.parent_id = parent.id))
              AND EXISTS (
                  SELECT 1
                  FROM media_sources child_source
                  JOIN filesystem_entries child_entry
                    ON child_entry.id = child_source.filesystem_entry_id
                  WHERE child_source.item_id = child.id
                    AND child_entry.is_missing = 0
              )
             WHERE parent.id IN ({placeholders})
               AND parent.item_type IN ('SERIES', 'SEASON')
               AND parent.removed_at IS NULL
             GROUP BY parent.id"
        );
        let mut statement = self.query(sqlx::AssertSqlSafe(query));
        for item_id in item_ids {
            statement = statement.bind(item_id);
        }
        statement
            .fetch_all(&self.pool)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(|row| (row.get("id"), row.get("episode_count")))
                    .collect()
            })
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_catalog_children(
        &self,
        parent_id: &str,
        item_type: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        let query = format!(
            "SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                    mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                    mi.title, mi.sort_title, mi.original_title, mi.overview,
                    mi.production_year, mi.rating, mi.rating_source, mi.runtime_ticks,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                     ORDER BY image_index LIMIT 1) AS poster_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                     ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'THUMB'
                     ORDER BY image_index LIMIT 1) AS thumb_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'LOGO'
                     ORDER BY image_index LIMIT 1) AS logo_image_tag,
                    ms.id AS source_id, ms.source_kind, ms.container, ms.size, ms.external_url,
                    ms.edition_name, ms.quality_label,
                    ms.bitrate, ms.duration_ticks, ms.is_default, ms.probe_status,
                    mt.id AS stream_id, mt.stream_index, mt.stream_type,
                    mt.codec, mt.language, mt.title AS stream_title,
                    mt.details_json AS stream_details_json,
                    mt.is_external AS stream_is_external,
                    mt.is_default AS stream_is_default,
                    mt.is_forced AS stream_is_forced
             FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             LEFT JOIN media_sources ms
               ON ms.item_id = mi.id
              AND EXISTS (
                  SELECT 1 FROM filesystem_entries fe
                  WHERE fe.id = ms.filesystem_entry_id AND fe.is_missing = 0
              )
             LEFT JOIN media_streams mt ON mt.media_source_id = ms.id
             WHERE mi.parent_id = ? AND mi.item_type = ? AND mi.removed_at IS NULL
               {CATALOG_VISIBLE_PREDICATE}
             ORDER BY mi.season_number, mi.episode_number, mi.sort_title, mi.id,
                      ms.id, mt.stream_index
             LIMIT ? OFFSET ?"
        );
        self.fetch_catalog_rows(
            &query,
            &[
                CatalogBind::Text(parent_id),
                CatalogBind::Text(item_type),
                CatalogBind::Integer(limit),
                CatalogBind::Integer(offset),
            ],
        )
        .await
    }

    pub(crate) async fn list_series_episode_ids(
        &self,
        series_id: &str,
        season_id: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<String>, i64), StorageError> {
        let season_filter = if season_id.is_some() {
            " AND mi.parent_id = ?"
        } else {
            ""
        };
        let count_sql = format!(
            "SELECT COUNT(*)
             FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             WHERE mi.series_id = ? AND mi.item_type = 'EPISODE'
               AND mi.removed_at IS NULL{season_filter}
               {CATALOG_VISIBLE_PREDICATE}"
        );
        let mut count_statement = self
            .query_scalar::<i64>(sqlx::AssertSqlSafe(count_sql))
            .bind(series_id);
        if let Some(season_id) = season_id {
            count_statement = count_statement.bind(season_id);
        }
        let total = count_statement
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let list_sql = format!(
            "SELECT mi.id
             FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             WHERE mi.series_id = ? AND mi.item_type = 'EPISODE'
               AND mi.removed_at IS NULL{season_filter}
               {CATALOG_VISIBLE_PREDICATE}
             ORDER BY mi.season_number, mi.episode_number, mi.sort_title, mi.id
             LIMIT ? OFFSET ?"
        );
        let mut list_statement = self.query(sqlx::AssertSqlSafe(list_sql)).bind(series_id);
        if let Some(season_id) = season_id {
            list_statement = list_statement.bind(season_id);
        }
        let rows = list_statement
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok((rows.into_iter().map(|row| row.get("id")).collect(), total))
    }

    pub(crate) async fn count_resume_items(
        &self,
        user_id: &str,
        library_ids: &[String],
        item_types: &[&str],
        played_percent: i64,
        minimum_ticks: i64,
    ) -> Result<i64, StorageError> {
        if library_ids.is_empty() || item_types.is_empty() {
            return Ok(0);
        }
        let item_type_placeholders = std::iter::repeat_n("?", item_types.len())
            .collect::<Vec<_>>()
            .join(", ");
        let library_placeholders = std::iter::repeat_n("?", library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let runtime_ticks = resume_runtime_ticks_sql();
        let statement_sql = format!(
            "WITH candidates AS (
                 SELECT us.position_ticks,
                        {runtime_ticks} AS resume_runtime_ticks
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 JOIN user_item_state us ON us.item_id = mi.id AND us.user_id = ?
                 WHERE mi.item_type IN ({item_type_placeholders}) AND mi.removed_at IS NULL{CATALOG_VISIBLE_PREDICATE}
                   AND us.is_played = 0 AND us.position_ticks >= ?
                   AND mi.library_id IN ({library_placeholders})
             )
             SELECT COUNT(*) FROM candidates
             WHERE resume_runtime_ticks > 0
               AND position_ticks * 100 < resume_runtime_ticks * ?"
        );
        let mut statement = self
            .query_scalar::<i64>(sqlx::AssertSqlSafe(statement_sql))
            .bind(user_id);
        for item_type in item_types {
            statement = statement.bind(*item_type);
        }
        statement = statement.bind(minimum_ticks);
        for library_id in library_ids {
            statement = statement.bind(library_id);
        }
        statement
            .bind(played_percent)
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_resume_items(
        &self,
        query: &ResumeItemsQuery<'_>,
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        if query.library_ids.is_empty() || query.item_types.is_empty() {
            return Ok(Vec::new());
        }
        let item_type_placeholders = std::iter::repeat_n("?", query.item_types.len())
            .collect::<Vec<_>>()
            .join(", ");
        let library_placeholders = std::iter::repeat_n("?", query.library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let runtime_ticks = resume_runtime_ticks_sql();
        let statement_sql = format!(
            "WITH candidates AS (
                 SELECT mi.id, mi.sort_title, us.position_ticks, us.last_played_at,
                        {runtime_ticks} AS resume_runtime_ticks
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 JOIN user_item_state us ON us.item_id = mi.id AND us.user_id = ?
                 WHERE mi.item_type IN ({item_type_placeholders}) AND mi.removed_at IS NULL{CATALOG_VISIBLE_PREDICATE}
                   AND us.is_played = 0 AND us.position_ticks >= ?
                   AND mi.library_id IN ({library_placeholders})
             ),
             ranked AS (
                 SELECT id, sort_title, last_played_at
                 FROM candidates
                 WHERE resume_runtime_ticks > 0
                   AND position_ticks * 100 < resume_runtime_ticks * ?
                 ORDER BY last_played_at DESC, sort_title, id
                 LIMIT ? OFFSET ?
             )
             SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                    mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                    mi.title, mi.sort_title, mi.original_title, mi.overview,
                    mi.production_year, mi.rating, mi.rating_source, mi.runtime_ticks,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                     ORDER BY image_index LIMIT 1) AS poster_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                     ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'THUMB'
                     ORDER BY image_index LIMIT 1) AS thumb_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'LOGO'
                     ORDER BY image_index LIMIT 1) AS logo_image_tag,
                    ms.id AS source_id, ms.source_kind, ms.container, ms.size, ms.external_url,
                    ms.edition_name, ms.quality_label,
                    ms.bitrate, ms.duration_ticks, ms.is_default, ms.probe_status,
                    mt.id AS stream_id, mt.stream_index, mt.stream_type,
                    mt.codec, mt.language, mt.title AS stream_title,
                    mt.details_json AS stream_details_json,
                    mt.is_external AS stream_is_external,
                    mt.is_default AS stream_is_default,
                    mt.is_forced AS stream_is_forced
             FROM ranked
             JOIN media_items mi ON mi.id = ranked.id
             LEFT JOIN media_sources ms
               ON ms.item_id = mi.id
              AND EXISTS (
                  SELECT 1 FROM filesystem_entries fe
                  WHERE fe.id = ms.filesystem_entry_id AND fe.is_missing = 0
              )
             LEFT JOIN media_streams mt ON mt.media_source_id = ms.id
             ORDER BY ranked.last_played_at DESC, ranked.sort_title, ranked.id,
                      ms.id, mt.stream_index"
        );
        let mut binds = Vec::with_capacity(query.item_types.len() + query.library_ids.len() + 5);
        binds.push(CatalogBind::Text(query.user_id));
        binds.extend(query.item_types.iter().copied().map(CatalogBind::Text));
        binds.push(CatalogBind::Integer(query.minimum_ticks));
        binds.extend(
            query
                .library_ids
                .iter()
                .map(|value| CatalogBind::Text(value)),
        );
        binds.push(CatalogBind::Integer(query.played_percent));
        binds.push(CatalogBind::Integer(query.limit));
        binds.push(CatalogBind::Integer(query.offset));
        self.fetch_catalog_rows(&statement_sql, &binds).await
    }

    pub(crate) async fn count_progress_items(
        &self,
        user_id: &str,
        library_ids: &[String],
        item_types: &[&str],
        series_id: Option<&str>,
    ) -> Result<i64, StorageError> {
        if library_ids.is_empty() || item_types.is_empty() {
            return Ok(0);
        }
        let item_type_placeholders = std::iter::repeat_n("?", item_types.len())
            .collect::<Vec<_>>()
            .join(", ");
        let library_placeholders = std::iter::repeat_n("?", library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let series_predicate = series_id.map(|_| " AND mi.series_id = ?").unwrap_or("");
        let query = format!(
            "SELECT COUNT(*) FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             JOIN user_item_state us ON us.item_id = mi.id AND us.user_id = ?
             WHERE mi.item_type IN ({item_type_placeholders}) AND mi.removed_at IS NULL{CATALOG_VISIBLE_PREDICATE}
               AND us.is_played = 0 AND us.position_ticks > 0
               AND mi.library_id IN ({library_placeholders}){series_predicate}"
        );
        let mut statement = self
            .query_scalar::<i64>(sqlx::AssertSqlSafe(query))
            .bind(user_id);
        for item_type in item_types {
            statement = statement.bind(*item_type);
        }
        for library_id in library_ids {
            statement = statement.bind(library_id);
        }
        if let Some(series_id) = series_id {
            statement = statement.bind(series_id);
        }
        statement
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_progress_items(
        &self,
        user_id: &str,
        library_ids: &[String],
        item_types: &[&str],
        series_id: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        if library_ids.is_empty() || item_types.is_empty() {
            return Ok(Vec::new());
        }
        let item_type_placeholders = std::iter::repeat_n("?", item_types.len())
            .collect::<Vec<_>>()
            .join(", ");
        let library_placeholders = std::iter::repeat_n("?", library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let series_predicate = series_id.map(|_| " AND mi.series_id = ?").unwrap_or("");
        let query = format!(
            "SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                    mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                    mi.title, mi.sort_title, mi.original_title, mi.overview,
                    mi.production_year, mi.rating, mi.rating_source, mi.runtime_ticks,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                     ORDER BY image_index LIMIT 1) AS poster_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                     ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'THUMB'
                     ORDER BY image_index LIMIT 1) AS thumb_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'LOGO'
                     ORDER BY image_index LIMIT 1) AS logo_image_tag,
                    ms.id AS source_id, ms.source_kind, ms.container, ms.size, ms.external_url,
                    ms.edition_name, ms.quality_label,
                    ms.bitrate, ms.duration_ticks, ms.is_default, ms.probe_status,
                    mt.id AS stream_id, mt.stream_index, mt.stream_type,
                    mt.codec, mt.language, mt.title AS stream_title,
                    mt.details_json AS stream_details_json,
                    mt.is_external AS stream_is_external,
                    mt.is_default AS stream_is_default,
                    mt.is_forced AS stream_is_forced
             FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             JOIN user_item_state us ON us.item_id = mi.id AND us.user_id = ?
             LEFT JOIN media_sources ms
               ON ms.item_id = mi.id
              AND EXISTS (
                  SELECT 1 FROM filesystem_entries fe
                  WHERE fe.id = ms.filesystem_entry_id AND fe.is_missing = 0
              )
             LEFT JOIN media_streams mt ON mt.media_source_id = ms.id
             WHERE mi.item_type IN ({item_type_placeholders}) AND mi.removed_at IS NULL{CATALOG_VISIBLE_PREDICATE}
               AND us.is_played = 0 AND us.position_ticks > 0
               AND mi.library_id IN ({library_placeholders}){series_predicate}
             ORDER BY us.last_played_at DESC, mi.series_id, mi.season_number,
                      mi.episode_number, mi.id
             LIMIT ? OFFSET ?"
        );
        let mut binds = Vec::with_capacity(item_types.len() + library_ids.len() + 4);
        binds.push(CatalogBind::Text(user_id));
        binds.extend(item_types.iter().copied().map(CatalogBind::Text));
        binds.extend(library_ids.iter().map(|value| CatalogBind::Text(value)));
        if let Some(series_id) = series_id {
            binds.push(CatalogBind::Text(series_id));
        }
        binds.push(CatalogBind::Integer(limit));
        binds.push(CatalogBind::Integer(offset));
        self.fetch_catalog_rows(&query, &binds).await
    }

    pub(crate) async fn list_filtered_catalog_rows(
        &self,
        filter: &CatalogFilterQuery<'_>,
    ) -> Result<(Vec<StoredCatalogRow>, i64), StorageError> {
        if filter.library_ids.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let (where_clause, filter_binds) = catalog_filter_where_clause(filter);
        let count_query = format!(
            "SELECT COUNT(*) FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             {where_clause}"
        );
        let mut count_statement = self.query_scalar::<i64>(sqlx::AssertSqlSafe(count_query));
        for bind in &filter_binds {
            count_statement = match bind {
                CatalogBind::Text(value) => count_statement.bind(*value),
                CatalogBind::Integer(value) => count_statement.bind(*value),
            };
        }
        let item_order = match (filter.sort_by, filter.descending) {
            (CatalogSort::DateCreated, true) => "mi.added_at DESC, LOWER(mi.title) ASC, mi.id ASC",
            (CatalogSort::DateCreated, false) => "mi.added_at ASC, LOWER(mi.title) ASC, mi.id ASC",
            (CatalogSort::PremiereDate, true) => {
                "CASE WHEN NULLIF(mi.premiere_date, '') IS NULL THEN 1 ELSE 0 END ASC,
                 mi.premiere_date DESC, LOWER(mi.title) ASC, mi.id ASC"
            }
            (CatalogSort::PremiereDate, false) => {
                "CASE WHEN NULLIF(mi.premiere_date, '') IS NULL THEN 1 ELSE 0 END ASC,
                 mi.premiere_date ASC, LOWER(mi.title) ASC, mi.id ASC"
            }
            (CatalogSort::Rating, true) => {
                "CASE WHEN mi.rating IS NULL THEN 1 ELSE 0 END ASC,
                 mi.rating DESC, LOWER(mi.title) ASC, mi.id ASC"
            }
            (CatalogSort::Rating, false) => {
                "CASE WHEN mi.rating IS NULL THEN 1 ELSE 0 END ASC,
                 mi.rating ASC, LOWER(mi.title) ASC, mi.id ASC"
            }
            (CatalogSort::Name, true) => "LOWER(mi.title) DESC, mi.id DESC",
            (CatalogSort::Name, false) => "LOWER(mi.title) ASC, mi.id ASC",
        };
        let query = format!(
            "SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                    mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                    mi.title, mi.sort_title, mi.original_title, mi.overview,
                    mi.production_year, mi.rating, mi.rating_source, mi.runtime_ticks,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                     ORDER BY image_index LIMIT 1) AS poster_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                     ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'THUMB'
                     ORDER BY image_index LIMIT 1) AS thumb_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'LOGO'
                     ORDER BY image_index LIMIT 1) AS logo_image_tag,
                    ms.id AS source_id, ms.source_kind, ms.container, ms.size, ms.external_url,
                    ms.edition_name, ms.quality_label,
                    ms.bitrate, ms.duration_ticks, ms.is_default, ms.probe_status,
                    mt.id AS stream_id, mt.stream_index, mt.stream_type,
                    mt.codec, mt.language, mt.title AS stream_title,
                    mt.details_json AS stream_details_json,
                    mt.is_external AS stream_is_external,
                    mt.is_default AS stream_is_default,
                    mt.is_forced AS stream_is_forced
             FROM (
                 SELECT mi.id
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 {where_clause}
                 ORDER BY {item_order}
                 LIMIT ? OFFSET ?
             ) selected
             JOIN media_items mi ON mi.id = selected.id
             LEFT JOIN media_sources ms
               ON ms.item_id = mi.id
              AND EXISTS (
                  SELECT 1 FROM filesystem_entries fe
                  WHERE fe.id = ms.filesystem_entry_id AND fe.is_missing = 0
              )
             LEFT JOIN media_streams mt ON mt.media_source_id = ms.id
             ORDER BY {item_order}, ms.id, mt.stream_index"
        );
        let mut list_binds = filter_binds.clone();
        list_binds.push(CatalogBind::Integer(filter.limit));
        list_binds.push(CatalogBind::Integer(filter.offset));
        let total_future = async {
            count_statement
                .fetch_one(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })
        };
        let rows_future = self.fetch_catalog_rows(&query, &list_binds);
        let (total, rows) = tokio::try_join!(total_future, rows_future)?;
        Ok((rows, total))
    }

    pub(crate) async fn list_catalog_rows(
        &self,
        library_id: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        let (query, binds) = match library_id {
            Some(library_id) => (
                format!(
                    "SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                        mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                        mi.title, mi.sort_title, mi.original_title, mi.overview,
                        mi.production_year, mi.rating, mi.rating_source, mi.runtime_ticks,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                         ORDER BY image_index LIMIT 1) AS poster_image_tag,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                         ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'THUMB'
                         ORDER BY image_index LIMIT 1) AS thumb_image_tag,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'LOGO'
                         ORDER BY image_index LIMIT 1) AS logo_image_tag,
                        ms.id AS source_id, ms.source_kind, ms.container, ms.size, ms.external_url,
                        ms.edition_name, ms.quality_label,
                        ms.bitrate, ms.duration_ticks, ms.is_default, ms.probe_status,
                        mt.id AS stream_id, mt.stream_index, mt.stream_type,
                        mt.codec, mt.language, mt.title AS stream_title,
                        mt.details_json AS stream_details_json,
                        mt.is_external AS stream_is_external,
                        mt.is_default AS stream_is_default,
                        mt.is_forced AS stream_is_forced
                 FROM (
                     SELECT mi.id, mi.library_id, mi.item_type,
                            mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                            mi.title, mi.sort_title,
                            mi.original_title, mi.overview, mi.production_year,
                            mi.rating, mi.rating_source, mi.runtime_ticks
                     FROM media_items mi
                     JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 WHERE mi.library_id = ? AND mi.item_type <> 'FOLDER'
                   AND mi.removed_at IS NULL{CATALOG_VISIBLE_PREDICATE}
                     ORDER BY mi.sort_title, mi.id
                     LIMIT ? OFFSET ?
                 ) mi
                 LEFT JOIN media_sources ms
                   ON ms.item_id = mi.id
                  AND EXISTS (
                      SELECT 1 FROM filesystem_entries fe
                      WHERE fe.id = ms.filesystem_entry_id AND fe.is_missing = 0
                  )
                 LEFT JOIN media_streams mt ON mt.media_source_id = ms.id
                 ORDER BY mi.sort_title, mi.id, ms.id, mt.stream_index"
                ),
                vec![
                    CatalogBind::Text(library_id),
                    CatalogBind::Integer(limit),
                    CatalogBind::Integer(offset),
                ],
            ),
            None => (
                format!(
                    "SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                        mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                        mi.title, mi.sort_title, mi.original_title, mi.overview,
                        mi.production_year, mi.rating, mi.rating_source, mi.runtime_ticks,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                         ORDER BY image_index LIMIT 1) AS poster_image_tag,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                         ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'THUMB'
                         ORDER BY image_index LIMIT 1) AS thumb_image_tag,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'LOGO'
                         ORDER BY image_index LIMIT 1) AS logo_image_tag,
                        ms.id AS source_id, ms.source_kind, ms.container, ms.size, ms.external_url,
                        ms.edition_name, ms.quality_label,
                        ms.bitrate, ms.duration_ticks, ms.is_default, ms.probe_status,
                        mt.id AS stream_id, mt.stream_index, mt.stream_type,
                        mt.codec, mt.language, mt.title AS stream_title,
                        mt.details_json AS stream_details_json,
                        mt.is_external AS stream_is_external,
                        mt.is_default AS stream_is_default,
                        mt.is_forced AS stream_is_forced
                 FROM (
                     SELECT mi.id, mi.library_id, mi.item_type,
                            mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                            mi.title, mi.sort_title,
                            mi.original_title, mi.overview, mi.production_year,
                            mi.rating, mi.rating_source, mi.runtime_ticks
                     FROM media_items mi
                     JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                     WHERE mi.item_type <> 'FOLDER'
                       AND mi.removed_at IS NULL{CATALOG_VISIBLE_PREDICATE}
                     ORDER BY mi.sort_title, mi.id
                     LIMIT ? OFFSET ?
                 ) mi
                 LEFT JOIN media_sources ms
                   ON ms.item_id = mi.id
                  AND EXISTS (
                      SELECT 1 FROM filesystem_entries fe
                      WHERE fe.id = ms.filesystem_entry_id AND fe.is_missing = 0
                  )
                 LEFT JOIN media_streams mt ON mt.media_source_id = ms.id
                 ORDER BY mi.sort_title, mi.id, ms.id, mt.stream_index"
                ),
                vec![CatalogBind::Integer(limit), CatalogBind::Integer(offset)],
            ),
        };
        self.fetch_catalog_rows(&query, &binds).await
    }

    pub(crate) async fn find_catalog_rows(
        &self,
        item_id: &str,
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        let query = format!(
            "SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                    mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                    mi.title, mi.sort_title, mi.original_title, mi.overview,
                    mi.production_year, mi.rating, mi.rating_source, mi.runtime_ticks,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                     ORDER BY image_index LIMIT 1) AS poster_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                     ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'THUMB'
                     ORDER BY image_index LIMIT 1) AS thumb_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'LOGO'
                     ORDER BY image_index LIMIT 1) AS logo_image_tag,
                    ms.id AS source_id, ms.source_kind, ms.container, ms.size, ms.external_url,
                    ms.edition_name, ms.quality_label,
                    ms.bitrate, ms.duration_ticks, ms.is_default, ms.probe_status,
                    mt.id AS stream_id, mt.stream_index, mt.stream_type,
                    mt.codec, mt.language, mt.title AS stream_title,
                    mt.details_json AS stream_details_json,
                    mt.is_external AS stream_is_external,
                    mt.is_default AS stream_is_default,
                    mt.is_forced AS stream_is_forced
             FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             LEFT JOIN media_sources ms
               ON ms.item_id = mi.id
              AND EXISTS (
                  SELECT 1 FROM filesystem_entries fe
                  WHERE fe.id = ms.filesystem_entry_id AND fe.is_missing = 0
              )
             LEFT JOIN media_streams mt ON mt.media_source_id = ms.id
             WHERE mi.id = ? AND mi.removed_at IS NULL{CATALOG_VISIBLE_PREDICATE}
             ORDER BY mi.sort_title, mi.id, ms.id, mt.stream_index",
        );
        self.fetch_catalog_rows(&query, &[CatalogBind::Text(item_id)])
            .await
    }

    pub(crate) async fn list_catalog_rows_by_ids(
        &self,
        item_ids: &[String],
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        let mut rows = Vec::new();
        for item_ids in item_ids.chunks(500) {
            if item_ids.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", item_ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                        mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                        mi.title, mi.sort_title, mi.original_title, mi.overview,
                        mi.production_year, mi.rating, mi.rating_source, mi.runtime_ticks,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                         ORDER BY image_index LIMIT 1) AS poster_image_tag,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                         ORDER BY image_index LIMIT 1) AS fanart_image_tag,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'THUMB'
                         ORDER BY image_index LIMIT 1) AS thumb_image_tag,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'LOGO'
                         ORDER BY image_index LIMIT 1) AS logo_image_tag,
                        ms.id AS source_id, ms.source_kind, ms.container, ms.size, ms.external_url,
                        ms.edition_name, ms.quality_label,
                        ms.bitrate, ms.duration_ticks, ms.is_default, ms.probe_status,
                        mt.id AS stream_id, mt.stream_index, mt.stream_type,
                        mt.codec, mt.language, mt.title AS stream_title,
                        mt.details_json AS stream_details_json,
                        mt.is_external AS stream_is_external,
                        mt.is_default AS stream_is_default,
                        mt.is_forced AS stream_is_forced
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 LEFT JOIN media_sources ms
                   ON ms.item_id = mi.id
                  AND EXISTS (
                      SELECT 1 FROM filesystem_entries fe
                      WHERE fe.id = ms.filesystem_entry_id AND fe.is_missing = 0
                  )
                 LEFT JOIN media_streams mt ON mt.media_source_id = ms.id
                 WHERE mi.id IN ({placeholders})
                   AND mi.removed_at IS NULL{CATALOG_VISIBLE_PREDICATE}
                 ORDER BY mi.sort_title, mi.id, ms.id, mt.stream_index"
            );
            let binds = item_ids
                .iter()
                .map(|item_id| CatalogBind::Text(item_id))
                .collect::<Vec<_>>();
            rows.extend(self.fetch_catalog_rows(&query, &binds).await?);
        }
        Ok(rows)
    }

    pub(crate) async fn find_catalog_detail(
        &self,
        item_id: &str,
    ) -> Result<Option<StoredCatalogDetail>, StorageError> {
        Ok(self
            .list_catalog_details_by_ids(&[item_id.to_owned()])
            .await?
            .remove(item_id))
    }

    pub(crate) async fn list_catalog_details_by_ids(
        &self,
        item_ids: &[String],
    ) -> Result<HashMap<String, StoredCatalogDetail>, StorageError> {
        let mut details = HashMap::with_capacity(item_ids.len());
        for item_ids in item_ids.chunks(500) {
            if item_ids.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", item_ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT mi.id AS item_id, mi.premiere_date, mi.last_air_date,
                        mi.status, mi.original_language, mi.provider_ids_json,
                        (SELECT series.title
                         FROM media_items series
                         WHERE series.id = CASE
                             WHEN mi.item_type = 'SEASON' THEN mi.parent_id
                             ELSE mi.series_id
                         END
                           AND series.removed_at IS NULL) AS series_name,
                        (SELECT COUNT(*) FROM media_items child
                         WHERE child.parent_id = mi.id AND child.item_type = 'SEASON'
                           AND child.removed_at IS NULL) AS season_count,
                        (SELECT COUNT(DISTINCT CASE
                                    WHEN mi.item_type = 'SERIES' THEN
                                        COALESCE(CAST(child.season_number AS TEXT), '') || ':' ||
                                        COALESCE(CAST(child.episode_number AS TEXT), child.id)
                                    ELSE COALESCE(CAST(child.episode_number AS TEXT), child.id)
                                END)
                         FROM media_items child
                         WHERE child.item_type = 'EPISODE'
                           AND child.removed_at IS NULL
                           AND ((mi.item_type = 'SERIES' AND child.series_id = mi.id)
                             OR (mi.item_type = 'SEASON' AND child.parent_id = mi.id))) AS episode_count
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 WHERE mi.id IN ({placeholders}) AND mi.removed_at IS NULL"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for item_id in item_ids {
                statement = statement.bind(item_id);
            }
            let batch =
                statement
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
            for row in batch {
                let item_id: String = row.get("item_id");
                details.insert(
                    item_id.clone(),
                    StoredCatalogDetail {
                        premiere_date: row.get("premiere_date"),
                        last_air_date: row.get("last_air_date"),
                        status: row.get("status"),
                        original_language: row.get("original_language"),
                        provider_ids_json: row.get("provider_ids_json"),
                        series_name: row.get("series_name"),
                        season_count: row.get("season_count"),
                        episode_count: row.get("episode_count"),
                    },
                );
            }
        }
        Ok(details)
    }

    pub(crate) async fn list_media_chapters_by_source_ids(
        &self,
        source_ids: &[String],
    ) -> Result<HashMap<String, Vec<StoredMediaChapter>>, StorageError> {
        let mut chapters = HashMap::<String, Vec<StoredMediaChapter>>::new();
        for source_ids in source_ids.chunks(500) {
            if source_ids.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", source_ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT mc.media_source_id, mc.start_position_ticks, mc.name, mc.marker_type, mc.chapter_index
                 FROM media_chapters mc
                 JOIN media_sources ms ON ms.id = mc.media_source_id
                 JOIN media_items mi ON mi.id = ms.item_id
                 JOIN libraries l ON l.id = mi.library_id
                 WHERE mc.media_source_id IN ({placeholders})
                   AND mi.item_type = 'EPISODE'
                   AND l.chapter_source_id = mc.provider_id
                 ORDER BY media_source_id, start_position_ticks,
                          CASE marker_type
                              WHEN 'INTRO_START' THEN 0
                              WHEN 'INTRO_END' THEN 1
                              WHEN 'CREDITS_START' THEN 2
                              ELSE 99
                          END,
                          chapter_index, mc.id"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for source_id in source_ids {
                statement = statement.bind(source_id);
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
                let source_id: String = row.get("media_source_id");
                chapters
                    .entry(source_id.clone())
                    .or_default()
                    .push(StoredMediaChapter {
                        source_id,
                        start_position_ticks: row.get("start_position_ticks"),
                        name: row.get("name"),
                        marker_type: row.get("marker_type"),
                        chapter_index: row.get("chapter_index"),
                    });
            }
        }
        Ok(chapters)
    }

    async fn fetch_catalog_rows(
        &self,
        query: &str,
        binds: &[CatalogBind<'_>],
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        let mut statement = self.query(sqlx::AssertSqlSafe(query));
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
                        parent_id: row.get("parent_id"),
                        series_id: row.get("series_id"),
                        season_number: row.get("season_number"),
                        episode_number: row.get("episode_number"),
                        title: row.get("title"),
                        sort_title: row.get("sort_title"),
                        original_title: row.get("original_title"),
                        overview: row.get("overview"),
                        production_year: row.get("production_year"),
                        rating: row.get("rating"),
                        rating_source: row.get("rating_source"),
                        runtime_ticks: row.get("runtime_ticks"),
                        poster_image_tag: row.get("poster_image_tag"),
                        fanart_image_tag: row.get("fanart_image_tag"),
                        thumb_image_tag: row.get("thumb_image_tag"),
                        logo_image_tag: row.get("logo_image_tag"),
                        source_id: row.get("source_id"),
                        source_kind: row.get("source_kind"),
                        container: row.get("container"),
                        size: row.get("size"),
                        external_url: row.get("external_url"),
                        edition_name: row.get("edition_name"),
                        quality_label: row.get("quality_label"),
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
                        stream_details_json: row.get("stream_details_json"),
                        stream_is_external: row
                            .get::<Option<i64>, _>("stream_is_external")
                            .map(|value| value != 0),
                        stream_is_default: row
                            .get::<Option<i64>, _>("stream_is_default")
                            .map(|value| value != 0),
                        stream_is_forced: row
                            .get::<Option<i64>, _>("stream_is_forced")
                            .map(|value| value != 0),
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
        self.query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title,
                original_title, production_year, provider_ids_json, identification_status
            ) VALUES (?, ?, 'MOVIE', ?, ?, ?, ?, ?, 'LOCAL_CONFIRMED')",
        )
        .bind(item.id)
        .bind(item.library_id)
        .bind(item.title)
        .bind(item.sort_title)
        .bind(item.original_title)
        .bind(item.production_year)
        .bind(item.provider_ids_json)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn update_local_provider_ids_if_empty(
        &self,
        item_id: &str,
        provider_ids_json: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE media_items
             SET provider_ids_json = ?
             WHERE id = ? AND (provider_ids_json IS NULL OR provider_ids_json = '{}')",
        )
        .bind(provider_ids_json)
        .bind(item_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn update_local_provider_ids_for_identity_if_empty(
        &self,
        identity_key: &str,
        provider_ids_json: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE media_items
             SET provider_ids_json = ?
             WHERE identity_key = ? AND item_type = 'SERIES'
               AND removed_at IS NULL
               AND (provider_ids_json IS NULL OR provider_ids_json = '{}')",
        )
        .bind(provider_ids_json)
        .bind(identity_key)
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
        let is_strm = source.source_kind == "STRM_URL";
        self.query(
            "INSERT INTO media_sources (
                id, item_id, source_kind, filesystem_entry_id,
                edition_name, quality_label, container, size,
                external_url, strm_target_kind, is_default, probe_status
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'PENDING')",
        )
        .bind(source.id)
        .bind(source.item_id)
        .bind(source.source_kind)
        .bind(source.filesystem_entry_id)
        .bind(source.edition_name)
        .bind(source.quality_label)
        .bind(source.container)
        .bind(source.size)
        .bind(source.external_url)
        .bind(source.strm_target_kind)
        .bind(database_flag(source.is_default))
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        if is_strm {
            self.query(
                "UPDATE media_items
                 SET poster_fallback_required = 1
                 WHERE id = ?
                   AND NOT EXISTS (
                       SELECT 1 FROM item_images
                       WHERE item_id = media_items.id
                         AND image_type IN ('POSTER', 'THUMB')
                         AND image_index = 0
                   )",
            )
            .bind(source.item_id)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        Ok(())
    }

    pub(crate) async fn list_media_sources_for_library_page(
        &self,
        library_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<StoredMediaSourcePath>, StorageError> {
        self.query(
            "SELECT ms.id AS source_id, ms.item_id, ms.probe_status,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE mi.library_id = ? AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
               AND fe.is_missing = 0
             ORDER BY ms.item_id, fe.relative_path
             LIMIT ? OFFSET ?",
        )
        .bind(library_id)
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

    pub(crate) async fn list_movie_metadata_sources_page(
        &self,
        library_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<StoredMediaSourcePath>, StorageError> {
        self.query(
            "SELECT ms.id AS source_id, ms.item_id, ms.probe_status,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE mi.library_id = ? AND mi.item_type = 'MOVIE'
               AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
               AND fe.is_missing = 0
             ORDER BY ms.item_id, fe.relative_path
             LIMIT ? OFFSET ?",
        )
        .bind(library_id)
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

    pub(crate) async fn list_movie_metadata_sources_for_incremental_scan(
        &self,
        scan_job_id: &str,
    ) -> Result<Vec<StoredMediaSourcePath>, StorageError> {
        self.query(
            "SELECT ms.id AS source_id, ms.item_id, ms.probe_status,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE mi.item_type = 'MOVIE'
               AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
               AND fe.is_missing = 0
               AND mi.removed_at IS NULL
               AND EXISTS (
                   SELECT 1 FROM scan_job_paths sjp
                   WHERE sjp.job_id = ?
                     AND sjp.processed_at IS NOT NULL
                     AND sjp.library_root_id = fe.library_root_id
                     AND (
                           sjp.relative_path = '.'
                           OR
                           fe.relative_path = sjp.relative_path
                           OR substr(fe.relative_path, 1, length(sjp.relative_path) + 1)
                              = sjp.relative_path || '/'
                     )
               )
             ORDER BY ms.item_id, fe.relative_path",
        )
        .bind(scan_job_id)
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

    pub(crate) async fn find_local_danmaku_source_for_item(
        &self,
        item_id: &str,
    ) -> Result<Option<StoredDanmakuSource>, StorageError> {
        self.query(
            "SELECT ms.id AS source_id, ms.item_id,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE mi.id = ? AND ms.source_kind = 'LOCAL_FILE'
               AND fe.is_missing = 0
             ORDER BY ms.is_default DESC, fe.relative_path
             LIMIT 1",
        )
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredDanmakuSource {
                source_id: row.get("source_id"),
                root_path: row.get("root_path"),
                relative_path: row.get("relative_path"),
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn upsert_danmaku_track(
        &self,
        track: NewDanmakuTrack<'_>,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO danmaku_tracks (
                id, media_source_id, relative_path, format, provider,
                provider_anime_id, provider_episode_id, fingerprint,
                status, error_code, last_checked_at
             ) VALUES (?, ?, ?, 'XML', ?, ?, ?, ?, ?, ?, unixepoch())
             ON CONFLICT(media_source_id) DO UPDATE SET
                relative_path = excluded.relative_path,
                provider = excluded.provider,
                provider_anime_id = excluded.provider_anime_id,
                provider_episode_id = excluded.provider_episode_id,
                fingerprint = excluded.fingerprint,
                status = excluded.status,
                error_code = excluded.error_code,
                last_checked_at = unixepoch(),
                updated_at = unixepoch()",
        )
        .bind(track.id)
        .bind(track.media_source_id)
        .bind(track.relative_path)
        .bind(track.provider)
        .bind(track.provider_anime_id)
        .bind(track.provider_episode_id)
        .bind(track.fingerprint)
        .bind(track.status)
        .bind(track.error_code)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn create_danmaku_match_job(
        &self,
        job: NewDanmakuMatchJob<'_>,
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
            "INSERT INTO danmaku_match_jobs (
                id, library_id, status, overwrite, concurrency, total_count
             ) VALUES (?, ?, 'PENDING', ?, ?, ?)",
        )
        .bind(job.id)
        .bind(job.library_id)
        .bind(database_flag(job.overwrite))
        .bind(job.concurrency)
        .bind(0_i64)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        let inserted = self
            .query(
                "INSERT INTO danmaku_match_job_items (
                    id, job_id, media_source_id, status
                 )
                 SELECT ? || ':' || ms.id, ?, ms.id, 'PENDING'
                 FROM media_sources ms
                 JOIN media_items mi ON mi.id = ms.item_id
                 JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                 WHERE mi.library_id = ? AND ms.source_kind = 'LOCAL_FILE'
                   AND fe.is_missing = 0",
            )
            .bind(job.id)
            .bind(job.id)
            .bind(job.library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let total_count = i64::try_from(inserted.rows_affected()).unwrap_or(i64::MAX);
        self.query(
            "UPDATE danmaku_match_jobs
             SET total_count = ?, updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(total_count)
        .bind(job.id)
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

    pub(crate) async fn has_active_danmaku_match_jobs(
        &self,
        library_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(
                SELECT 1 FROM danmaku_match_jobs
                WHERE library_id = ? AND status IN ('PENDING', 'RUNNING')
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

    pub(crate) async fn find_danmaku_match_job(
        &self,
        id: &str,
    ) -> Result<Option<StoredDanmakuMatchJob>, StorageError> {
        self.query(
            "SELECT id, library_id, status, overwrite, concurrency,
                    total_count, processed_count, success_count,
                    skipped_count, failed_count, cancel_requested, error
             FROM danmaku_match_jobs WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_danmaku_match_job))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_danmaku_match_jobs(
        &self,
        status: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredDanmakuMatchJob>, StorageError> {
        let rows = if let Some(status) = status {
            self.query(
                "SELECT id, library_id, status, overwrite, concurrency,
                        total_count, processed_count, success_count,
                        skipped_count, failed_count, cancel_requested, error
                 FROM danmaku_match_jobs
                 WHERE status = ?
                 ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
            )
            .bind(status)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        } else {
            self.query(
                "SELECT id, library_id, status, overwrite, concurrency,
                        total_count, processed_count, success_count,
                        skipped_count, failed_count, cancel_requested, error
                 FROM danmaku_match_jobs
                 ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
            )
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        };
        rows.map(|rows| rows.into_iter().map(stored_danmaku_match_job).collect())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_active_danmaku_match_job_ids(
        &self,
    ) -> Result<Vec<String>, StorageError> {
        self.query_scalar(
            "SELECT id FROM danmaku_match_jobs
             WHERE status IN ('PENDING', 'RUNNING')
             ORDER BY created_at, id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn claim_danmaku_match_job(&self, id: &str) -> Result<bool, StorageError> {
        self.query(
            "UPDATE danmaku_match_jobs
             SET status = 'RUNNING',
                 started_at = COALESCE(started_at, unixepoch()),
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

    pub(crate) async fn reset_running_danmaku_match_items(
        &self,
        job_id: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE danmaku_match_job_items
             SET status = 'PENDING', updated_at = unixepoch()
             WHERE job_id = ? AND status = 'RUNNING'",
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

    pub(crate) async fn cancel_pending_danmaku_match_items(
        &self,
        job_id: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE danmaku_match_job_items
             SET status = 'CANCELLED', updated_at = unixepoch()
             WHERE job_id = ? AND status = 'PENDING'",
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

    pub(crate) async fn list_pending_danmaku_match_items(
        &self,
        job_id: &str,
        limit: i64,
    ) -> Result<Vec<StoredDanmakuMatchItem>, StorageError> {
        self.query(
            "SELECT ji.id, ji.media_source_id,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM danmaku_match_job_items ji
             LEFT JOIN media_sources ms
               ON ms.id = ji.media_source_id AND ms.source_kind = 'LOCAL_FILE'
             LEFT JOIN filesystem_entries fe
               ON fe.id = ms.filesystem_entry_id AND fe.is_missing = 0
             LEFT JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE ji.job_id = ? AND ji.status = 'PENDING'
             ORDER BY ji.id
             LIMIT ?",
        )
        .bind(job_id)
        .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredDanmakuMatchItem {
                    id: row.get("id"),
                    media_source_id: row.get("media_source_id"),
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

    pub(crate) async fn claim_danmaku_match_item(&self, id: &str) -> Result<bool, StorageError> {
        self.query(
            "UPDATE danmaku_match_job_items
             SET status = 'RUNNING', attempts = attempts + 1, updated_at = unixepoch()
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

    pub(crate) async fn finish_danmaku_match_item(
        &self,
        id: &str,
        status: &str,
        provider_anime_id: Option<&str>,
        provider_episode_id: Option<&str>,
        error_code: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE danmaku_match_job_items
             SET status = ?, provider_anime_id = ?, provider_episode_id = ?,
                 error_code = ?, error_message = ?, updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(status)
        .bind(provider_anime_id)
        .bind(provider_episode_id)
        .bind(error_code)
        .bind(error_message)
        .bind(id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn increment_danmaku_match_progress(
        &self,
        job_id: &str,
        success: bool,
        skipped: bool,
        failed: bool,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE danmaku_match_jobs
             SET processed_count = processed_count + 1,
                 success_count = success_count + ?,
                 skipped_count = skipped_count + ?,
                 failed_count = failed_count + ?,
                 updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(i64::from(success))
        .bind(i64::from(skipped))
        .bind(i64::from(failed))
        .bind(job_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn danmaku_match_job_cancel_requested(
        &self,
        id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar("SELECT cancel_requested FROM danmaku_match_jobs WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .map(|value: i64| value != 0)
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn request_danmaku_match_job_cancel(
        &self,
        id: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE danmaku_match_jobs
             SET cancel_requested = 1, updated_at = unixepoch()
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

    pub(crate) async fn finish_danmaku_match_job(
        &self,
        id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE danmaku_match_jobs
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

    pub(crate) async fn list_local_thumbnail_sources_for_library_page(
        &self,
        library_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<StoredThumbnailSource>, StorageError> {
        self.query(
            "SELECT ms.item_id, lr.canonical_path AS root_path, fe.relative_path,
                    ii.local_path AS thumbnail_path
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             LEFT JOIN item_images ii
               ON ii.item_id = ms.item_id
              AND ii.image_type = 'THUMB'
              AND ii.image_index = 0
             WHERE mi.library_id = ? AND ms.source_kind = 'LOCAL_FILE'
               AND fe.is_missing = 0
             ORDER BY ms.item_id, ms.is_default DESC, ms.id
             LIMIT ? OFFSET ?",
        )
        .bind(library_id)
        .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
        .bind(offset.max(0))
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredThumbnailSource {
                    item_id: row.get("item_id"),
                    root_path: row.get("root_path"),
                    relative_path: row.get("relative_path"),
                    thumbnail_path: row.get("thumbnail_path"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_local_thumbnail_sources_for_incremental_scan_page(
        &self,
        scan_job_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<StoredThumbnailSource>, StorageError> {
        self.query(
            "SELECT ms.item_id, lr.canonical_path AS root_path, fe.relative_path,
                    ii.local_path AS thumbnail_path
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             LEFT JOIN item_images ii
               ON ii.item_id = ms.item_id
              AND ii.image_type = 'THUMB'
              AND ii.image_index = 0
             WHERE ms.source_kind = 'LOCAL_FILE'
               AND fe.is_missing = 0
               AND mi.removed_at IS NULL
               AND EXISTS (
                   SELECT 1 FROM scan_job_paths sjp
                   WHERE sjp.job_id = ?
                     AND sjp.processed_at IS NOT NULL
                     AND sjp.library_root_id = fe.library_root_id
                     AND (
                           sjp.relative_path = '.'
                           OR
                           fe.relative_path = sjp.relative_path
                           OR substr(fe.relative_path, 1, length(sjp.relative_path) + 1)
                              = sjp.relative_path || '/'
                     )
               )
             ORDER BY ms.item_id, ms.is_default DESC, ms.id
             LIMIT ? OFFSET ?",
        )
        .bind(scan_job_id)
        .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
        .bind(offset.max(0))
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredThumbnailSource {
                    item_id: row.get("item_id"),
                    root_path: row.get("root_path"),
                    relative_path: row.get("relative_path"),
                    thumbnail_path: row.get("thumbnail_path"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_strm_media_sources_for_library_page(
        &self,
        library_id: &str,
        after_source_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<StoredStrmMediaSource>, StorageError> {
        let rows = if let Some(after_source_id) = after_source_id {
            self.query(
                "SELECT ms.id AS source_id, ms.item_id, ms.external_url,
                        mi.poster_fallback_required,
                        CASE WHEN EXISTS (
                            SELECT 1 FROM media_streams mt
                            WHERE mt.media_source_id = ms.id
                        ) OR ms.duration_ticks IS NOT NULL
                            OR ms.bitrate IS NOT NULL
                            OR (ms.container IS NOT NULL AND lower(ms.container) <> 'strm')
                        THEN 1 ELSE 0 END AS has_media_info,
                        lr.canonical_path AS root_path, fe.relative_path,
                        ii.local_path AS thumbnail_path
                 FROM media_sources ms
                 JOIN media_items mi ON mi.id = ms.item_id
                 JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                 JOIN library_roots lr ON lr.id = fe.library_root_id
                 LEFT JOIN item_images ii
                   ON ii.item_id = ms.item_id AND ii.image_type = 'THUMB'
                  AND ii.image_index = 0
                 WHERE mi.library_id = ? AND ms.source_kind = 'STRM_URL'
                   AND fe.is_missing = 0 AND ms.id > ?
                 ORDER BY ms.id, fe.relative_path
                 LIMIT ?",
            )
            .bind(library_id)
            .bind(after_source_id)
            .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
            .fetch_all(&self.pool)
            .await
        } else {
            self.query(
                "SELECT ms.id AS source_id, ms.item_id, ms.external_url,
                        mi.poster_fallback_required,
                        CASE WHEN EXISTS (
                            SELECT 1 FROM media_streams mt
                            WHERE mt.media_source_id = ms.id
                        ) OR ms.duration_ticks IS NOT NULL
                            OR ms.bitrate IS NOT NULL
                            OR (ms.container IS NOT NULL AND lower(ms.container) <> 'strm')
                        THEN 1 ELSE 0 END AS has_media_info,
                        lr.canonical_path AS root_path, fe.relative_path,
                        ii.local_path AS thumbnail_path
                 FROM media_sources ms
                 JOIN media_items mi ON mi.id = ms.item_id
                 JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                 JOIN library_roots lr ON lr.id = fe.library_root_id
                 LEFT JOIN item_images ii
                   ON ii.item_id = ms.item_id AND ii.image_type = 'THUMB'
                  AND ii.image_index = 0
                 WHERE mi.library_id = ? AND ms.source_kind = 'STRM_URL'
                   AND fe.is_missing = 0
                 ORDER BY ms.id, fe.relative_path
                 LIMIT ?",
            )
            .bind(library_id)
            .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
            .fetch_all(&self.pool)
            .await
        };
        rows.map(|rows| {
            rows.into_iter()
                .map(|row| StoredStrmMediaSource {
                    source_id: row.get("source_id"),
                    item_id: row.get("item_id"),
                    poster_fallback_required: row.get::<i64, _>("poster_fallback_required") != 0,
                    has_media_info: row.get::<i64, _>("has_media_info") != 0,
                    external_url: row.get("external_url"),
                    root_path: row.get("root_path"),
                    relative_path: row.get("relative_path"),
                    thumbnail_path: row.get("thumbnail_path"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn count_strm_media_sources_for_library(
        &self,
        library_id: &str,
    ) -> Result<i64, StorageError> {
        self.query_scalar(
            "SELECT COUNT(*)
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             WHERE mi.library_id = ? AND ms.source_kind = 'STRM_URL'
               AND fe.is_missing = 0",
        )
        .bind(library_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_strm_media_sources_for_incremental_scan_page(
        &self,
        scan_job_id: &str,
        after_source_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<StoredStrmMediaSource>, StorageError> {
        let rows = if let Some(after_source_id) = after_source_id {
            self.query(
                "SELECT ms.id AS source_id, ms.item_id, ms.external_url,
                        mi.poster_fallback_required,
                        CASE WHEN EXISTS (
                            SELECT 1 FROM media_streams mt
                            WHERE mt.media_source_id = ms.id
                        ) OR ms.duration_ticks IS NOT NULL
                            OR ms.bitrate IS NOT NULL
                            OR (ms.container IS NOT NULL AND lower(ms.container) <> 'strm')
                        THEN 1 ELSE 0 END AS has_media_info,
                        lr.canonical_path AS root_path, fe.relative_path,
                        ii.local_path AS thumbnail_path
                 FROM media_sources ms
                 JOIN media_items mi ON mi.id = ms.item_id
                 JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                 JOIN library_roots lr ON lr.id = fe.library_root_id
                 LEFT JOIN item_images ii
                   ON ii.item_id = ms.item_id AND ii.image_type = 'THUMB'
                  AND ii.image_index = 0
                 WHERE ms.source_kind = 'STRM_URL'
                   AND fe.is_missing = 0 AND mi.removed_at IS NULL
                   AND ms.id > ? AND EXISTS (
                       SELECT 1 FROM scan_job_paths sjp
                       WHERE sjp.job_id = ? AND sjp.processed_at IS NOT NULL
                         AND sjp.library_root_id = fe.library_root_id
                         AND (
                               sjp.relative_path = '.'
                               OR
                               fe.relative_path = sjp.relative_path
                               OR substr(fe.relative_path, 1, length(sjp.relative_path) + 1)
                                  = sjp.relative_path || '/'
                             )
                   )
                 ORDER BY ms.id, fe.relative_path
                 LIMIT ?",
            )
            .bind(after_source_id)
            .bind(scan_job_id)
            .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
            .fetch_all(&self.pool)
            .await
        } else {
            self.query(
                "SELECT ms.id AS source_id, ms.item_id, ms.external_url,
                        mi.poster_fallback_required,
                        CASE WHEN EXISTS (
                            SELECT 1 FROM media_streams mt
                            WHERE mt.media_source_id = ms.id
                        ) OR ms.duration_ticks IS NOT NULL
                            OR ms.bitrate IS NOT NULL
                            OR (ms.container IS NOT NULL AND lower(ms.container) <> 'strm')
                        THEN 1 ELSE 0 END AS has_media_info,
                        lr.canonical_path AS root_path, fe.relative_path,
                        ii.local_path AS thumbnail_path
                 FROM media_sources ms
                 JOIN media_items mi ON mi.id = ms.item_id
                 JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                 JOIN library_roots lr ON lr.id = fe.library_root_id
                 LEFT JOIN item_images ii
                   ON ii.item_id = ms.item_id AND ii.image_type = 'THUMB'
                  AND ii.image_index = 0
                 WHERE ms.source_kind = 'STRM_URL'
                   AND fe.is_missing = 0 AND mi.removed_at IS NULL
                   AND EXISTS (
                       SELECT 1 FROM scan_job_paths sjp
                       WHERE sjp.job_id = ? AND sjp.processed_at IS NOT NULL
                         AND sjp.library_root_id = fe.library_root_id
                         AND (
                               sjp.relative_path = '.'
                               OR
                               fe.relative_path = sjp.relative_path
                               OR substr(fe.relative_path, 1, length(sjp.relative_path) + 1)
                                  = sjp.relative_path || '/'
                             )
                   )
                 ORDER BY ms.id, fe.relative_path
                 LIMIT ?",
            )
            .bind(scan_job_id)
            .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
            .fetch_all(&self.pool)
            .await
        };
        rows.map(|rows| {
            rows.into_iter()
                .map(|row| StoredStrmMediaSource {
                    source_id: row.get("source_id"),
                    item_id: row.get("item_id"),
                    poster_fallback_required: row.get::<i64, _>("poster_fallback_required") != 0,
                    has_media_info: row.get::<i64, _>("has_media_info") != 0,
                    external_url: row.get("external_url"),
                    root_path: row.get("root_path"),
                    relative_path: row.get("relative_path"),
                    thumbnail_path: row.get("thumbnail_path"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn count_strm_media_sources_for_incremental_scan(
        &self,
        scan_job_id: &str,
    ) -> Result<i64, StorageError> {
        self.query_scalar(
            "SELECT COUNT(DISTINCT ms.id)
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             WHERE ms.source_kind = 'STRM_URL'
               AND fe.is_missing = 0 AND mi.removed_at IS NULL
               AND EXISTS (
                   SELECT 1 FROM scan_job_paths sjp
                   WHERE sjp.job_id = ? AND sjp.processed_at IS NOT NULL
                     AND sjp.library_root_id = fe.library_root_id
                     AND (
                           sjp.relative_path = '.'
                           OR
                           fe.relative_path = sjp.relative_path
                           OR substr(fe.relative_path, 1, length(sjp.relative_path) + 1)
                              = sjp.relative_path || '/'
                         )
               )",
        )
        .bind(scan_job_id)
        .fetch_one(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_download_source(
        &self,
        item_id: &str,
    ) -> Result<Option<StoredDownloadSource>, StorageError> {
        self.query(
            "SELECT ms.source_kind,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE mi.id = ? AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
               AND fe.is_missing = 0
             ORDER BY ms.is_default DESC, ms.id LIMIT 1",
        )
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredDownloadSource {
                source_kind: row.get("source_kind"),
                root_path: row.get("root_path"),
                relative_path: row.get("relative_path"),
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_metadata_writeback_source_path(
        &self,
        item_id: &str,
    ) -> Result<Option<StoredMediaSourcePath>, StorageError> {
        self.query(
            "SELECT ms.id AS source_id, ms.item_id, ms.probe_status,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE mi.id = ? AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
               AND fe.is_missing = 0
             ORDER BY ms.is_default DESC, ms.id LIMIT 1",
        )
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredMediaSourcePath {
                source_id: row.get("source_id"),
                item_id: row.get("item_id"),
                probe_status: row.get("probe_status"),
                root_path: row.get("root_path"),
                relative_path: row.get("relative_path"),
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_playback_source(
        &self,
        item_id: &str,
        source_id: Option<&str>,
    ) -> Result<Option<StoredPlaybackSource>, StorageError> {
        self.query(
            "SELECT ms.source_kind, ms.external_url,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE mi.id = ?
               AND (? IS NULL OR ms.id = ?)
               AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
               AND fe.is_missing = 0
             ORDER BY ms.is_default DESC, ms.id
             LIMIT 1",
        )
        .bind(item_id)
        .bind(source_id)
        .bind(source_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredPlaybackSource {
                source_kind: row.get("source_kind"),
                external_url: row.get("external_url"),
                root_path: row.get("root_path"),
                relative_path: row.get("relative_path"),
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_download_source_by_id(
        &self,
        item_id: &str,
        source_id: &str,
    ) -> Result<Option<StoredDownloadSource>, StorageError> {
        self.query(
            "SELECT ms.source_kind,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE ms.id = ? AND mi.id = ? AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
               AND fe.is_missing = 0
             LIMIT 1",
        )
        .bind(source_id)
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredDownloadSource {
                source_kind: row.get("source_kind"),
                root_path: row.get("root_path"),
                relative_path: row.get("relative_path"),
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_deletable_media_source_path(
        &self,
        item_id: &str,
    ) -> Result<Option<StoredMediaSourcePath>, StorageError> {
        self.query(
            "SELECT ms.id AS source_id, ms.item_id, ms.probe_status,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE mi.id = ? AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
             ORDER BY ms.is_default DESC, ms.id LIMIT 1",
        )
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredMediaSourcePath {
                source_id: row.get("source_id"),
                item_id: row.get("item_id"),
                probe_status: row.get("probe_status"),
                root_path: row.get("root_path"),
                relative_path: row.get("relative_path"),
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_deletable_media_source_path_by_id(
        &self,
        item_id: &str,
        source_id: &str,
    ) -> Result<Option<StoredMediaSourcePath>, StorageError> {
        self.query(
            "SELECT ms.id AS source_id, ms.item_id, ms.probe_status,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE ms.id = ? AND mi.id = ?
               AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
             LIMIT 1",
        )
        .bind(source_id)
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredMediaSourcePath {
                source_id: row.get("source_id"),
                item_id: row.get("item_id"),
                probe_status: row.get("probe_status"),
                root_path: row.get("root_path"),
                relative_path: row.get("relative_path"),
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_media_item_kind(
        &self,
        item_id: &str,
    ) -> Result<Option<StoredMediaItemKind>, StorageError> {
        self.query("SELECT item_type, season_number FROM media_items WHERE id = ?")
            .bind(item_id)
            .fetch_optional(&self.pool)
            .await
            .map(|row| {
                row.map(|row| StoredMediaItemKind {
                    item_type: row.get("item_type"),
                    season_number: row.get("season_number"),
                })
            })
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn find_first_episode_source_path(
        &self,
        item_id: &str,
    ) -> Result<Option<StoredMediaSourcePath>, StorageError> {
        self.query(
            "SELECT ms.id AS source_id, episode.id AS item_id, ms.probe_status,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_items episode
             JOIN media_sources ms ON ms.item_id = episode.id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE episode.item_type = 'EPISODE'
               AND (episode.series_id = ? OR episode.parent_id = ?)
               AND fe.is_missing = 0
             ORDER BY episode.id, fe.relative_path LIMIT 1",
        )
        .bind(item_id)
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredMediaSourcePath {
                source_id: row.get("source_id"),
                item_id: row.get("item_id"),
                probe_status: row.get("probe_status"),
                root_path: row.get("root_path"),
                relative_path: row.get("relative_path"),
            })
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
        self.query(
            "UPDATE media_sources
             SET container = CASE
                     WHEN source_kind = 'STRM_URL' THEN COALESCE(?, container)
                     ELSE container
                 END,
                 size = COALESCE(?, size),
                 duration_ticks = ?, bitrate = ?,
                 probe_status = 'READY', probe_error = NULL,
                 updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(update.container)
        .bind(update.source_size)
        .bind(update.duration_ticks)
        .bind(update.bitrate)
        .bind(update.source_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query("DELETE FROM media_streams WHERE media_source_id = ?")
            .bind(update.source_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        for stream in update.streams {
            self.query(
                "INSERT INTO media_streams (
                    id, media_source_id, stream_index, stream_type,
                    codec, language, title, details_json, external_path,
                    is_external, is_default, is_forced
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(Uuid::now_v7().to_string())
            .bind(update.source_id)
            .bind(stream.stream_index)
            .bind(stream.stream_type)
            .bind(stream.codec)
            .bind(stream.language)
            .bind(stream.title)
            .bind(stream.details_json)
            .bind(stream.external_path)
            .bind(database_flag(stream.is_external))
            .bind(database_flag(stream.is_default))
            .bind(database_flag(stream.is_forced))
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

    pub(crate) async fn list_series_metadata_sources_page(
        &self,
        library_id: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<StoredSeriesMetadataSource>, StorageError> {
        self.query(
            "SELECT series.id AS series_id, season.id AS season_id,
                    episode.id AS episode_id, season.season_number,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_items episode
             JOIN media_items season ON season.id = episode.parent_id
             JOIN media_items series ON series.id = episode.series_id
             JOIN media_sources ms ON ms.item_id = episode.id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE episode.item_type = 'EPISODE'
               AND season.item_type = 'SEASON'
               AND series.item_type = 'SERIES'
               AND episode.library_id = ?
               AND episode.removed_at IS NULL
               AND fe.is_missing = 0
             ORDER BY series.id, season.season_number, episode.id, fe.relative_path
             LIMIT ? OFFSET ?",
        )
        .bind(library_id)
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

    pub(crate) async fn list_series_metadata_sources_for_incremental_scan(
        &self,
        scan_job_id: &str,
    ) -> Result<Vec<StoredSeriesMetadataSource>, StorageError> {
        self.query(
            "SELECT series.id AS series_id, season.id AS season_id,
                    episode.id AS episode_id, season.season_number,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_items episode
             JOIN media_items season ON season.id = episode.parent_id
             JOIN media_items series ON series.id = episode.series_id
             JOIN media_sources ms ON ms.item_id = episode.id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE episode.item_type = 'EPISODE'
               AND season.item_type = 'SEASON'
               AND series.item_type = 'SERIES'
               AND episode.removed_at IS NULL
               AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
               AND fe.is_missing = 0
               AND EXISTS (
                   SELECT 1 FROM scan_job_paths sjp
                   WHERE sjp.job_id = ?
                     AND sjp.processed_at IS NOT NULL
                     AND sjp.library_root_id = fe.library_root_id
                     AND (
                           sjp.relative_path = '.'
                           OR
                           fe.relative_path = sjp.relative_path
                           OR substr(fe.relative_path, 1, length(sjp.relative_path) + 1)
                              = sjp.relative_path || '/'
                     )
               )
             ORDER BY series.id, season.season_number, episode.id, fe.relative_path",
        )
        .bind(scan_job_id)
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

    pub(crate) async fn list_chapter_detection_sources_page(
        &self,
        library_id: &str,
        plugin_id: &str,
        after_source_id: Option<&str>,
        limit: i64,
        require_fingerprint: bool,
    ) -> Result<Vec<StoredChapterDetectionSource>, StorageError> {
        let query_with_fingerprint =
            "SELECT ms.id AS source_id, episode.id AS item_id, season.id AS season_id,
                    fe.fingerprint, ms.duration_ticks,
                    episode.provider_ids_json,
                    series.provider_ids_json AS series_provider_ids_json,
                    episode.season_number, episode.episode_number,
                    states.input_fingerprint AS state_input_fingerprint,
                    states.status AS state_status,
                    states.last_checked_at AS state_last_checked_at,
                    states.next_retry_at AS state_next_retry_at,
                    states.intro_fingerprint AS state_intro_fingerprint,
                    states.credits_fingerprint AS state_credits_fingerprint
             FROM media_sources ms
             JOIN media_items episode ON episode.id = ms.item_id
             JOIN media_items season ON season.id = episode.parent_id
             JOIN media_items series ON series.id = episode.series_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             LEFT JOIN chapter_detection_source_states states
               ON states.source_id = ms.id AND states.plugin_id = ?
             WHERE episode.library_id = ?
               AND episode.item_type = 'EPISODE'
               AND season.item_type = 'SEASON'
               AND series.item_type = 'SERIES'
               AND ms.source_kind = 'LOCAL_FILE'
               AND episode.removed_at IS NULL
               AND fe.is_missing = 0
               AND fe.fingerprint IS NOT NULL
               AND (ms.is_default = 1 OR NOT EXISTS (
                   SELECT 1 FROM media_sources preferred
                   WHERE preferred.item_id = episode.id AND preferred.is_default = 1
               ))
               AND (? IS NULL OR ms.id > ?)
             ORDER BY ms.id
             LIMIT ?";
        let query_without_fingerprint =
            "SELECT ms.id AS source_id, episode.id AS item_id, season.id AS season_id,
                    fe.fingerprint, ms.duration_ticks,
                    episode.provider_ids_json,
                    series.provider_ids_json AS series_provider_ids_json,
                    episode.season_number, episode.episode_number,
                    states.input_fingerprint AS state_input_fingerprint,
                    states.status AS state_status,
                    states.last_checked_at AS state_last_checked_at,
                    states.next_retry_at AS state_next_retry_at,
                    states.intro_fingerprint AS state_intro_fingerprint,
                    states.credits_fingerprint AS state_credits_fingerprint
             FROM media_sources ms
             JOIN media_items episode ON episode.id = ms.item_id
             JOIN media_items season ON season.id = episode.parent_id
             JOIN media_items series ON series.id = episode.series_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             LEFT JOIN chapter_detection_source_states states
               ON states.source_id = ms.id AND states.plugin_id = ?
             WHERE episode.library_id = ?
               AND episode.item_type = 'EPISODE'
               AND season.item_type = 'SEASON'
               AND series.item_type = 'SERIES'
               AND ms.source_kind = 'LOCAL_FILE'
               AND episode.removed_at IS NULL
               AND fe.is_missing = 0
               AND (ms.is_default = 1 OR NOT EXISTS (
                   SELECT 1 FROM media_sources preferred
                   WHERE preferred.item_id = episode.id AND preferred.is_default = 1
               ))
               AND (? IS NULL OR ms.id > ?)
             ORDER BY ms.id
             LIMIT ?";
        let limit = limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE);
        let rows = if require_fingerprint {
            self.query(query_with_fingerprint)
                .bind(plugin_id)
                .bind(library_id)
                .bind(after_source_id)
                .bind(after_source_id)
                .bind(limit)
                .fetch_all(&self.pool)
                .await
        } else {
            self.query(query_without_fingerprint)
                .bind(plugin_id)
                .bind(library_id)
                .bind(after_source_id)
                .bind(after_source_id)
                .bind(limit)
                .fetch_all(&self.pool)
                .await
        };
        rows.map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredChapterDetectionSource {
                    source_id: row.get("source_id"),
                    item_id: row.get("item_id"),
                    season_id: row.get("season_id"),
                    fingerprint: row.get("fingerprint"),
                    duration_ticks: row.get("duration_ticks"),
                    provider_ids_json: row.get("provider_ids_json"),
                    series_provider_ids_json: row.get("series_provider_ids_json"),
                    season_number: row.get("season_number"),
                    episode_number: row.get("episode_number"),
                    state: row.get::<Option<String>, _>("state_status").map(|status| {
                        StoredChapterDetectionSourceState {
                            input_fingerprint: row.get("state_input_fingerprint"),
                            status,
                            last_checked_at: row.get("state_last_checked_at"),
                            next_retry_at: row.get("state_next_retry_at"),
                            intro_fingerprint: row.get("state_intro_fingerprint"),
                            credits_fingerprint: row.get("state_credits_fingerprint"),
                        }
                    }),
                })
                .collect()
        })
    }

    pub(crate) async fn create_chapter_detection_job(
        &self,
        job: NewChapterDetectionJob<'_>,
    ) -> Result<bool, StorageError> {
        self.query(
            "INSERT INTO chapter_detection_jobs (
                id, library_id, plugin_id, status, concurrency,
                intro_window_seconds, credits_window_seconds, match_threshold, total_count
             ) VALUES (?, ?, ?, 'PENDING', ?, ?, ?, ?, ?)
             ON CONFLICT DO NOTHING",
        )
        .bind(job.id)
        .bind(job.library_id)
        .bind(job.plugin_id)
        .bind(job.concurrency)
        .bind(job.intro_window_seconds)
        .bind(job.credits_window_seconds)
        .bind(job.match_threshold)
        .bind(job.total_count)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn insert_chapter_detection_job_items(
        &self,
        items: &[NewChapterDetectionJobItem<'_>],
    ) -> Result<(), StorageError> {
        if items.is_empty() {
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
        for item in items {
            self.query(
                "INSERT INTO chapter_detection_job_items (
                    job_id, source_id, item_id, season_id, source_fingerprint,
                    input_fingerprint, is_context, status
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, 'PENDING')",
            )
            .bind(item.job_id)
            .bind(item.source_id)
            .bind(item.item_id)
            .bind(item.season_id)
            .bind(item.source_fingerprint)
            .bind(item.input_fingerprint)
            .bind(database_flag(item.is_context))
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

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn upsert_chapter_detection_source_state(
        &self,
        source_id: &str,
        plugin_id: &str,
        input_fingerprint: &[u8],
        status: &str,
        last_checked_at: i64,
        last_success_at: Option<i64>,
        next_retry_at: Option<i64>,
        error: Option<&str>,
        intro_fingerprint: Option<&[u8]>,
        credits_fingerprint: Option<&[u8]>,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO chapter_detection_source_states (
                source_id, plugin_id, input_fingerprint, status, last_checked_at,
                last_success_at, next_retry_at, error, intro_fingerprint,
                credits_fingerprint, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, unixepoch())
             ON CONFLICT(source_id, plugin_id) DO UPDATE SET
                input_fingerprint = excluded.input_fingerprint,
                status = excluded.status,
                last_checked_at = excluded.last_checked_at,
                last_success_at = excluded.last_success_at,
                next_retry_at = excluded.next_retry_at,
                error = excluded.error,
                intro_fingerprint = excluded.intro_fingerprint,
                credits_fingerprint = excluded.credits_fingerprint,
                updated_at = unixepoch()",
        )
        .bind(source_id)
        .bind(plugin_id)
        .bind(input_fingerprint)
        .bind(status)
        .bind(last_checked_at)
        .bind(last_success_at)
        .bind(next_retry_at)
        .bind(error)
        .bind(intro_fingerprint)
        .bind(credits_fingerprint)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn delete_chapter_detection_job(&self, id: &str) -> Result<(), StorageError> {
        self.query("DELETE FROM chapter_detection_jobs WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn find_chapter_detection_job(
        &self,
        id: &str,
    ) -> Result<Option<StoredChapterDetectionJob>, StorageError> {
        self.query(
            "SELECT id, library_id, plugin_id, status, concurrency,
                    intro_window_seconds, credits_window_seconds, match_threshold,
                    cursor, processed_count, total_count, cancel_requested, error
             FROM chapter_detection_jobs WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_chapter_detection_job))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn has_active_chapter_detection_job_for_library(
        &self,
        library_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(
                SELECT 1 FROM chapter_detection_jobs
                WHERE library_id = ? AND status IN ('PENDING', 'RUNNING')
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

    pub(crate) async fn list_chapter_detection_jobs(
        &self,
        status: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredChapterDetectionJob>, StorageError> {
        let query = if status.is_some() {
            "SELECT id, library_id, plugin_id, status, concurrency,
                    intro_window_seconds, credits_window_seconds, match_threshold,
                    cursor, processed_count, total_count, cancel_requested, error
             FROM chapter_detection_jobs WHERE status = ?
             ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?"
        } else {
            "SELECT id, library_id, plugin_id, status, concurrency,
                    intro_window_seconds, credits_window_seconds, match_threshold,
                    cursor, processed_count, total_count, cancel_requested, error
             FROM chapter_detection_jobs
             ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?"
        };
        let rows = if let Some(status) = status {
            self.query(query)
                .bind(status)
                .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
                .bind(offset.max(0))
                .fetch_all(&self.pool)
                .await
        } else {
            self.query(query)
                .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
                .bind(offset.max(0))
                .fetch_all(&self.pool)
                .await
        };
        rows.map(|rows| rows.into_iter().map(stored_chapter_detection_job).collect())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_active_chapter_detection_job_ids(
        &self,
    ) -> Result<Vec<String>, StorageError> {
        self.query_scalar(
            "SELECT id FROM chapter_detection_jobs
             WHERE status IN ('PENDING', 'RUNNING') ORDER BY created_at, id LIMIT 10000",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn claim_chapter_detection_job(&self, id: &str) -> Result<bool, StorageError> {
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
                "UPDATE chapter_detection_jobs
                 SET status = 'RUNNING', started_at = COALESCE(started_at, unixepoch()),
                     updated_at = unixepoch()
                 WHERE id = ? AND status = 'PENDING'",
            )
            .bind(id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if result.rows_affected() == 1 {
            self.query(
                "UPDATE chapter_detection_job_items SET status = 'PENDING', updated_at = unixepoch()
                 WHERE job_id = ? AND status = 'RUNNING'",
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

    pub(crate) async fn requeue_running_chapter_detection_items(
        &self,
        job_id: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE chapter_detection_job_items
             SET status = 'PENDING', updated_at = unixepoch()
             WHERE job_id = ? AND status = 'RUNNING'",
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

    pub(crate) async fn list_pending_chapter_detection_items(
        &self,
        job_id: &str,
        limit: i64,
    ) -> Result<Vec<StoredChapterDetectionItem>, StorageError> {
        self.query(
            "SELECT cdi.source_id, cdi.season_id,
                    cdi.source_fingerprint,
                    cdi.input_fingerprint, cdi.is_context,
                    states.intro_fingerprint, states.credits_fingerprint,
                    ms.duration_ticks,
                    lr.canonical_path AS root_path, fe.relative_path,
                    item.provider_ids_json,
                    series.provider_ids_json AS series_provider_ids_json,
                    item.season_number, item.episode_number
             FROM chapter_detection_job_items cdi
             JOIN chapter_detection_jobs job ON job.id = cdi.job_id
             JOIN media_sources ms ON ms.id = cdi.source_id
             JOIN media_items item ON item.id = cdi.item_id
             LEFT JOIN media_items series ON series.id = item.series_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             LEFT JOIN chapter_detection_source_states states
               ON states.source_id = cdi.source_id AND states.plugin_id = job.plugin_id
             WHERE cdi.job_id = ? AND cdi.status = 'PENDING'
               AND ms.source_kind = 'LOCAL_FILE'
             ORDER BY cdi.season_id, cdi.source_id
             LIMIT ?",
        )
        .bind(job_id)
        .bind(limit.clamp(1, MAX_BACKGROUND_PAGE_SIZE))
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredChapterDetectionItem {
                    source_id: row.get("source_id"),
                    season_id: row.get("season_id"),
                    source_fingerprint: row.get("source_fingerprint"),
                    input_fingerprint: row.get("input_fingerprint"),
                    is_context: row.get::<i64, _>("is_context") != 0,
                    intro_fingerprint: row.get("intro_fingerprint"),
                    credits_fingerprint: row.get("credits_fingerprint"),
                    duration_ticks: row.get("duration_ticks"),
                    root_path: row.get("root_path"),
                    relative_path: row.get("relative_path"),
                    provider_ids_json: row.get("provider_ids_json"),
                    series_provider_ids_json: row.get("series_provider_ids_json"),
                    season_number: row.get("season_number"),
                    episode_number: row.get("episode_number"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn set_chapter_detection_item_status(
        &self,
        job_id: &str,
        source_id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE chapter_detection_job_items
             SET status = ?, error = ?, updated_at = unixepoch()
             WHERE job_id = ? AND source_id = ?",
        )
        .bind(status)
        .bind(error)
        .bind(job_id)
        .bind(source_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn update_chapter_detection_job_progress(
        &self,
        id: &str,
        cursor: Option<&str>,
        processed_count: i64,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE chapter_detection_jobs
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

    pub(crate) async fn chapter_detection_job_cancel_requested(
        &self,
        id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar("SELECT cancel_requested FROM chapter_detection_jobs WHERE id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await
            .map(|value: i64| value != 0)
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn request_chapter_detection_job_cancel(
        &self,
        id: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE chapter_detection_jobs SET cancel_requested = 1, updated_at = unixepoch()
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

    pub(crate) async fn finish_chapter_detection_job(
        &self,
        id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE chapter_detection_jobs
             SET status = CASE WHEN cancel_requested = 1 THEN 'CANCELLED' ELSE ? END,
                 error = CASE WHEN cancel_requested = 1 THEN NULL ELSE ? END,
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

    pub(crate) async fn replace_detected_media_chapters(
        &self,
        source_id: &str,
        provider_id: &str,
        source_fingerprint: &[u8],
        markers: &[NewMediaChapterMarker],
    ) -> Result<bool, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let current = if source_fingerprint.is_empty() {
            None
        } else {
            self.query_scalar::<Vec<u8>>(
                "SELECT fe.fingerprint
                 FROM media_sources ms
                 JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                 WHERE ms.id = ? AND ms.source_kind = 'LOCAL_FILE' AND fe.is_missing = 0",
            )
            .bind(source_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
        };
        if !source_fingerprint.is_empty() && current.as_deref() != Some(source_fingerprint) {
            transaction
                .rollback()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(false);
        }
        self.query("DELETE FROM media_chapters WHERE media_source_id = ? AND provider_id = ?")
            .bind(source_id)
            .bind(provider_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        for marker in markers {
            self.query(
                "INSERT INTO media_chapters (
                    id, media_source_id, start_position_ticks, name, marker_type,
                    chapter_index, provider_id, confidence
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(Uuid::now_v7().to_string())
            .bind(source_id)
            .bind(marker.start_position_ticks)
            .bind(marker.name.clone())
            .bind(marker.marker_type.clone())
            .bind(marker.chapter_index)
            .bind(provider_id)
            .bind(marker.confidence)
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

    pub(crate) async fn insert_hierarchy_item(
        &self,
        item: NewHierarchyItem<'_>,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO media_items (
                id, library_id, item_type, parent_id, series_id,
                season_number, episode_number, absolute_number,
                title, sort_title, original_title, production_year,
                provider_ids_json, identification_status, identity_key
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(item.id)
        .bind(item.library_id)
        .bind(item.item_type)
        .bind(item.parent_id)
        .bind(item.series_id)
        .bind(item.season_number)
        .bind(item.episode_number)
        .bind(item.absolute_number)
        .bind(item.title)
        .bind(item.sort_title)
        .bind(item.original_title)
        .bind(item.production_year)
        .bind(item.provider_ids_json)
        .bind(item.identification_status)
        .bind(item.identity_key)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn update_unconfirmed_hierarchy_item(
        &self,
        item_id: &str,
        title: &str,
        sort_title: &str,
        original_title: Option<&str>,
        production_year: Option<i64>,
        provider_ids_json: Option<&str>,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE media_items
             SET title = ?, sort_title = ?, original_title = ?, production_year = ?,
                 provider_ids_json = CASE
                     WHEN ? IS NOT NULL AND (provider_ids_json IS NULL OR provider_ids_json = '{}')
                     THEN ? ELSE provider_ids_json END
             WHERE id = ?
               AND identification_status IN ('LOCAL_CONFIRMED', 'PENDING')
               AND metadata_provenance_json IS NULL
               AND (provider_ids_json IS NULL OR provider_ids_json = '{}')",
        )
        .bind(title)
        .bind(sort_title)
        .bind(original_title)
        .bind(production_year)
        .bind(provider_ids_json)
        .bind(provider_ids_json)
        .bind(item_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
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
        self.query(
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
        update: MediaMetadataUpdate<'_>,
    ) -> Result<(), StorageError> {
        let sort_title = update.title.to_lowercase();
        self.query(
            "UPDATE media_items
             SET title = ?,
                 sort_title = ?,
                 original_title = ?,
                 overview = ?,
                 production_year = ?,
                 metadata_fingerprint = ?,
                 metadata_provenance_json = ?,
                 locked_fields_json = ?
             WHERE id = ?",
        )
        .bind(update.title)
        .bind(sort_title)
        .bind(update.original_title)
        .bind(update.overview)
        .bind(update.production_year)
        .bind(update.metadata_fingerprint)
        .bind(update.provenance_json)
        .bind(update.locked_fields_json)
        .bind(update.item_id)
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
        self.query_scalar(
            "SELECT metadata_fingerprint
             FROM media_items
             WHERE id = ? AND metadata_fingerprint IS NOT NULL",
        )
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn media_item_nfo_metadata_json(
        &self,
        item_id: &str,
    ) -> Result<Option<String>, StorageError> {
        self.query_scalar(
            "SELECT nfo_metadata_json
             FROM media_items
             WHERE id = ? AND nfo_metadata_json IS NOT NULL",
        )
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn media_item_nfo_metadata_state(
        &self,
        item_id: &str,
    ) -> Result<(bool, Option<Vec<u8>>), StorageError> {
        self.query_as(
            "SELECT CASE WHEN nfo_metadata_json IS NULL THEN 0 ELSE 1 END,
                    nfo_metadata_fingerprint
             FROM media_items
             WHERE id = ?",
        )
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row: Option<(i64, Option<Vec<u8>>)>| {
            row.map_or((false, None), |(has_snapshot, fingerprint)| {
                (has_snapshot != 0, fingerprint)
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn update_media_item_nfo_metadata(
        &self,
        item_id: &str,
        nfo_metadata_json: Option<&str>,
        source_fingerprint: Option<&[u8]>,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE media_items
             SET nfo_metadata_json = ?, nfo_metadata_fingerprint = ?
             WHERE id = ?",
        )
        .bind(nfo_metadata_json)
        .bind(source_fingerprint)
        .bind(item_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn clear_media_item_nfo_metadata_if_json(
        &self,
        item_id: &str,
        expected_json: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE media_items
             SET nfo_metadata_json = NULL, nfo_metadata_fingerprint = NULL
             WHERE id = ? AND nfo_metadata_json = ?",
        )
        .bind(item_id)
        .bind(expected_json)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn invalidate_media_item_nfo_metadata_if_source_changed(
        &self,
        item_id: &str,
        source_fingerprint: &[u8],
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE media_items
             SET nfo_metadata_json = NULL, nfo_metadata_fingerprint = NULL
             WHERE id = ?
               AND (nfo_metadata_fingerprint IS NULL OR nfo_metadata_fingerprint <> ?)",
        )
        .bind(item_id)
        .bind(source_fingerprint)
        .execute(&self.pool)
        .await
        .map(|_| ())
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
        self.query("UPDATE media_items SET metadata_fingerprint = ? WHERE id = ?")
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
        metadata: ItemImageMetadata<'_>,
    ) -> Result<bool, StorageError> {
        let id = Uuid::now_v7().to_string();
        self.query(
            "INSERT INTO item_images (
                id, item_id, image_type, image_index, local_path, width, height,
                file_size, content_tag, source
            ) VALUES (?, ?, ?, 0, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(item_id, image_type, image_index) DO UPDATE SET
                id = excluded.id,
                local_path = excluded.local_path,
                width = excluded.width,
                height = excluded.height,
                file_size = excluded.file_size,
                content_tag = excluded.content_tag,
                source = excluded.source,
                updated_at = unixepoch()
            WHERE item_images.local_path <> excluded.local_path
               OR COALESCE(item_images.content_tag, '') <> COALESCE(excluded.content_tag, '')
               OR COALESCE(item_images.width, -1) <> COALESCE(excluded.width, -1)
               OR COALESCE(item_images.height, -1) <> COALESCE(excluded.height, -1)
               OR item_images.source <> excluded.source",
        )
        .bind(id)
        .bind(item_id)
        .bind(image_type)
        .bind(local_path.to_string_lossy().as_ref())
        .bind(metadata.width)
        .bind(metadata.height)
        .bind(metadata.file_size)
        .bind(metadata.content_tag)
        .bind(metadata.source)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn set_poster_fallback_required(
        &self,
        item_id: &str,
        required: bool,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE media_items
             SET poster_fallback_required = ?
             WHERE id = ?",
        )
        .bind(database_flag(required))
        .bind(item_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn upsert_item_image(
        &self,
        item_id: &str,
        image_type: &str,
        local_path: &std::path::Path,
        metadata: ItemImageMetadata<'_>,
    ) -> Result<String, StorageError> {
        let id = Uuid::now_v7().to_string();
        self.query(
            "INSERT INTO item_images (
                id, item_id, image_type, image_index, local_path, width, height,
                file_size, content_tag, source
            ) VALUES (?, ?, ?, 0, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(item_id, image_type, image_index) DO UPDATE SET
                id = excluded.id,
                local_path = excluded.local_path,
                width = excluded.width,
                height = excluded.height,
                file_size = excluded.file_size,
                content_tag = excluded.content_tag,
                source = excluded.source,
                updated_at = unixepoch()",
        )
        .bind(&id)
        .bind(item_id)
        .bind(image_type)
        .bind(local_path.to_string_lossy().as_ref())
        .bind(metadata.width)
        .bind(metadata.height)
        .bind(metadata.file_size)
        .bind(metadata.content_tag)
        .bind(metadata.source)
        .execute(&self.pool)
        .await
        .map(|_| id)
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
        self.query(
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

    pub(crate) async fn list_item_images(
        &self,
        item_id: &str,
    ) -> Result<Vec<StoredItemImage>, StorageError> {
        self.query(
            "SELECT ii.id, ii.item_id, ii.image_type, ii.image_index,
                    ii.local_path, ii.file_size, ii.content_tag, ii.source,
                    MIN(lr.canonical_path) AS root_path
             FROM item_images ii
             JOIN media_items mi ON mi.id = ii.item_id
             LEFT JOIN library_roots lr ON lr.library_id = mi.library_id
             WHERE ii.item_id = ?
             GROUP BY ii.id
             ORDER BY ii.image_type, ii.image_index, ii.id",
        )
        .bind(item_id)
        .fetch_all(&self.pool)
        .await
        .map(|rows| rows.into_iter().map(stored_item_image).collect())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_primary_image_dimensions(
        &self,
        item_id: &str,
    ) -> Result<Option<(i32, i32)>, StorageError> {
        self.query(
            "SELECT width, height
             FROM item_images
             WHERE item_id = ? AND image_type = 'POSTER' AND image_index = 0
               AND width IS NOT NULL AND height IS NOT NULL",
        )
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(|row| (row.get("width"), row.get("height"))))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn set_item_image_dimensions(
        &self,
        item_id: &str,
        image_type: &str,
        image_index: i64,
        width: i32,
        height: i32,
    ) -> Result<(), StorageError> {
        self.query(
            "UPDATE item_images
             SET width = ?, height = ?, updated_at = unixepoch()
             WHERE item_id = ? AND image_type = ? AND image_index = ?",
        )
        .bind(width)
        .bind(height)
        .bind(item_id)
        .bind(image_type)
        .bind(image_index)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_catalog_image_tags_by_ids(
        &self,
        item_ids: &[String],
    ) -> Result<HashMap<String, Vec<StoredCatalogImageTag>>, StorageError> {
        let mut tags = HashMap::with_capacity(item_ids.len());
        for item_ids in item_ids.chunks(500) {
            if item_ids.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", item_ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT item_id, id, image_type, image_index
                 FROM item_images
                 WHERE item_id IN ({placeholders})
                 ORDER BY item_id, image_type, image_index, id"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for item_id in item_ids {
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
            for row in rows {
                let item_id: String = row.get("item_id");
                tags.entry(item_id.clone())
                    .or_insert_with(Vec::new)
                    .push(StoredCatalogImageTag {
                        id: row.get("id"),
                        image_type: row.get("image_type"),
                        image_index: row.get("image_index"),
                    });
            }
        }
        Ok(tags)
    }

    pub(crate) async fn find_item_image_source(
        &self,
        item_id: &str,
        image_type: &str,
    ) -> Result<Option<String>, StorageError> {
        self.query_scalar(
            "SELECT source
             FROM item_images
             WHERE item_id = ? AND image_type = ? AND image_index = 0",
        )
        .bind(item_id)
        .bind(image_type)
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn item_image_path_is_shared(
        &self,
        local_path: &str,
        image_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS (
                 SELECT 1 FROM item_images
                 WHERE local_path = ? AND id <> ?
             ) THEN 1 ELSE 0 END",
        )
        .bind(local_path)
        .bind(image_id)
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_random_library_poster_paths(
        &self,
        library_id: &str,
        limit: i64,
    ) -> Result<Vec<StoredLibraryPoster>, StorageError> {
        self.query(
            "SELECT ii.item_id, ii.local_path, lr.canonical_path AS root_path
             FROM item_images ii
             JOIN media_items mi ON mi.id = ii.item_id
             JOIN library_roots lr ON lr.library_id = mi.library_id
             WHERE mi.library_id = ?
               AND mi.removed_at IS NULL
               AND ii.image_type = 'POSTER'
               AND ii.image_index = 0
             GROUP BY ii.item_id, ii.local_path, lr.canonical_path
             ORDER BY random()
             LIMIT ?",
        )
        .bind(library_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredLibraryPoster {
                    item_id: row.get("item_id"),
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

    pub(crate) async fn find_item_image(
        &self,
        item_id: &str,
        image_id: &str,
    ) -> Result<Option<StoredItemImage>, StorageError> {
        self.query(
            "SELECT ii.id, ii.item_id, ii.image_type, ii.image_index,
                    ii.local_path, ii.file_size, ii.content_tag, ii.source,
                    MIN(lr.canonical_path) AS root_path
             FROM item_images ii
             JOIN media_items mi ON mi.id = ii.item_id
             LEFT JOIN library_roots lr ON lr.library_id = mi.library_id
             WHERE ii.item_id = ? AND ii.id = ?
             GROUP BY ii.id",
        )
        .bind(item_id)
        .bind(image_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_item_image))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn delete_item_image(
        &self,
        item_id: &str,
        image_id: &str,
    ) -> Result<bool, StorageError> {
        self.query("DELETE FROM item_images WHERE item_id = ? AND id = ?")
            .bind(item_id)
            .bind(image_id)
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected() == 1)
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn create_access_token(
        &self,
        token: NewAccessToken<'_>,
    ) -> Result<(), StorageError> {
        self.query(
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
        self.query(
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
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(
                SELECT 1 FROM access_tokens
                WHERE token_hash = ? AND revoked_at IS NULL
            ) THEN 1 ELSE 0 END",
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
        self.query(
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
        self.query(
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
        self.query(
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

    pub(crate) async fn list_web_session_summaries(
        &self,
        user_id: &str,
        current_session_token_hash: &[u8],
    ) -> Result<Vec<StoredWebSessionSummary>, StorageError> {
        self.query(
            "SELECT id, created_at, updated_at, expires_at, last_seen_at,
                    session_token_hash = ? AS is_current
             FROM web_sessions
             WHERE user_id = ? AND revoked_at IS NULL AND expires_at > unixepoch()
             ORDER BY updated_at DESC, id DESC",
        )
        .bind(current_session_token_hash)
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map(|rows| {
            rows.into_iter()
                .map(|row| StoredWebSessionSummary {
                    id: row.get("id"),
                    created_at: row.get("created_at"),
                    updated_at: row.get("updated_at"),
                    expires_at: row.get("expires_at"),
                    last_seen_at: row.get("last_seen_at"),
                    is_current: row.get::<i64, _>("is_current") != 0,
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn revoke_web_session_by_id(
        &self,
        user_id: &str,
        session_id: &str,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE web_sessions
             SET revoked_at = unixepoch(), updated_at = unixepoch()
             WHERE id = ? AND user_id = ? AND revoked_at IS NULL",
        )
        .bind(session_id)
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub async fn schema_version(&self) -> Result<i64, StorageError> {
        self.query("SELECT COALESCE(MAX(version), 0) AS version FROM _sqlx_migrations")
            .fetch_one(&self.pool)
            .await
            .map(|row| row.get::<i64, _>("version"))
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn identity_stability_repair_completed(&self) -> Result<bool, StorageError> {
        self.query_scalar::<String>(
            "SELECT value FROM lux_meta WHERE key = 'identity_stability_repair_v1'",
        )
        .fetch_optional(&self.pool)
        .await
        .map(|value| value.as_deref() == Some("completed"))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn mark_identity_stability_repair_completed(
        &self,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO lux_meta (key, value)
             VALUES ('identity_stability_repair_v1', 'completed')
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    /// Verifies that SQLite can commit a real write transaction.
    ///
    /// The probe only changes a reserved metadata key and never touches
    /// application data or the schema. Committing is intentional: a rollback
    /// can succeed even when the filesystem cannot persist a durable write.
    pub async fn probe_write(&self) -> Result<(), StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.query(
            "INSERT INTO lux_meta (key, value)
             VALUES ('__lux_write_probe__', ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(format!("lux-write-probe-{}", Uuid::now_v7()))
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

    pub async fn close(self) {
        self.pool.close().await;
    }

    fn query(
        &self,
        sql: impl sqlx::SqlSafeStr,
    ) -> sqlx::query::Query<'static, sqlx::Any, sqlx::any::AnyArguments> {
        sqlx::query(sqlx::AssertSqlSafe(adapt_sql_for_backend(
            self.backend,
            sql,
        )))
    }

    fn query_as<O>(
        &self,
        sql: impl sqlx::SqlSafeStr,
    ) -> sqlx::query::QueryAs<'static, sqlx::Any, O, sqlx::any::AnyArguments>
    where
        O: for<'r> sqlx::FromRow<'r, sqlx::any::AnyRow>,
    {
        sqlx::query_as(sqlx::AssertSqlSafe(adapt_sql_for_backend(
            self.backend,
            sql,
        )))
    }

    fn query_scalar<O>(
        &self,
        sql: impl sqlx::SqlSafeStr,
    ) -> sqlx::query::QueryScalar<'static, sqlx::Any, O, sqlx::any::AnyArguments>
    where
        (O,): for<'r> sqlx::FromRow<'r, sqlx::any::AnyRow>,
    {
        sqlx::query_scalar(sqlx::AssertSqlSafe(adapt_sql_for_backend(
            self.backend,
            sql,
        )))
    }
}

async fn remove_sqlite_title_year_unique(pool: &AnyPool, path: &Path) -> Result<(), StorageError> {
    let mut connection = pool.acquire().await.map_err(|source| StorageError::Sqlx {
        path: path.to_path_buf(),
        source,
    })?;
    let has_legacy_unique = sqlx::query_scalar::<_, i64>(
        "SELECT EXISTS (
             SELECT 1
             FROM sqlite_master
             WHERE type = 'table'
               AND name = 'media_items'
               AND sql LIKE '%UNIQUE (library_id, sort_title, production_year)%'
         )",
    )
    .fetch_one(&mut *connection)
    .await
    .map_err(|source| StorageError::Sqlx {
        path: path.to_path_buf(),
        source,
    })?;
    if has_legacy_unique == 0 {
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_media_items_people_visible
             ON media_items(library_id, id)
             WHERE removed_at IS NULL",
        )
        .execute(&mut *connection)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: path.to_path_buf(),
            source,
        })?;
        return Ok(());
    }

    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *connection)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: path.to_path_buf(),
            source,
        })?;

    let migration_result = async {
        let mut transaction = connection
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: path.to_path_buf(),
                source,
            })?;
        let statements = [
            "DROP TRIGGER IF EXISTS media_items_search_ai",
            "DROP TRIGGER IF EXISTS media_items_search_au",
            "DROP TRIGGER IF EXISTS media_items_search_ad",
            "DROP TRIGGER IF EXISTS trg_media_sources_availability_insert",
            "DROP TRIGGER IF EXISTS trg_media_sources_availability_update",
            "DROP TRIGGER IF EXISTS trg_media_sources_availability_delete",
            "DROP TRIGGER IF EXISTS trg_filesystem_entries_availability_update",
            "CREATE TABLE media_items_new (
                id TEXT PRIMARY KEY NOT NULL,
                library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
                item_type TEXT NOT NULL CHECK (item_type IN ('MOVIE', 'SERIES', 'SEASON', 'EPISODE', 'BOX_SET', 'FOLDER', 'UNRESOLVED')),
                parent_id TEXT,
                series_id TEXT,
                season_number INTEGER,
                episode_number INTEGER,
                absolute_number INTEGER,
                title TEXT NOT NULL,
                sort_title TEXT NOT NULL,
                original_title TEXT,
                overview TEXT,
                production_year INTEGER,
                premiere_date TEXT,
                runtime_ticks INTEGER,
                provider_ids_json TEXT,
                metadata_provenance_json TEXT,
                locked_fields_json TEXT,
                identification_status TEXT NOT NULL CHECK (identification_status IN ('LOCAL_CONFIRMED', 'ONLINE_CONFIRMED', 'PENDING', 'FAILED')),
                added_at INTEGER NOT NULL DEFAULT (unixepoch()),
                removed_at INTEGER,
                metadata_fingerprint BLOB,
                identity_key TEXT,
                rating REAL,
                rating_source TEXT,
                last_air_date TEXT,
                status TEXT,
                original_language TEXT,
                has_available_source INTEGER NOT NULL DEFAULT 0 CHECK (has_available_source IN (0, 1)),
                poster_fallback_required INTEGER NOT NULL DEFAULT 0 CHECK (poster_fallback_required IN (0, 1)),
                nfo_metadata_json TEXT,
                nfo_metadata_fingerprint BLOB
            )",
            "INSERT INTO media_items_new (
                id, library_id, item_type, parent_id, series_id, season_number, episode_number,
                absolute_number, title, sort_title, original_title, overview, production_year,
                premiere_date, runtime_ticks, provider_ids_json, metadata_provenance_json,
                locked_fields_json, identification_status, added_at, removed_at,
                metadata_fingerprint, identity_key, rating, rating_source, last_air_date, status,
                original_language, has_available_source, poster_fallback_required,
                nfo_metadata_json, nfo_metadata_fingerprint
             )
             SELECT
                id, library_id, item_type, parent_id, series_id, season_number, episode_number,
                absolute_number, title, sort_title, original_title, overview, production_year,
                premiere_date, runtime_ticks, provider_ids_json, metadata_provenance_json,
                locked_fields_json, identification_status, added_at, removed_at,
                metadata_fingerprint, identity_key, rating, rating_source, last_air_date, status,
                original_language, has_available_source, poster_fallback_required,
                nfo_metadata_json, nfo_metadata_fingerprint
             FROM media_items",
            "DROP TABLE media_items",
            "ALTER TABLE media_items_new RENAME TO media_items",
            "CREATE INDEX idx_media_items_library_sort ON media_items(library_id, sort_title, id)",
            "CREATE UNIQUE INDEX idx_media_items_identity_key
             ON media_items(identity_key)
             WHERE identity_key IS NOT NULL",
            "CREATE INDEX idx_media_items_parent_removed
             ON media_items(parent_id, removed_at)",
            "CREATE INDEX idx_media_items_series_removed
             ON media_items(series_id, removed_at)",
            "CREATE INDEX idx_media_items_library_type_visible
             ON media_items(library_id, item_type, id)
             WHERE removed_at IS NULL",
            "CREATE INDEX idx_media_items_people_visible
             ON media_items(library_id, id)
             WHERE removed_at IS NULL",
            "CREATE TRIGGER media_items_search_ai AFTER INSERT ON media_items BEGIN
                INSERT INTO media_search (item_id, title, sort_title, original_title, aliases)
                VALUES (NEW.id, NEW.title, NEW.sort_title, COALESCE(NEW.original_title, ''),
                        COALESCE((SELECT group_concat(alias, ' ') FROM item_aliases WHERE item_id = NEW.id), ''));
            END",
            "CREATE TRIGGER media_items_search_au AFTER UPDATE OF title, sort_title, original_title ON media_items BEGIN
                DELETE FROM media_search WHERE item_id = OLD.id;
                INSERT INTO media_search (item_id, title, sort_title, original_title, aliases)
                VALUES (NEW.id, NEW.title, NEW.sort_title, COALESCE(NEW.original_title, ''),
                        COALESCE((SELECT group_concat(alias, ' ') FROM item_aliases WHERE item_id = NEW.id), ''));
            END",
            "CREATE TRIGGER media_items_search_ad AFTER DELETE ON media_items BEGIN
                DELETE FROM media_search WHERE item_id = OLD.id;
            END",
            "CREATE TRIGGER trg_media_sources_availability_insert
             AFTER INSERT ON media_sources
             BEGIN
                 UPDATE media_items
                 SET has_available_source = 1
                 WHERE id = NEW.item_id
                   AND EXISTS (
                       SELECT 1
                       FROM filesystem_entries
                       WHERE id = NEW.filesystem_entry_id
                         AND is_missing = 0
                   );
             END",
            "CREATE TRIGGER trg_media_sources_availability_update
             AFTER UPDATE OF item_id, filesystem_entry_id ON media_sources
             BEGIN
                 UPDATE media_items
                 SET has_available_source = EXISTS (
                     SELECT 1
                     FROM media_sources ms
                     JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                     WHERE ms.item_id = media_items.id
                       AND fe.is_missing = 0
                 )
                 WHERE id IN (OLD.item_id, NEW.item_id);
             END",
            "CREATE TRIGGER trg_media_sources_availability_delete
             AFTER DELETE ON media_sources
             BEGIN
                 UPDATE media_items
                 SET has_available_source = EXISTS (
                     SELECT 1
                     FROM media_sources ms
                     JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                     WHERE ms.item_id = media_items.id
                       AND fe.is_missing = 0
                 )
                 WHERE id = OLD.item_id;
             END",
            "CREATE TRIGGER trg_filesystem_entries_availability_update
             AFTER UPDATE OF is_missing ON filesystem_entries
             BEGIN
                 UPDATE media_items
                 SET has_available_source = EXISTS (
                     SELECT 1
                     FROM media_sources ms
                     JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                     WHERE ms.item_id = media_items.id
                       AND fe.is_missing = 0
                 )
                 WHERE id IN (
                     SELECT item_id
                     FROM media_sources
                     WHERE filesystem_entry_id = NEW.id
                 );
             END",
        ];
        for statement in statements {
            sqlx::query(statement)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: path.to_path_buf(),
                    source,
                })?;
        }
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: path.to_path_buf(),
                source,
            })?;
        Ok::<(), StorageError>(())
    }
    .await;

    let foreign_keys_result = sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *connection)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: path.to_path_buf(),
            source,
        });
    migration_result.and(foreign_keys_result.map(|_| ()))
}

async fn validate_postgres_schema(pool: &AnyPool) -> Result<(), StorageError> {
    let path = PathBuf::from("external PostgreSQL");
    let application_table_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM information_schema.tables
         WHERE table_schema = 'public' AND table_name <> '_sqlx_migrations'",
    )
    .fetch_one(pool)
    .await
    .map_err(|source| StorageError::Sqlx {
        path: path.clone(),
        source,
    })?;
    if application_table_count == 0 {
        return Ok(());
    }

    let lux_meta_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)
         FROM information_schema.tables
         WHERE table_schema = 'public' AND table_name = 'lux_meta'",
    )
    .fetch_one(pool)
    .await
    .map_err(|source| StorageError::Sqlx { path, source })?;
    if lux_meta_count == 0 {
        return Err(StorageError::Configuration(
            DatabaseConfigurationError::Invalid(
                "PostgreSQL 数据库必须为空或已经是 Lux 数据库".to_owned(),
            ),
        ));
    }
    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct DashboardStats {
    pub(crate) movie_count: i64,
    pub(crate) series_count: i64,
    pub(crate) user_count: i64,
}

#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct StoredCatalogItemCounts {
    pub(crate) movie_count: i64,
    pub(crate) series_count: i64,
    pub(crate) episode_count: i64,
    pub(crate) box_set_count: i64,
    pub(crate) item_count: i64,
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
pub(crate) struct StoredAccessTokenDevice {
    pub(crate) device_id: String,
    pub(crate) client_name: String,
    pub(crate) device_name: String,
    pub(crate) client_version: String,
}

fn stored_user(row: sqlx::any::AnyRow) -> StoredUser {
    StoredUser {
        id: row.get("id"),
        username_normalized: row.get("username_normalized"),
        display_name: row.get("display_name"),
        password_hash: row.get("password_hash"),
        is_disabled: row.get::<i64, _>("is_disabled") != 0,
        is_admin: row.get::<i64, _>("is_admin") != 0,
        can_manage_server: row.get::<i64, _>("can_manage_server") != 0,
        can_remote_access: row.get::<i64, _>("can_remote_access") != 0,
        can_download: row.get::<i64, _>("can_download") != 0,
    }
}

pub(crate) struct UpdateUser<'a> {
    pub(crate) display_name: Option<&'a str>,
    pub(crate) password_hash: Option<&'a str>,
    pub(crate) is_disabled: Option<bool>,
    pub(crate) is_admin: Option<bool>,
    pub(crate) can_manage_server: Option<bool>,
    pub(crate) can_remote_access: Option<bool>,
    pub(crate) can_download: Option<bool>,
}

pub(crate) struct NewAuditEvent<'a> {
    pub(crate) actor_user_id: Option<&'a str>,
    pub(crate) event_type: &'a str,
    pub(crate) target_type: Option<&'a str>,
    pub(crate) target_id: Option<&'a str>,
    pub(crate) metadata_json: &'a str,
}

#[derive(Debug)]
pub(crate) struct StoredAuditEvent {
    pub(crate) id: String,
    pub(crate) actor_user_id: Option<String>,
    pub(crate) actor_username: Option<String>,
    pub(crate) event_type: String,
    pub(crate) target_type: Option<String>,
    pub(crate) target_id: Option<String>,
    pub(crate) metadata_json: String,
    pub(crate) created_at: i64,
}

#[derive(Debug)]
pub(crate) struct StoredActivityEvent {
    pub(crate) id: String,
    pub(crate) actor_user_id: Option<String>,
    pub(crate) actor_username: Option<String>,
    pub(crate) event_type: String,
    pub(crate) target_type: Option<String>,
    pub(crate) target_id: Option<String>,
    pub(crate) target_title: Option<String>,
    pub(crate) metadata_json: String,
    pub(crate) created_at: i64,
}

#[derive(Debug)]
pub(crate) struct StoredLibrary {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) is_enabled: bool,
    pub(crate) realtime_watch_enabled: bool,
    pub(crate) realtime_metadata_auto_match_enabled: bool,
    pub(crate) incremental_schedule: Option<String>,
    pub(crate) reconciliation_schedule: Option<String>,
    pub(crate) metadata_schedule: Option<String>,
    pub(crate) scan_concurrency: i64,
    pub(crate) probe_concurrency: i64,
    pub(crate) last_scan_at: Option<i64>,
    pub(crate) scraper_id: Option<String>,
    pub(crate) chapter_source_id: Option<String>,
    pub(crate) cover_image_path: Option<String>,
    pub(crate) cover_image_content_type: Option<String>,
    pub(crate) cover_image_size: Option<i64>,
    pub(crate) cover_image_tag: Option<String>,
    pub(crate) media_strategy_json: Option<String>,
}

#[derive(Debug)]
pub(crate) struct StoredScheduledTaskConfig {
    pub(crate) owner_type: String,
    pub(crate) owner_id: String,
    pub(crate) task_type: String,
    pub(crate) task_name: String,
    pub(crate) task_description: String,
    pub(crate) source_type: String,
    pub(crate) plugin_id: Option<String>,
    pub(crate) cron_or_interval: Option<String>,
    pub(crate) is_enabled: bool,
    pub(crate) resource_limit_json: String,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) library_name: Option<String>,
}

#[derive(Debug)]
pub(crate) struct StoredNotificationDestination {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) url: String,
    pub(crate) enabled: bool,
    pub(crate) allow_private_network: bool,
    pub(crate) event_types_json: String,
    pub(crate) payload_format: String,
    pub(crate) provider_plugin_id: String,
    pub(crate) provider_config_json: String,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

pub(crate) struct NewNotificationDestination<'a> {
    pub(crate) id: &'a str,
    pub(crate) name: &'a str,
    pub(crate) url: &'a str,
    pub(crate) enabled: bool,
    pub(crate) allow_private_network: bool,
    pub(crate) event_types_json: &'a str,
    pub(crate) payload_format: &'a str,
    pub(crate) provider_plugin_id: &'a str,
    pub(crate) provider_config_json: &'a str,
}

pub(crate) struct UpdateNotificationDestination<'a> {
    pub(crate) name: Option<&'a str>,
    pub(crate) url: Option<&'a str>,
    pub(crate) enabled: Option<bool>,
    pub(crate) allow_private_network: Option<bool>,
    pub(crate) event_types_json: Option<&'a str>,
    pub(crate) payload_format: Option<&'a str>,
    pub(crate) provider_plugin_id: Option<&'a str>,
    pub(crate) provider_config_json: Option<&'a str>,
}

#[derive(Debug)]
pub(crate) struct StoredNotificationDelivery {
    pub(crate) id: String,
    pub(crate) event_id: String,
    pub(crate) destination_id: String,
    pub(crate) status: String,
    pub(crate) attempt_count: i64,
    pub(crate) next_attempt_at: i64,
    pub(crate) last_http_status: Option<i64>,
    pub(crate) last_error: Option<String>,
    pub(crate) delivered_at: Option<i64>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) event_type: String,
    pub(crate) payload_json: String,
    pub(crate) destination_name: String,
    pub(crate) destination_url: String,
    pub(crate) allow_private_network: bool,
    pub(crate) provider_plugin_id: String,
    pub(crate) provider_config_json: String,
}

pub(crate) struct NewNotificationEvent<'a> {
    pub(crate) id: &'a str,
    pub(crate) event_type: &'a str,
    pub(crate) schema_version: i64,
    pub(crate) occurred_at: i64,
    pub(crate) dedupe_key: &'a str,
    pub(crate) payload_json: &'a str,
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

fn stored_scheduled_task(row: sqlx::any::AnyRow) -> StoredScheduledTaskConfig {
    StoredScheduledTaskConfig {
        owner_type: row.get("owner_type"),
        owner_id: row.get("owner_id"),
        task_type: row.get("task_type"),
        task_name: row.get("task_name"),
        task_description: row.get("task_description"),
        source_type: row.get("source_type"),
        plugin_id: row.get("plugin_id"),
        cron_or_interval: row.get("cron_or_interval"),
        is_enabled: row.get::<i64, _>("is_enabled") != 0,
        resource_limit_json: row.get("resource_limit_json"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        library_name: row.get("library_name"),
    }
}

fn stored_notification_destination(row: sqlx::any::AnyRow) -> StoredNotificationDestination {
    StoredNotificationDestination {
        id: row.get("id"),
        name: row.get("name"),
        url: row.get("url"),
        enabled: row.get::<i64, _>("enabled") != 0,
        allow_private_network: row.get::<i64, _>("allow_private_network") != 0,
        event_types_json: row.get("event_types_json"),
        payload_format: row.get("payload_format"),
        provider_plugin_id: row.get("provider_plugin_id"),
        provider_config_json: row.get("provider_config_json"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

fn stored_notification_delivery(row: sqlx::any::AnyRow) -> StoredNotificationDelivery {
    StoredNotificationDelivery {
        id: row.get("id"),
        event_id: row.get("event_id"),
        destination_id: row.get("destination_id"),
        status: row.get("status"),
        attempt_count: row.get("attempt_count"),
        next_attempt_at: row.get("next_attempt_at"),
        last_http_status: row.get("last_http_status"),
        last_error: row.get("last_error"),
        delivered_at: row.get("delivered_at"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        event_type: row.get("event_type"),
        payload_json: row.get("payload_json"),
        destination_name: row.get("name"),
        destination_url: row.get("url"),
        allow_private_network: row.get::<i64, _>("allow_private_network") != 0,
        provider_plugin_id: row.get("provider_plugin_id"),
        provider_config_json: row.get("provider_config_json"),
    }
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
    pub(crate) finished_at: Option<i64>,
    pub(crate) discovery_completed: bool,
    pub(crate) auto_metadata_match: bool,
    pub(crate) current_item: Option<String>,
    pub(crate) scan_phase: String,
}

#[derive(Debug)]
pub(crate) struct StoredScanJobPath {
    pub(crate) library_root_id: String,
    pub(crate) relative_path: String,
    pub(crate) change_kind: String,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredReconciliationScanEntry {
    pub(crate) library_root_id: String,
    pub(crate) relative_path: String,
}

#[derive(Debug)]
pub(crate) struct StoredStrmProbeJob {
    pub(crate) id: String,
    pub(crate) operation_id: String,
    pub(crate) library_id: String,
    pub(crate) status: String,
    pub(crate) concurrency: i64,
    pub(crate) include_ready: bool,
    pub(crate) write_sidecars: bool,
    pub(crate) media_info_enabled: bool,
    pub(crate) thumbnail_enabled: bool,
    pub(crate) thumbnail_position_percent: i64,
    pub(crate) target_scan_job_id: Option<String>,
    pub(crate) cursor: Option<String>,
    pub(crate) processed_count: i64,
    pub(crate) total_count: i64,
    pub(crate) cancel_requested: bool,
    pub(crate) error: Option<String>,
}

pub(crate) struct NewStrmProbeJob<'a> {
    pub(crate) id: &'a str,
    pub(crate) operation_id: &'a str,
    pub(crate) library_id: &'a str,
    pub(crate) concurrency: i64,
    pub(crate) include_ready: bool,
    pub(crate) write_sidecars: bool,
    pub(crate) media_info_enabled: bool,
    pub(crate) thumbnail_enabled: bool,
    pub(crate) thumbnail_position_percent: i64,
    pub(crate) target_scan_job_id: Option<&'a str>,
    pub(crate) total_count: i64,
}

#[derive(Debug)]
pub(crate) struct StoredScanJobEvent {
    pub(crate) id: String,
    pub(crate) job_id: String,
    pub(crate) level: String,
    pub(crate) event_code: String,
    pub(crate) message: String,
    pub(crate) details_json: String,
    pub(crate) created_at: i64,
}

pub(crate) struct NewScanJobEvent<'a> {
    pub(crate) id: &'a str,
    pub(crate) job_id: &'a str,
    pub(crate) level: &'a str,
    pub(crate) event_code: &'a str,
    pub(crate) message: &'a str,
    pub(crate) details_json: &'a str,
}

fn stored_scan_job(row: sqlx::any::AnyRow) -> StoredScanJob {
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
        finished_at: row.get("finished_at"),
        discovery_completed: row.get::<i64, _>("discovery_completed") != 0,
        auto_metadata_match: row.get::<i64, _>("auto_metadata_match") != 0,
        current_item: row.get("current_item"),
        scan_phase: row.get("scan_phase"),
    }
}

fn stored_scan_job_path(row: sqlx::any::AnyRow) -> StoredScanJobPath {
    StoredScanJobPath {
        library_root_id: row.get("library_root_id"),
        relative_path: row.get("relative_path"),
        change_kind: row.get("change_kind"),
    }
}

fn stored_reconciliation_scan_entry(row: sqlx::any::AnyRow) -> StoredReconciliationScanEntry {
    StoredReconciliationScanEntry {
        library_root_id: row.get("library_root_id"),
        relative_path: row.get("relative_path"),
    }
}

fn stored_strm_probe_job(row: sqlx::any::AnyRow) -> StoredStrmProbeJob {
    StoredStrmProbeJob {
        id: row.get("id"),
        operation_id: row.get("operation_id"),
        library_id: row.get("library_id"),
        status: row.get("status"),
        concurrency: row.get("concurrency"),
        include_ready: row.get::<i64, _>("include_ready") != 0,
        write_sidecars: row.get::<i64, _>("write_sidecars") != 0,
        media_info_enabled: row.get::<i64, _>("media_info_enabled") != 0,
        thumbnail_enabled: row.get::<i64, _>("thumbnail_enabled") != 0,
        thumbnail_position_percent: row.get("thumbnail_position_percent"),
        target_scan_job_id: row.get("target_scan_job_id"),
        cursor: row.get("cursor"),
        processed_count: row.get("processed_count"),
        total_count: row.get("total_count"),
        cancel_requested: row.get::<i64, _>("cancel_requested") != 0,
        error: row.get("error"),
    }
}

fn stored_chapter_detection_job(row: sqlx::any::AnyRow) -> StoredChapterDetectionJob {
    StoredChapterDetectionJob {
        id: row.get("id"),
        library_id: row.get("library_id"),
        plugin_id: row.get("plugin_id"),
        status: row.get("status"),
        concurrency: row.get("concurrency"),
        intro_window_seconds: row.get("intro_window_seconds"),
        credits_window_seconds: row.get("credits_window_seconds"),
        match_threshold: row.get("match_threshold"),
        cursor: row.get("cursor"),
        processed_count: row.get("processed_count"),
        total_count: row.get("total_count"),
        cancel_requested: row.get::<i64, _>("cancel_requested") != 0,
        error: row.get("error"),
    }
}

fn stored_scan_job_event(row: sqlx::any::AnyRow) -> StoredScanJobEvent {
    StoredScanJobEvent {
        id: row.get("id"),
        job_id: row.get("job_id"),
        level: row.get("level"),
        event_code: row.get("event_code"),
        message: row.get("message"),
        details_json: row.get("details_json"),
        created_at: row.get("created_at"),
    }
}

fn stored_library_root(row: sqlx::any::AnyRow) -> StoredLibraryRoot {
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
    pub(crate) relative_path: String,
    pub(crate) fingerprint: Option<Vec<u8>>,
    pub(crate) item_id: Option<String>,
}

#[derive(Debug)]
pub(crate) struct StoredItemSourceLocator {
    pub(crate) item_id: String,
    pub(crate) root_path: String,
    pub(crate) relative_path: String,
    pub(crate) fingerprint: Option<Vec<u8>>,
    pub(crate) size: i64,
    pub(crate) modified_at: i64,
    pub(crate) title: String,
    pub(crate) production_year: Option<i32>,
}

#[derive(Debug)]
pub(crate) struct StoredEpisodeIdentityCandidate {
    pub(crate) episode_id: String,
    pub(crate) filesystem_entry_id: String,
    pub(crate) library_root_id: String,
    pub(crate) relative_path: String,
}

fn stored_filesystem_entry(row: sqlx::any::AnyRow) -> StoredFilesystemEntry {
    StoredFilesystemEntry {
        id: row.get("id"),
        relative_path: row.get("relative_path"),
        fingerprint: row.get("fingerprint"),
        item_id: row.get("item_id"),
    }
}

#[derive(Debug)]
pub(crate) struct StoredMediaItem {
    pub(crate) id: String,
}

#[derive(Debug)]
pub(crate) struct NewPersonCredit {
    pub(crate) person_id: String,
    pub(crate) lux_person_id: Option<String>,
    pub(crate) person_type: String,
    pub(crate) person_name: String,
    pub(crate) provider: String,
    pub(crate) role: String,
    pub(crate) sort_order: i64,
    pub(crate) biography: Option<String>,
    pub(crate) birthday: Option<String>,
    pub(crate) deathday: Option<String>,
    pub(crate) known_for_department: Option<String>,
    pub(crate) place_of_birth: Option<String>,
    pub(crate) provider_ids: BTreeMap<String, String>,
    pub(crate) genres: Vec<String>,
    pub(crate) tags: Vec<String>,
    pub(crate) production_locations: Vec<String>,
    pub(crate) premiere_date: Option<String>,
    pub(crate) production_year: Option<i64>,
    pub(crate) taglines: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct StoredCanonicalPerson {
    pub(crate) id: String,
}

#[derive(Debug)]
pub(crate) struct StoredCanonicalPersonMatch {
    pub(crate) id: String,
    pub(crate) birthdays: Vec<String>,
}

fn stored_canonical_person(row: sqlx::any::AnyRow) -> StoredCanonicalPerson {
    StoredCanonicalPerson { id: row.get("id") }
}

#[derive(Debug)]
pub(crate) struct StoredPersonMatchCandidate {
    pub(crate) id: String,
    pub(crate) item_id: String,
    pub(crate) provider: String,
    pub(crate) provider_id: String,
    pub(crate) candidate_person_ids_json: String,
    pub(crate) status: String,
    pub(crate) score: Option<f64>,
    pub(crate) evidence_json: String,
    pub(crate) target_person_id: Option<String>,
    pub(crate) previous_person_id: Option<String>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

pub(crate) struct PersonMatchCandidateRestore<'a> {
    pub(crate) candidate_id: &'a str,
    pub(crate) item_id: &'a str,
    pub(crate) provider: &'a str,
    pub(crate) provider_id: &'a str,
    pub(crate) candidate_person_ids_json: &'a str,
    pub(crate) status: &'a str,
    pub(crate) score: Option<f64>,
    pub(crate) evidence_json: &'a str,
    pub(crate) target_person_id: Option<&'a str>,
    pub(crate) previous_person_id: Option<&'a str>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

fn stored_person_match_candidate(row: sqlx::any::AnyRow) -> StoredPersonMatchCandidate {
    StoredPersonMatchCandidate {
        id: row.get("id"),
        item_id: row.get("item_id"),
        provider: row.get("provider"),
        provider_id: row.get("provider_id"),
        candidate_person_ids_json: row.get("candidate_person_ids_json"),
        status: row.get("status"),
        score: row.get("score"),
        evidence_json: row.get("evidence_json"),
        target_person_id: row.get("target_person_id"),
        previous_person_id: row.get("previous_person_id"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

#[derive(Debug)]
pub(crate) struct StoredPersonIdentityMove {
    pub(crate) previous_person_id: Option<String>,
}

#[derive(Debug)]
pub(crate) struct StoredPersonCredit {
    pub(crate) item_id: String,
    pub(crate) person_id: String,
    pub(crate) lux_person_id: Option<String>,
    pub(crate) provider: String,
    pub(crate) person_name: String,
    pub(crate) role: String,
    pub(crate) date_created: i64,
    pub(crate) biography: Option<String>,
    pub(crate) birthday: Option<String>,
    pub(crate) deathday: Option<String>,
    pub(crate) known_for_department: Option<String>,
    pub(crate) place_of_birth: Option<String>,
    pub(crate) provider_ids: BTreeMap<String, String>,
    pub(crate) genres: Vec<String>,
    pub(crate) tags: Vec<String>,
    pub(crate) production_locations: Vec<String>,
    pub(crate) premiere_date: Option<String>,
    pub(crate) production_year: Option<i64>,
    pub(crate) taglines: Vec<String>,
}

fn stored_person_credit(row: sqlx::any::AnyRow) -> StoredPersonCredit {
    let provider_ids_json: String = row.get("provider_ids_json");
    let genres_json: String = row.get("genres_json");
    let tags_json: String = row.get("tags_json");
    let production_locations_json: String = row.get("production_locations_json");
    let taglines_json: String = row.get("taglines_json");
    StoredPersonCredit {
        item_id: row.get("item_id"),
        person_id: row.get("person_id"),
        lux_person_id: row.try_get("lux_person_id").ok(),
        provider: row.get("provider"),
        person_name: row.get("person_name"),
        role: row.get("role"),
        date_created: row.get("date_created"),
        biography: row.get("biography"),
        birthday: row.get("birthday"),
        deathday: row.get("deathday"),
        known_for_department: row.get("known_for_department"),
        place_of_birth: row.get("place_of_birth"),
        provider_ids: serde_json::from_str(&provider_ids_json).unwrap_or_default(),
        genres: serde_json::from_str(&genres_json).unwrap_or_default(),
        tags: serde_json::from_str(&tags_json).unwrap_or_default(),
        production_locations: serde_json::from_str(&production_locations_json).unwrap_or_default(),
        premiere_date: row.get("premiere_date"),
        production_year: row.get("production_year"),
        taglines: serde_json::from_str(&taglines_json).unwrap_or_default(),
    }
}

#[derive(Debug)]
pub(crate) struct StoredPersonIndexRebuildJob {
    pub(crate) library_id: String,
    pub(crate) status: String,
    pub(crate) cursor_id: Option<String>,
    pub(crate) processed_count: i64,
    pub(crate) total_count: i64,
    pub(crate) cancel_requested: bool,
}

fn stored_person_index_rebuild_job(row: sqlx::any::AnyRow) -> StoredPersonIndexRebuildJob {
    StoredPersonIndexRebuildJob {
        library_id: row.get("library_id"),
        status: row.get("status"),
        cursor_id: row.get("cursor_id"),
        processed_count: row.get("processed_count"),
        total_count: row.get("total_count"),
        cancel_requested: row.get::<i64, _>("cancel_requested") != 0,
    }
}

#[derive(Debug)]
pub(crate) struct StoredCollectionRefresh {
    pub(crate) collection_item_id: String,
    pub(crate) member_count: usize,
}

pub(crate) struct NewCollection<'a> {
    pub(crate) library_id: &'a str,
    pub(crate) provider: &'a str,
    pub(crate) provider_id: &'a str,
    pub(crate) title: &'a str,
    pub(crate) overview: Option<&'a str>,
    pub(crate) poster_path: Option<&'a str>,
    pub(crate) backdrop_path: Option<&'a str>,
    pub(crate) member_provider_ids: &'a [(String, String, i64)],
}

#[derive(Debug)]
pub(crate) struct StoredMediaMetadata {
    pub(crate) item_type: String,
    pub(crate) title: String,
    pub(crate) original_title: Option<String>,
    pub(crate) overview: Option<String>,
    pub(crate) production_year: Option<i64>,
    pub(crate) premiere_date: Option<String>,
    pub(crate) last_air_date: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) original_language: Option<String>,
    pub(crate) rating: Option<f64>,
    pub(crate) provider_ids_json: Option<String>,
    pub(crate) scraper_id: Option<String>,
    pub(crate) provenance_json: Option<String>,
    pub(crate) locked_fields_json: Option<String>,
    pub(crate) series_item_id: Option<String>,
    pub(crate) series_title: Option<String>,
    pub(crate) series_production_year: Option<i64>,
    pub(crate) series_provider_name: Option<String>,
    pub(crate) series_provider_id: Option<String>,
    pub(crate) season_number: Option<i64>,
    pub(crate) episode_number: Option<i64>,
}

#[derive(Debug)]
pub(crate) struct StoredMetadataCandidate {
    pub(crate) id: String,
    pub(crate) item_id: String,
    pub(crate) provider: String,
    pub(crate) provider_id: String,
    pub(crate) candidate_json: String,
    pub(crate) score: f64,
    pub(crate) status: String,
    pub(crate) expires_at: Option<i64>,
    pub(crate) item_title: String,
}

pub(crate) struct NewMetadataCandidate<'a> {
    pub(crate) id: &'a str,
    pub(crate) item_id: &'a str,
    pub(crate) provider: &'a str,
    pub(crate) provider_id: &'a str,
    pub(crate) candidate_json: &'a str,
    pub(crate) score: f64,
    pub(crate) expires_at: Option<i64>,
}

#[derive(Debug)]
pub(crate) struct StoredMetadataReidentifyJob {
    pub(crate) id: String,
    pub(crate) status: String,
    pub(crate) processed_count: i64,
    pub(crate) total_count: i64,
    pub(crate) error: Option<String>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) started_at: Option<i64>,
    pub(crate) finished_at: Option<i64>,
    pub(crate) mode: String,
    pub(crate) cancel_requested: bool,
    pub(crate) library_id: Option<String>,
    pub(crate) job_scope: String,
    pub(crate) pending_count: i64,
}

#[derive(Debug)]
pub(crate) struct StoredMetadataReidentifyItem {
    pub(crate) job_id: String,
    pub(crate) item_id: String,
    pub(crate) status: String,
    pub(crate) candidate_count: i64,
    pub(crate) error: Option<String>,
    pub(crate) updated_at: i64,
}

fn stored_metadata_reidentify_job(row: sqlx::any::AnyRow) -> StoredMetadataReidentifyJob {
    StoredMetadataReidentifyJob {
        id: row.get("id"),
        status: row.get("status"),
        processed_count: row.get("processed_count"),
        total_count: row.get("total_count"),
        error: row.get("error"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        started_at: row.get("started_at"),
        finished_at: row.get("finished_at"),
        mode: row.get("mode"),
        cancel_requested: row.get::<i64, _>("cancel_requested") != 0,
        library_id: row.get("library_id"),
        job_scope: row.get("job_scope"),
        pending_count: row.get("pending_count"),
    }
}

fn stored_metadata_reidentify_item(row: sqlx::any::AnyRow) -> StoredMetadataReidentifyItem {
    StoredMetadataReidentifyItem {
        job_id: row.get("job_id"),
        item_id: row.get("item_id"),
        status: row.get("status"),
        candidate_count: row.get("candidate_count"),
        error: row.get("error"),
        updated_at: row.get("updated_at"),
    }
}

fn stored_metadata_candidate(row: sqlx::any::AnyRow) -> StoredMetadataCandidate {
    StoredMetadataCandidate {
        id: row.get("id"),
        item_id: row.get("item_id"),
        provider: row.get("provider"),
        provider_id: row.get("provider_id"),
        candidate_json: row.get("candidate_json"),
        score: row.get("score"),
        status: row.get("status"),
        expires_at: row.get("expires_at"),
        item_title: row.get("item_title"),
    }
}

fn stored_media_item(row: sqlx::any::AnyRow) -> StoredMediaItem {
    StoredMediaItem { id: row.get("id") }
}

#[derive(Debug)]
pub(crate) struct StoredCatalogRow {
    pub(crate) item_id: String,
    pub(crate) library_id: String,
    pub(crate) item_type: String,
    pub(crate) parent_id: Option<String>,
    pub(crate) series_id: Option<String>,
    pub(crate) season_number: Option<i64>,
    pub(crate) episode_number: Option<i64>,
    pub(crate) title: String,
    pub(crate) sort_title: String,
    pub(crate) original_title: Option<String>,
    pub(crate) overview: Option<String>,
    pub(crate) production_year: Option<i64>,
    pub(crate) rating: Option<f64>,
    pub(crate) rating_source: Option<String>,
    pub(crate) runtime_ticks: Option<i64>,
    pub(crate) poster_image_tag: Option<String>,
    pub(crate) fanart_image_tag: Option<String>,
    pub(crate) thumb_image_tag: Option<String>,
    pub(crate) logo_image_tag: Option<String>,
    pub(crate) source_id: Option<String>,
    pub(crate) source_kind: Option<String>,
    pub(crate) container: Option<String>,
    pub(crate) size: Option<i64>,
    pub(crate) external_url: Option<String>,
    pub(crate) edition_name: Option<String>,
    pub(crate) quality_label: Option<String>,
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
    pub(crate) stream_details_json: Option<String>,
    pub(crate) stream_is_external: Option<bool>,
    pub(crate) stream_is_default: Option<bool>,
    pub(crate) stream_is_forced: Option<bool>,
}

#[derive(Debug)]
pub(crate) struct StoredCatalogDetail {
    pub(crate) series_name: Option<String>,
    pub(crate) premiere_date: Option<String>,
    pub(crate) last_air_date: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) original_language: Option<String>,
    pub(crate) provider_ids_json: Option<String>,
    pub(crate) season_count: i64,
    pub(crate) episode_count: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredMediaChapter {
    pub(crate) source_id: String,
    pub(crate) start_position_ticks: i64,
    pub(crate) name: Option<String>,
    pub(crate) marker_type: String,
    pub(crate) chapter_index: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredUserItemState {
    pub(crate) position_ticks: i64,
    pub(crate) is_played: bool,
    pub(crate) is_favorite: bool,
    pub(crate) play_count: i64,
    pub(crate) last_played_at: Option<i64>,
    pub(crate) version: i64,
}

pub(crate) struct NewPlaybackEvent<'a> {
    pub(crate) user_id: &'a str,
    pub(crate) item_id: &'a str,
    pub(crate) media_source_id: Option<&'a str>,
    pub(crate) play_session_id: &'a str,
    pub(crate) device_id: &'a str,
    pub(crate) client: Option<&'a str>,
    pub(crate) device_name: Option<&'a str>,
    pub(crate) client_version: Option<&'a str>,
    pub(crate) device_type: Option<&'a str>,
    pub(crate) remote_ip: Option<&'a str>,
    pub(crate) state: &'a str,
    pub(crate) position_ticks: i64,
    pub(crate) duration_ticks: Option<i64>,
    pub(crate) played_percent: i64,
    pub(crate) is_paused: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredPlaybackSession {
    pub(crate) id: String,
    pub(crate) user_id: String,
    pub(crate) item_id: String,
    pub(crate) media_source_id: Option<String>,
    pub(crate) play_session_id: String,
    pub(crate) device_id: String,
    pub(crate) client: Option<String>,
    pub(crate) device_name: Option<String>,
    pub(crate) client_version: Option<String>,
    pub(crate) device_type: Option<String>,
    pub(crate) remote_ip: Option<String>,
    pub(crate) state: String,
    pub(crate) position_ticks: i64,
    pub(crate) duration_ticks: Option<i64>,
    pub(crate) is_paused: bool,
    pub(crate) started_at: i64,
    pub(crate) last_event_at: i64,
}

fn stored_playback_session(row: sqlx::any::AnyRow) -> StoredPlaybackSession {
    StoredPlaybackSession {
        id: row.get("id"),
        user_id: row.get("user_id"),
        item_id: row.get("item_id"),
        media_source_id: row.get("media_source_id"),
        play_session_id: row.get("play_session_id"),
        device_id: row.get("device_id"),
        client: row.get("client"),
        device_name: row.get("device_name"),
        client_version: row.get("client_version"),
        device_type: row.get("device_type"),
        remote_ip: row.get("remote_ip"),
        state: row.get("state"),
        position_ticks: row.get("position_ticks"),
        duration_ticks: row.get("duration_ticks"),
        is_paused: row.get::<i64, _>("is_paused") != 0,
        started_at: row.get("started_at"),
        last_event_at: row.get("last_event_at"),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredExternalSubtitle {
    pub(crate) media_source_id: String,
    pub(crate) item_id: String,
    pub(crate) external_path: String,
    pub(crate) language: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) root_path: String,
}

#[derive(Debug)]
pub(crate) struct StoredItemImageCandidate {
    pub(crate) id: String,
    pub(crate) local_path: String,
    pub(crate) root_path: String,
}

pub(crate) struct ItemImageMetadata<'a> {
    pub(crate) file_size: i64,
    pub(crate) width: Option<i32>,
    pub(crate) height: Option<i32>,
    pub(crate) content_tag: &'a str,
    pub(crate) source: &'a str,
}

#[derive(Debug)]
pub(crate) struct StoredItemImage {
    pub(crate) id: String,
    pub(crate) item_id: String,
    pub(crate) image_type: String,
    pub(crate) image_index: i64,
    pub(crate) local_path: String,
    pub(crate) file_size: Option<i64>,
    pub(crate) content_tag: Option<String>,
    pub(crate) source: String,
    pub(crate) root_path: Option<String>,
}

#[derive(Debug)]
pub(crate) struct StoredCatalogImageTag {
    pub(crate) id: String,
    pub(crate) image_type: String,
    pub(crate) image_index: i64,
}

#[derive(Debug)]
pub(crate) struct StoredLibraryPoster {
    pub(crate) item_id: String,
    pub(crate) local_path: String,
    pub(crate) root_path: String,
}

fn stored_item_image(row: sqlx::any::AnyRow) -> StoredItemImage {
    StoredItemImage {
        id: row.get("id"),
        item_id: row.get("item_id"),
        image_type: row.get("image_type"),
        image_index: row.get("image_index"),
        local_path: row.get("local_path"),
        file_size: row.get("file_size"),
        content_tag: row.get("content_tag"),
        source: row.get("source"),
        root_path: row.get("root_path"),
    }
}

fn catalog_filter_where_clause<'a>(
    filter: &CatalogFilterQuery<'a>,
) -> (String, Vec<CatalogBind<'a>>) {
    let library_ids = filter.library_ids;
    let user_id = filter.user_id;
    let item_types = filter.item_types;
    let item_ids = filter.item_ids;
    let media_source_ids = filter.media_source_ids;
    let excluded_item_types = filter.excluded_item_types;
    let years = filter.years;
    let is_played = filter.is_played;
    let is_favorite = filter.is_favorite;
    let metadata_pending = filter.metadata_pending;
    let mut where_clause = format!(
        "WHERE mi.removed_at IS NULL
         AND mi.library_id IN ({})",
        std::iter::repeat_n("?", library_ids.len())
            .collect::<Vec<_>>()
            .join(", ")
    );
    if item_types.is_empty() {
        where_clause.push_str(" AND mi.item_type <> 'FOLDER'");
    }
    where_clause.push_str(CATALOG_VISIBLE_PREDICATE);
    let mut binds = library_ids
        .iter()
        .map(|library_id| CatalogBind::Text(library_id.as_str()))
        .collect::<Vec<_>>();
    let mut id_predicates = Vec::new();
    if let Some(item_ids) = item_ids
        && !item_ids.is_empty()
    {
        id_predicates.push(format!(
            "mi.id IN ({})",
            std::iter::repeat_n("?", item_ids.len())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        binds.extend(
            item_ids
                .iter()
                .map(|item_id| CatalogBind::Text(item_id.as_str())),
        );
    }
    if let Some(media_source_ids) = media_source_ids
        && !media_source_ids.is_empty()
    {
        id_predicates.push(format!(
            "EXISTS (SELECT 1 FROM media_sources ms_filter
                     WHERE ms_filter.item_id = mi.id AND ms_filter.id IN ({}))",
            std::iter::repeat_n("?", media_source_ids.len())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        binds.extend(
            media_source_ids
                .iter()
                .map(|media_source_id| CatalogBind::Text(media_source_id.as_str())),
        );
    }
    if item_ids.is_some() || media_source_ids.is_some() {
        if id_predicates.is_empty() {
            where_clause.push_str(" AND 1 = 0");
        } else {
            where_clause.push_str(&format!(" AND ({})", id_predicates.join(" OR ")));
        }
    }
    if !item_types.is_empty() {
        where_clause.push_str(&format!(
            " AND mi.item_type IN ({})",
            std::iter::repeat_n("?", item_types.len())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        binds.extend(
            item_types
                .iter()
                .map(|item_type| CatalogBind::Text(item_type.as_str())),
        );
    }
    if !excluded_item_types.is_empty() {
        where_clause.push_str(&format!(
            " AND mi.item_type NOT IN ({})",
            std::iter::repeat_n("?", excluded_item_types.len())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        binds.extend(
            excluded_item_types
                .iter()
                .map(|item_type| CatalogBind::Text(item_type.as_str())),
        );
    }
    if !years.is_empty() {
        where_clause.push_str(&format!(
            " AND mi.production_year IN ({})",
            std::iter::repeat_n("?", years.len())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        binds.extend(years.iter().copied().map(CatalogBind::Integer));
    }
    if let Some(is_played) = is_played {
        where_clause.push_str(
            " AND COALESCE(
                (SELECT state_filter.is_played
                 FROM user_item_state state_filter
                 WHERE state_filter.user_id = ? AND state_filter.item_id = mi.id),
                0
            ) = ?",
        );
        binds.push(CatalogBind::Text(user_id));
        binds.push(CatalogBind::Integer(i64::from(is_played)));
    }
    if let Some(is_favorite) = is_favorite {
        where_clause.push_str(
            " AND COALESCE(
                (SELECT state_filter.is_favorite
                 FROM user_item_state state_filter
                 WHERE state_filter.user_id = ? AND state_filter.item_id = mi.id),
                0
            ) = ?",
        );
        binds.push(CatalogBind::Text(user_id));
        binds.push(CatalogBind::Integer(i64::from(is_favorite)));
    }
    if metadata_pending {
        where_clause.push_str(
            " AND EXISTS (
                SELECT 1 FROM metadata_candidates pending_metadata
                WHERE pending_metadata.item_id = mi.id
                  AND pending_metadata.status = 'PENDING'
            )",
        );
    }
    (where_clause, binds)
}

fn movie_parent_folder_identity(library_root_id: &str, relative_path: &str) -> Option<String> {
    let directory = relative_path
        .rsplit_once('/')
        .map(|(directory, _)| directory)
        .or_else(|| {
            relative_path
                .rsplit_once('\\')
                .map(|(directory, _)| directory)
        })
        .unwrap_or_default();
    let mut directory_key = String::new();
    for component in directory.split(['/', '\\']) {
        if component.is_empty() || component == "." {
            continue;
        }
        if !directory_key.is_empty() {
            directory_key.push('/');
        }
        directory_key.push_str(component);
    }
    (!directory_key.is_empty()).then(|| format!("folder:{library_root_id}:{directory_key}"))
}

const CATALOG_VISIBLE_PREDICATE: &str = " AND (
    mi.has_available_source = 1
    OR (
        mi.item_type IN ('SERIES', 'SEASON', 'BOX_SET', 'FOLDER')
        AND EXISTS (
            SELECT 1
            FROM media_items visible_child
            WHERE visible_child.removed_at IS NULL
              AND visible_child.has_available_source = 1
              AND (visible_child.parent_id = mi.id OR visible_child.series_id = mi.id)
        )
        OR EXISTS (
            SELECT 1
            FROM collection_items visible_collection_item
            JOIN collections visible_collection
              ON visible_collection.id = visible_collection_item.collection_id
            JOIN media_items visible_child
              ON visible_child.id = visible_collection_item.item_id
            WHERE visible_collection.item_id = mi.id
              AND visible_child.removed_at IS NULL
              AND visible_child.has_available_source = 1
        )
    )
)";

fn resume_runtime_ticks_sql() -> &'static str {
    "COALESCE(
        NULLIF(mi.runtime_ticks, 0),
        (
            SELECT ms_default.duration_ticks
            FROM media_sources ms_default
            JOIN filesystem_entries fe_default
              ON fe_default.id = ms_default.filesystem_entry_id
             AND fe_default.is_missing = 0
            WHERE ms_default.item_id = mi.id
              AND ms_default.is_default = 1
              AND ms_default.duration_ticks > 0
            ORDER BY ms_default.id
            LIMIT 1
        ),
        (
            SELECT ms_first.duration_ticks
            FROM media_sources ms_first
            JOIN filesystem_entries fe_first
              ON fe_first.id = ms_first.filesystem_entry_id
             AND fe_first.is_missing = 0
            WHERE ms_first.item_id = mi.id
              AND ms_first.duration_ticks > 0
            ORDER BY ms_first.id
            LIMIT 1
        )
    )"
}

#[derive(Clone, Copy)]
enum CatalogBind<'a> {
    Text(&'a str),
    Integer(i64),
}

#[derive(Clone, Copy)]
pub enum PersonSort {
    Name,
    DateCreated,
}

#[derive(Clone, Copy)]
pub struct PersonListOptions {
    pub recursive: bool,
    pub sort_by: PersonSort,
    pub descending: bool,
    pub offset: i64,
    pub limit: i64,
}

fn person_sort_order(sort_by: PersonSort, descending: bool) -> String {
    let direction = if descending { "DESC" } else { "ASC" };
    match sort_by {
        PersonSort::Name => format!(
            "MIN(pc.person_name) {direction}, MIN(mi.added_at) DESC, MIN(pc.provider) ASC, MIN(pc.person_id) ASC"
        ),
        PersonSort::DateCreated => format!(
            "MIN(mi.added_at) {direction}, MIN(pc.person_name) ASC, MIN(pc.provider) ASC, MIN(pc.person_id) ASC"
        ),
    }
}

pub(crate) struct CatalogFilterQuery<'a> {
    pub(crate) library_ids: &'a [String],
    pub(crate) user_id: &'a str,
    pub(crate) item_types: &'a [String],
    pub(crate) excluded_item_types: &'a [String],
    pub(crate) item_ids: Option<&'a [String]>,
    pub(crate) media_source_ids: Option<&'a [String]>,
    pub(crate) years: &'a [i64],
    pub(crate) is_played: Option<bool>,
    pub(crate) is_favorite: Option<bool>,
    pub(crate) metadata_pending: bool,
    pub(crate) sort_by: CatalogSort,
    pub(crate) descending: bool,
    pub(crate) offset: i64,
    pub(crate) limit: i64,
}

pub(crate) struct ResumeItemsQuery<'a> {
    pub(crate) user_id: &'a str,
    pub(crate) library_ids: &'a [String],
    pub(crate) item_types: &'a [&'a str],
    pub(crate) played_percent: i64,
    pub(crate) minimum_ticks: i64,
    pub(crate) offset: i64,
    pub(crate) limit: i64,
}

#[derive(Clone, Copy)]
pub(crate) enum CatalogSort {
    Name,
    DateCreated,
    PremiereDate,
    Rating,
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
pub(crate) struct StoredItemScanPath {
    pub(crate) library_id: String,
    pub(crate) library_root_id: String,
    pub(crate) relative_path: String,
}

pub(crate) struct StoredPlaybackSource {
    pub(crate) source_kind: String,
    pub(crate) external_url: Option<String>,
    pub(crate) root_path: String,
    pub(crate) relative_path: String,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredDanmakuSource {
    pub(crate) source_id: String,
    pub(crate) root_path: String,
    pub(crate) relative_path: String,
}

pub(crate) struct NewDanmakuTrack<'a> {
    pub(crate) id: &'a str,
    pub(crate) media_source_id: &'a str,
    pub(crate) relative_path: &'a str,
    pub(crate) provider: Option<&'a str>,
    pub(crate) provider_anime_id: Option<&'a str>,
    pub(crate) provider_episode_id: Option<&'a str>,
    pub(crate) fingerprint: Option<&'a [u8]>,
    pub(crate) status: &'a str,
    pub(crate) error_code: Option<&'a str>,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredDanmakuMatchJob {
    pub(crate) id: String,
    pub(crate) library_id: String,
    pub(crate) status: String,
    pub(crate) overwrite: bool,
    pub(crate) concurrency: i64,
    pub(crate) total_count: i64,
    pub(crate) processed_count: i64,
    pub(crate) success_count: i64,
    pub(crate) skipped_count: i64,
    pub(crate) failed_count: i64,
    pub(crate) cancel_requested: bool,
    pub(crate) error: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredDanmakuMatchItem {
    pub(crate) id: String,
    pub(crate) media_source_id: String,
    pub(crate) root_path: Option<String>,
    pub(crate) relative_path: Option<String>,
}

pub(crate) struct NewDanmakuMatchJob<'a> {
    pub(crate) id: &'a str,
    pub(crate) library_id: &'a str,
    pub(crate) overwrite: bool,
    pub(crate) concurrency: i64,
}

#[derive(Debug)]
pub(crate) struct StoredThumbnailSource {
    pub(crate) item_id: String,
    pub(crate) root_path: String,
    pub(crate) relative_path: String,
    pub(crate) thumbnail_path: Option<String>,
}

#[derive(Debug)]
pub(crate) struct StoredStrmMediaSource {
    pub(crate) source_id: String,
    pub(crate) item_id: String,
    pub(crate) poster_fallback_required: bool,
    pub(crate) has_media_info: bool,
    pub(crate) external_url: Option<String>,
    pub(crate) root_path: String,
    pub(crate) relative_path: String,
    pub(crate) thumbnail_path: Option<String>,
}

#[derive(Debug)]
pub(crate) struct StoredImageIdentity {
    pub(crate) item_type: String,
    pub(crate) provider_name: Option<String>,
    pub(crate) provider_id: Option<String>,
    pub(crate) season_number: Option<i64>,
    pub(crate) episode_number: Option<i64>,
}

#[derive(Debug)]
pub(crate) struct StoredMovieIdentity {
    pub(crate) library_id: String,
    pub(crate) provider_name: String,
    pub(crate) provider_id: String,
}

fn stored_danmaku_match_job(row: sqlx::any::AnyRow) -> StoredDanmakuMatchJob {
    StoredDanmakuMatchJob {
        id: row.get("id"),
        library_id: row.get("library_id"),
        status: row.get("status"),
        overwrite: row.get::<i64, _>("overwrite") != 0,
        concurrency: row.get("concurrency"),
        total_count: row.get("total_count"),
        processed_count: row.get("processed_count"),
        success_count: row.get("success_count"),
        skipped_count: row.get("skipped_count"),
        failed_count: row.get("failed_count"),
        cancel_requested: row.get::<i64, _>("cancel_requested") != 0,
        error: row.get("error"),
    }
}

fn first_provider_id(
    primary: Option<String>,
    secondary: Option<String>,
    preferred: Option<&str>,
) -> Option<(String, String)> {
    let providers = [primary, secondary]
        .into_iter()
        .flatten()
        .filter_map(|raw| {
            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&raw).ok()
        })
        .flat_map(|object| object.into_iter())
        .filter_map(|(name, value)| {
            let id = value
                .as_str()
                .map(str::to_owned)
                .or_else(|| value.as_i64().map(|value| value.to_string()))?;
            (!name.trim().is_empty() && !id.trim().is_empty()).then_some((name, id))
        })
        .collect::<Vec<_>>();
    if let Some(preferred) = preferred {
        let short_preferred = preferred
            .rsplit(['.', ':', '/'])
            .next()
            .unwrap_or(preferred);
        providers
            .iter()
            .find(|(name, _)| {
                name.eq_ignore_ascii_case(preferred) || name.eq_ignore_ascii_case(short_preferred)
            })
            .cloned()
    } else {
        providers.into_iter().next()
    }
}

#[derive(Debug)]
pub(crate) struct StoredMediaItemKind {
    pub(crate) item_type: String,
    pub(crate) season_number: Option<i64>,
}

#[derive(Debug)]
pub(crate) struct StoredSeriesMetadataSource {
    pub(crate) series_id: String,
    pub(crate) season_id: String,
    pub(crate) episode_id: String,
    pub(crate) season_number: Option<i64>,
    pub(crate) root_path: String,
    pub(crate) relative_path: String,
}

#[derive(Debug)]
pub(crate) struct StoredChapterDetectionSource {
    pub(crate) source_id: String,
    pub(crate) item_id: String,
    pub(crate) season_id: String,
    pub(crate) fingerprint: Option<Vec<u8>>,
    pub(crate) duration_ticks: Option<i64>,
    pub(crate) provider_ids_json: Option<String>,
    pub(crate) series_provider_ids_json: Option<String>,
    pub(crate) season_number: Option<i64>,
    pub(crate) episode_number: Option<i64>,
    pub(crate) state: Option<StoredChapterDetectionSourceState>,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredChapterDetectionSourceState {
    pub(crate) input_fingerprint: Vec<u8>,
    pub(crate) status: String,
    pub(crate) last_checked_at: i64,
    pub(crate) next_retry_at: Option<i64>,
    pub(crate) intro_fingerprint: Option<Vec<u8>>,
    pub(crate) credits_fingerprint: Option<Vec<u8>>,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredChapterDetectionItem {
    pub(crate) source_id: String,
    pub(crate) season_id: String,
    pub(crate) source_fingerprint: Option<Vec<u8>>,
    pub(crate) input_fingerprint: Vec<u8>,
    pub(crate) is_context: bool,
    pub(crate) intro_fingerprint: Option<Vec<u8>>,
    pub(crate) credits_fingerprint: Option<Vec<u8>>,
    pub(crate) duration_ticks: Option<i64>,
    pub(crate) root_path: String,
    pub(crate) relative_path: String,
    pub(crate) provider_ids_json: Option<String>,
    pub(crate) series_provider_ids_json: Option<String>,
    pub(crate) season_number: Option<i64>,
    pub(crate) episode_number: Option<i64>,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredChapterDetectionJob {
    pub(crate) id: String,
    pub(crate) library_id: String,
    pub(crate) plugin_id: String,
    pub(crate) status: String,
    pub(crate) concurrency: i64,
    pub(crate) intro_window_seconds: i64,
    pub(crate) credits_window_seconds: i64,
    pub(crate) match_threshold: f64,
    pub(crate) cursor: Option<String>,
    pub(crate) processed_count: i64,
    pub(crate) total_count: i64,
    pub(crate) cancel_requested: bool,
    pub(crate) error: Option<String>,
}

pub(crate) struct NewChapterDetectionJob<'a> {
    pub(crate) id: &'a str,
    pub(crate) library_id: &'a str,
    pub(crate) plugin_id: &'a str,
    pub(crate) concurrency: i64,
    pub(crate) intro_window_seconds: i64,
    pub(crate) credits_window_seconds: i64,
    pub(crate) match_threshold: f64,
    pub(crate) total_count: i64,
}

pub(crate) struct NewChapterDetectionJobItem<'a> {
    pub(crate) job_id: &'a str,
    pub(crate) source_id: &'a str,
    pub(crate) item_id: &'a str,
    pub(crate) season_id: &'a str,
    pub(crate) source_fingerprint: &'a [u8],
    pub(crate) input_fingerprint: &'a [u8],
    pub(crate) is_context: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredLibraryCoverJob {
    pub(crate) id: String,
    pub(crate) library_id: String,
    pub(crate) is_manual: bool,
    pub(crate) status: String,
    pub(crate) processed_count: i64,
    pub(crate) total_count: i64,
    pub(crate) error: Option<String>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) started_at: Option<i64>,
    pub(crate) finished_at: Option<i64>,
}

fn stored_library_cover_job(row: sqlx::any::AnyRow) -> StoredLibraryCoverJob {
    StoredLibraryCoverJob {
        id: row.get("id"),
        library_id: row.get("library_id"),
        is_manual: row.get::<i64, _>("is_manual") != 0,
        status: row.get("status"),
        processed_count: row.get("processed_count"),
        total_count: row.get("total_count"),
        error: row.get("error"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        started_at: row.get("started_at"),
        finished_at: row.get("finished_at"),
    }
}

pub(crate) struct NewMediaChapterMarker {
    pub(crate) start_position_ticks: i64,
    pub(crate) name: Option<String>,
    pub(crate) marker_type: String,
    pub(crate) chapter_index: i64,
    pub(crate) confidence: f64,
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
pub(crate) struct StoredWebSessionSummary {
    pub(crate) id: String,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) expires_at: i64,
    pub(crate) last_seen_at: Option<i64>,
    pub(crate) is_current: bool,
}

#[derive(Debug)]
pub(crate) struct StoredDownloadSource {
    pub(crate) source_kind: String,
    pub(crate) root_path: String,
    pub(crate) relative_path: String,
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
    pub(crate) realtime_metadata_auto_match_enabled: bool,
    pub(crate) reconciliation_schedule: Option<&'a str>,
    pub(crate) metadata_schedule: Option<&'a str>,
    pub(crate) scan_concurrency: i64,
    pub(crate) probe_concurrency: i64,
    pub(crate) scraper_id: Option<&'a str>,
    pub(crate) chapter_source_id: Option<&'a str>,
}

pub(crate) struct MediaMetadataUpdate<'a> {
    pub(crate) item_id: &'a str,
    pub(crate) title: &'a str,
    pub(crate) original_title: Option<&'a str>,
    pub(crate) overview: Option<&'a str>,
    pub(crate) production_year: Option<i64>,
    pub(crate) metadata_fingerprint: &'a [u8],
    pub(crate) provenance_json: &'a str,
    pub(crate) locked_fields_json: &'a str,
}

pub(crate) struct ExternalSubtitleUpdate<'a> {
    pub(crate) item_id: &'a str,
    pub(crate) media_source_id: &'a str,
    pub(crate) stream_index: i64,
    pub(crate) title: Option<&'a str>,
    pub(crate) language: Option<&'a str>,
    pub(crate) is_default: bool,
    pub(crate) is_forced: bool,
}

pub(crate) struct SelectedMetadataUpdate<'a> {
    pub(crate) item_id: &'a str,
    pub(crate) candidate_id: &'a str,
    pub(crate) title: &'a str,
    pub(crate) original_title: Option<&'a str>,
    pub(crate) overview: Option<&'a str>,
    pub(crate) production_year: Option<i64>,
    pub(crate) premiere_date: Option<&'a str>,
    pub(crate) last_air_date: Option<&'a str>,
    pub(crate) status: Option<&'a str>,
    pub(crate) original_language: Option<&'a str>,
    pub(crate) rating: Option<f64>,
    pub(crate) rating_source: Option<&'a str>,
    pub(crate) provider_ids_json: &'a str,
    pub(crate) metadata_fingerprint: &'a [u8],
    pub(crate) provenance_json: &'a str,
    pub(crate) locked_fields_json: &'a str,
    pub(crate) poster_fallback_required: bool,
    pub(crate) keep_pending: bool,
}

pub(crate) struct LibrarySettingsUpdate<'a> {
    pub(crate) name: Option<&'a str>,
    pub(crate) kind: Option<&'a str>,
    pub(crate) is_enabled: Option<bool>,
    pub(crate) realtime_watch_enabled: Option<bool>,
    pub(crate) realtime_metadata_auto_match_enabled: Option<bool>,
    pub(crate) reconciliation_schedule: Option<Option<&'a str>>,
    pub(crate) metadata_schedule: Option<Option<&'a str>>,
    pub(crate) scan_concurrency: Option<i64>,
    pub(crate) probe_concurrency: Option<i64>,
    pub(crate) scraper_id: Option<Option<&'a str>>,
    pub(crate) chapter_source_id: Option<Option<&'a str>>,
    pub(crate) media_strategy_json: Option<Option<&'a str>>,
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
    pub(crate) inode: Option<i64>,
    pub(crate) fingerprint: &'a [u8],
    pub(crate) last_seen_generation: &'a str,
}

pub(crate) struct FilesystemEntryMove<'a> {
    pub(crate) entry_id: &'a str,
    pub(crate) library_root_id: &'a str,
    pub(crate) relative_path: &'a str,
    pub(crate) size: i64,
    pub(crate) modified_at: i64,
    pub(crate) inode: Option<i64>,
    pub(crate) fingerprint: &'a [u8],
    pub(crate) generation: &'a str,
}

pub(crate) struct NewMediaItem<'a> {
    pub(crate) id: &'a str,
    pub(crate) library_id: &'a str,
    pub(crate) title: &'a str,
    pub(crate) sort_title: &'a str,
    pub(crate) original_title: Option<&'a str>,
    pub(crate) production_year: Option<i64>,
    pub(crate) provider_ids_json: Option<&'a str>,
}

pub(crate) struct NewHierarchyItem<'a> {
    pub(crate) id: &'a str,
    pub(crate) library_id: &'a str,
    pub(crate) item_type: &'a str,
    pub(crate) parent_id: Option<&'a str>,
    pub(crate) series_id: Option<&'a str>,
    pub(crate) season_number: Option<i64>,
    pub(crate) episode_number: Option<i64>,
    pub(crate) absolute_number: Option<i64>,
    pub(crate) title: &'a str,
    pub(crate) sort_title: &'a str,
    pub(crate) original_title: Option<&'a str>,
    pub(crate) production_year: Option<i64>,
    pub(crate) provider_ids_json: Option<&'a str>,
    pub(crate) identification_status: &'a str,
    pub(crate) identity_key: &'a str,
}

pub(crate) struct NewMediaSource<'a> {
    pub(crate) id: &'a str,
    pub(crate) item_id: &'a str,
    pub(crate) source_kind: &'a str,
    pub(crate) filesystem_entry_id: &'a str,
    pub(crate) edition_name: Option<&'a str>,
    pub(crate) quality_label: Option<&'a str>,
    pub(crate) container: &'a str,
    pub(crate) size: i64,
    pub(crate) external_url: Option<&'a str>,
    pub(crate) strm_target_kind: Option<&'a str>,
    pub(crate) is_default: bool,
}

pub(crate) struct NewMovieFile {
    pub(crate) filesystem_entry_id: String,
    pub(crate) source_id: String,
    pub(crate) relative_path: String,
    pub(crate) size: i64,
    pub(crate) modified_at: i64,
    pub(crate) fingerprint: Vec<u8>,
    pub(crate) title: String,
    pub(crate) sort_title: String,
    pub(crate) original_title: String,
    pub(crate) production_year: Option<i64>,
    pub(crate) provider_ids_json: Option<String>,
    pub(crate) source_kind: String,
    pub(crate) strm_target_kind: Option<String>,
    pub(crate) edition_name: Option<String>,
    pub(crate) quality_label: Option<String>,
    pub(crate) container: String,
    pub(crate) external_url: Option<String>,
}

pub(crate) struct MediaProbeUpdate<'a> {
    pub(crate) source_id: &'a str,
    pub(crate) container: Option<&'a str>,
    pub(crate) source_size: Option<i64>,
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
    pub(crate) details_json: Option<&'a str>,
    pub(crate) external_path: Option<&'a str>,
    pub(crate) is_external: bool,
    pub(crate) is_default: bool,
    pub(crate) is_forced: bool,
}

async fn ensure_server_id(pool: &AnyPool, backend: DatabaseBackend) -> Result<String, sqlx::Error> {
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
    sqlx::query(sqlx::AssertSqlSafe(adapt_sql_for_backend(
        backend,
        "INSERT INTO lux_meta (key, value) VALUES ('server_id', ?)
         ON CONFLICT(key) DO NOTHING",
    )))
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
    Configuration(DatabaseConfigurationError),
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
    Conflict(String),
    Serialization(String),
    LastManager,
}

impl StorageError {
    pub(crate) fn is_unique_violation(&self) -> bool {
        matches!(
            self,
            Self::Sqlx { source, .. }
                if source
                    .as_database_error()
                    .is_some_and(|error| error.is_unique_violation())
        )
    }
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Configuration(source) => write!(formatter, "数据库配置无效: {source}"),
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
            Self::Conflict(source) => write!(formatter, "database conflict: {source}"),
            Self::Serialization(source) => {
                write!(formatter, "database serialization failed: {source}")
            }
            Self::LastManager => {
                formatter.write_str("at least one active server manager is required")
            }
        }
    }
}

fn adapt_sql_for_backend(backend: DatabaseBackend, sql: impl sqlx::SqlSafeStr) -> String {
    let sql = sql.into_sql_str();
    if backend == DatabaseBackend::Sqlite {
        return sql.as_str().to_owned();
    }

    let mut adapted = String::with_capacity(sql.as_str().len() + 8);
    let mut parameter_index = 1;
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut characters = sql.as_str().chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\'' if !in_double_quote => {
                adapted.push(character);
                if in_single_quote && characters.peek() == Some(&'\'') {
                    if let Some(escaped_quote) = characters.next() {
                        adapted.push(escaped_quote);
                    }
                } else {
                    in_single_quote = !in_single_quote;
                }
            }
            '"' if !in_single_quote => {
                in_double_quote = !in_double_quote;
                adapted.push(character);
            }
            '?' if !in_single_quote && !in_double_quote => {
                adapted.push('$');
                adapted.push_str(&parameter_index.to_string());
                parameter_index += 1;
            }
            _ => adapted.push(character),
        }
    }
    adapted
}

impl std::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Configuration(source) => Some(source),
            Self::Io { source, .. } => Some(source),
            Self::Sqlx { source, .. } => Some(source),
            Self::Migration { source, .. } => Some(source),
            Self::Conflict(_) | Self::LastManager | Self::Serialization(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        application::{libraries::LibraryService, scanner::LibraryScanner},
        config::{Config, PostgresConnection},
        library::LibraryKind,
    };

    #[tokio::test]
    async fn metadata_job_list_counts_only_pending_items_on_the_requested_page() {
        sqlx::any::install_default_drivers();
        let pool = AnyPoolOptions::new()
            .max_connections(1)
            .connect_with(
                AnyConnectOptions::from_str("sqlite://?mode=memory")
                    .expect("in-memory SQLite options"),
            )
            .await
            .expect("in-memory SQLite connection");
        sqlx::query(
            "CREATE TABLE metadata_reidentify_jobs (
                id TEXT PRIMARY KEY,
                status TEXT NOT NULL,
                processed_count INTEGER NOT NULL,
                total_count INTEGER NOT NULL,
                error TEXT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                started_at INTEGER,
                finished_at INTEGER,
                mode TEXT NOT NULL,
                cancel_requested INTEGER NOT NULL,
                library_id TEXT,
                job_scope TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("create metadata jobs table");
        sqlx::query(
            "CREATE TABLE metadata_reidentify_job_items (
                job_id TEXT NOT NULL,
                item_id TEXT NOT NULL,
                status TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("create metadata job items table");
        sqlx::query(
            "CREATE TABLE metadata_candidates (
                item_id TEXT NOT NULL,
                status TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("create metadata candidates table");
        sqlx::query(
            "CREATE INDEX idx_metadata_reidentify_items_status
             ON metadata_reidentify_job_items(job_id, status, item_id)",
        )
        .execute(&pool)
        .await
        .expect("create metadata job item index");
        sqlx::query(
            "CREATE INDEX idx_metadata_candidates_item
             ON metadata_candidates(item_id, status)",
        )
        .execute(&pool)
        .await
        .expect("create metadata candidate index");
        for (id, created_at) in [("older", 1_i64), ("newer", 2_i64)] {
            sqlx::query(
                "INSERT INTO metadata_reidentify_jobs (
                    id, status, processed_count, total_count, error,
                    created_at, updated_at, started_at, finished_at, mode,
                    cancel_requested, library_id, job_scope
                 ) VALUES (?, 'QUEUED', 0, 2, NULL, ?, ?, NULL, NULL,
                           'REIDENTIFY', 0, NULL, 'ITEMS')",
            )
            .bind(id)
            .bind(created_at)
            .bind(created_at)
            .execute(&pool)
            .await
            .expect("insert metadata job");
        }
        for (job_id, item_id) in [("older", "old-item"), ("newer", "new-item")] {
            sqlx::query(
                "INSERT INTO metadata_reidentify_job_items (job_id, item_id, status)
                 VALUES (?, ?, 'PENDING')",
            )
            .bind(job_id)
            .bind(item_id)
            .execute(&pool)
            .await
            .expect("insert metadata job item");
        }
        sqlx::query(
            "INSERT INTO metadata_candidates (item_id, status)
             VALUES ('old-item', 'PENDING'), ('new-item', 'PENDING'),
                    ('new-item', 'PENDING')",
        )
        .execute(&pool)
        .await
        .expect("insert metadata candidates");

        let database = Database {
            pool,
            path: PathBuf::from("metadata-summary-test.db"),
            server_id: "test".to_owned(),
            backend: DatabaseBackend::Sqlite,
        };
        let jobs = database
            .list_metadata_reidentify_jobs(None, 0, 1)
            .await
            .expect("list metadata jobs");

        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "newer");
        assert_eq!(jobs[0].pending_count, 1);

        let plan = sqlx::query(
            "EXPLAIN QUERY PLAN
             WITH selected_jobs AS (
                 SELECT id
                 FROM metadata_reidentify_jobs
                 ORDER BY created_at DESC, id DESC
                 LIMIT 1 OFFSET 0
             ), pending_counts AS (
                 SELECT job_items.job_id, COUNT(DISTINCT candidates.item_id) AS pending_count
                 FROM metadata_reidentify_job_items job_items
                 JOIN selected_jobs ON selected_jobs.id = job_items.job_id
                 JOIN metadata_candidates candidates
                   ON candidates.item_id = job_items.item_id
                  AND candidates.status = 'PENDING'
                 GROUP BY job_items.job_id
             )
             SELECT selected_jobs.id, pending_counts.pending_count
             FROM selected_jobs
             LEFT JOIN pending_counts ON pending_counts.job_id = selected_jobs.id",
        )
        .fetch_all(database.pool())
        .await
        .expect("explain metadata summary query");
        let plan_details = plan
            .iter()
            .map(|row| row.get::<String, _>("detail"))
            .collect::<Vec<_>>();
        assert!(
            plan_details.iter().any(|detail| detail
                .contains("USING COVERING INDEX idx_metadata_reidentify_items_status")),
            "metadata summary should seek selected job items by job_id: {plan_details:?}"
        );
        assert!(
            plan_details
                .iter()
                .any(|detail| detail.contains("USING COVERING INDEX idx_metadata_candidates_item")),
            "metadata summary should seek candidates by item_id: {plan_details:?}"
        );
        assert!(
            plan_details
                .iter()
                .all(|detail| !detail.contains("SCAN metadata_reidentify_job_items")),
            "metadata summary must not scan all job items: {plan_details:?}"
        );
        assert!(
            plan_details
                .iter()
                .all(|detail| !detail.contains("SCAN metadata_candidates")),
            "metadata summary must not scan all candidates: {plan_details:?}"
        );
        database.close().await;
    }

    #[tokio::test]
    async fn person_credits_migration_creates_the_index_table() {
        let temp_dir = tempfile::tempdir().expect("temporary directory");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("test address"),
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await.expect("database");
        let table_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name = 'person_credits'",
        )
        .fetch_one(database.pool())
        .await
        .expect("person credits table");
        assert_eq!(table_count, 1);

        let libraries = LibraryService::new(database.clone());
        let library = libraries
            .create_library("Movies", LibraryKind::Movie, false)
            .await
            .expect("library");
        let library_id = library.id.to_string();
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('item-credits', ?, 'MOVIE', 'Credits', 'credits', 'LOCAL_CONFIRMED')",
        )
        .bind(&library_id)
        .execute(database.pool())
        .await
        .expect("media item");
        database
            .replace_person_credits(
                "item-credits",
                &[
                    NewPersonCredit {
                        person_id: "1".to_owned(),
                        lux_person_id: Some("lux-000001".to_owned()),
                        person_type: "Actor".to_owned(),
                        person_name: "演员甲".to_owned(),
                        provider: "tmdb".to_owned(),
                        role: "角色甲".to_owned(),
                        sort_order: 0,
                        biography: None,
                        birthday: None,
                        deathday: None,
                        known_for_department: None,
                        place_of_birth: None,
                        provider_ids: BTreeMap::new(),
                        genres: Vec::new(),
                        tags: Vec::new(),
                        production_locations: Vec::new(),
                        premiere_date: None,
                        production_year: None,
                        taglines: Vec::new(),
                    },
                    NewPersonCredit {
                        person_id: "9".to_owned(),
                        lux_person_id: Some("lux-000001".to_owned()),
                        person_type: "Actor".to_owned(),
                        person_name: "演员甲".to_owned(),
                        provider: "douban".to_owned(),
                        role: "角色甲".to_owned(),
                        sort_order: 0,
                        biography: None,
                        birthday: None,
                        deathday: None,
                        known_for_department: None,
                        place_of_birth: None,
                        provider_ids: BTreeMap::new(),
                        genres: Vec::new(),
                        tags: Vec::new(),
                        production_locations: Vec::new(),
                        premiere_date: None,
                        production_year: None,
                        taglines: Vec::new(),
                    },
                    NewPersonCredit {
                        person_id: "2".to_owned(),
                        lux_person_id: None,
                        person_type: "Actor".to_owned(),
                        person_name: "演员乙".to_owned(),
                        provider: "tmdb".to_owned(),
                        role: "角色乙".to_owned(),
                        sort_order: 1,
                        biography: None,
                        birthday: None,
                        deathday: None,
                        known_for_department: None,
                        place_of_birth: None,
                        provider_ids: BTreeMap::new(),
                        genres: Vec::new(),
                        tags: Vec::new(),
                        production_locations: Vec::new(),
                        premiere_date: None,
                        production_year: None,
                        taglines: Vec::new(),
                    },
                ],
            )
            .await
            .expect("person credits");
        let (credits, total) = database
            .list_person_credits_for_library(
                &library_id,
                "Actor",
                PersonListOptions {
                    recursive: true,
                    sort_by: PersonSort::Name,
                    descending: false,
                    offset: 0,
                    limit: 10,
                },
            )
            .await
            .expect("list person credits");
        assert_eq!(total, 2);
        assert_eq!(credits.len(), 2);
        let names = credits
            .iter()
            .map(|credit| credit.person_name.as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"演员甲"));
        assert!(names.contains(&"演员乙"));
        assert_eq!(
            credits
                .iter()
                .filter(|credit| credit.lux_person_id.as_deref() == Some("lux-000001"))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn canonical_people_migration_creates_recoverable_identity_tables() {
        let temp_dir = tempfile::tempdir().expect("temporary directory");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("test address"),
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await.expect("database");
        for table in ["people", "person_identities", "person_id_sequence"] {
            let table_count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
            )
            .bind(table)
            .fetch_one(database.pool())
            .await
            .expect("canonical people table");
            assert_eq!(table_count, 1, "missing canonical people table {table}");
        }
    }

    #[tokio::test]
    async fn canonical_people_reuse_one_lux_id_across_provider_identities() {
        let temp_dir = tempfile::tempdir().expect("temporary directory");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("test address"),
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await.expect("database");
        let first = database
            .resolve_or_create_canonical_person(
                "华晨宇",
                "tmdb",
                "57975",
                "PROVIDER_ID",
                Some(1.0),
                r#"{"source":"tmdb"}"#,
            )
            .await
            .expect("first canonical person");
        assert_eq!(first.id, "lux-000001");

        let second = database
            .attach_canonical_person_identity(
                &first.id,
                "douban",
                "1313123",
                "MEDIA_BRIDGE",
                Some(0.98),
                r#"{"source":"same-media"}"#,
            )
            .await
            .expect("second canonical identity");
        assert_eq!(second.id, first.id);

        let repeated = database
            .resolve_or_create_canonical_person(
                "华晨宇",
                "tmdb",
                "57975",
                "PROVIDER_ID",
                Some(1.0),
                r#"{"source":"tmdb"}"#,
            )
            .await
            .expect("repeated canonical identity");
        assert_eq!(repeated.id, first.id);

        let identity_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM person_identities")
            .fetch_one(database.pool())
            .await
            .expect("identity count");
        assert_eq!(identity_count, 2);
    }

    #[tokio::test]
    async fn restoring_a_manifest_rejects_a_provider_identity_owned_by_another_person() {
        let temp_dir = tempfile::tempdir().expect("temporary directory");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("test address"),
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await.expect("database");
        database
            .resolve_or_create_canonical_person(
                "华晨宇",
                "tmdb",
                "57975",
                "PROVIDER_ID",
                Some(1.0),
                r#"{"source":"tmdb"}"#,
            )
            .await
            .expect("first canonical person");

        let error = database
            .restore_canonical_person("lux-000002", "另一位演员", &[("tmdb", "57975")])
            .await
            .expect_err("conflicting manifest must be rejected");
        assert!(matches!(error, StorageError::Conflict(_)));
        assert_eq!(
            database
                .find_canonical_person_by_identity("tmdb", "57975")
                .await
                .expect("identity lookup")
                .expect("existing identity")
                .id,
            "lux-000001"
        );
    }

    #[tokio::test]
    async fn person_match_candidates_are_persistent_and_idempotent() {
        let temp_dir = tempfile::tempdir().expect("temporary directory");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("test address"),
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await.expect("database");
        let library = LibraryService::new(database.clone())
            .create_library("Movies", LibraryKind::Movie, false)
            .await
            .expect("library");
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('item-1', ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
        )
        .bind(library.id.to_string())
        .execute(database.pool())
        .await
        .expect("media item");
        let first_id = database
            .enqueue_person_match_candidate(
                "item-1",
                "douban",
                "1313123",
                r#"["lux-000001","lux-000002"]"#,
                Some(0.62),
                r#"{"method":"same-media-ambiguous"}"#,
            )
            .await
            .expect("first candidate");
        let second_id = database
            .enqueue_person_match_candidate(
                "item-1",
                "douban",
                "1313123",
                r#"["lux-000002","lux-000001"]"#,
                Some(0.65),
                r#"{"method":"same-media-ambiguous","retry":true}"#,
            )
            .await
            .expect("idempotent candidate update");
        assert_eq!(first_id, second_id);

        let (count, status, score): (i64, String, f64) = sqlx::query_as(
            "SELECT COUNT(*), MIN(status), MAX(score)
             FROM person_match_candidates
             WHERE item_id = 'item-1' AND provider = 'douban' AND provider_id = '1313123'",
        )
        .fetch_one(database.pool())
        .await
        .expect("candidate row");
        assert_eq!(count, 1);
        assert_eq!(status, "PENDING");
        assert_eq!(score, 0.65);

        sqlx::query("UPDATE person_match_candidates SET status = 'CONFIRMED' WHERE id = ?")
            .bind(&first_id)
            .execute(database.pool())
            .await
            .expect("mark candidate decided");
        database
            .enqueue_person_match_candidate(
                "item-1",
                "douban",
                "1313123",
                r#"["lux-000002"]"#,
                Some(0.9),
                r#"{"method":"retry"}"#,
            )
            .await
            .expect("retry decided candidate");
        let preserved_status: String =
            sqlx::query_scalar("SELECT status FROM person_match_candidates WHERE id = ?")
                .bind(&first_id)
                .fetch_one(database.pool())
                .await
                .expect("preserved candidate status");
        assert_eq!(preserved_status, "CONFIRMED");
    }

    #[tokio::test]
    async fn confirming_person_match_moves_identity_and_credit_atomically() {
        let temp_dir = tempfile::tempdir().expect("temporary directory");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("test address"),
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await.expect("database");
        let library = LibraryService::new(database.clone())
            .create_library("Movies", LibraryKind::Movie, false)
            .await
            .expect("library");
        let library_id = library.id.to_string();
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('item-confirm', ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
        )
        .bind(&library_id)
        .execute(database.pool())
        .await
        .expect("media item");
        let old = database
            .resolve_or_create_canonical_person(
                "旧人物",
                "douban",
                "1313123",
                "PROVIDER_ID",
                Some(1.0),
                r#"{"source":"douban"}"#,
            )
            .await
            .expect("old person");
        let target = database
            .resolve_or_create_canonical_person(
                "目标人物",
                "tmdb",
                "57975",
                "PROVIDER_ID",
                Some(1.0),
                r#"{"source":"tmdb"}"#,
            )
            .await
            .expect("target person");
        database
            .replace_person_credits(
                "item-confirm",
                &[NewPersonCredit {
                    person_id: "1313123".to_owned(),
                    lux_person_id: Some(old.id.clone()),
                    person_type: "Actor".to_owned(),
                    person_name: "旧人物".to_owned(),
                    provider: "douban".to_owned(),
                    role: "角色".to_owned(),
                    sort_order: 0,
                    biography: None,
                    birthday: None,
                    deathday: None,
                    known_for_department: None,
                    place_of_birth: None,
                    provider_ids: BTreeMap::new(),
                    genres: Vec::new(),
                    tags: Vec::new(),
                    production_locations: Vec::new(),
                    premiere_date: None,
                    production_year: None,
                    taglines: Vec::new(),
                }],
            )
            .await
            .expect("credit");
        database
            .enqueue_person_match_candidate(
                "item-confirm",
                "douban",
                "1313123",
                &format!("[\"{}\"]", target.id),
                Some(0.9),
                r#"{"method":"same-media"}"#,
            )
            .await
            .expect("candidate");
        let candidate_id: String = sqlx::query_scalar(
            "SELECT id FROM person_match_candidates
             WHERE item_id = 'item-confirm'",
        )
        .fetch_one(database.pool())
        .await
        .expect("candidate id");

        let moved = database
            .confirm_person_match_candidate(
                &candidate_id,
                &target.id,
                r#"{"method":"manual-confirm"}"#,
            )
            .await
            .expect("confirm candidate");
        assert_eq!(moved.previous_person_id.as_deref(), Some(old.id.as_str()));
        assert_eq!(
            database
                .find_canonical_person_by_identity("douban", "1313123")
                .await
                .expect("identity lookup")
                .expect("moved identity")
                .id,
            target.id
        );
        let lux_id: String = sqlx::query_scalar(
            "SELECT lux_person_id FROM person_credits
             WHERE item_id = 'item-confirm'",
        )
        .fetch_one(database.pool())
        .await
        .expect("credit lux id");
        assert_eq!(lux_id, target.id);
        let status: String = sqlx::query_scalar(
            "SELECT status FROM person_match_candidates
             WHERE id = ?",
        )
        .bind(&candidate_id)
        .fetch_one(database.pool())
        .await
        .expect("candidate status");
        assert_eq!(status, "CONFIRMED");

        database
            .undo_person_match_candidate(&candidate_id, r#"{"reason":"test-undo"}"#)
            .await
            .expect("undo candidate");
        assert_eq!(
            database
                .find_canonical_person_by_identity("douban", "1313123")
                .await
                .expect("identity lookup after undo")
                .expect("restored identity")
                .id,
            old.id
        );
        let restored_lux_id: String = sqlx::query_scalar(
            "SELECT lux_person_id FROM person_credits
             WHERE item_id = 'item-confirm'",
        )
        .fetch_one(database.pool())
        .await
        .expect("restored credit lux id");
        assert_eq!(restored_lux_id, old.id);
        let undone_status: String =
            sqlx::query_scalar("SELECT status FROM person_match_candidates WHERE id = ?")
                .bind(candidate_id)
                .fetch_one(database.pool())
                .await
                .expect("undone candidate status");
        assert_eq!(undone_status, "REJECTED");
    }

    #[tokio::test]
    async fn splitting_person_identity_allocates_a_new_lux_person_and_repoints_credits() {
        let temp_dir = tempfile::tempdir().expect("temporary directory");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("test address"),
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await.expect("database");
        let library = LibraryService::new(database.clone())
            .create_library("Movies", LibraryKind::Movie, false)
            .await
            .expect("library");
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('item-split', ?, 'MOVIE', 'Movie', 'movie', 'LOCAL_CONFIRMED')",
        )
        .bind(library.id.to_string())
        .execute(database.pool())
        .await
        .expect("media item");
        let old = database
            .resolve_or_create_canonical_person(
                "人物甲",
                "douban",
                "1313123",
                "PROVIDER_ID",
                Some(1.0),
                r#"{"source":"douban"}"#,
            )
            .await
            .expect("person");
        database
            .replace_person_credits(
                "item-split",
                &[NewPersonCredit {
                    person_id: "1313123".to_owned(),
                    lux_person_id: Some(old.id.clone()),
                    person_type: "Actor".to_owned(),
                    person_name: "人物甲".to_owned(),
                    provider: "douban".to_owned(),
                    role: "角色".to_owned(),
                    sort_order: 0,
                    biography: None,
                    birthday: None,
                    deathday: None,
                    known_for_department: None,
                    place_of_birth: None,
                    provider_ids: BTreeMap::new(),
                    genres: Vec::new(),
                    tags: Vec::new(),
                    production_locations: Vec::new(),
                    premiere_date: None,
                    production_year: None,
                    taglines: Vec::new(),
                }],
            )
            .await
            .expect("credit");
        let split = database
            .split_canonical_person_identity(
                &old.id,
                "douban",
                "1313123",
                "人物乙",
                r#"{"method":"undo-merge"}"#,
            )
            .await
            .expect("split");
        assert_ne!(split.id, old.id);
        assert_eq!(split.id, "lux-000002");
        assert_eq!(
            database
                .find_canonical_person_by_identity("douban", "1313123")
                .await
                .expect("identity")
                .expect("new owner")
                .id,
            split.id
        );
        let lux_id: String = sqlx::query_scalar(
            "SELECT lux_person_id FROM person_credits WHERE item_id = 'item-split'",
        )
        .fetch_one(database.pool())
        .await
        .expect("credit owner");
        assert_eq!(lux_id, split.id);
    }

    #[tokio::test]
    async fn catalog_tie_breakers_use_displayed_title_when_sort_key_is_stale() {
        let temp_dir = tempfile::tempdir().expect("temporary directory");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("test address"),
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await.expect("database");
        let libraries = LibraryService::new(database.clone());
        let library = libraries
            .create_library("Movies", LibraryKind::Movie, false)
            .await
            .expect("library");
        let library_id = library.id.to_string();
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, premiere_date,
                rating, identification_status, added_at, has_available_source
             ) VALUES
                ('item-alpha', ?, 'MOVIE', 'Alpha', 'zzz', '2020-01-01', 8.0, 'LOCAL_CONFIRMED', 100, 1),
                ('item-beta', ?, 'MOVIE', 'Beta', 'aaa', '2020-01-01', 8.0, 'LOCAL_CONFIRMED', 100, 1)",
        )
        .bind(&library_id)
        .bind(&library_id)
        .execute(database.pool())
        .await
        .expect("media items");

        let library_ids = vec![library_id];
        let item_types = vec!["MOVIE".to_owned()];
        let empty = Vec::new();
        let empty_years = Vec::<i64>::new();
        for (sort_by, descending) in [
            (CatalogSort::DateCreated, false),
            (CatalogSort::DateCreated, true),
            (CatalogSort::PremiereDate, false),
            (CatalogSort::PremiereDate, true),
            (CatalogSort::Rating, false),
            (CatalogSort::Rating, true),
        ] {
            let filter = CatalogFilterQuery {
                library_ids: &library_ids,
                user_id: "test-user",
                item_types: &item_types,
                excluded_item_types: &empty,
                item_ids: None,
                media_source_ids: None,
                years: &empty_years,
                is_played: None,
                is_favorite: None,
                metadata_pending: false,
                sort_by,
                descending,
                offset: 0,
                limit: 10,
            };
            let (rows, total) = database
                .list_filtered_catalog_rows(&filter)
                .await
                .expect("catalog rows");
            let titles = rows.into_iter().map(|row| row.title).collect::<Vec<_>>();
            let expected = vec!["Alpha", "Beta"];
            assert_eq!(total, 2);
            assert_eq!(titles, expected, "descending={descending}");
        }
    }

    #[tokio::test]
    async fn media_source_library_page_respects_limit_and_offset() {
        let temp_dir = tempfile::tempdir().expect("temporary directory");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("test address"),
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await.expect("database");
        let libraries = LibraryService::new(database.clone());
        let library = libraries
            .create_library("Movies", LibraryKind::Movie, false)
            .await
            .expect("library");
        let root_path = temp_dir.path().join("media");
        tokio::fs::create_dir_all(&root_path)
            .await
            .expect("media root");
        tokio::fs::write(root_path.join("First.Movie.2024.mkv"), b"first")
            .await
            .expect("first movie");
        tokio::fs::write(root_path.join("Second.Movie.2024.mkv"), b"second")
            .await
            .expect("second movie");
        tokio::fs::write(
            root_path.join("First.Remote.2024.strm"),
            b"https://media.example.invalid/first.mkv",
        )
        .await
        .expect("first STRM");
        tokio::fs::write(
            root_path.join("Second.Remote.2024.strm"),
            b"https://media.example.invalid/second.mkv",
        )
        .await
        .expect("second STRM");
        libraries
            .add_root(library.id, root_path.to_str().expect("utf-8 media root"))
            .await
            .expect("library root");
        LibraryScanner::new(database.clone())
            .scan_movie_library(library.id)
            .await
            .expect("scan");

        let first_page = database
            .list_media_sources_for_library_page(&library.id.to_string(), 1, 0)
            .await
            .expect("first page");
        let second_page = database
            .list_media_sources_for_library_page(&library.id.to_string(), 1, 1)
            .await
            .expect("second page");

        assert_eq!(first_page.len(), 1);
        assert_eq!(second_page.len(), 1);
        assert_ne!(first_page[0].source_id, second_page[0].source_id);
        let existing_entries = database
            .list_filesystem_entries_for_paths(
                &database
                    .list_library_roots(&library.id.to_string())
                    .await
                    .expect("roots")
                    .into_iter()
                    .next()
                    .expect("root")
                    .id,
                &["First.Movie.2024.mkv".to_owned()],
            )
            .await
            .expect("existing entries");
        assert_eq!(existing_entries.len(), 1);
        assert_eq!(
            database
                .list_local_thumbnail_sources_for_library_page(&library.id.to_string(), 1, 0,)
                .await
                .expect("thumbnail page")
                .len(),
            1
        );
        assert_eq!(
            database
                .list_movie_metadata_sources_page(&library.id.to_string(), 1, 1)
                .await
                .expect("metadata page")
                .len(),
            1
        );
        let first_strm_page = database
            .list_strm_media_sources_for_library_page(&library.id.to_string(), None, 1)
            .await
            .expect("first STRM page");
        let second_strm_page = database
            .list_strm_media_sources_for_library_page(
                &library.id.to_string(),
                first_strm_page
                    .first()
                    .map(|source| source.source_id.as_str()),
                1,
            )
            .await
            .expect("second STRM page");
        let final_strm_page = database
            .list_strm_media_sources_for_library_page(
                &library.id.to_string(),
                second_strm_page
                    .first()
                    .map(|source| source.source_id.as_str()),
                1,
            )
            .await
            .expect("final STRM page");
        assert_eq!(first_strm_page.len(), 1);
        assert_eq!(second_strm_page.len(), 1);
        assert_ne!(first_strm_page[0].source_id, second_strm_page[0].source_id);
        assert!(final_strm_page.is_empty());
        database.close().await;
    }

    #[tokio::test]
    async fn movie_batch_insert_uses_one_item_for_multiple_sources() {
        let temp_dir = tempfile::tempdir().expect("temporary directory");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("test address"),
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await.expect("database");
        let libraries = LibraryService::new(database.clone());
        let library = libraries
            .create_library("Movies", LibraryKind::Movie, false)
            .await
            .expect("library");
        let root_path = temp_dir.path().join("media");
        tokio::fs::create_dir_all(&root_path)
            .await
            .expect("media root");
        libraries
            .add_root(library.id, root_path.to_str().expect("utf-8 media root"))
            .await
            .expect("library root");
        let root = database
            .list_library_roots(&library.id.to_string())
            .await
            .expect("roots")
            .into_iter()
            .next()
            .expect("root");
        let files = vec![
            NewMovieFile {
                filesystem_entry_id: "entry-1".to_owned(),
                source_id: "source-1".to_owned(),
                relative_path: "Movie/Movie.2024.mkv".to_owned(),
                size: 1,
                modified_at: 1,
                fingerprint: vec![1],
                title: "Movie".to_owned(),
                sort_title: "movie".to_owned(),
                original_title: "Movie".to_owned(),
                production_year: Some(2024),
                provider_ids_json: None,
                source_kind: "LOCAL_FILE".to_owned(),
                strm_target_kind: None,
                edition_name: None,
                quality_label: None,
                container: "mkv".to_owned(),
                external_url: None,
            },
            NewMovieFile {
                filesystem_entry_id: "entry-2".to_owned(),
                source_id: "source-2".to_owned(),
                relative_path: "Movie/Movie.2024.Directors.Cut.mkv".to_owned(),
                size: 2,
                modified_at: 2,
                fingerprint: vec![2],
                title: "Movie".to_owned(),
                sort_title: "movie".to_owned(),
                original_title: "Movie".to_owned(),
                production_year: Some(2024),
                provider_ids_json: Some(r#"{"tmdb":"1"}"#.to_owned()),
                source_kind: "LOCAL_FILE".to_owned(),
                strm_target_kind: None,
                edition_name: Some("Director's Cut".to_owned()),
                quality_label: None,
                container: "mkv".to_owned(),
                external_url: None,
            },
        ];
        let created_items = database
            .insert_movie_files_batch(&library.id.to_string(), &root.id, "generation", &files)
            .await
            .expect("batch insert");

        assert_eq!(created_items, 1);
        let item_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM media_items WHERE item_type <> 'FOLDER'")
                .fetch_one(database.pool())
                .await
                .expect("item count");
        let source_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM media_sources")
            .fetch_one(database.pool())
            .await
            .expect("source count");
        assert_eq!(item_count, 1);
        assert_eq!(source_count, 2);
        let folder_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM media_items WHERE item_type = 'FOLDER'")
                .fetch_one(database.pool())
                .await
                .expect("folder count");
        assert_eq!(folder_count, 1);

        let item_id: String =
            sqlx::query_scalar("SELECT id FROM media_items WHERE item_type <> 'FOLDER'")
                .fetch_one(database.pool())
                .await
                .expect("item id");
        let rows = database
            .list_catalog_rows_by_ids(std::slice::from_ref(&item_id))
            .await
            .expect("catalog rows");
        assert_eq!(rows.iter().filter(|row| row.item_id == item_id).count(), 2);
        let details = database
            .list_catalog_details_by_ids(std::slice::from_ref(&item_id))
            .await
            .expect("catalog details");
        assert!(details.contains_key(&item_id));
        let provider_ids: Option<String> =
            sqlx::query_scalar("SELECT provider_ids_json FROM media_items WHERE id = ?")
                .bind(&item_id)
                .fetch_one(database.pool())
                .await
                .expect("provider ids");
        assert_eq!(provider_ids.as_deref(), Some(r#"{"tmdb":"1"}"#));
    }

    #[tokio::test]
    async fn write_probe_reports_a_query_only_sqlite_connection() {
        let pool = AnyPoolOptions::new()
            .max_connections(1)
            .connect_with(
                AnyConnectOptions::from_str("sqlite://?mode=memory")
                    .expect("in-memory SQLite options"),
            )
            .await
            .expect("in-memory SQLite connection");
        sqlx::query("CREATE TABLE lux_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .execute(&pool)
            .await
            .expect("create probe table");
        sqlx::query("PRAGMA query_only = ON")
            .execute(&pool)
            .await
            .expect("enable query-only mode");

        let database = Database {
            pool,
            path: PathBuf::from("query-only-test.db"),
            server_id: "test".to_owned(),
            backend: DatabaseBackend::Sqlite,
        };
        assert!(database.probe_write().await.is_err());
        database.close().await;
    }

    #[tokio::test]
    async fn metadata_jobs_process_series_before_seasons_and_episodes() {
        let pool = AnyPoolOptions::new()
            .max_connections(1)
            .connect_with(
                AnyConnectOptions::from_str("sqlite://?mode=memory")
                    .expect("in-memory SQLite options"),
            )
            .await
            .expect("in-memory SQLite connection");
        sqlx::query("CREATE TABLE media_items (id TEXT PRIMARY KEY, item_type TEXT NOT NULL)")
            .execute(&pool)
            .await
            .expect("create media items table");
        sqlx::query(
            "CREATE TABLE metadata_reidentify_job_items (
                job_id TEXT NOT NULL,
                item_id TEXT NOT NULL,
                status TEXT NOT NULL,
                PRIMARY KEY (job_id, item_id)
            )",
        )
        .execute(&pool)
        .await
        .expect("create metadata job items table");
        for (item_id, item_type) in [
            ("episode", "EPISODE"),
            ("season", "SEASON"),
            ("series", "SERIES"),
        ] {
            sqlx::query("INSERT INTO media_items (id, item_type) VALUES (?, ?)")
                .bind(item_id)
                .bind(item_type)
                .execute(&pool)
                .await
                .expect("insert media item");
            sqlx::query(
                "INSERT INTO metadata_reidentify_job_items (job_id, item_id, status)
                 VALUES ('job', ?, 'PENDING')",
            )
            .bind(item_id)
            .execute(&pool)
            .await
            .expect("insert metadata job item");
        }
        let database = Database {
            pool,
            path: PathBuf::from("metadata-order-test.db"),
            server_id: "test".to_owned(),
            backend: DatabaseBackend::Sqlite,
        };

        assert_eq!(
            database.next_metadata_reidentify_item("job").await.unwrap(),
            Some("series".to_owned())
        );
        sqlx::query(
            "UPDATE metadata_reidentify_job_items
             SET status = 'COMPLETED'
             WHERE job_id = 'job' AND item_id = 'series'",
        )
        .execute(&database.pool)
        .await
        .expect("complete series item");
        assert_eq!(
            database.next_metadata_reidentify_item("job").await.unwrap(),
            Some("season".to_owned())
        );
        database.close().await;
    }

    #[tokio::test]
    async fn metadata_jobs_reconcile_items_left_running_by_workers() {
        sqlx::any::install_default_drivers();
        let pool = AnyPoolOptions::new()
            .max_connections(1)
            .connect_with(
                AnyConnectOptions::from_str("sqlite://?mode=memory")
                    .expect("in-memory SQLite options"),
            )
            .await
            .expect("in-memory SQLite connection");
        sqlx::query(
            "CREATE TABLE metadata_reidentify_jobs (
                id TEXT PRIMARY KEY,
                processed_count INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .expect("create metadata jobs table");
        sqlx::query(
            "CREATE TABLE metadata_reidentify_job_items (
                job_id TEXT NOT NULL,
                item_id TEXT NOT NULL,
                status TEXT NOT NULL,
                candidate_count INTEGER NOT NULL,
                error TEXT,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (job_id, item_id)
            )",
        )
        .execute(&pool)
        .await
        .expect("create metadata job items table");
        sqlx::query(
            "INSERT INTO metadata_reidentify_jobs (id, processed_count, updated_at)
             VALUES ('job', 0, unixepoch())",
        )
        .execute(&pool)
        .await
        .expect("insert metadata job");
        for (item_id, status) in [
            ("running-1", "RUNNING"),
            ("running-2", "RUNNING"),
            ("done", "COMPLETED"),
        ] {
            sqlx::query(
                "INSERT INTO metadata_reidentify_job_items (
                    job_id, item_id, status, candidate_count, error, updated_at
                 ) VALUES ('job', ?, ?, 0, NULL, unixepoch())",
            )
            .bind(item_id)
            .bind(status)
            .execute(&pool)
            .await
            .expect("insert metadata job item");
        }
        let database = Database {
            pool,
            path: PathBuf::from("metadata-reconcile-test.db"),
            server_id: "test".to_owned(),
            backend: DatabaseBackend::Sqlite,
        };

        let reconciled = database
            .fail_running_metadata_reidentify_items("job", "WORKER_FAILED")
            .await
            .expect("reconcile running items");

        assert_eq!(reconciled, 2);
        let failed_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM metadata_reidentify_job_items
             WHERE job_id = 'job' AND status = 'FAILED' AND error = 'WORKER_FAILED'",
        )
        .fetch_one(database.pool())
        .await
        .expect("failed item count");
        assert_eq!(failed_count, 2);
        let processed_count: i64 = sqlx::query_scalar(
            "SELECT processed_count FROM metadata_reidentify_jobs WHERE id = 'job'",
        )
        .fetch_one(database.pool())
        .await
        .expect("processed count");
        assert_eq!(processed_count, 2);
        database.close().await;
    }

    #[test]
    fn provider_identity_uses_the_selected_scraper_without_falling_back_to_another_id() {
        let providers = Some(
            serde_json::json!({
                "Imdb": "tt123",
                "Tvdb": "456"
            })
            .to_string(),
        );

        assert_eq!(
            first_provider_id(providers.clone(), None, Some("org.example.tvdb")),
            Some(("Tvdb".to_owned(), "456".to_owned()))
        );
        assert_eq!(first_provider_id(providers, None, Some("tmdb")), None);
    }

    #[test]
    fn postgres_placeholder_adapter_preserves_quoted_question_marks() {
        let sql = "SELECT ?, '?' AS literal, \"?\" AS identifier, ?";
        assert_eq!(
            adapt_sql_for_backend(DatabaseBackend::Postgres, sql),
            "SELECT $1, '?' AS literal, \"?\" AS identifier, $2"
        );
        assert_eq!(adapt_sql_for_backend(DatabaseBackend::Sqlite, sql), sql);
    }

    #[tokio::test]
    async fn chapter_detection_job_creation_is_atomic_per_library() {
        let temp_dir = tempfile::tempdir().expect("temporary directory");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("test address"),
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await.expect("database");
        let library = LibraryService::new(database.clone())
            .create_library("Shows", LibraryKind::Series, false)
            .await
            .expect("library");
        let library_id = library.id.to_string();
        fn new_job<'a>(id: &'a str, library_id: &'a str) -> NewChapterDetectionJob<'a> {
            NewChapterDetectionJob {
                id,
                library_id,
                plugin_id: "org.lux.intro-outro-detector",
                concurrency: 1,
                intro_window_seconds: 180,
                credits_window_seconds: 180,
                match_threshold: 0.8,
                total_count: 0,
            }
        }

        assert!(
            database
                .create_chapter_detection_job(new_job("chapter-job-1", &library_id))
                .await
                .expect("first job should be created")
        );
        assert!(
            !database
                .create_chapter_detection_job(new_job("chapter-job-2", &library_id))
                .await
                .expect("active duplicate should be rejected")
        );
    }

    #[tokio::test]
    async fn person_index_rebuild_tasks_are_token_guarded_and_requeueable() {
        let temp_dir = tempfile::tempdir().expect("temporary directory");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("test address"),
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await.expect("database");
        let library = LibraryService::new(database.clone())
            .create_library("People", LibraryKind::Movie, false)
            .await
            .expect("library");
        let library_id = library.id.to_string();

        let jobs = database
            .sync_person_index_rebuild_jobs(1)
            .await
            .expect("sync jobs");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].status, "QUEUED");
        assert!(
            database
                .claim_person_index_rebuild_job(&library_id, "run-a")
                .await
                .expect("claim first run")
        );
        sqlx::query(
            "UPDATE person_index_rebuild_jobs
             SET updated_at = unixepoch() - 61
             WHERE library_id = ?",
        )
        .bind(&library_id)
        .execute(database.pool())
        .await
        .expect("mark interrupted run stale");
        let recovered_jobs = database
            .sync_person_index_rebuild_jobs(1)
            .await
            .expect("recover stale run");
        assert_eq!(recovered_jobs[0].status, "QUEUED");
        let recovered_token: Option<String> = sqlx::query_scalar(
            "SELECT run_token FROM person_index_rebuild_jobs WHERE library_id = ?",
        )
        .bind(&library_id)
        .fetch_one(database.pool())
        .await
        .expect("read recovered token");
        assert_eq!(recovered_token, None);
        assert!(
            database
                .claim_person_index_rebuild_job(&library_id, "run-b")
                .await
                .expect("claim recovered run")
        );
        assert!(
            database
                .request_person_index_rebuild_job_cancel(&library_id)
                .await
                .expect("request cancellation")
        );
        assert!(
            database
                .request_person_index_rebuild_job(&library_id, 1)
                .await
                .expect("requeue job")
        );
        assert!(
            !database
                .finish_person_index_rebuild_job(&library_id, "run-a", "COMPLETED", None)
                .await
                .expect("ignore stale completion")
        );
        assert!(
            !database
                .finish_person_index_rebuild_job(&library_id, "run-b", "COMPLETED", None)
                .await
                .expect("ignore cancelled run completion")
        );
        assert!(
            database
                .claim_person_index_rebuild_job(&library_id, "run-c")
                .await
                .expect("claim requeued run")
        );
        assert!(
            database
                .update_person_index_rebuild_progress(&library_id, "run-a", "item-a", 1, 2)
                .await
                .expect("ignore stale progress")
                .is_none()
        );
        assert!(
            database
                .update_person_index_rebuild_progress(&library_id, "run-c", "item-c", 2, 2)
                .await
                .expect("update progress")
                .is_some()
        );
        assert!(
            database
                .finish_person_index_rebuild_job(&library_id, "run-c", "COMPLETED", None)
                .await
                .expect("finish current run")
        );
        let jobs = database
            .list_person_index_rebuild_jobs(0, 20)
            .await
            .expect("list jobs");
        assert_eq!(jobs[0].status, "COMPLETED");
        assert_eq!(jobs[0].processed_count, 2);
    }

    #[tokio::test]
    async fn person_index_keyset_pages_and_fingerprints_are_conservative() {
        let temp_dir = tempfile::tempdir().expect("temporary directory");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("test address"),
            config_dir: temp_dir.path().join("config"),
        };
        let database = Database::connect(&config).await.expect("database");
        let library = LibraryService::new(database.clone())
            .create_library("People", LibraryKind::Movie, false)
            .await
            .expect("library");
        let library_id = library.id.to_string();
        for item_id in ["item-a", "item-b", "item-c"] {
            sqlx::query(
                "INSERT INTO media_items (
                    id, library_id, item_type, title, sort_title, identification_status
                 ) VALUES (?, ?, 'MOVIE', ?, ?, 'LOCAL_CONFIRMED')",
            )
            .bind(item_id)
            .bind(&library_id)
            .bind(item_id)
            .bind(item_id)
            .execute(database.pool())
            .await
            .expect("media item");
        }
        let first_page = database
            .list_person_index_item_ids(&library_id, None, 2)
            .await
            .expect("first keyset page");
        assert_eq!(first_page, ["item-a", "item-b"]);
        let second_page = database
            .list_person_index_item_ids(&library_id, first_page.last().map(String::as_str), 2)
            .await
            .expect("second keyset page");
        assert_eq!(second_page, ["item-c"]);
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('item-ab', ?, 'MOVIE', 'item-ab', 'item-ab', 'LOCAL_CONFIRMED')",
        )
        .bind(&library_id)
        .execute(database.pool())
        .await
        .expect("insert item before the cursor");
        let second_page_after_insert = database
            .list_person_index_item_ids(&library_id, first_page.last().map(String::as_str), 2)
            .await
            .expect("second keyset page after insert");
        assert_eq!(second_page_after_insert, ["item-c"]);
        sqlx::query("DELETE FROM media_items WHERE id = 'item-c'")
            .execute(database.pool())
            .await
            .expect("delete item after the cursor");
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status
             ) VALUES ('item-z', ?, 'MOVIE', 'item-z', 'item-z', 'LOCAL_CONFIRMED')",
        )
        .bind(&library_id)
        .execute(database.pool())
        .await
        .expect("insert item after the cursor");
        let second_page_after_delete = database
            .list_person_index_item_ids(&library_id, first_page.last().map(String::as_str), 2)
            .await
            .expect("second keyset page after delete");
        assert_eq!(second_page_after_delete, ["item-z"]);

        database
            .replace_person_credits_with_fingerprint("item-a", &[], Some("fingerprint-a"))
            .await
            .expect("store fingerprint");
        assert!(
            database
                .person_index_item_state_is_current("item-a", Some("fingerprint-a"))
                .await
                .expect("same fingerprint")
        );
        assert!(
            !database
                .person_index_item_state_is_current("item-a", None)
                .await
                .expect("missing fingerprint must not be current")
        );
        assert!(
            !database
                .person_index_item_state_is_current("item-a", Some("fingerprint-b"))
                .await
                .expect("changed fingerprint")
        );
        sqlx::query(
            "UPDATE person_index_item_state
             SET relation_schema_version = 3
             WHERE item_id = 'item-a'",
        )
        .execute(database.pool())
        .await
        .expect("change relation schema version");
        assert!(
            !database
                .person_index_item_state_is_current("item-a", Some("fingerprint-a"))
                .await
                .expect("changed relation schema version")
        );
        database
            .clear_person_credits("item-a")
            .await
            .expect("clear person credits");
        assert!(
            !database
                .person_index_item_state_is_current("item-a", Some("fingerprint-a"))
                .await
                .expect("cleared relation must be rebuilt")
        );
    }

    #[tokio::test]
    #[ignore = "requires a local PostgreSQL instance"]
    async fn postgres_metadata_candidate_selection_accepts_integer_boolean_flags() {
        let temp_dir = tempfile::tempdir().expect("temporary directory");
        let config = Config {
            http_addr: "127.0.0.1:8097".parse().expect("test address"),
            config_dir: temp_dir.path().join("config"),
        };
        let connection = DatabaseConfiguration::Postgres(PostgresConnection {
            host: std::env::var("POSTGRES_TEST_HOST").unwrap_or_else(|_| "127.0.0.1".to_owned()),
            port: std::env::var("POSTGRES_TEST_PORT")
                .unwrap_or_else(|_| "55432".to_owned())
                .parse()
                .expect("test port"),
            database: std::env::var("POSTGRES_TEST_DATABASE").unwrap_or_else(|_| "lux".to_owned()),
            username: std::env::var("POSTGRES_TEST_USER").unwrap_or_else(|_| "lux".to_owned()),
            password: std::env::var("POSTGRES_TEST_PASSWORD")
                .unwrap_or_else(|_| "lux-test-password".to_owned()),
            ssl_mode: "disable".to_owned(),
        });
        let database = Database::connect_with_configuration(&config, &connection)
            .await
            .expect("PostgreSQL database");
        let library = LibraryService::new(database.clone())
            .create_library("Metadata selection", LibraryKind::Movie, false)
            .await
            .expect("library");
        let item_id = Uuid::now_v7().to_string();
        let candidate_id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, title, sort_title, identification_status,
                has_available_source
             ) VALUES (?, ?, 'MOVIE', 'Metadata selection', 'metadata selection',
                       'LOCAL_CONFIRMED', 1)",
        )
        .bind(&item_id)
        .bind(library.id.to_string())
        .execute(database.pool())
        .await
        .expect("media item");
        sqlx::query(
            "INSERT INTO metadata_candidates (
                id, item_id, provider, provider_id, candidate_json, score, status
             ) VALUES (?, ?, 'TMDB', '603', '{}', 100, 'PENDING')",
        )
        .bind(&candidate_id)
        .bind(&item_id)
        .execute(database.pool())
        .await
        .expect("metadata candidate");

        let update = |keep_pending| SelectedMetadataUpdate {
            item_id: &item_id,
            candidate_id: &candidate_id,
            title: "Metadata selection",
            original_title: None,
            overview: None,
            production_year: None,
            premiere_date: None,
            last_air_date: None,
            status: None,
            original_language: None,
            rating: None,
            rating_source: None,
            provider_ids_json: "{}",
            metadata_fingerprint: &[],
            provenance_json: "{}",
            locked_fields_json: "[]",
            poster_fallback_required: false,
            keep_pending,
        };

        assert!(
            database
                .select_metadata_candidate(update(true))
                .await
                .expect("keep-pending selection")
        );
        let identification_status: String =
            sqlx::query_scalar("SELECT identification_status FROM media_items WHERE id = ?")
                .bind(&item_id)
                .fetch_one(database.pool())
                .await
                .expect("identification status");
        assert_eq!(identification_status, "PENDING");
        let candidate_status: String =
            sqlx::query_scalar("SELECT status FROM metadata_candidates WHERE id = ?")
                .bind(&candidate_id)
                .fetch_one(database.pool())
                .await
                .expect("candidate status");
        assert_eq!(candidate_status, "PENDING");

        assert!(
            database
                .select_metadata_candidate(update(false))
                .await
                .expect("confirmed selection")
        );
        let identification_status: String =
            sqlx::query_scalar("SELECT identification_status FROM media_items WHERE id = ?")
                .bind(&item_id)
                .fetch_one(database.pool())
                .await
                .expect("confirmed identification status");
        assert_eq!(identification_status, "ONLINE_CONFIRMED");
        let candidate_status: String =
            sqlx::query_scalar("SELECT status FROM metadata_candidates WHERE id = ?")
                .bind(&candidate_id)
                .fetch_one(database.pool())
                .await
                .expect("confirmed candidate status");
        assert_eq!(candidate_status, "SELECTED");
    }
}
