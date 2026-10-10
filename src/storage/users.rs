use super::*;

use std::time::{Duration, Instant};

const SQLITE_USER_UPDATE_RETRY_DELAY: Duration = Duration::from_millis(50);
const SQLITE_USER_UPDATE_RETRY_WINDOW: Duration = Duration::from_secs(5);
const MAX_LOGIN_BACKGROUND_CACHE_BYTES: usize = 256 * 1024;

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
        self.list_users_with_disabled(false).await
    }

    pub(crate) async fn list_all_users(&self) -> Result<Vec<StoredUser>, StorageError> {
        self.list_users_with_disabled(true).await
    }

    pub(crate) async fn find_user_display_names(
        &self,
        user_ids: &[String],
    ) -> Result<HashMap<String, String>, StorageError> {
        if user_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let unique_user_ids = user_ids
            .iter()
            .cloned()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let mut display_names = HashMap::with_capacity(unique_user_ids.len());
        for user_ids in unique_user_ids.chunks(500) {
            let placeholders = std::iter::repeat_n("?", user_ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!("SELECT id, display_name FROM users WHERE id IN ({placeholders})");
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for user_id in user_ids {
                statement = statement.bind(user_id);
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
                display_names.insert(row.get("id"), row.get("display_name"));
            }
        }
        Ok(display_names)
    }

    async fn list_users_with_disabled(
        &self,
        include_disabled: bool,
    ) -> Result<Vec<StoredUser>, StorageError> {
        let where_clause = if include_disabled {
            ""
        } else {
            " WHERE is_disabled = 0"
        };
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
             FROM users{where_clause} ORDER BY username_normalized"
        );
        self.query(sqlx::AssertSqlSafe(query))
            .fetch_all(&self.pool)
            .await
            .map(|rows| rows.into_iter().map(stored_user).collect())
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
        self.query(
            "INSERT INTO user_library_order_preferences (user_id, use_admin_library_order)
             SELECT ?, use_admin_library_order
             FROM user_library_order_preferences WHERE user_id = ?
             ON CONFLICT(user_id) DO UPDATE SET
                 use_admin_library_order = excluded.use_admin_library_order,
                 updated_at = unixepoch()",
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
        let actor_username = if let Some(actor_user_id) = event.actor_user_id {
            self.query_scalar::<String>("SELECT username_normalized FROM users WHERE id = ?")
                .bind(actor_user_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?
        } else {
            None
        };
        let event_id = Uuid::now_v7().to_string();
        self.log_store
            .append_audit_event(super::NewAuditLogEvent {
                id: &event_id,
                actor_user_id: event.actor_user_id,
                actor_username: actor_username.as_deref(),
                event_type: event.event_type,
                target_type: event.target_type,
                target_id: event.target_id,
                metadata_json: event.metadata_json,
            })
            .await
            .map_err(|source| StorageError::Io {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn list_activity_events(
        &self,
        limit: i64,
    ) -> Result<Vec<StoredActivityEvent>, StorageError> {
        let category_limit = (limit / 2).max(1);
        let file_events = self
            .log_store
            .list_activity_events(limit)
            .await
            .map_err(|source| StorageError::Io {
                path: self.path.clone(),
                source,
            })?;
        let actor_user_ids = file_events
            .iter()
            .filter_map(|event| event.actor_user_id.clone())
            .collect::<Vec<_>>();
        let actor_usernames = self.list_usernames_by_ids(&actor_user_ids).await?;
        let item_ids = file_events
            .iter()
            .filter_map(|event| event.target_id.clone())
            .collect::<Vec<_>>();
        let item_metadata = self.list_media_item_metadata_by_ids(&item_ids).await?;
        let mut events = file_events
            .into_iter()
            .map(|event| StoredActivityEvent {
                actor_username: event.actor_username.or_else(|| {
                    actor_usernames
                        .get(event.actor_user_id.as_deref()?)
                        .cloned()
                }),
                target_title: event
                    .target_id
                    .as_deref()
                    .and_then(|item_id| item_metadata.get(item_id))
                    .map(|metadata| metadata.title.clone()),
                id: event.id,
                actor_user_id: event.actor_user_id,
                event_type: event.event_type,
                target_type: event.target_type,
                target_id: event.target_id,
                metadata_json: event.metadata_json,
                created_at: event.created_at,
            })
            .collect::<Vec<_>>();
        events.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| right.id.cmp(&left.id))
        });
        let mut seen_ids = HashSet::with_capacity(events.len());
        events.retain(|event| seen_ids.insert(event.id.clone()));
        let mut login_count = 0_i64;
        let mut playback_count = 0_i64;
        events.retain(|event| {
            if event.event_type == "AUTH_LOGIN" {
                login_count += 1;
                login_count <= category_limit
            } else {
                playback_count += 1;
                playback_count <= category_limit
            }
        });
        events.truncate(usize::try_from(limit.max(0)).unwrap_or(usize::MAX));
        Ok(events)
    }

    async fn list_usernames_by_ids(
        &self,
        user_ids: &[String],
    ) -> Result<HashMap<String, String>, StorageError> {
        let user_ids = user_ids
            .iter()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let mut usernames = HashMap::with_capacity(user_ids.len());
        for chunk in user_ids.chunks(500) {
            if chunk.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query =
                format!("SELECT id, username_normalized FROM users WHERE id IN ({placeholders})");
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for user_id in chunk {
                statement = statement.bind(user_id.as_str());
            }
            let rows =
                statement
                    .fetch_all(&self.pool)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
            usernames.extend(rows.into_iter().map(|row| {
                (
                    row.get::<String, _>("id"),
                    row.get::<String, _>("username_normalized"),
                )
            }));
        }
        Ok(usernames)
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
                SELECT 1 FROM libraries WHERE id = ? AND is_enabled = 1
            ) AND (NOT EXISTS(
                SELECT 1 FROM user_library_access
                WHERE user_id = ? AND can_view = 1
            ) OR EXISTS(
                SELECT 1 FROM user_library_access
                WHERE user_id = ? AND library_id = ? AND can_view = 1
            )) THEN 1 ELSE 0 END",
        )
        .bind(library_id)
        .bind(user_id)
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
        // No positive entries means no library restriction; enabled libraries are all visible.
        self.query_scalar(
            "SELECT l.id
             FROM libraries l
             WHERE l.is_enabled = 1
               AND (
                   NOT EXISTS (
                       SELECT 1 FROM user_library_access
                       WHERE user_id = ? AND can_view = 1
                   )
                   OR EXISTS (
                       SELECT 1 FROM user_library_access ula
                       WHERE ula.user_id = ? AND ula.library_id = l.id AND ula.can_view = 1
                   )
               )
             ORDER BY l.name, l.id",
        )
        .bind(user_id)
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn list_selected_library_ids(
        &self,
        user_id: &str,
    ) -> Result<Vec<String>, StorageError> {
        self.query_scalar(
            "SELECT ula.library_id
             FROM user_library_access ula
             JOIN libraries l ON l.id = ula.library_id
             WHERE ula.user_id = ? AND ula.can_view = 1
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

    pub(crate) async fn list_plugin_installation_statuses_by_ids(
        &self,
        plugin_ids: &[String],
    ) -> Result<HashMap<String, bool>, StorageError> {
        let mut statuses = HashMap::with_capacity(plugin_ids.len());
        for chunk in plugin_ids.chunks(500) {
            if chunk.is_empty() {
                continue;
            }
            let placeholders = std::iter::repeat_n("?", chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT plugin_id, is_enabled
                 FROM installed_plugins
                 WHERE plugin_id IN ({placeholders})"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for plugin_id in chunk {
                statement = statement.bind(plugin_id);
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
                statuses.insert(
                    row.get::<String, _>("plugin_id"),
                    row.get::<i64, _>("is_enabled") != 0,
                );
            }
        }
        Ok(statuses)
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

    pub(crate) async fn home_resume_settings(
        &self,
        user_id: &str,
    ) -> Result<(i64, i64), StorageError> {
        let (played_percent, minimum_ticks): (Option<i64>, Option<String>) = self
            .query_as(
                "SELECT
                     (SELECT played_percent FROM user_playback_settings WHERE user_id = ?),
                     (SELECT value FROM server_settings WHERE key = 'resume_min_ticks')",
            )
            .bind(user_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let played_percent = played_percent
            .unwrap_or(DEFAULT_PLAYED_PERCENT)
            .clamp(1, 100);
        let minimum_ticks = minimum_ticks
            .and_then(|value| value.parse().ok())
            .unwrap_or(1_200_000_000)
            .max(0);
        Ok((played_percent, minimum_ticks))
    }

    pub(crate) async fn force_admin_library_order(&self) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT value FROM server_settings
             WHERE key = 'force_admin_library_order'",
        )
        .fetch_optional(&self.pool)
        .await
        .map(|value: Option<String>| value.as_deref() == Some("1"))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn user_uses_admin_library_order(
        &self,
        user_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT use_admin_library_order FROM user_library_order_preferences
             WHERE user_id = ?",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map(|value: Option<i64>| value.unwrap_or(1) != 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn set_user_uses_admin_library_order(
        &self,
        user_id: &str,
        enabled: bool,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO user_library_order_preferences (user_id, use_admin_library_order)
             VALUES (?, ?)
             ON CONFLICT(user_id) DO UPDATE SET
                 use_admin_library_order = excluded.use_admin_library_order,
                 updated_at = unixepoch()",
        )
        .bind(user_id)
        .bind(database_flag(enabled))
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn admin_user_id(&self) -> Result<Option<String>, StorageError> {
        self.query_scalar(
            "SELECT id FROM users
             WHERE is_admin = 1 AND is_disabled = 0
             ORDER BY created_at, id
             LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
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
        const MAX_ROWS_PER_BATCH: usize = 100;
        for (batch_index, batch) in library_ids.chunks(MAX_ROWS_PER_BATCH).enumerate() {
            let values = std::iter::repeat_n("(?, ?, ?)", batch.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "INSERT INTO user_library_order (user_id, library_id, position)
                 VALUES {values}"
            );
            let mut statement = self.query(sqlx::AssertSqlSafe(query));
            for (offset, library_id) in batch.iter().enumerate() {
                let position = batch_index * MAX_ROWS_PER_BATCH + offset;
                statement = statement.bind(user_id).bind(library_id).bind(
                    i64::try_from(position).map_err(|_| {
                        StorageError::Serialization("媒体库排序位置超出范围".to_owned())
                    })?,
                );
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

    pub(crate) async fn set_server_settings(
        &self,
        percent: i64,
        min_ticks: i64,
        media_strategy: &str,
        force_admin_library_order: bool,
        login_background_source: &str,
    ) -> Result<(), StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let settings = [
            ("resume_played_percent", percent.to_string()),
            ("resume_min_ticks", min_ticks.to_string()),
            ("media_strategy", media_strategy.to_owned()),
            (
                "force_admin_library_order",
                if force_admin_library_order { "1" } else { "0" }.to_owned(),
            ),
            (
                "login_background_source",
                login_background_source.to_owned(),
            ),
        ];
        let values = std::iter::repeat_n("(?, ?)", settings.len())
            .collect::<Vec<_>>()
            .join(", ");
        let mut statement = self.query(sqlx::AssertSqlSafe(format!(
            "INSERT INTO server_settings (key, value)
             VALUES {values}
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = unixepoch()"
        )));
        for (key, value) in settings {
            statement = statement.bind(key).bind(value);
        }
        statement
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

    pub(crate) async fn uninstall_plugin(&self, plugin_id: &str) -> Result<(), StorageError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let rows: Vec<(String, String, i64, String)> = self
            .query_as(
                "SELECT library_id, scraper_id, position, role
                 FROM library_scrapers
                 WHERE library_id IN (
                     SELECT DISTINCT library_id FROM library_scrapers WHERE scraper_id = ?
                 )
                 ORDER BY library_id, position, scraper_id",
            )
            .bind(plugin_id)
            .fetch_all(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let mut scrapers_by_library = BTreeMap::<String, Vec<(String, String)>>::new();
        for (library_id, scraper_id, _position, role) in rows {
            if scraper_id != plugin_id {
                scrapers_by_library
                    .entry(library_id)
                    .or_default()
                    .push((scraper_id, role));
            } else {
                scrapers_by_library.entry(library_id).or_default();
            }
        }
        let library_ids = scrapers_by_library.keys().cloned().collect::<Vec<_>>();

        const LIBRARY_ID_QUERY_CHUNK_SIZE: usize = 500;
        for library_id_chunk in library_ids.chunks(LIBRARY_ID_QUERY_CHUNK_SIZE) {
            let placeholders = std::iter::repeat_n("?", library_id_chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let mut query = self.query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM library_scrapers WHERE library_id IN ({placeholders})"
            )));
            for library_id in library_id_chunk {
                query = query.bind(library_id);
            }
            query
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }

        let mut rebuilt_rows = Vec::new();
        let mut primary_by_library = Vec::with_capacity(scrapers_by_library.len());
        for (library_id, scrapers) in scrapers_by_library {
            let primary = scrapers.first().map(|(scraper_id, _)| scraper_id.clone());
            primary_by_library.push((library_id.clone(), primary));
            for (position, (scraper_id, stored_role)) in scrapers.into_iter().enumerate() {
                let role = if position == 0 {
                    "PRIMARY"
                } else if stored_role == "PRIMARY" {
                    "BACKUP"
                } else {
                    stored_role.as_str()
                };
                let position = i64::try_from(position)
                    .map_err(|_| StorageError::Serialization("刮削器位置超出范围".to_owned()))?;
                rebuilt_rows.push((library_id.clone(), scraper_id, position, role.to_owned()));
            }
        }

        const SCRAPER_ROW_INSERT_CHUNK_SIZE: usize = 100;
        for row_chunk in rebuilt_rows.chunks(SCRAPER_ROW_INSERT_CHUNK_SIZE) {
            let values = std::iter::repeat_n("(?, ?, ?, ?)", row_chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let mut query = self.query(sqlx::AssertSqlSafe(format!(
                "INSERT INTO library_scrapers (library_id, scraper_id, position, role)
                 VALUES {values}"
            )));
            for (library_id, scraper_id, position, role) in row_chunk {
                query = query
                    .bind(library_id)
                    .bind(scraper_id)
                    .bind(*position)
                    .bind(role);
            }
            query
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }

        const LIBRARY_UPDATE_CHUNK_SIZE: usize = 100;
        for library_chunk in primary_by_library.chunks(LIBRARY_UPDATE_CHUNK_SIZE) {
            let cases = std::iter::repeat_n("WHEN ? THEN ?", library_chunk.len())
                .collect::<Vec<_>>()
                .join(" ");
            let placeholders = std::iter::repeat_n("?", library_chunk.len())
                .collect::<Vec<_>>()
                .join(", ");
            let mut query = self.query(sqlx::AssertSqlSafe(format!(
                "UPDATE libraries
                 SET scraper_id = CASE id {cases} END, updated_at = unixepoch()
                 WHERE id IN ({placeholders})"
            )));
            for (library_id, primary) in library_chunk {
                query = query.bind(library_id).bind(primary);
            }
            for (library_id, _) in library_chunk {
                query = query.bind(library_id);
            }
            query
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

    pub(crate) async fn login_background_source(&self) -> Result<Option<String>, StorageError> {
        self.query_scalar(
            "SELECT value FROM server_settings
             WHERE key = 'login_background_source'",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn upsert_login_background_plugin_cache(
        &self,
        plugin_id: &str,
        payload_json: &str,
        refreshed_at: i64,
    ) -> Result<(), StorageError> {
        if payload_json.len() > MAX_LOGIN_BACKGROUND_CACHE_BYTES || refreshed_at <= 0 {
            return Err(StorageError::Serialization(
                "login background cache value is outside the allowed bounds".to_owned(),
            ));
        }
        self.query(
            "INSERT INTO login_background_plugin_cache (plugin_id, payload_json, refreshed_at)
             VALUES (?, ?, ?)
             ON CONFLICT(plugin_id) DO UPDATE SET
                 payload_json = excluded.payload_json,
                 refreshed_at = excluded.refreshed_at",
        )
        .bind(plugin_id)
        .bind(payload_json)
        .bind(refreshed_at)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn login_background_plugin_cache(
        &self,
        plugin_id: &str,
    ) -> Result<Option<(String, i64)>, StorageError> {
        self.query_as::<(String, i64)>(
            "SELECT payload_json, refreshed_at
             FROM login_background_plugin_cache
             WHERE plugin_id = ?",
        )
        .bind(plugin_id)
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
    async fn login_background_plugin_cache_upserts_and_reads_latest_payload()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let plugin_id = "org.lux.background-cache-test";
        database.install_plugin(plugin_id).await?;

        database
            .upsert_login_background_plugin_cache(plugin_id, "{\"sourceName\":\"first\"}", 100)
            .await?;
        database
            .upsert_login_background_plugin_cache(plugin_id, "{\"sourceName\":\"latest\"}", 200)
            .await?;

        assert_eq!(
            database.login_background_plugin_cache(plugin_id).await?,
            Some(("{\"sourceName\":\"latest\"}".to_owned(), 200))
        );
        assert_eq!(
            database
                .login_background_plugin_cache("org.lux.missing")
                .await?,
            None
        );
        assert!(
            database
                .upsert_login_background_plugin_cache("org.lux.missing", "{}", 300)
                .await
                .is_err()
        );
        assert!(
            database
                .upsert_login_background_plugin_cache(plugin_id, "{}", 0)
                .await
                .is_err()
        );
        assert!(
            database
                .upsert_login_background_plugin_cache(plugin_id, &"x".repeat(262_145), 300,)
                .await
                .is_err()
        );
        Ok(())
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
    async fn uninstall_plugin_rebuilds_affected_library_scrapers_in_batches()
    -> Result<(), Box<dyn std::error::Error>> {
        const LIBRARY_COUNT: usize = 205;
        let (_temp_dir, database) = test_database().await?;
        database.install_plugin("org.lux.batch-uninstall").await?;

        for index in 0..LIBRARY_COUNT {
            let library_id = format!("batch-uninstall-library-{index:03}");
            sqlx::query(
                "INSERT INTO libraries (id, name, kind, scraper_id)
                 VALUES (?, ?, 'MOVIE', 'org.lux.batch-uninstall')",
            )
            .bind(&library_id)
            .bind(&library_id)
            .execute(database.pool())
            .await?;
            for (position, (scraper_id, role)) in [
                ("org.lux.batch-uninstall", "PRIMARY"),
                ("org.lux.secondary", "SUPPLEMENT"),
                ("org.lux.backup", "BACKUP"),
            ]
            .into_iter()
            .enumerate()
            {
                sqlx::query(
                    "INSERT INTO library_scrapers (library_id, scraper_id, position, role)
                     VALUES (?, ?, ?, ?)",
                )
                .bind(&library_id)
                .bind(scraper_id)
                .bind(position as i64)
                .bind(role)
                .execute(database.pool())
                .await?;
            }
        }

        database.reset_query_count();
        database.uninstall_plugin("org.lux.batch-uninstall").await?;
        assert_eq!(database.query_count(), 12);

        let rows: Vec<(String, i64, String)> = sqlx::query_as(
            "SELECT scraper_id, position, role
             FROM library_scrapers
             WHERE library_id = 'batch-uninstall-library-000'
             ORDER BY position",
        )
        .fetch_all(database.pool())
        .await?;
        assert_eq!(
            rows,
            vec![
                ("org.lux.secondary".to_owned(), 0, "PRIMARY".to_owned()),
                ("org.lux.backup".to_owned(), 1, "BACKUP".to_owned()),
            ]
        );
        let primary: Option<String> = sqlx::query_scalar(
            "SELECT scraper_id FROM libraries
             WHERE id = 'batch-uninstall-library-000'",
        )
        .fetch_one(database.pool())
        .await?;
        assert_eq!(primary.as_deref(), Some("org.lux.secondary"));
        let installed: Option<String> = sqlx::query_scalar(
            "SELECT plugin_id FROM installed_plugins
             WHERE plugin_id = 'org.lux.batch-uninstall'",
        )
        .fetch_optional(database.pool())
        .await?;
        assert_eq!(installed, None);
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
