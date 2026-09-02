use super::*;

use std::time::{Duration, Instant};

const SQLITE_USER_UPDATE_RETRY_DELAY: Duration = Duration::from_millis(50);
const SQLITE_USER_UPDATE_RETRY_WINDOW: Duration = Duration::from_secs(5);

impl Database {
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
        has_password: bool,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO users (
                id, username_normalized, display_name, password_hash,
                is_admin, can_manage_server, has_password
            ) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(username_normalized)
        .bind(display_name)
        .bind(password_hash)
        .bind(database_flag(is_admin))
        .bind(database_flag(is_admin))
        .bind(database_flag(has_password))
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
                    has_password,
                    is_disabled, is_admin, can_manage_server,
                    can_remote_access, can_download, last_login_at,
                    COALESCE(
                        (SELECT MAX(COALESCE(at.last_seen_at, at.created_at))
                         FROM access_tokens at WHERE at.user_id = users.id),
                        last_login_at
                    ) AS last_activity_at
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
                has_password: row.get::<i64, _>("has_password") != 0,
                is_disabled: row.get::<i64, _>("is_disabled") != 0,
                is_admin: row.get::<i64, _>("is_admin") != 0,
                can_manage_server: row.get::<i64, _>("can_manage_server") != 0,
                can_remote_access: row.get::<i64, _>("can_remote_access") != 0,
                can_download: row.get::<i64, _>("can_download") != 0,
                last_login_at: row.get("last_login_at"),
                last_activity_at: row.get("last_activity_at"),
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_users_by_normalized_usernames(
        &self,
        usernames: &[String],
    ) -> Result<Vec<StoredUser>, StorageError> {
        if usernames.is_empty() {
            return Ok(Vec::new());
        }
        let mut users = Vec::new();
        for chunk in usernames.chunks(BATCH_INSERT_CHUNK_SIZE) {
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT id, username_normalized, display_name, password_hash,
                        has_password,
                        is_disabled, is_admin, can_manage_server,
                        can_remote_access, can_download, last_login_at,
                        COALESCE(
                            (SELECT MAX(COALESCE(at.last_seen_at, at.created_at))
                             FROM access_tokens at WHERE at.user_id = users.id),
                            last_login_at
                        ) AS last_activity_at
                 FROM users WHERE username_normalized IN ({placeholders})
                 ORDER BY username_normalized"
            );
            let mut query = self.query(sqlx::AssertSqlSafe(query));
            for username in chunk {
                query = query.bind(username);
            }
            users.extend(
                query
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?
                    .into_iter()
                    .map(stored_user),
            );
        }
        Ok(users)
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
                    has_password,
                    is_disabled, is_admin, can_manage_server,
                    can_remote_access, can_download, last_login_at,
                    COALESCE(
                        (SELECT MAX(COALESCE(at.last_seen_at, at.created_at))
                         FROM access_tokens at WHERE at.user_id = users.id),
                        last_login_at
                    ) AS last_activity_at
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
                    has_password: row.get::<i64, _>("has_password") != 0,
                    is_disabled: row.get::<i64, _>("is_disabled") != 0,
                    is_admin: row.get::<i64, _>("is_admin") != 0,
                    can_manage_server: row.get::<i64, _>("can_manage_server") != 0,
                    can_remote_access: row.get::<i64, _>("can_remote_access") != 0,
                    can_download: row.get::<i64, _>("can_download") != 0,
                    last_login_at: row.get("last_login_at"),
                    last_activity_at: row.get("last_activity_at"),
                })
                .collect()
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn query_users(
        &self,
        is_disabled: Option<bool>,
        name_starts_with_or_greater: Option<&str>,
        descending: bool,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<StoredUser>, i64), StorageError> {
        let mut conditions = Vec::new();
        if is_disabled.is_some() {
            conditions.push("is_disabled = ?");
        }
        if name_starts_with_or_greater.is_some() {
            conditions.push("username_normalized >= ?");
        }
        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", conditions.join(" AND "))
        };
        let direction = if descending { "DESC" } else { "ASC" };
        let query = format!(
            "SELECT id, username_normalized, display_name, password_hash,
                    has_password,
                    is_disabled, is_admin, can_manage_server,
                    can_remote_access, can_download, last_login_at,
                    COALESCE(
                        (SELECT MAX(COALESCE(at.last_seen_at, at.created_at))
                         FROM access_tokens at WHERE at.user_id = users.id),
                        last_login_at
                    ) AS last_activity_at,
                    COUNT(*) OVER () AS total_count
             FROM users{where_clause}
             ORDER BY username_normalized {direction}, id {direction}
             LIMIT ? OFFSET ?"
        );
        let mut query = self.query(sqlx::AssertSqlSafe(query));
        if let Some(is_disabled) = is_disabled {
            query = query.bind(database_flag(is_disabled));
        }
        if let Some(name_starts_with_or_greater) = name_starts_with_or_greater {
            query = query.bind(name_starts_with_or_greater);
        }
        let rows = query
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let total_count = rows
            .first()
            .map(|row| row.get::<i64, _>("total_count"))
            .unwrap_or(0);
        let users = rows.into_iter().map(stored_user).collect();
        Ok((users, total_count))
    }

    pub(crate) async fn find_user_by_id(
        &self,
        user_id: &str,
    ) -> Result<Option<StoredUser>, StorageError> {
        self.query(
            "SELECT id, username_normalized, display_name, password_hash,
                    has_password,
                    is_disabled, is_admin, can_manage_server,
                    can_remote_access, can_download, last_login_at,
                    COALESCE(
                        (SELECT MAX(COALESCE(at.last_seen_at, at.created_at))
                         FROM access_tokens at WHERE at.user_id = users.id),
                        last_login_at
                    ) AS last_activity_at
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
        let retry_deadline = Instant::now() + SQLITE_USER_UPDATE_RETRY_WINDOW;
        loop {
            match self.update_user_once(user_id, &update).await {
                Err(error)
                    if self.backend == DatabaseBackend::Sqlite
                        && is_sqlite_lock_error(&error)
                        && Instant::now() < retry_deadline =>
                {
                    tokio::time::sleep(SQLITE_USER_UPDATE_RETRY_DELAY).await;
                }
                result => return result,
            }
        }
    }

    async fn update_user_once(
        &self,
        user_id: &str,
        update: &UpdateUser<'_>,
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
                 has_password = COALESCE(?, has_password),
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
        .bind(update.has_password.map(database_flag))
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

    pub(crate) async fn delete_user(&self, user_id: &str) -> Result<bool, StorageError> {
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
            return Ok(false);
        };
        let current_disabled = current.get::<i64, _>("is_disabled") != 0;
        let current_can_manage = current.get::<i64, _>("can_manage_server") != 0;
        if current_can_manage && !current_disabled {
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
        self.query("DELETE FROM users WHERE id = ?")
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
        Ok(true)
    }

    pub(crate) async fn find_user_emby_configuration(
        &self,
        user_id: &str,
    ) -> Result<Option<String>, StorageError> {
        self.query_scalar(
            "SELECT configuration_json
             FROM user_emby_configuration WHERE user_id = ?",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn set_user_emby_configuration(
        &self,
        user_id: &str,
        configuration_json: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO user_emby_configuration (user_id, configuration_json)
             VALUES (?, ?)
             ON CONFLICT(user_id) DO UPDATE SET
                 configuration_json = excluded.configuration_json,
                 updated_at = unixepoch()",
        )
        .bind(user_id)
        .bind(configuration_json)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn copy_user_library_settings(
        &self,
        source_user_id: &str,
        target_user_id: &str,
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
            "INSERT INTO user_library_access (user_id, library_id, can_view)
             SELECT ?, library_id, can_view
             FROM user_library_access WHERE user_id = ?",
        )
        .bind(target_user_id)
        .bind(source_user_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query(
            "INSERT INTO user_library_order (user_id, library_id, position)
             SELECT ?, library_id, position
             FROM user_library_order WHERE user_id = ?",
        )
        .bind(target_user_id)
        .bind(source_user_id)
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

    pub(crate) async fn mark_user_logged_in(&self, user_id: &str) -> Result<(), StorageError> {
        self.query("UPDATE users SET last_login_at = unixepoch() WHERE id = ?")
            .bind(user_id)
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn touch_access_token(&self, token_hash: &[u8]) -> Result<(), StorageError> {
        self.query(
            "UPDATE access_tokens SET last_seen_at = unixepoch(), updated_at = unixepoch()
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
                    u.has_password, u.is_disabled, u.is_admin, u.can_manage_server,
                    u.can_remote_access, u.can_download, u.last_login_at,
                    COALESCE(
                        (SELECT MAX(COALESCE(at2.last_seen_at, at2.created_at))
                         FROM access_tokens at2 WHERE at2.user_id = u.id),
                        u.last_login_at
                    ) AS last_activity_at
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
                has_password: row.get::<i64, _>("has_password") != 0,
                is_disabled: row.get::<i64, _>("is_disabled") != 0,
                is_admin: row.get::<i64, _>("is_admin") != 0,
                can_manage_server: row.get::<i64, _>("can_manage_server") != 0,
                can_remote_access: row.get::<i64, _>("can_remote_access") != 0,
                can_download: row.get::<i64, _>("can_download") != 0,
                last_login_at: row.get("last_login_at"),
                last_activity_at: row.get("last_activity_at"),
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

    pub(crate) async fn set_user_library_access_batch(
        &self,
        user_id: &str,
        updates: &[(String, bool)],
    ) -> Result<(), StorageError> {
        if updates.is_empty() {
            return Ok(());
        }
        let mut transaction = self.begin_metadata_write_transaction().await?;
        for chunk in updates.chunks(BATCH_INSERT_CHUNK_SIZE) {
            // The dynamic fragment is derived only from the bounded chunk length. Library IDs
            // and permissions remain bound values, so no external data can become SQL text.
            let placeholders = (0..chunk.len())
                .map(|_| "(?, ?, ?)")
                .collect::<Vec<_>>()
                .join(", ");
            let statement = sqlx::AssertSqlSafe(format!(
                "INSERT INTO user_library_access (user_id, library_id, can_view)
                 VALUES {placeholders}
                 ON CONFLICT(user_id, library_id) DO UPDATE SET
                     can_view = excluded.can_view, updated_at = unixepoch()
                 WHERE user_library_access.can_view <> excluded.can_view"
            ));
            let mut statement = self.query(statement);
            for (library_id, can_view) in chunk {
                statement = statement
                    .bind(user_id)
                    .bind(library_id)
                    .bind(database_flag(*can_view));
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

    pub(crate) async fn find_user_person_favorite(
        &self,
        user_id: &str,
        person_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT COALESCE((
                 SELECT is_favorite
                 FROM user_person_state
                 WHERE user_id = ? AND person_id = ?
             ), 0)",
        )
        .bind(user_id)
        .bind(person_id)
        .fetch_one(&self.pool)
        .await
        .map(|value: i64| value != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn set_user_person_favorite(
        &self,
        user_id: &str,
        person_id: &str,
        favorite: bool,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO user_person_state (user_id, person_id, is_favorite)
             VALUES (?, ?, ?)
             ON CONFLICT(user_id, person_id) DO UPDATE SET
                 is_favorite = excluded.is_favorite,
                 updated_at = unixepoch()",
        )
        .bind(user_id)
        .bind(person_id)
        .bind(database_flag(favorite))
        .execute(&self.pool)
        .await
        .map(|_| ())
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
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let library_ids = self
            .query_scalar::<String>(
                "SELECT DISTINCT library_id FROM library_scrapers WHERE scraper_id = ?",
            )
            .bind(plugin_id)
            .fetch_all(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        self.query("DELETE FROM library_scrapers WHERE scraper_id = ?")
            .bind(plugin_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        for library_id in library_ids {
            let rows = self
                .query(
                    "SELECT scraper_id, role FROM library_scrapers
                     WHERE library_id = ? ORDER BY position, scraper_id",
                )
                .bind(&library_id)
                .fetch_all(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            self.query("DELETE FROM library_scrapers WHERE library_id = ?")
                .bind(&library_id)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            for (position, row) in rows.into_iter().enumerate() {
                let stored_role: String = row.get("role");
                let role = if position == 0 {
                    "PRIMARY"
                } else if stored_role.as_str() == "PRIMARY" {
                    "BACKUP"
                } else {
                    stored_role.as_str()
                };
                let position = i64::try_from(position)
                    .map_err(|_| StorageError::Serialization("刮削器位置超出范围".to_owned()))?;
                self.query(
                    "INSERT INTO library_scrapers (library_id, scraper_id, position, role)
                     VALUES (?, ?, ?, ?)",
                )
                .bind(&library_id)
                .bind(row.get::<String, _>("scraper_id"))
                .bind(position)
                .bind(role)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            }
            let primary = self
                .query_scalar::<String>(
                    "SELECT scraper_id FROM library_scrapers
                     WHERE library_id = ? AND position = 0",
                )
                .bind(&library_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            self.query(
                "UPDATE libraries SET scraper_id = ?, updated_at = unixepoch()
                 WHERE id = ?",
            )
            .bind(primary)
            .bind(&library_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        }
        self.query(
            "UPDATE libraries
             SET chapter_source_id = CASE WHEN chapter_source_id = ? THEN NULL ELSE chapter_source_id END,
                 scraper_id = CASE WHEN scraper_id = ? THEN NULL ELSE scraper_id END,
                 updated_at = unixepoch()
             WHERE chapter_source_id = ? OR scraper_id = ?",
        )
        .bind(plugin_id)
        .bind(plugin_id)
        .bind(plugin_id)
        .bind(plugin_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query("DELETE FROM installed_plugins WHERE plugin_id = ?")
            .bind(plugin_id)
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
}

fn is_sqlite_lock_error(error: &StorageError) -> bool {
    matches!(
        error,
        StorageError::Sqlx { source, .. }
            if source.as_database_error().is_some_and(|database_error| {
                database_error.message().contains("locked")
                    || database_error
                        .code()
                        .is_some_and(|code| code == "5" || code == "6")
            })
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use uuid::Uuid;

    async fn test_database() -> Result<(tempfile::TempDir, Database), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        })
        .await?;
        Ok((temp_dir, database))
    }

    #[tokio::test]
    async fn set_user_library_access_batch_upserts_multiple_libraries_in_one_statement()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let user_id = Uuid::now_v7().to_string();
        let first_library_id = Uuid::now_v7().to_string();
        let second_library_id = Uuid::now_v7().to_string();
        database
            .insert_initial_user(&user_id, "migration-admin", "Migration Admin", "hash")
            .await?;
        for library_id in [&first_library_id, &second_library_id] {
            sqlx::query("INSERT INTO libraries (id, name, kind) VALUES (?, ?, 'MOVIE')")
                .bind(library_id)
                .bind(library_id)
                .execute(database.pool())
                .await?;
        }

        database.reset_query_count();
        database
            .set_user_library_access_batch(
                &user_id,
                &[
                    (first_library_id.clone(), true),
                    (second_library_id.clone(), false),
                ],
            )
            .await?;
        assert_eq!(database.query_count(), 1);
        assert!(
            database
                .has_user_library_access(&user_id, &first_library_id)
                .await?
        );
        assert!(
            !database
                .has_user_library_access(&user_id, &second_library_id)
                .await?
        );

        sqlx::query("CREATE TABLE library_access_update_counts (count INTEGER NOT NULL)")
            .execute(database.pool())
            .await?;
        sqlx::query("INSERT INTO library_access_update_counts (count) VALUES (0)")
            .execute(database.pool())
            .await?;
        sqlx::query(
            "CREATE TRIGGER count_repeated_library_access_updates
             AFTER UPDATE ON user_library_access
             BEGIN
                 UPDATE library_access_update_counts SET count = count + 1;
             END",
        )
        .execute(database.pool())
        .await?;
        database
            .set_user_library_access_batch(
                &user_id,
                &[
                    (first_library_id.clone(), true),
                    (second_library_id.clone(), false),
                ],
            )
            .await?;
        let repeated_updates: i64 =
            sqlx::query_scalar("SELECT count FROM library_access_update_counts")
                .fetch_one(database.pool())
                .await?;
        assert_eq!(repeated_updates, 0);

        database
            .set_user_library_access_batch(&user_id, &[(second_library_id.clone(), true)])
            .await?;
        assert!(
            database
                .has_user_library_access(&user_id, &second_library_id)
                .await?
        );
        Ok(())
    }

    #[tokio::test]
    async fn list_users_by_normalized_usernames_fetches_only_requested_users()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let first_user_id = Uuid::now_v7().to_string();
        let second_user_id = Uuid::now_v7().to_string();
        database
            .insert_initial_user(&first_user_id, "alice", "Alice", "hash")
            .await?;
        database
            .insert_user(&second_user_id, "bob", "Bob", "hash", false, true)
            .await?;
        sqlx::query("UPDATE users SET is_disabled = 1 WHERE username_normalized = 'bob'")
            .execute(database.pool())
            .await?;

        database.reset_query_count();
        let users = database
            .list_users_by_normalized_usernames(&[String::from("alice"), String::from("bob")])
            .await?;

        assert_eq!(database.query_count(), 1);
        assert_eq!(users.len(), 2);
        let bob = users
            .iter()
            .find(|user| user.id == second_user_id)
            .expect("batched lookup should include Bob");
        assert!(bob.is_disabled);
        Ok(())
    }
}
