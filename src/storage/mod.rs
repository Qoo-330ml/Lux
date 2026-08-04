use std::{collections::HashMap, path::PathBuf, time::Duration};

use sqlx::{
    QueryBuilder, Row,
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

    pub(crate) async fn find_user_by_id(
        &self,
        user_id: &str,
    ) -> Result<Option<StoredUser>, StorageError> {
        sqlx::query(
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
        let Some(current) = sqlx::query(
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
        if current_can_manage && (!next_can_manage || next_disabled) {
            let remaining: i64 = sqlx::query_scalar(
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
        sqlx::query(
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
        .bind(update.is_disabled)
        .bind(update.is_admin)
        .bind(update.can_manage_server)
        .bind(update.can_remote_access)
        .bind(update.can_download)
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
        sqlx::query(
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
        sqlx::query(
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

    pub(crate) async fn find_item_scraper_id(
        &self,
        item_id: &str,
    ) -> Result<Option<String>, StorageError> {
        let value = sqlx::query_scalar::<_, String>(
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

    pub(crate) async fn list_media_item_ids_for_library(
        &self,
        library_id: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<String>, StorageError> {
        sqlx::query_scalar(
            "SELECT id FROM media_items
             WHERE library_id = ? AND removed_at IS NULL
             ORDER BY id LIMIT ? OFFSET ?",
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
        sqlx::query(
            "INSERT INTO libraries (
                id, name, kind, is_enabled, realtime_watch_enabled,
                incremental_schedule, reconciliation_schedule, metadata_schedule,
                scan_concurrency, probe_concurrency, scraper_id
            ) VALUES (?, ?, ?, 1, ?, ?, ?, ?, ?, ?, ?)",
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
        .bind(library.scraper_id)
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
                    scan_concurrency, probe_concurrency, last_scan_at, scraper_id,
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
                    incremental_schedule: row.get("incremental_schedule"),
                    reconciliation_schedule: row.get("reconciliation_schedule"),
                    metadata_schedule: row.get("metadata_schedule"),
                    scan_concurrency: row.get("scan_concurrency"),
                    probe_concurrency: row.get("probe_concurrency"),
                    last_scan_at: row.get("last_scan_at"),
                    scraper_id: row.get("scraper_id"),
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
        sqlx::query(
            "SELECT id, name, kind, is_enabled, realtime_watch_enabled,
                    incremental_schedule, reconciliation_schedule, metadata_schedule,
                    scan_concurrency, probe_concurrency, last_scan_at, scraper_id,
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
                incremental_schedule: row.get("incremental_schedule"),
                reconciliation_schedule: row.get("reconciliation_schedule"),
                metadata_schedule: row.get("metadata_schedule"),
                scan_concurrency: row.get("scan_concurrency"),
                probe_concurrency: row.get("probe_concurrency"),
                last_scan_at: row.get("last_scan_at"),
                scraper_id: row.get("scraper_id"),
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

    pub(crate) async fn delete_library(&self, id: &str) -> Result<bool, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        sqlx::query(
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
        let deleted = sqlx::query("DELETE FROM libraries WHERE id = ?")
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

        let exists: i64 = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM libraries WHERE id = ?)")
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
            sqlx::query(
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
            sqlx::query(
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
            sqlx::query(
                "UPDATE libraries
                 SET is_enabled = ?, updated_at = unixepoch()
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
        if let Some(value) = settings.realtime_watch_enabled {
            sqlx::query(
                "UPDATE libraries
                 SET realtime_watch_enabled = ?, updated_at = unixepoch()
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
        if let Some(value) = settings.incremental_schedule {
            sqlx::query(
                "UPDATE libraries
                 SET incremental_schedule = ?, updated_at = unixepoch()
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
        if let Some(value) = settings.reconciliation_schedule {
            sqlx::query(
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
            sqlx::query(
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
            sqlx::query(
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
        if let Some(value) = settings.media_strategy_json {
            sqlx::query(
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
            sqlx::query(
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
            sqlx::query(
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

        let current: (Option<String>, Option<String>, Option<String>, i64, i64) = sqlx::query_as(
            "SELECT incremental_schedule, reconciliation_schedule, metadata_schedule,
                    scan_concurrency, probe_concurrency
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
            current.3, current.4
        );
        let task_configs = [
            ("INCREMENTAL_SCAN", current.0.as_deref(), resources.as_str()),
            (
                "RECONCILIATION_SCAN",
                current.1.as_deref(),
                resources.as_str(),
            ),
            ("METADATA_PARSE", current.2.as_deref(), "{}"),
        ];
        for (task_type, schedule, resource_limit_json) in task_configs {
            sqlx::query(
                "INSERT INTO scheduled_task_configs (
                    owner_type, owner_id, task_type, cron_or_interval,
                    is_enabled, resource_limit_json, updated_at
                ) VALUES ('LIBRARY', ?, ?, ?, ?, ?, unixepoch())
                ON CONFLICT(owner_type, owner_id, task_type) DO UPDATE SET
                    cron_or_interval = excluded.cron_or_interval,
                    is_enabled = excluded.is_enabled,
                    resource_limit_json = excluded.resource_limit_json,
                    updated_at = unixepoch()",
            )
            .bind(library_id)
            .bind(task_type)
            .bind(schedule)
            .bind(schedule.is_some())
            .bind(resource_limit_json)
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

    pub(crate) async fn update_library_cover(
        &self,
        library_id: &str,
        path: &str,
        content_type: &str,
        size: i64,
        tag: &str,
    ) -> Result<bool, StorageError> {
        sqlx::query(
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

    pub(crate) async fn find_user_item_state(
        &self,
        user_id: &str,
        item_id: &str,
    ) -> Result<Option<StoredUserItemState>, StorageError> {
        sqlx::query(
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

    pub(crate) async fn is_plugin_installed(&self, plugin_id: &str) -> Result<bool, StorageError> {
        sqlx::query_scalar(
            "SELECT EXISTS(
                SELECT 1 FROM installed_plugins
                WHERE plugin_id = ? AND is_enabled = 1
            )",
        )
        .bind(plugin_id)
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn install_plugin(&self, plugin_id: &str) -> Result<(), StorageError> {
        sqlx::query(
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
            let mut query_builder = QueryBuilder::<sqlx::Sqlite>::new(
                "SELECT item_id, position_ticks, is_played, is_favorite, play_count,
                        last_played_at, version
                 FROM user_item_state WHERE user_id = ",
            );
            query_builder.push_bind(user_id).push(" AND item_id IN (");
            let mut separated = query_builder.separated(", ");
            for item_id in chunk {
                separated.push_bind(item_id);
            }
            separated.push_unseparated(")");
            let rows = query_builder
                .build()
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
        let values: Vec<(String, String)> = sqlx::query_as(
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
            sqlx::query(
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

    pub(crate) async fn media_strategy_settings(&self) -> Result<Option<String>, StorageError> {
        sqlx::query_scalar(
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
        sqlx::query(
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
        .bind(played)
        .bind(played)
        .bind(played)
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
        sqlx::query(
            "INSERT INTO user_item_state (user_id, item_id, is_favorite)
             VALUES (?, ?, ?)
             ON CONFLICT(user_id, item_id) DO UPDATE SET
                 is_favorite = excluded.is_favorite,
                 version = user_item_state.version + CASE
                     WHEN excluded.is_favorite != user_item_state.is_favorite THEN 1 ELSE 0 END",
        )
        .bind(user_id)
        .bind(item_id)
        .bind(favorite)
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
        sqlx::query(
            "INSERT INTO playback_sessions (
                id, user_id, item_id, media_source_id, play_session_id,
                device_id, client, device_name, state, position_ticks,
                duration_ticks, is_paused
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(user_id, play_session_id) DO UPDATE SET
                item_id = excluded.item_id,
                media_source_id = excluded.media_source_id,
                device_id = CASE
                    WHEN excluded.device_id = 'unknown' THEN playback_sessions.device_id
                    ELSE excluded.device_id END,
                client = COALESCE(excluded.client, playback_sessions.client),
                device_name = COALESCE(excluded.device_name, playback_sessions.device_name),
                state = excluded.state,
                position_ticks = MAX(playback_sessions.position_ticks, excluded.position_ticks),
                duration_ticks = COALESCE(excluded.duration_ticks, playback_sessions.duration_ticks),
                is_paused = excluded.is_paused,
                last_event_at = unixepoch()",
        )
        .bind(Uuid::now_v7().to_string())
        .bind(event.user_id)
        .bind(event.item_id)
        .bind(event.media_source_id)
        .bind(event.play_session_id)
        .bind(event.device_id)
        .bind(event.client)
        .bind(event.device_name)
        .bind(event.state)
        .bind(event.position_ticks)
        .bind(event.duration_ticks)
        .bind(event.is_paused)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        sqlx::query(
            "INSERT INTO user_item_state (user_id, item_id, position_ticks, last_played_at)
             VALUES (?, ?, ?, unixepoch())
             ON CONFLICT(user_id, item_id) DO UPDATE SET
                 position_ticks = MAX(user_item_state.position_ticks, excluded.position_ticks),
                 last_played_at = CASE
                     WHEN excluded.position_ticks > user_item_state.position_ticks
                     THEN excluded.last_played_at ELSE user_item_state.last_played_at END,
                 version = user_item_state.version + CASE
                     WHEN excluded.position_ticks > user_item_state.position_ticks THEN 1 ELSE 0 END",
        )
        .bind(event.user_id)
        .bind(event.item_id)
        .bind(event.position_ticks)
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

    pub(crate) async fn list_playback_sessions(
        &self,
        user_id: Option<&str>,
    ) -> Result<Vec<StoredPlaybackSession>, StorageError> {
        let (query, bind) = if user_id.is_some() {
            (
                "SELECT id, user_id, item_id, media_source_id, play_session_id,
                        device_id, client, device_name, state, position_ticks,
                        duration_ticks, is_paused, started_at, last_event_at
                 FROM playback_sessions
                 WHERE user_id = ? AND state != 'STOPPED'
                 ORDER BY last_event_at DESC, id",
                user_id,
            )
        } else {
            (
                "SELECT id, user_id, item_id, media_source_id, play_session_id,
                        device_id, client, device_name, state, position_ticks,
                        duration_ticks, is_paused, started_at, last_event_at
                 FROM playback_sessions
                 WHERE state != 'STOPPED'
                 ORDER BY last_event_at DESC, id",
                None,
            )
        };
        let mut statement = sqlx::query(query);
        if let Some(user_id) = bind {
            statement = statement.bind(user_id);
        }
        statement
            .fetch_all(&self.pool)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(|row| StoredPlaybackSession {
                        id: row.get("id"),
                        user_id: row.get("user_id"),
                        item_id: row.get("item_id"),
                        media_source_id: row.get("media_source_id"),
                        play_session_id: row.get("play_session_id"),
                        device_id: row.get("device_id"),
                        client: row.get("client"),
                        device_name: row.get("device_name"),
                        state: row.get("state"),
                        position_ticks: row.get("position_ticks"),
                        duration_ticks: row.get("duration_ticks"),
                        is_paused: row.get::<i64, _>("is_paused") != 0,
                        started_at: row.get("started_at"),
                        last_event_at: row.get("last_event_at"),
                    })
                    .collect()
            })
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
        sqlx::query_scalar(
            "SELECT EXISTS(
                SELECT 1 FROM media_sources WHERE id = ? AND item_id = ?
            )",
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

    pub(crate) async fn delete_library_root(
        &self,
        library_id: &str,
        root_id: &str,
    ) -> Result<bool, StorageError> {
        sqlx::query("DELETE FROM library_roots WHERE id = ? AND library_id = ?")
            .bind(root_id)
            .bind(library_id)
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected() == 1)
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

    pub(crate) async fn append_scan_job_event(
        &self,
        event: NewScanJobEvent<'_>,
    ) -> Result<(), StorageError> {
        sqlx::query(
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
                sqlx::query_scalar(
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
                sqlx::query_scalar(
                    "SELECT COUNT(*) FROM scan_job_events
                     WHERE job_id = ? AND level = ?",
                )
                .bind(job_id)
                .bind(level)
                .fetch_one(&self.pool)
                .await
            }
            (None, Some(_)) => {
                sqlx::query_scalar(
                    "SELECT COUNT(*) FROM scan_job_events
                     WHERE job_id = ? AND event_code = ?",
                )
                .bind(job_id)
                .bind(event_code)
                .fetch_one(&self.pool)
                .await
            }
            (None, None) => {
                sqlx::query_scalar("SELECT COUNT(*) FROM scan_job_events WHERE job_id = ?")
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
                sqlx::query(
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
                sqlx::query(
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
                sqlx::query(
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
                sqlx::query(
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
        sqlx::query(
            "INSERT INTO metadata_reidentify_jobs (id, status, total_count, mode)
             VALUES (?, 'QUEUED', ?, ?)",
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
            sqlx::query(
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
            })
    }

    pub(crate) async fn find_metadata_reidentify_job(
        &self,
        job_id: &str,
    ) -> Result<Option<StoredMetadataReidentifyJob>, StorageError> {
        sqlx::query(
            "SELECT id, status, processed_count, total_count, error,
                    created_at, updated_at, started_at, finished_at, mode
             FROM metadata_reidentify_jobs WHERE id = ?",
        )
        .bind(job_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_metadata_reidentify_job))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn claim_metadata_reidentify_job(
        &self,
        job_id: &str,
    ) -> Result<bool, StorageError> {
        sqlx::query(
            "UPDATE metadata_reidentify_jobs
             SET status = 'RUNNING', started_at = COALESCE(started_at, unixepoch()),
                 updated_at = unixepoch()
             WHERE id = ? AND status = 'QUEUED'",
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
        sqlx::query_scalar(
            "SELECT item_id FROM metadata_reidentify_job_items
             WHERE job_id = ? AND status = 'PENDING'
             ORDER BY item_id LIMIT 1",
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
        sqlx::query(
            "UPDATE metadata_reidentify_job_items
             SET status = 'RUNNING', updated_at = unixepoch()
             WHERE job_id = ? AND item_id = ? AND status = 'PENDING'",
        )
        .bind(job_id)
        .bind(item_id)
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
        sqlx::query(
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
        sqlx::query(
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

    pub(crate) async fn finish_metadata_reidentify_job(
        &self,
        job_id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "UPDATE metadata_reidentify_jobs
             SET status = ?, error = ?, finished_at = unixepoch(), updated_at = unixepoch()
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
        let result = sqlx::query(
            "UPDATE metadata_reidentify_jobs
             SET status = 'QUEUED',
                 processed_count = (
                     SELECT COUNT(*) FROM metadata_reidentify_job_items
                     WHERE job_id = ? AND status = 'COMPLETED'
                 ),
                 error = NULL, started_at = NULL, finished_at = NULL,
                 updated_at = unixepoch()
             WHERE id = ? AND status = 'FAILED'",
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
            sqlx::query(
                "UPDATE metadata_reidentify_job_items
                 SET status = 'PENDING', candidate_count = 0, error = NULL,
                     updated_at = unixepoch()
                 WHERE job_id = ? AND status = 'FAILED'",
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
    ) -> Result<Vec<StoredMetadataReidentifyItem>, StorageError> {
        sqlx::query(
            "SELECT job_id, item_id, status, candidate_count, error, updated_at
             FROM metadata_reidentify_job_items WHERE job_id = ? ORDER BY item_id",
        )
        .bind(job_id)
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

    pub(crate) async fn list_scan_jobs(
        &self,
        status: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredScanJob>, StorageError> {
        let rows = if let Some(status) = status {
            sqlx::query(
                "SELECT id, library_id, job_type, status, generation, cursor,
                        processed_count, total_count, cancel_requested, error
                 FROM scan_jobs WHERE status = ?
                 ORDER BY created_at DESC, id DESC LIMIT ? OFFSET ?",
            )
            .bind(status)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
        } else {
            sqlx::query(
                "SELECT id, library_id, job_type, status, generation, cursor,
                        processed_count, total_count, cancel_requested, error
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

    pub(crate) async fn list_active_metadata_reidentify_job_ids(
        &self,
    ) -> Result<Vec<String>, StorageError> {
        sqlx::query_scalar(
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

    pub(crate) async fn find_external_subtitle(
        &self,
        item_id: &str,
        media_source_id: Option<&str>,
        stream_index: i64,
    ) -> Result<Option<StoredExternalSubtitle>, StorageError> {
        let row = if let Some(media_source_id) = media_source_id {
            sqlx::query(
                "SELECT ms.id AS media_source_id, ms.item_id, mt.external_path,
                        mt.language, mt.title, lr.canonical_path AS root_path
                 FROM media_streams mt
                 JOIN media_sources ms ON ms.id = mt.media_source_id
                 JOIN media_items mi ON mi.id = ms.item_id
                 JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                 JOIN library_roots lr ON lr.id = fe.library_root_id
                 WHERE ms.id = ? AND mi.id = ? AND mt.stream_index = ?
                   AND mt.stream_type = 'SUBTITLE' AND mt.external_path IS NOT NULL
                 LIMIT 1",
            )
            .bind(media_source_id)
            .bind(item_id)
            .bind(stream_index)
            .fetch_optional(&self.pool)
            .await
        } else {
            sqlx::query(
                "SELECT ms.id AS media_source_id, ms.item_id, mt.external_path,
                        mt.language, mt.title, lr.canonical_path AS root_path
                 FROM media_streams mt
                 JOIN media_sources ms ON ms.id = mt.media_source_id
                 JOIN media_items mi ON mi.id = ms.item_id
                 JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
                 JOIN library_roots lr ON lr.id = fe.library_root_id
                 WHERE mi.id = ? AND mt.stream_index = ?
                   AND mt.stream_type = 'SUBTITLE' AND mt.external_path IS NOT NULL
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
        let exists = sqlx::query_scalar::<_, i64>(
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
            sqlx::query(
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
        sqlx::query(
            "UPDATE media_streams
             SET title = ?, language = ?, is_default = ?, is_forced = ?,
                 updated_at = unixepoch()
             WHERE media_source_id = ? AND stream_index = ?
               AND stream_type = 'SUBTITLE' AND is_external = 1",
        )
        .bind(update.title)
        .bind(update.language)
        .bind(update.is_default)
        .bind(update.is_forced)
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

    pub(crate) async fn update_media_source_external_url(
        &self,
        filesystem_entry_id: &str,
        external_url: Option<&str>,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "UPDATE media_sources
             SET external_url = ?, updated_at = unixepoch()
             WHERE filesystem_entry_id = ?",
        )
        .bind(external_url)
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
        sqlx::query(
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
        let Some((old_item_id, parent_id, series_id)) =
            sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
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
        sqlx::query(
            "INSERT INTO user_item_state (
                user_id, item_id, position_ticks, is_played, is_favorite,
                play_count, last_played_at, version
             )
             SELECT user_id, ?, position_ticks, is_played, is_favorite,
                    play_count, last_played_at, version
             FROM user_item_state
             WHERE item_id = ?
             ON CONFLICT(user_id, item_id) DO UPDATE SET
                position_ticks = MAX(user_item_state.position_ticks, excluded.position_ticks),
                is_played = MAX(user_item_state.is_played, excluded.is_played),
                is_favorite = MAX(user_item_state.is_favorite, excluded.is_favorite),
                play_count = MAX(user_item_state.play_count, excluded.play_count),
                last_played_at = MAX(user_item_state.last_played_at, excluded.last_played_at),
                version = MAX(user_item_state.version, excluded.version)",
        )
        .bind(new_item_id)
        .bind(&old_item_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        sqlx::query("DELETE FROM user_item_state WHERE item_id = ?")
            .bind(&old_item_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        sqlx::query(
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
            sqlx::query(
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
        let Some((old_item_id, parent_id, series_id)) =
            sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
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
        sqlx::query("DELETE FROM media_sources WHERE id = ? AND item_id = ?")
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
            sqlx::query(
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

    pub(crate) async fn find_media_item_by_identity(
        &self,
        identity_key: &str,
    ) -> Result<Option<StoredMediaItem>, StorageError> {
        sqlx::query("SELECT id FROM media_items WHERE identity_key = ? AND removed_at IS NULL")
            .bind(identity_key)
            .fetch_optional(&self.pool)
            .await
            .map(|row| row.map(stored_media_item))
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn find_media_item_metadata(
        &self,
        item_id: &str,
    ) -> Result<Option<StoredMediaMetadata>, StorageError> {
        sqlx::query(
            "SELECT item_type, title, original_title, overview, production_year,
                    metadata_provenance_json, locked_fields_json
             FROM media_items WHERE id = ?",
        )
        .bind(item_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| StoredMediaMetadata {
                item_type: row.get("item_type"),
                title: row.get("title"),
                original_title: row.get("original_title"),
                overview: row.get("overview"),
                production_year: row.get("production_year"),
                provenance_json: row.get("metadata_provenance_json"),
                locked_fields_json: row.get("locked_fields_json"),
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_media_item_image_identity(
        &self,
        item_id: &str,
    ) -> Result<Option<StoredImageIdentity>, StorageError> {
        sqlx::query(
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
        sqlx::query(
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
        let existing = sqlx::query(
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
            sqlx::query(
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
            sqlx::query(
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
        sqlx::query(
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
        sqlx::query(
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
        sqlx::query("DELETE FROM collection_items WHERE collection_id = ?")
            .bind(&collection_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let mut member_count = 0_usize;
        for (member_provider, member_provider_id, sort_order) in member_provider_ids {
            let Some(member_item_id) = sqlx::query_scalar::<_, String>(
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
            sqlx::query(
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

    pub(crate) async fn list_collection_member_ids(
        &self,
        collection_item_id: &str,
    ) -> Result<Vec<String>, StorageError> {
        sqlx::query_scalar(
            "SELECT ci.item_id
             FROM collection_items ci
             JOIN collections c ON c.id = ci.collection_id
             WHERE c.item_id = ?
             ORDER BY ci.sort_order, ci.item_id",
        )
        .bind(collection_item_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn count_pending_metadata_candidates(&self) -> Result<i64, StorageError> {
        sqlx::query_scalar("SELECT COUNT(*) FROM metadata_candidates WHERE status = 'PENDING'")
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn insert_metadata_candidate(
        &self,
        candidate: NewMetadataCandidate<'_>,
    ) -> Result<(), StorageError> {
        sqlx::query(
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
        sqlx::query(
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
            sqlx::query_scalar::<_, i64>(
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
            sqlx::query_scalar::<_, i64>(
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
            sqlx::query(
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
            sqlx::query(
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
        sqlx::query(
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

    pub(crate) async fn select_metadata_candidate(
        &self,
        update: SelectedMetadataUpdate<'_>,
    ) -> Result<bool, StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        sqlx::query(
            "UPDATE media_items
             SET title = ?, original_title = ?, overview = ?, production_year = ?,
                 provider_ids_json = ?, identification_status = 'ONLINE_CONFIRMED',
                 metadata_fingerprint = ?, metadata_provenance_json = ?, locked_fields_json = ?
             WHERE id = ? AND removed_at IS NULL",
        )
        .bind(update.title)
        .bind(update.original_title)
        .bind(update.overview)
        .bind(update.production_year)
        .bind(update.provider_ids_json)
        .bind(update.metadata_fingerprint)
        .bind(update.provenance_json)
        .bind(update.locked_fields_json)
        .bind(update.item_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        let selected = sqlx::query(
            "UPDATE metadata_candidates
             SET status = 'SELECTED', updated_at = unixepoch()
             WHERE id = ? AND item_id = ? AND status = 'PENDING'",
        )
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
        sqlx::query(
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

    pub(crate) async fn search_catalog_item_ids(
        &self,
        query: &str,
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
        let fts_query = format!(
            "SELECT mi.id FROM media_search
             JOIN media_items mi ON mi.id = media_search.item_id
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             WHERE media_search MATCH ? AND mi.removed_at IS NULL{}",
            library_filter.as_deref().unwrap_or_default()
        );
        let like_query_sql = format!(
            "SELECT mi.id FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             WHERE (mi.title LIKE ? OR COALESCE(mi.original_title, '') LIKE ?
                    OR EXISTS (SELECT 1 FROM item_aliases ia
                               WHERE ia.item_id = mi.id AND ia.alias LIKE ?))
               AND mi.removed_at IS NULL{}",
            library_filter.as_deref().unwrap_or_default()
        );
        let union_query = format!("{fts_query} UNION {like_query_sql}");
        let count_query = format!("SELECT COUNT(*) FROM ({union_query}) matches");
        let mut count_statement =
            sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(count_query)).bind(query);
        if let Some(library_ids) = library_ids {
            for library_id in library_ids {
                count_statement = count_statement.bind(library_id);
            }
        }
        count_statement = count_statement
            .bind(like_query)
            .bind(like_query)
            .bind(like_query);
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
        let list_query = format!(
            "SELECT matches.id FROM ({union_query}) matches
             JOIN media_items mi ON mi.id = matches.id
             ORDER BY mi.sort_title, mi.id LIMIT ? OFFSET ?"
        );
        let mut list_statement = sqlx::query(sqlx::AssertSqlSafe(list_query)).bind(query);
        if let Some(library_ids) = library_ids {
            for library_id in library_ids {
                list_statement = list_statement.bind(library_id);
            }
        }
        list_statement = list_statement
            .bind(like_query)
            .bind(like_query)
            .bind(like_query);
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
            "SELECT COUNT(*) FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             WHERE mi.removed_at IS NULL AND mi.library_id IN ({placeholders})"
        );
        let mut count_statement = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(count_query));
        for library_id in library_ids {
            count_statement = count_statement.bind(library_id);
        }
        let total = count_statement
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;

        let list_query = format!(
            "SELECT mi.id
             FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             WHERE mi.removed_at IS NULL AND mi.library_id IN ({placeholders})
             ORDER BY mi.added_at DESC, mi.sort_title, mi.id
             LIMIT ? OFFSET ?"
        );
        let mut list_statement = sqlx::query(sqlx::AssertSqlSafe(list_query));
        for library_id in library_ids {
            list_statement = list_statement.bind(library_id);
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

    pub(crate) async fn list_recent_catalog_rows_by_library(
        &self,
        library_ids: &[String],
        limit: i64,
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        if library_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat_n("?", library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "WITH ranked AS (
                 SELECT mi.id, mi.library_id,
                        ROW_NUMBER() OVER (
                            PARTITION BY mi.library_id
                            ORDER BY mi.added_at DESC, mi.sort_title ASC, mi.id ASC
                        ) AS library_rank
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 WHERE mi.removed_at IS NULL
                   AND mi.item_type IN ('MOVIE', 'SERIES')
                   AND mi.library_id IN ({placeholders})
             )
             SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                    mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                    mi.title, mi.sort_title, mi.original_title, mi.overview,
                    mi.production_year, mi.runtime_ticks,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                     ORDER BY image_index LIMIT 1) AS poster_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                     ORDER BY image_index LIMIT 1) AS fanart_image_tag,
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
             LEFT JOIN media_sources ms ON ms.item_id = mi.id
             LEFT JOIN media_streams mt ON mt.media_source_id = ms.id
             WHERE ranked.library_rank <= ?
             ORDER BY ranked.library_id, ranked.library_rank, ms.id, mt.stream_index"
        );
        let mut binds = Vec::with_capacity(library_ids.len() + 1);
        binds.extend(library_ids.iter().map(|value| CatalogBind::Text(value)));
        binds.push(CatalogBind::Integer(limit));
        self.fetch_catalog_rows(&query, &binds).await
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
                            + MIN(30, MAX(0, 30 - CAST((unixepoch() - mi.added_at) / 86400 AS INTEGER)))
                            + CASE WHEN us.last_played_at IS NULL THEN 0 ELSE
                                MIN(30, MAX(0, 30 - CAST((unixepoch() - us.last_played_at) / 86400 AS INTEGER)))
                              END
                        ) AS recommendation_score,
                        mi.added_at,
                        mi.sort_title
                 FROM media_items mi
                 JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
                 LEFT JOIN user_item_state us
                   ON us.item_id = mi.id AND us.user_id = ?
                 WHERE mi.removed_at IS NULL
                   AND mi.item_type IN ('MOVIE', 'SERIES')
                   AND mi.library_id IN ({placeholders})
                 ORDER BY recommendation_score DESC, mi.added_at DESC,
                          mi.sort_title, mi.id
                 LIMIT ? OFFSET ?
             )
             SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                    mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                    mi.title, mi.sort_title, mi.original_title, mi.overview,
                    mi.production_year, mi.runtime_ticks,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                     ORDER BY image_index LIMIT 1) AS poster_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                     ORDER BY image_index LIMIT 1) AS fanart_image_tag,
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
             LEFT JOIN media_sources ms ON ms.item_id = mi.id
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
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             WHERE mi.parent_id = ? AND mi.item_type = ? AND mi.removed_at IS NULL",
        )
        .bind(parent_id)
        .bind(item_type)
        .fetch_one(&self.pool)
        .await
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
        self.fetch_catalog_rows(
            "SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                    mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                    mi.title, mi.sort_title, mi.original_title, mi.overview,
                    mi.production_year, mi.runtime_ticks,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                     ORDER BY image_index LIMIT 1) AS poster_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                     ORDER BY image_index LIMIT 1) AS fanart_image_tag,
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
             LEFT JOIN media_sources ms ON ms.item_id = mi.id
             LEFT JOIN media_streams mt ON mt.media_source_id = ms.id
             WHERE mi.parent_id = ? AND mi.item_type = ? AND mi.removed_at IS NULL
             ORDER BY mi.season_number, mi.episode_number, mi.sort_title, mi.id,
                      ms.id, mt.stream_index
             LIMIT ? OFFSET ?",
            &[
                CatalogBind::Text(parent_id),
                CatalogBind::Text(item_type),
                CatalogBind::Integer(limit),
                CatalogBind::Integer(offset),
            ],
        )
        .await
    }

    pub(crate) async fn count_next_up_items(
        &self,
        user_id: &str,
        library_ids: &[String],
    ) -> Result<i64, StorageError> {
        if library_ids.is_empty() {
            return Ok(0);
        }
        let placeholders = std::iter::repeat_n("?", library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "SELECT COUNT(*) FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             JOIN user_item_state us ON us.item_id = mi.id AND us.user_id = ?
             WHERE mi.item_type = 'EPISODE' AND mi.removed_at IS NULL
               AND us.is_played = 0 AND us.position_ticks > 0
               AND mi.library_id IN ({placeholders})"
        );
        let mut statement = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(query)).bind(user_id);
        for library_id in library_ids {
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

    pub(crate) async fn list_next_up_items(
        &self,
        user_id: &str,
        library_ids: &[String],
        offset: i64,
        limit: i64,
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        if library_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = std::iter::repeat_n("?", library_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        let query = format!(
            "SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                    mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                    mi.title, mi.sort_title, mi.original_title, mi.overview,
                    mi.production_year, mi.runtime_ticks,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                     ORDER BY image_index LIMIT 1) AS poster_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                     ORDER BY image_index LIMIT 1) AS fanart_image_tag,
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
             LEFT JOIN media_sources ms ON ms.item_id = mi.id
             LEFT JOIN media_streams mt ON mt.media_source_id = ms.id
             WHERE mi.item_type = 'EPISODE' AND mi.removed_at IS NULL
               AND us.is_played = 0 AND us.position_ticks > 0
               AND mi.library_id IN ({placeholders})
             ORDER BY us.last_played_at DESC, mi.series_id, mi.season_number,
                      mi.episode_number, mi.id
             LIMIT ? OFFSET ?"
        );
        let mut binds = Vec::with_capacity(library_ids.len() + 3);
        binds.push(CatalogBind::Text(user_id));
        binds.extend(library_ids.iter().map(|value| CatalogBind::Text(value)));
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
        let (where_clause, filter_binds) = catalog_filter_where_clause(
            filter.library_ids,
            filter.user_id,
            filter.item_types,
            filter.years,
            filter.is_played,
            filter.is_favorite,
        );
        let count_query = format!(
            "SELECT COUNT(*) FROM media_items mi
             JOIN libraries l ON l.id = mi.library_id AND l.is_enabled = 1
             {where_clause}"
        );
        let mut count_statement = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(count_query));
        for bind in &filter_binds {
            count_statement = match bind {
                CatalogBind::Text(value) => count_statement.bind(*value),
                CatalogBind::Integer(value) => count_statement.bind(*value),
            };
        }
        let total = count_statement
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;

        let item_order = if filter.sort_by_date_created {
            if filter.descending {
                "mi.added_at DESC, mi.sort_title ASC, mi.id ASC"
            } else {
                "mi.added_at ASC, mi.sort_title ASC, mi.id ASC"
            }
        } else if filter.descending {
            "mi.sort_title DESC, mi.id DESC"
        } else {
            "mi.sort_title ASC, mi.id ASC"
        };
        let query = format!(
            "SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                    mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                    mi.title, mi.sort_title, mi.original_title, mi.overview,
                    mi.production_year, mi.runtime_ticks,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                     ORDER BY image_index LIMIT 1) AS poster_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                     ORDER BY image_index LIMIT 1) AS fanart_image_tag,
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
             LEFT JOIN media_sources ms ON ms.item_id = mi.id
             LEFT JOIN media_streams mt ON mt.media_source_id = ms.id
             ORDER BY {item_order}, ms.id, mt.stream_index"
        );
        let mut list_binds = filter_binds;
        list_binds.push(CatalogBind::Integer(filter.limit));
        list_binds.push(CatalogBind::Integer(filter.offset));
        let rows = self.fetch_catalog_rows(&query, &list_binds).await?;
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
                "SELECT mi.id AS item_id, mi.library_id, mi.item_type,
                        mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                        mi.title, mi.sort_title, mi.original_title, mi.overview,
                        mi.production_year, mi.runtime_ticks,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                         ORDER BY image_index LIMIT 1) AS poster_image_tag,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                         ORDER BY image_index LIMIT 1) AS fanart_image_tag,
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
                        mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                        mi.title, mi.sort_title, mi.original_title, mi.overview,
                        mi.production_year, mi.runtime_ticks,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                         ORDER BY image_index LIMIT 1) AS poster_image_tag,
                        (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                         ORDER BY image_index LIMIT 1) AS fanart_image_tag,
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
                    mi.parent_id, mi.series_id, mi.season_number, mi.episode_number,
                    mi.title, mi.sort_title, mi.original_title, mi.overview,
                    mi.production_year, mi.runtime_ticks,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'POSTER'
                     ORDER BY image_index LIMIT 1) AS poster_image_tag,
                    (SELECT id FROM item_images WHERE item_id = mi.id AND image_type = 'FANART'
                     ORDER BY image_index LIMIT 1) AS fanart_image_tag,
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
        query: &str,
        binds: &[CatalogBind<'_>],
    ) -> Result<Vec<StoredCatalogRow>, StorageError> {
        let mut statement = sqlx::query(sqlx::AssertSqlSafe(query));
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
                        runtime_ticks: row.get("runtime_ticks"),
                        poster_image_tag: row.get("poster_image_tag"),
                        fanart_image_tag: row.get("fanart_image_tag"),
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
                edition_name, quality_label, container, size,
                external_url, is_default, probe_status
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'PENDING')",
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
             WHERE mi.library_id = ? AND ms.source_kind IN ('LOCAL_FILE', 'STRM_URL')
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

    pub(crate) async fn find_media_source_path(
        &self,
        item_id: &str,
    ) -> Result<Option<StoredMediaSourcePath>, StorageError> {
        sqlx::query(
            "SELECT ms.id AS source_id, ms.item_id, ms.probe_status,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE mi.id = ? AND ms.source_kind = 'LOCAL_FILE'
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

    pub(crate) async fn find_metadata_writeback_source_path(
        &self,
        item_id: &str,
    ) -> Result<Option<StoredMediaSourcePath>, StorageError> {
        sqlx::query(
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

    pub(crate) async fn find_media_source_path_by_id(
        &self,
        item_id: &str,
        source_id: &str,
    ) -> Result<Option<StoredMediaSourcePath>, StorageError> {
        sqlx::query(
            "SELECT ms.id AS source_id, ms.item_id, ms.probe_status,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_sources ms
             JOIN media_items mi ON mi.id = ms.item_id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE ms.id = ? AND mi.id = ? AND ms.source_kind = 'LOCAL_FILE'
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

    pub(crate) async fn find_deletable_media_source_path(
        &self,
        item_id: &str,
    ) -> Result<Option<StoredMediaSourcePath>, StorageError> {
        sqlx::query(
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
        sqlx::query(
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
        sqlx::query("SELECT item_type, season_number FROM media_items WHERE id = ?")
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
        sqlx::query(
            "SELECT ms.id AS source_id, episode.id AS item_id, ms.probe_status,
                    lr.canonical_path AS root_path, fe.relative_path
             FROM media_items episode
             JOIN media_sources ms ON ms.item_id = episode.id
             JOIN filesystem_entries fe ON fe.id = ms.filesystem_entry_id
             JOIN library_roots lr ON lr.id = fe.library_root_id
             WHERE episode.item_type = 'EPISODE'
               AND (episode.series_id = ? OR episode.parent_id = ?)
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
        sqlx::query(
            "UPDATE media_sources
             SET container = COALESCE(?, container), size = COALESCE(?, size),
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
            .bind(stream.is_external)
            .bind(stream.is_default)
            .bind(stream.is_forced)
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

    pub(crate) async fn list_series_metadata_sources(
        &self,
        library_id: &str,
    ) -> Result<Vec<StoredSeriesMetadataSource>, StorageError> {
        sqlx::query(
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
             ORDER BY series.id, season.season_number, episode.id, fe.relative_path",
        )
        .bind(library_id)
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

    pub(crate) async fn insert_hierarchy_item(
        &self,
        item: NewHierarchyItem<'_>,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO media_items (
                id, library_id, item_type, parent_id, series_id,
                season_number, episode_number, absolute_number,
                title, sort_title, original_title, production_year,
                identification_status, identity_key
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
        update: MediaMetadataUpdate<'_>,
    ) -> Result<(), StorageError> {
        sqlx::query(
            "UPDATE media_items
             SET title = ?,
                 original_title = ?,
                 overview = ?,
                 production_year = ?,
                 metadata_fingerprint = ?,
                 metadata_provenance_json = ?,
                 locked_fields_json = ?
             WHERE id = ?",
        )
        .bind(update.title)
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

    pub(crate) async fn upsert_item_image(
        &self,
        item_id: &str,
        image_type: &str,
        local_path: &std::path::Path,
        file_size: i64,
        content_tag: &str,
        source: &str,
    ) -> Result<String, StorageError> {
        let id = Uuid::now_v7().to_string();
        sqlx::query(
            "INSERT INTO item_images (
                id, item_id, image_type, image_index, local_path, file_size, content_tag, source
            ) VALUES (?, ?, ?, 0, ?, ?, ?, ?)
            ON CONFLICT(item_id, image_type, image_index) DO UPDATE SET
                id = excluded.id,
                local_path = excluded.local_path,
                file_size = excluded.file_size,
                content_tag = excluded.content_tag,
                source = excluded.source,
                updated_at = unixepoch()",
        )
        .bind(&id)
        .bind(item_id)
        .bind(image_type)
        .bind(local_path.to_string_lossy().as_ref())
        .bind(file_size)
        .bind(content_tag)
        .bind(source)
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

    pub(crate) async fn list_item_images(
        &self,
        item_id: &str,
    ) -> Result<Vec<StoredItemImage>, StorageError> {
        sqlx::query(
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

    pub(crate) async fn find_item_image(
        &self,
        item_id: &str,
        image_id: &str,
    ) -> Result<Option<StoredItemImage>, StorageError> {
        sqlx::query(
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
        sqlx::query("DELETE FROM item_images WHERE item_id = ? AND id = ?")
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

    pub(crate) async fn list_web_session_summaries(
        &self,
        user_id: &str,
        current_session_token_hash: &[u8],
    ) -> Result<Vec<StoredWebSessionSummary>, StorageError> {
        sqlx::query(
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
        sqlx::query(
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
        sqlx::query("SELECT COALESCE(MAX(version), 0) AS version FROM _sqlx_migrations")
            .fetch_one(&self.pool)
            .await
            .map(|row| row.get::<i64, _>("version"))
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
        sqlx::query(
            "INSERT OR REPLACE INTO lux_meta (key, value)
             VALUES ('__lux_write_probe__', ?)",
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

fn stored_user(row: sqlx::sqlite::SqliteRow) -> StoredUser {
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
    pub(crate) scraper_id: Option<String>,
    pub(crate) cover_image_path: Option<String>,
    pub(crate) cover_image_content_type: Option<String>,
    pub(crate) cover_image_size: Option<i64>,
    pub(crate) cover_image_tag: Option<String>,
    pub(crate) media_strategy_json: Option<String>,
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

fn stored_scan_job_event(row: sqlx::sqlite::SqliteRow) -> StoredScanJobEvent {
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
    pub(crate) provenance_json: Option<String>,
    pub(crate) locked_fields_json: Option<String>,
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

fn stored_metadata_reidentify_job(row: sqlx::sqlite::SqliteRow) -> StoredMetadataReidentifyJob {
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
    }
}

fn stored_metadata_reidentify_item(row: sqlx::sqlite::SqliteRow) -> StoredMetadataReidentifyItem {
    StoredMetadataReidentifyItem {
        job_id: row.get("job_id"),
        item_id: row.get("item_id"),
        status: row.get("status"),
        candidate_count: row.get("candidate_count"),
        error: row.get("error"),
        updated_at: row.get("updated_at"),
    }
}

fn stored_metadata_candidate(row: sqlx::sqlite::SqliteRow) -> StoredMetadataCandidate {
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

fn stored_media_item(row: sqlx::sqlite::SqliteRow) -> StoredMediaItem {
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
    pub(crate) runtime_ticks: Option<i64>,
    pub(crate) poster_image_tag: Option<String>,
    pub(crate) fanart_image_tag: Option<String>,
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
    pub(crate) state: &'a str,
    pub(crate) position_ticks: i64,
    pub(crate) duration_ticks: Option<i64>,
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
    pub(crate) state: String,
    pub(crate) position_ticks: i64,
    pub(crate) duration_ticks: Option<i64>,
    pub(crate) is_paused: bool,
    pub(crate) started_at: i64,
    pub(crate) last_event_at: i64,
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

fn stored_item_image(row: sqlx::sqlite::SqliteRow) -> StoredItemImage {
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
    library_ids: &'a [String],
    user_id: &'a str,
    item_types: &'a [String],
    years: &'a [i64],
    is_played: Option<bool>,
    is_favorite: Option<bool>,
) -> (String, Vec<CatalogBind<'a>>) {
    let mut where_clause = format!(
        "WHERE mi.removed_at IS NULL
         AND mi.library_id IN ({})",
        std::iter::repeat_n("?", library_ids.len())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut binds = library_ids
        .iter()
        .map(|library_id| CatalogBind::Text(library_id.as_str()))
        .collect::<Vec<_>>();
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
    (where_clause, binds)
}

#[derive(Clone, Copy)]
enum CatalogBind<'a> {
    Text(&'a str),
    Integer(i64),
}

pub(crate) struct CatalogFilterQuery<'a> {
    pub(crate) library_ids: &'a [String],
    pub(crate) user_id: &'a str,
    pub(crate) item_types: &'a [String],
    pub(crate) years: &'a [i64],
    pub(crate) is_played: Option<bool>,
    pub(crate) is_favorite: Option<bool>,
    pub(crate) sort_by_date_created: bool,
    pub(crate) descending: bool,
    pub(crate) offset: i64,
    pub(crate) limit: i64,
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
    preferred
        .and_then(|preferred| {
            providers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(preferred))
                .cloned()
        })
        .or_else(|| providers.into_iter().next())
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
    pub(crate) scraper_id: Option<&'a str>,
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
    pub(crate) provider_ids_json: &'a str,
    pub(crate) metadata_fingerprint: &'a [u8],
    pub(crate) provenance_json: &'a str,
    pub(crate) locked_fields_json: &'a str,
}

pub(crate) struct LibrarySettingsUpdate<'a> {
    pub(crate) name: Option<&'a str>,
    pub(crate) kind: Option<&'a str>,
    pub(crate) is_enabled: Option<bool>,
    pub(crate) realtime_watch_enabled: Option<bool>,
    pub(crate) incremental_schedule: Option<Option<&'a str>>,
    pub(crate) reconciliation_schedule: Option<Option<&'a str>>,
    pub(crate) metadata_schedule: Option<Option<&'a str>>,
    pub(crate) scan_concurrency: Option<i64>,
    pub(crate) probe_concurrency: Option<i64>,
    pub(crate) scraper_id: Option<Option<&'a str>>,
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
    pub(crate) is_default: bool,
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
    LastManager,
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
            Self::LastManager => {
                formatter.write_str("at least one active server manager is required")
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
            Self::LastManager => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn write_probe_reports_a_query_only_sqlite_connection() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(SqliteConnectOptions::new().filename(":memory:"))
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
        };
        assert!(database.probe_write().await.is_err());
        database.close().await;
    }
}
