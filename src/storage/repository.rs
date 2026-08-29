use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

use sqlx::{
    Acquire, Any, AnyPool, Executor, Row,
    any::{AnyConnectOptions, AnyPoolOptions},
    migrate::{MigrateError, Migrator},
};
use tokio::{fs, sync::Mutex as AsyncMutex};
use uuid::Uuid;

#[path = "catalog.rs"]
mod catalog;
#[path = "emby_migration.rs"]
mod emby_migration;
#[path = "jobs.rs"]
mod jobs;
#[path = "library.rs"]
mod library;
#[path = "media.rs"]
mod media;
#[path = "metadata.rs"]
mod metadata;
#[path = "migration.rs"]
mod migration;
#[path = "notifications.rs"]
mod notifications;
#[path = "people.rs"]
mod people;
#[path = "sessions.rs"]
mod sessions;
#[path = "users.rs"]
mod users;

pub(crate) use emby_migration::{
    EmbyMigrationHandledItemBatch, EmbyMigrationImportRecordBatch, EmbyMigrationItemMatchBatch,
    EmbyMigrationItemPageBatch, EmbyMigrationJobProgress, EmbyMigrationPersonFavoriteBatch,
    EmbyMigrationPersonFavoriteStateBatch, EmbyMigrationUserItemStateBatch,
    EmbyMigrationUserItemStateFields, MigrationMediaIdentityLookup, MigrationPersonIdentityLookup,
    NewEmbyMigrationJob, StoredEmbyMigrationImportRecord, StoredEmbyMigrationItemMatch,
    StoredEmbyMigrationJob, StoredEmbyMigrationPersonFavorite, StoredEmbyMigrationSource,
    StoredEmbyMigrationUserBinding, StoredEmbyMigrationUserLink, StoredMigrationMediaIdentity,
    StoredMigrationPersonIdentity, StoredPlaybackHistoryEvent,
};

use crate::config::{Config, DatabaseBackend, DatabaseConfiguration, DatabaseConfigurationError};

static SQLITE_MIGRATOR: Migrator = sqlx::migrate!();
static POSTGRES_MIGRATOR: Migrator = sqlx::migrate!("./migrations-postgres");

pub(crate) const PLAYBACK_SESSION_STALE_AFTER_SECONDS: i64 = 90;
pub(crate) const DEFAULT_PLAYED_PERCENT: i64 = 95;
const MAX_BACKGROUND_PAGE_SIZE: i64 = 500;
const BATCH_INSERT_CHUNK_SIZE: usize = 100;
const DATABASE_POOL_MAX_CONNECTIONS_ENV: &str = "LUX_DB_MAX_CONNECTIONS";
const SQLITE_DATABASE_POOL_MAX_CONNECTIONS: u32 = 8;
const POSTGRES_DATABASE_POOL_MAX_CONNECTIONS: u32 = 20;
const MIN_DATABASE_POOL_MAX_CONNECTIONS: u32 = 1;
const MAX_DATABASE_POOL_MAX_CONNECTIONS: u32 = 100;

fn resolve_database_pool_max_connections(
    backend: DatabaseBackend,
    configured: Option<&str>,
) -> Result<u32, DatabaseConfigurationError> {
    let default = match backend {
        DatabaseBackend::Sqlite => SQLITE_DATABASE_POOL_MAX_CONNECTIONS,
        DatabaseBackend::Postgres => POSTGRES_DATABASE_POOL_MAX_CONNECTIONS,
    };
    let Some(configured) = configured
        .map(str::trim)
        .filter(|configured| !configured.is_empty())
    else {
        return Ok(default);
    };
    let value = configured.parse::<u32>().map_err(|_| {
        DatabaseConfigurationError::Invalid(format!(
            "{DATABASE_POOL_MAX_CONNECTIONS_ENV} 必须是 {MIN_DATABASE_POOL_MAX_CONNECTIONS} 到 {MAX_DATABASE_POOL_MAX_CONNECTIONS} 之间的整数"
        ))
    })?;
    if !(MIN_DATABASE_POOL_MAX_CONNECTIONS..=MAX_DATABASE_POOL_MAX_CONNECTIONS).contains(&value) {
        return Err(DatabaseConfigurationError::Invalid(format!(
            "{DATABASE_POOL_MAX_CONNECTIONS_ENV} 必须是 {MIN_DATABASE_POOL_MAX_CONNECTIONS} 到 {MAX_DATABASE_POOL_MAX_CONNECTIONS} 之间的整数"
        )));
    }
    Ok(value)
}

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
    pool_max_connections: u32,
    path: PathBuf,
    server_id: String,
    backend: DatabaseBackend,
    person_credits_write_lock: Arc<AsyncMutex<()>>,
    metadata_write_lock: Arc<AsyncMutex<()>>,
    #[cfg(test)]
    query_count: Arc<AtomicUsize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DatabasePoolSnapshot {
    pub max_connections: u32,
    pub size: u32,
    pub idle: u32,
    pub in_use: u32,
    pub saturated: bool,
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
        let configured_max_connections = std::env::var(DATABASE_POOL_MAX_CONNECTIONS_ENV).ok();
        let pool_max_connections =
            resolve_database_pool_max_connections(backend, configured_max_connections.as_deref())
                .map_err(StorageError::Configuration)?;
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
            .max_connections(pool_max_connections)
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
            && let Err(error) = migration::validate_postgres_schema(&pool).await
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
            migration::remove_sqlite_title_year_unique(&pool, &path).await?;
        }
        let server_id = migration::ensure_server_id(&pool, backend)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: path.clone(),
                source,
            })?;

        Ok(Self {
            pool,
            pool_max_connections,
            path,
            server_id,
            backend,
            person_credits_write_lock: Arc::new(AsyncMutex::new(())),
            metadata_write_lock: Arc::new(AsyncMutex::new(())),
            #[cfg(test)]
            query_count: Arc::new(AtomicUsize::new(0)),
        })
    }

    pub(crate) async fn acquire_metadata_write_lock(
        &self,
    ) -> Option<tokio::sync::OwnedMutexGuard<()>> {
        if self.backend == DatabaseBackend::Sqlite {
            Some(self.metadata_write_lock.clone().lock_owned().await)
        } else {
            None
        }
    }

    async fn begin_metadata_write_transaction(
        &self,
    ) -> Result<sqlx::Transaction<'_, Any>, StorageError> {
        let transaction = if self.backend == DatabaseBackend::Sqlite {
            self.pool.begin_with("BEGIN IMMEDIATE").await
        } else {
            self.pool.begin().await
        };
        transaction.map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
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
        migration::validate_postgres_schema(&pool).await?;
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

    pub(crate) fn pool_snapshot(&self) -> DatabasePoolSnapshot {
        let size = self.pool.size();
        let idle = self.pool.num_idle().min(size as usize) as u32;
        let in_use = size.saturating_sub(idle);
        DatabasePoolSnapshot {
            max_connections: self.pool_max_connections,
            size,
            idle,
            in_use,
            saturated: size >= self.pool_max_connections && idle == 0,
        }
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

    #[cfg(test)]
    fn reset_query_count(&self) {
        self.query_count.store(0, AtomicOrdering::Relaxed);
    }

    #[cfg(test)]
    fn query_count(&self) -> usize {
        self.query_count.load(AtomicOrdering::Relaxed)
    }

    fn query(
        &self,
        sql: impl sqlx::SqlSafeStr,
    ) -> sqlx::query::Query<'static, sqlx::Any, sqlx::any::AnyArguments> {
        #[cfg(test)]
        self.query_count.fetch_add(1, AtomicOrdering::Relaxed);
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
        #[cfg(test)]
        self.query_count.fetch_add(1, AtomicOrdering::Relaxed);
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
        #[cfg(test)]
        self.query_count.fetch_add(1, AtomicOrdering::Relaxed);
        sqlx::query_scalar(sqlx::AssertSqlSafe(adapt_sql_for_backend(
            self.backend,
            sql,
        )))
    }
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
    pub(crate) scrapers: Vec<StoredLibraryScraper>,
    pub(crate) chapter_source_id: Option<String>,
    pub(crate) cover_image_path: Option<String>,
    pub(crate) cover_image_content_type: Option<String>,
    pub(crate) cover_image_size: Option<i64>,
    pub(crate) cover_image_tag: Option<String>,
    pub(crate) media_strategy_json: Option<String>,
}

#[derive(Debug)]
pub(crate) struct StoredLibraryIdentity {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) root_paths: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct StoredLibraryScraper {
    pub(crate) scraper_id: String,
    pub(crate) position: i64,
    pub(crate) role: String,
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

#[derive(Clone, Debug)]
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
    pub(crate) created_at: i64,
    pub(crate) started_at: Option<i64>,
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
    pub(crate) created_at: i64,
    pub(crate) started_at: Option<i64>,
    pub(crate) finished_at: Option<i64>,
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
        created_at: row.get("created_at"),
        started_at: row.get("started_at"),
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
        created_at: row.get("created_at"),
        started_at: row.get("started_at"),
        finished_at: row.get("finished_at"),
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
        created_at: row.get("created_at"),
        started_at: row.get("started_at"),
        finished_at: row.get("finished_at"),
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

#[derive(Clone, Debug)]
pub(crate) struct StoredFilesystemEntry {
    pub(crate) id: String,
    pub(crate) relative_path: String,
    pub(crate) fingerprint: Option<Vec<u8>>,
    pub(crate) item_id: Option<String>,
    pub(crate) parent_identity_key: Option<String>,
    pub(crate) item_type: Option<String>,
    pub(crate) item_identity_key: Option<String>,
    pub(crate) series_provider_ids_json: Option<String>,
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
        parent_identity_key: row.get("parent_identity_key"),
        item_type: row.get("item_type"),
        item_identity_key: row.get("item_identity_key"),
        series_provider_ids_json: row.get("series_provider_ids_json"),
    }
}

#[derive(Debug)]
pub(crate) struct StoredMediaItem {
    pub(crate) id: String,
}

#[derive(Clone, Debug)]
struct PrefetchedMovieItem {
    id: String,
    parent_id: Option<String>,
    provider_ids_json: Option<String>,
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
    pub(crate) metadata_scraper_id: Option<String>,
    pub(crate) identification_status: String,
    pub(crate) scraper_id: Option<String>,
    pub(crate) provenance_json: Option<String>,
    pub(crate) locked_fields_json: Option<String>,
    pub(crate) nfo_metadata_json: Option<String>,
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

#[derive(Debug)]
pub(crate) struct StoredMetadataCapabilityAttempt {
    pub(crate) provider: String,
    pub(crate) provider_id: String,
    pub(crate) capability: String,
    pub(crate) status: String,
    pub(crate) next_retry_at: Option<i64>,
}

pub(crate) struct MetadataCapabilityResult<'a> {
    pub(crate) capability: &'a str,
    pub(crate) has_data: bool,
}

#[derive(Debug)]
pub(crate) struct StoredMetadataImageAttempt {
    pub(crate) image_type: String,
    pub(crate) candidate_key: String,
    pub(crate) status: String,
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

pub(crate) struct NewWebPlaybackSession<'a> {
    pub(crate) id: &'a str,
    pub(crate) user_id: &'a str,
    pub(crate) item_id: &'a str,
    pub(crate) media_source_id: Option<&'a str>,
    pub(crate) play_session_id: &'a str,
    pub(crate) tier: i64,
    pub(crate) plan: &'a str,
    pub(crate) temp_dir: Option<&'a str>,
    pub(crate) is_admin: bool,
    pub(crate) expires_at: i64,
    pub(crate) now: i64,
}

pub(crate) struct NewWebPlaybackEvent<'a> {
    pub(crate) session_id: &'a str,
    pub(crate) user_id: &'a str,
    pub(crate) event_id: &'a str,
    pub(crate) sequence: i64,
    pub(crate) state: &'a str,
    pub(crate) position_ticks: i64,
    pub(crate) duration_ticks: Option<i64>,
    pub(crate) now: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WebPlaybackEventClaim {
    Accepted,
    Duplicate,
    Stale,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredWebPlaybackSession {
    pub(crate) id: String,
    pub(crate) user_id: String,
    pub(crate) item_id: String,
    pub(crate) media_source_id: Option<String>,
    pub(crate) play_session_id: String,
    pub(crate) tier: i64,
    pub(crate) plan: String,
    pub(crate) state: String,
    pub(crate) temp_dir: Option<String>,
    pub(crate) is_admin: bool,
    pub(crate) expires_at: i64,
    pub(crate) last_heartbeat_at: i64,
    pub(crate) last_sequence: i64,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

fn stored_web_playback_session(row: sqlx::any::AnyRow) -> StoredWebPlaybackSession {
    StoredWebPlaybackSession {
        id: row.get("id"),
        user_id: row.get("user_id"),
        item_id: row.get("item_id"),
        media_source_id: row.get("media_source_id"),
        play_session_id: row.get("play_session_id"),
        tier: row.get("tier"),
        plan: row.get("plan"),
        state: row.get("state"),
        temp_dir: row.get("temp_dir"),
        is_admin: row.get::<i64, _>("is_admin") != 0,
        expires_at: row.get("expires_at"),
        last_heartbeat_at: row.get("last_heartbeat_at"),
        last_sequence: row.get("last_sequence"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
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

#[allow(dead_code)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StoredSubtitleStream {
    pub(crate) media_source_id: String,
    pub(crate) item_id: String,
    pub(crate) source_kind: String,
    pub(crate) probe_status: String,
    pub(crate) root_path: String,
    pub(crate) relative_path: String,
    pub(crate) stream_index: i64,
    pub(crate) stream_type: String,
    pub(crate) codec: Option<String>,
    pub(crate) language: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) details_json: Option<String>,
    pub(crate) external_path: Option<String>,
    pub(crate) is_external: bool,
    pub(crate) is_default: bool,
    pub(crate) is_forced: bool,
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
    pub(crate) source_url: Option<&'a str>,
}

pub(crate) struct MetadataImageAttemptUpdate<'a> {
    pub(crate) item_id: &'a str,
    pub(crate) image_type: &'a str,
    pub(crate) candidate_key: &'a str,
    pub(crate) status: &'a str,
    pub(crate) next_retry_at: Option<i64>,
    pub(crate) error_code: Option<&'a str>,
    pub(crate) now: i64,
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
    let person_id = filter.person_id;
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
    if let Some(person_id) = person_id {
        where_clause.push_str(
            " AND EXISTS (
                 SELECT 1
                 FROM person_credits person_filter
                 JOIN media_items credit_item ON credit_item.id = person_filter.item_id
                 LEFT JOIN person_identities identity_filter
                   ON identity_filter.provider = person_filter.provider
                  AND identity_filter.provider_id = person_filter.person_id
                 WHERE person_filter.person_type = ?
                   AND (
                       person_filter.person_id = ?
                       OR person_filter.lux_person_id = ?
                       OR identity_filter.person_id = ?
                   )
                   AND credit_item.removed_at IS NULL
                   AND (
                       credit_item.has_available_source = 1
                       OR (
                           credit_item.item_type IN ('SERIES', 'SEASON', 'BOX_SET', 'FOLDER')
                           AND EXISTS (
                               SELECT 1
                               FROM media_items visible_credit_child
                               WHERE visible_credit_child.removed_at IS NULL
                                 AND visible_credit_child.has_available_source = 1
                                 AND (
                                     visible_credit_child.parent_id = credit_item.id
                                     OR visible_credit_child.series_id = credit_item.id
                                 )
                           )
                       )
                   )
                   AND (
                       person_filter.item_id = mi.id
                       OR credit_item.series_id = mi.id
                       OR EXISTS (
                           SELECT 1
                           FROM media_items credit_parent
                           WHERE credit_parent.id = credit_item.parent_id
                             AND (
                                 credit_parent.series_id = mi.id
                                 OR credit_parent.parent_id = mi.id
                             )
                       )
                   )
             )",
        );
        binds.push(CatalogBind::Text("Actor"));
        binds.push(CatalogBind::Text(person_id));
        binds.push(CatalogBind::Text(person_id));
        binds.push(CatalogBind::Text(person_id));
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
        if is_favorite {
            where_clause.push_str(
                " AND mi.id IN (
                    SELECT state_filter.item_id
                    FROM user_item_state state_filter
                    WHERE state_filter.user_id = ? AND state_filter.is_favorite = ?
                )",
            );
        } else {
            where_clause.push_str(
                " AND COALESCE(
                    (SELECT state_filter.is_favorite
                     FROM user_item_state state_filter
                     WHERE state_filter.user_id = ? AND state_filter.item_id = mi.id),
                    0
                ) = ?",
            );
        }
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

pub(crate) fn movie_parent_folder_identity(
    library_root_id: &str,
    relative_path: &str,
) -> Option<String> {
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
            "representative.person_name {direction}, representative.date_created DESC, representative.provider ASC, representative.person_id ASC"
        ),
        PersonSort::DateCreated => format!(
            "representative.date_created {direction}, representative.person_name ASC, representative.provider ASC, representative.person_id ASC"
        ),
    }
}

pub(crate) struct CatalogFilterQuery<'a> {
    pub(crate) library_ids: &'a [String],
    pub(crate) user_id: &'a str,
    pub(crate) item_types: &'a [String],
    pub(crate) excluded_item_types: &'a [String],
    pub(crate) item_ids: Option<&'a [String]>,
    pub(crate) person_id: Option<&'a str>,
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
    pub(crate) item_type: Option<String>,
    pub(crate) season_number: Option<i64>,
    pub(crate) episode_number: Option<i64>,
    pub(crate) title: Option<String>,
    pub(crate) original_title: Option<String>,
    pub(crate) series_title: Option<String>,
    pub(crate) series_original_title: Option<String>,
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
    pub(crate) created_at: i64,
    pub(crate) started_at: Option<i64>,
    pub(crate) finished_at: Option<i64>,
}

#[derive(Clone, Debug)]
pub(crate) struct StoredDanmakuMatchItem {
    pub(crate) id: String,
    pub(crate) media_source_id: String,
    pub(crate) root_path: Option<String>,
    pub(crate) relative_path: Option<String>,
    pub(crate) item_type: Option<String>,
    pub(crate) season_number: Option<i64>,
    pub(crate) episode_number: Option<i64>,
    pub(crate) title: Option<String>,
    pub(crate) original_title: Option<String>,
    pub(crate) series_title: Option<String>,
    pub(crate) series_original_title: Option<String>,
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
        created_at: row.get("created_at"),
        started_at: row.get("started_at"),
        finished_at: row.get("finished_at"),
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
    pub(crate) created_at: i64,
    pub(crate) started_at: Option<i64>,
    pub(crate) finished_at: Option<i64>,
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
    pub(crate) scrapers: &'a [crate::library::LibraryScraper],
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
    pub(crate) metadata_scraper_id: Option<&'a str>,
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
    pub(crate) scrapers: Option<&'a [crate::library::LibraryScraper]>,
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

pub(crate) struct NewEpisodeFile {
    pub(crate) filesystem_entry_id: String,
    pub(crate) source_id: String,
    pub(crate) relative_path: String,
    pub(crate) size: i64,
    pub(crate) modified_at: i64,
    pub(crate) inode: Option<i64>,
    pub(crate) fingerprint: Vec<u8>,
    pub(crate) series_identity: String,
    pub(crate) series_title: String,
    pub(crate) series_sort_title: String,
    pub(crate) series_production_year: Option<i64>,
    pub(crate) series_provider_ids_json: Option<String>,
    pub(crate) season_identity: String,
    pub(crate) season_number: i64,
    pub(crate) episode_identity: String,
    pub(crate) episode_title: String,
    pub(crate) episode_sort_title: String,
    pub(crate) episode_number: i64,
    pub(crate) episode_absolute_number: Option<i64>,
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
#[path = "repository_tests.rs"]
mod repository_tests;
