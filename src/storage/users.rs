use super::*;
use crate::storage::repository::{stored_admin_user_list_item, stored_user};
use crate::storage::{
    ApplyUserPermissionPreset, NewStoredUserPermissions, NewUserAccountPreferences,
    NewUserActivityEvent, StoredEmbyLoginSessionSummary, UserAccountPreferences,
    UserAccountPreferencesCasResult, UserPermissionCopyGroups, UserPermissionCopyResult,
    UserPermissionCopySnapshot, UserPlaybackPolicy,
};

use std::time::{Duration, Instant};
use uuid::Uuid;

#[allow(dead_code)] // LUX-732 wires the bounded activity-retention worker.
const USER_ACTIVITY_RETENTION_MAX_BATCH_SIZE: i64 = 1_000;

const SQLITE_USER_UPDATE_RETRY_DELAY: Duration = Duration::from_millis(50);
const SQLITE_USER_UPDATE_RETRY_WINDOW: Duration = Duration::from_secs(5);
const MAX_LOGIN_BACKGROUND_CACHE_BYTES: usize = 256 * 1024;
const DEFAULT_USER_DELETION_GRACE_DAYS: i64 = 7;
const MAX_USER_DELETION_GRACE_DAYS: i64 = 3_650;
#[allow(dead_code)] // LUX-732 exposes this default through the admin service.
const DEFAULT_USER_ACTIVITY_RETENTION_DAYS: i64 = 90;
#[allow(dead_code)] // LUX-732 exposes this range through the admin service.
const MAX_USER_ACTIVITY_RETENTION_DAYS: i64 = 3_650;
const SECONDS_PER_DAY: i64 = 86_400;
const MAX_USER_CONTENT_RESTRICTION_TAGS: usize = 100;
const MAX_PERMISSION_COPY_USER_IDS: usize = 101;
const PERMISSION_COPY_LIBRARY_QUERY_CHUNK_SIZE: usize = 500;

fn normalize_user_deletion_grace_days(value: Option<String>) -> i64 {
    value
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|days| (1..=MAX_USER_DELETION_GRACE_DAYS).contains(days))
        .unwrap_or(DEFAULT_USER_DELETION_GRACE_DAYS)
}

#[allow(dead_code)] // LUX-732 wires the stored value into the admin service.
fn normalize_user_activity_retention_days(value: Option<String>) -> i64 {
    value
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|days| (1..=MAX_USER_ACTIVITY_RETENTION_DAYS).contains(days))
        .unwrap_or(DEFAULT_USER_ACTIVITY_RETENTION_DAYS)
}

fn normalize_user_content_restriction_tags(tags: &[String]) -> Result<Vec<String>, StorageError> {
    if tags.len() > MAX_USER_CONTENT_RESTRICTION_TAGS {
        return Err(StorageError::Conflict(
            "too many user content restriction tags".to_owned(),
        ));
    }
    let mut seen = HashSet::with_capacity(tags.len());
    let mut normalized = Vec::with_capacity(tags.len());
    for tag in tags {
        let tag = tag.trim();
        if tag.chars().count() > 128 {
            return Err(StorageError::Conflict(
                "user content restriction tag length is invalid".to_owned(),
            ));
        }
        let tag = tag.to_lowercase();
        let tag_length = tag.chars().count();
        if !(1..=128).contains(&tag_length) {
            return Err(StorageError::Conflict(
                "user content restriction tag length is invalid".to_owned(),
            ));
        }
        if !seen.insert(tag.clone()) {
            return Err(StorageError::Conflict(
                "duplicate user content restriction tag".to_owned(),
            ));
        }
        normalized.push(tag);
    }
    normalized.sort_unstable();
    Ok(normalized)
}

impl Database {
    // LUX-645 wires this storage primitive into the application preset service.
    #[allow(dead_code)]
    pub(crate) async fn apply_user_permission_preset(
        &self,
        user_id: &str,
        preset: ApplyUserPermissionPreset<'_>,
    ) -> Result<bool, StorageError> {
        let permissions = preset.permissions;
        let valid_library_access = match permissions.library_access_mode {
            "ALL" | "NONE" => permissions.library_ids.is_empty(),
            "SELECTED" => !permissions.library_ids.is_empty(),
            _ => false,
        };
        if preset.preset_id.trim().is_empty()
            || preset.preset_name.trim().is_empty()
            || preset.preset_version <= 0
            || (preset.can_manage_server && !preset.is_admin)
            || permissions.max_simultaneous_playbacks < 0
            || !valid_library_access
            || permissions.library_ids.iter().collect::<HashSet<_>>().len()
                != permissions.library_ids.len()
        {
            return Err(StorageError::Conflict(
                "user permission preset snapshot is invalid".to_owned(),
            ));
        }

        let mut transaction = self.begin_metadata_write_transaction().await?;
        if self.backend() == crate::config::DatabaseBackend::Postgres && !preset.can_manage_server {
            // Lock all current managers in stable order so concurrent demotions cannot each see
            // the other's still-active row and remove the last manager together.
            let _active_manager_ids: Vec<String> = self
                .query_scalar(
                    "SELECT id FROM users
                     WHERE can_manage_server = 1 AND is_disabled = 0 AND deleted_at IS NULL
                     ORDER BY id FOR UPDATE",
                )
                .fetch_all(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }
        let row_lock = if self.backend() == crate::config::DatabaseBackend::Postgres {
            " FOR UPDATE"
        } else {
            ""
        };
        let user_query = format!(
            "SELECT is_disabled, can_manage_server, deleted_at FROM users WHERE id = ?{row_lock}"
        );
        let Some(current) = self
            .query(sqlx::AssertSqlSafe(user_query))
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
        if current.get::<Option<i64>, _>("deleted_at").is_some() {
            return Ok(false);
        }

        let current_disabled = current.get::<i64, _>("is_disabled") != 0;
        let current_can_manage_server = current.get::<i64, _>("can_manage_server") != 0;
        if current_can_manage_server && !current_disabled && !preset.can_manage_server {
            let remaining: i64 = self
                .query_scalar(
                    "SELECT COUNT(*) FROM users
                     WHERE can_manage_server = 1 AND is_disabled = 0
                       AND deleted_at IS NULL AND id != ?",
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

        if permissions.library_access_mode == "SELECTED" {
            for library_id in permissions.library_ids {
                let enabled_regular_library: i64 = self
                    .query_scalar(
                        "SELECT COUNT(*) FROM libraries
                         WHERE id = ? AND is_enabled = 1 AND system_kind IS NULL
                           AND kind IN ('MOVIE', 'SERIES', 'MIXED', 'HOMEVIDEOS')",
                    )
                    .bind(library_id)
                    .fetch_one(&mut *transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                if enabled_regular_library != 1 {
                    return Err(StorageError::Conflict(
                        "user permission preset references an unavailable library".to_owned(),
                    ));
                }
            }
        }

        let updated = self
            .query(
                "UPDATE users
                 SET is_admin = ?, can_manage_server = ?, can_download = ?, can_play = ?,
                     max_simultaneous_playbacks = ?, allow_remux = ?, allow_audio_transcode = ?,
                     allow_video_transcode = ?, library_access_mode = ?, updated_at = unixepoch()
                 WHERE id = ? AND deleted_at IS NULL",
            )
            .bind(database_flag(preset.is_admin))
            .bind(database_flag(preset.can_manage_server))
            .bind(database_flag(permissions.can_download))
            .bind(database_flag(permissions.can_play))
            .bind(permissions.max_simultaneous_playbacks)
            .bind(database_flag(permissions.allow_remux))
            .bind(database_flag(permissions.allow_audio_transcode))
            .bind(database_flag(permissions.allow_video_transcode))
            .bind(permissions.library_access_mode)
            .bind(user_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if updated.rows_affected() != 1 {
            return Ok(false);
        }

        self.query("DELETE FROM user_library_access WHERE user_id = ?")
            .bind(user_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if permissions.library_access_mode == "SELECTED" {
            for library_id in permissions.library_ids {
                self.query(
                    "INSERT INTO user_library_access (user_id, library_id, can_view)
                     VALUES (?, ?, 1)",
                )
                .bind(user_id)
                .bind(library_id)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            }
        }

        self.query(
            "INSERT INTO user_permission_preset_sources
             (user_id, preset_id, preset_name, preset_version, applied_at)
             VALUES (?, ?, ?, ?, unixepoch())
             ON CONFLICT(user_id) DO UPDATE SET
                 preset_id = excluded.preset_id,
                 preset_name = excluded.preset_name,
                 preset_version = excluded.preset_version,
                 applied_at = excluded.applied_at",
        )
        .bind(user_id)
        .bind(preset.preset_id)
        .bind(preset.preset_name)
        .bind(preset.preset_version)
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

    // LUX-648 wires these reads into administrator profile and list queries.
    #[allow(dead_code)]
    pub(crate) async fn user_permission_preset_source(
        &self,
        user_id: &str,
    ) -> Result<Option<StoredUserPermissionPresetSource>, StorageError> {
        self.query(
            "SELECT preset_id, preset_name, preset_version, applied_at
             FROM user_permission_preset_sources WHERE user_id = ?",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_user_permission_preset_source))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    #[allow(dead_code)]
    pub(crate) async fn user_permission_preset_sources(
        &self,
        user_ids: &[String],
    ) -> Result<HashMap<String, StoredUserPermissionPresetSource>, StorageError> {
        if user_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let mut sources = HashMap::with_capacity(user_ids.len());
        for user_ids in user_ids.chunks(500) {
            let placeholders = std::iter::repeat_n("?", user_ids.len())
                .collect::<Vec<_>>()
                .join(", ");
            let query = format!(
                "SELECT user_id, preset_id, preset_name, preset_version, applied_at
                 FROM user_permission_preset_sources WHERE user_id IN ({placeholders})"
            );
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
                let user_id = row.get("user_id");
                sources.insert(user_id, stored_user_permission_preset_source(row));
            }
        }
        Ok(sources)
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
                is_admin, can_manage_server, max_simultaneous_playbacks
            )
            SELECT ?, ?, ?, ?, 1, 1, 2
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
        self.insert_user_with_email(NewStoredUser {
            id,
            username_normalized,
            display_name,
            password_hash,
            is_admin,
            has_password,
            email: None,
        })
        .await
    }

    pub(crate) async fn insert_user_with_email(
        &self,
        user: NewStoredUser<'_>,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO users (
                id, username_normalized, display_name, password_hash,
                is_admin, can_manage_server, has_password, email,
                max_simultaneous_playbacks
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 2)",
        )
        .bind(user.id)
        .bind(user.username_normalized)
        .bind(user.display_name)
        .bind(user.password_hash)
        .bind(database_flag(user.is_admin))
        .bind(database_flag(user.is_admin))
        .bind(database_flag(user.has_password))
        .bind(user.email)
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn insert_user_with_email_and_permissions(
        &self,
        user: NewStoredUser<'_>,
        permissions: NewStoredUserPermissions<'_>,
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
            "INSERT INTO users (
                id, username_normalized, display_name, password_hash,
                is_admin, can_manage_server, has_password, email,
                can_download, can_play, max_simultaneous_playbacks,
                allow_remux, allow_audio_transcode, allow_video_transcode,
                library_access_mode
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(user.id)
        .bind(user.username_normalized)
        .bind(user.display_name)
        .bind(user.password_hash)
        .bind(database_flag(user.is_admin))
        .bind(database_flag(user.is_admin))
        .bind(database_flag(user.has_password))
        .bind(user.email)
        .bind(database_flag(permissions.can_download))
        .bind(database_flag(permissions.can_play))
        .bind(permissions.max_simultaneous_playbacks)
        .bind(database_flag(permissions.allow_remux))
        .bind(database_flag(permissions.allow_audio_transcode))
        .bind(database_flag(permissions.allow_video_transcode))
        .bind(permissions.library_access_mode)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;

        if permissions.library_access_mode == "SELECTED" {
            for library_id in permissions.library_ids {
                self.query(
                    "INSERT INTO user_library_access (user_id, library_id, can_view)
                     VALUES (?, ?, 1)",
                )
                .bind(user.id)
                .bind(library_id)
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
            })
    }

    pub(crate) async fn get_user_playback_policy(
        &self,
        user_id: &str,
    ) -> Result<Option<UserPlaybackPolicy>, StorageError> {
        self.query(
            "SELECT can_play, max_simultaneous_playbacks, allow_remux,
                    allow_audio_transcode, allow_video_transcode
             FROM users WHERE id = ?",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
        .map(|row| {
            row.map(|row| UserPlaybackPolicy {
                can_play: row.get::<i64, _>("can_play") != 0,
                max_simultaneous_playbacks: row.get("max_simultaneous_playbacks"),
                allow_remux: row.get::<i64, _>("allow_remux") != 0,
                allow_audio_transcode: row.get::<i64, _>("allow_audio_transcode") != 0,
                allow_video_transcode: row.get::<i64, _>("allow_video_transcode") != 0,
            })
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn update_user_playback_policy(
        &self,
        user_id: &str,
        policy: UserPlaybackPolicy,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE users
             SET can_play = ?, max_simultaneous_playbacks = ?, allow_remux = ?,
                 allow_audio_transcode = ?, allow_video_transcode = ?, updated_at = unixepoch()
             WHERE id = ? AND deleted_at IS NULL",
        )
        .bind(database_flag(policy.can_play))
        .bind(policy.max_simultaneous_playbacks)
        .bind(database_flag(policy.allow_remux))
        .bind(database_flag(policy.allow_audio_transcode))
        .bind(database_flag(policy.allow_video_transcode))
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() == 1)
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
                    can_remote_access, can_download, email, email_verified_at,
                    deleted_at, deletion_purge_at, last_login_at,
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
        .map(|row| row.map(stored_user))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn find_users_by_display_name_exact(
        &self,
        display_name: &str,
    ) -> Result<Vec<StoredUser>, StorageError> {
        self.query(
            "SELECT id, username_normalized, display_name, password_hash,
                    has_password,
                    is_disabled, is_admin, can_manage_server,
                    can_remote_access, can_download, email, email_verified_at,
                    deleted_at, deletion_purge_at, last_login_at,
                    COALESCE(
                        (SELECT MAX(COALESCE(at.last_seen_at, at.created_at))
                         FROM access_tokens at WHERE at.user_id = users.id),
                        last_login_at
                    ) AS last_activity_at
             FROM users WHERE display_name = ? ORDER BY id LIMIT 2",
        )
        .bind(display_name)
        .fetch_all(&self.pool)
        .await
        .map(|rows| rows.into_iter().map(stored_user).collect())
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
                        can_remote_access, can_download, email, email_verified_at,
                        deleted_at, deletion_purge_at, last_login_at,
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

    pub(crate) async fn query_admin_users(
        &self,
        status: AdminUserStatusFilter,
        search: Option<&str>,
        sort_by: AdminUserSortField,
        descending: bool,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<StoredAdminUserListItem>, i64), StorageError> {
        let status_clause = match status {
            AdminUserStatusFilter::All => "deleted_at IS NULL",
            AdminUserStatusFilter::Active => "deleted_at IS NULL AND is_disabled = 0",
            AdminUserStatusFilter::Disabled => "deleted_at IS NULL AND is_disabled = 1",
            AdminUserStatusFilter::Deleted => "deleted_at IS NOT NULL",
        };
        let search_pattern = search.map(|search| {
            let escaped = search
                .to_lowercase()
                .replace('!', "!!")
                .replace('%', "!%")
                .replace('_', "!_");
            format!("%{escaped}%")
        });
        let where_clause = if search_pattern.is_some() {
            format!(
                "{status_clause} AND (
                    LOWER(username_normalized) LIKE ? ESCAPE '!'
                    OR LOWER(display_name) LIKE ? ESCAPE '!'
                    OR LOWER(COALESCE(email, '')) LIKE ? ESCAPE '!'
                )"
            )
        } else {
            status_clause.to_owned()
        };
        let count_query = format!("SELECT COUNT(*) FROM users WHERE {where_clause}");
        let mut count_statement = self.query_scalar::<i64>(sqlx::AssertSqlSafe(count_query));
        if let Some(search_pattern) = search_pattern.as_deref() {
            count_statement = count_statement
                .bind(search_pattern)
                .bind(search_pattern)
                .bind(search_pattern);
        }
        let total_count = count_statement
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;

        let max_function = match self.backend() {
            crate::config::DatabaseBackend::Sqlite => "MAX",
            crate::config::DatabaseBackend::Postgres => "GREATEST",
        };
        let activity_expression = format!(
            "NULLIF({max_function}(
                COALESCE(users.last_login_at, 0),
                COALESCE((SELECT MAX(COALESCE(at.last_seen_at, at.created_at))
                          FROM access_tokens at WHERE at.user_id = users.id), 0),
                COALESCE((SELECT MAX(COALESCE(ws.last_seen_at, ws.created_at))
                          FROM web_sessions ws WHERE ws.user_id = users.id), 0),
                COALESCE((SELECT MAX(ps.last_event_at)
                          FROM playback_sessions ps WHERE ps.user_id = users.id), 0),
                COALESCE((SELECT MAX(wps.last_heartbeat_at)
                          FROM web_playback_sessions wps WHERE wps.user_id = users.id), 0)
            ), 0)"
        );
        let sort_column = match sort_by {
            AdminUserSortField::Created => "created_at",
            AdminUserSortField::LastLogin => "last_login_at",
            AdminUserSortField::LastActivity => "last_activity_at",
        };
        let direction = if descending { "DESC" } else { "ASC" };
        let list_query = format!(
            "SELECT id, username_normalized, display_name, has_password,
                    is_disabled, is_admin, can_manage_server, can_remote_access,
                    can_download, email, email_verified_at, deleted_at,
                    deletion_purge_at, last_login_at,
                    {activity_expression} AS last_activity_at,
                    created_at,
                    (SELECT COUNT(*) FROM (
                        SELECT ps.play_session_id
                        FROM playback_sessions ps
                        WHERE ps.user_id = users.id
                          AND ps.state IN ('PLAYING', 'PAUSED')
                          AND ps.last_event_at > unixepoch() - {PLAYBACK_SESSION_STALE_AFTER_SECONDS}
                        UNION
                        SELECT wps.play_session_id
                        FROM web_playback_sessions wps
                        WHERE wps.user_id = users.id AND wps.state = 'ACTIVE'
                          AND wps.last_heartbeat_at > unixepoch() - {PLAYBACK_SESSION_STALE_AFTER_SECONDS}
                          AND wps.expires_at >= unixepoch()
                    ) active_playback_sessions) AS current_playback_count
             FROM users
             WHERE {where_clause}
             ORDER BY {sort_column} {direction} NULLS LAST,
                      username_normalized ASC, id ASC
             LIMIT ? OFFSET ?"
        );
        let mut list_statement = self.query(sqlx::AssertSqlSafe(list_query));
        if let Some(search_pattern) = search_pattern.as_deref() {
            list_statement = list_statement
                .bind(search_pattern)
                .bind(search_pattern)
                .bind(search_pattern);
        }
        let rows = list_statement
            .bind(limit.clamp(1, 100))
            .bind(offset.max(0))
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let users = rows.into_iter().map(stored_admin_user_list_item).collect();
        Ok((users, total_count))
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
                    can_remote_access, can_download, email, email_verified_at,
                    deleted_at, deletion_purge_at, last_login_at,
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
                    can_remote_access, can_download, email, email_verified_at,
                    deleted_at, deletion_purge_at, last_login_at,
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
                    can_remote_access, can_download, email, email_verified_at,
                    deleted_at, deletion_purge_at, last_login_at,
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

    pub(crate) async fn update_user_profile(
        &self,
        user_id: &str,
        display_name: Option<&str>,
        email: Option<Option<&str>>,
    ) -> Result<Option<StoredUser>, StorageError> {
        let email_was_provided = email.is_some();
        let email = email.flatten();
        self.query(
            "UPDATE users
             SET display_name = COALESCE(?, display_name),
                 email = CASE WHEN ? != 0 THEN ? ELSE email END,
                 email_verified_at = CASE
                    WHEN ? != 0 AND LOWER(COALESCE(email, '')) != LOWER(COALESCE(?, ''))
                    THEN NULL ELSE email_verified_at END,
                 updated_at = unixepoch()
             WHERE id = ?",
        )
        .bind(display_name)
        .bind(database_flag(email_was_provided))
        .bind(email)
        .bind(database_flag(email_was_provided))
        .bind(email)
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.find_user_by_id(user_id).await
    }

    pub(crate) async fn update_user(
        &self,
        user_id: &str,
        update: UpdateUser<'_>,
    ) -> Result<Option<StoredUser>, StorageError> {
        Ok(self
            .update_user_with_hls_session_ids(user_id, update)
            .await?
            .map(|(user, _)| user))
    }

    pub(crate) async fn update_user_with_hls_session_ids(
        &self,
        user_id: &str,
        update: UpdateUser<'_>,
    ) -> Result<Option<(StoredUser, Vec<String>)>, StorageError> {
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
    ) -> Result<Option<(StoredUser, Vec<String>)>, StorageError> {
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let Some(current) = self
            .query(
                "SELECT is_disabled, can_manage_server, deleted_at
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
        if current.get::<Option<i64>, _>("deleted_at").is_some() {
            return Ok(None);
        }
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
                 WHERE can_manage_server = 1 AND is_disabled = 0
                   AND deleted_at IS NULL AND id != ?",
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
        let hls_session_ids = if !current_disabled && next_disabled {
            self.query_scalar(
                "SELECT id FROM web_playback_sessions
                 WHERE user_id = ? AND state = 'ACTIVE' AND plan = 'SERVER_HLS'
                 ORDER BY created_at ASC, id ASC",
            )
            .bind(user_id)
            .fetch_all(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
        } else {
            Vec::new()
        };
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
        if !current_disabled && next_disabled {
            let now: i64 = self
                .query_scalar("SELECT unixepoch()")
                .fetch_one(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            self.query(
                "UPDATE web_sessions
                 SET revoked_at = ?, updated_at = ?
                 WHERE user_id = ? AND revoked_at IS NULL",
            )
            .bind(now)
            .bind(now)
            .bind(user_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
            self.query(
                "UPDATE access_tokens
                 SET revoked_at = ?, updated_at = ?
                 WHERE user_id = ? AND revoked_at IS NULL",
            )
            .bind(now)
            .bind(now)
            .bind(user_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
            self.query(
                "UPDATE playback_sessions
                 SET state = 'STOPPED', is_paused = 0, last_event_at = ?
                 WHERE user_id = ? AND state IN ('PLAYING', 'PAUSED')",
            )
            .bind(now)
            .bind(user_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
            self.query(
                "UPDATE web_playback_sessions
                 SET state = 'STOPPED', updated_at = ?
                 WHERE user_id = ? AND state = 'ACTIVE'",
            )
            .bind(now)
            .bind(user_id)
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
        Ok(self
            .find_user_by_id(user_id)
            .await?
            .map(|user| (user, hls_session_ids)))
    }

    pub(crate) async fn delete_user(&self, user_id: &str) -> Result<bool, StorageError> {
        let mut transaction = self.begin_metadata_write_transaction().await?;
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

    pub(crate) async fn soft_delete_user(
        &self,
        user_id: &str,
    ) -> Result<Option<Vec<String>>, StorageError> {
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let Some(current) = self
            .query(
                "SELECT is_disabled, can_manage_server, deleted_at
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
        if current.get::<Option<i64>, _>("deleted_at").is_some() {
            return Ok(None);
        }
        let was_disabled = current.get::<i64, _>("is_disabled") != 0;
        let can_manage_server = current.get::<i64, _>("can_manage_server") != 0;
        if can_manage_server && !was_disabled {
            let remaining: i64 = self
                .query_scalar(
                    "SELECT COUNT(*) FROM users
                     WHERE can_manage_server = 1 AND is_disabled = 0
                       AND deleted_at IS NULL AND id != ?",
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
        let now: i64 = self
            .query_scalar("SELECT unixepoch()")
            .fetch_one(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let configured_grace_days: Option<String> = self
            .query_scalar(
                "SELECT value FROM server_settings
                 WHERE key = 'user_deletion_grace_days'",
            )
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let grace_days = normalize_user_deletion_grace_days(configured_grace_days);
        let hls_session_rows = self
            .query(
                "SELECT id FROM web_playback_sessions
                 WHERE user_id = ? AND state = 'ACTIVE' AND plan = 'SERVER_HLS'",
            )
            .bind(user_id)
            .fetch_all(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let hls_session_ids = hls_session_rows
            .into_iter()
            .map(|row| row.get::<String, _>("id"))
            .collect::<Vec<_>>();

        self.query(
            "UPDATE users
             SET is_disabled_before_delete = ?, is_disabled = 1,
                 deleted_at = ?, deletion_purge_at = ?, updated_at = ?
             WHERE id = ? AND deleted_at IS NULL",
        )
        .bind(database_flag(was_disabled))
        .bind(now)
        .bind(now.saturating_add(grace_days.saturating_mul(SECONDS_PER_DAY)))
        .bind(now)
        .bind(user_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query(
            "UPDATE web_sessions
             SET revoked_at = ?, updated_at = ?
             WHERE user_id = ? AND revoked_at IS NULL",
        )
        .bind(now)
        .bind(now)
        .bind(user_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query(
            "UPDATE access_tokens
             SET revoked_at = ?, updated_at = ?
             WHERE user_id = ? AND revoked_at IS NULL",
        )
        .bind(now)
        .bind(now)
        .bind(user_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query(
            "UPDATE playback_sessions
             SET state = 'STOPPED', is_paused = 0, last_event_at = ?
             WHERE user_id = ? AND state IN ('PLAYING', 'PAUSED')",
        )
        .bind(now)
        .bind(user_id)
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query(
            "UPDATE web_playback_sessions
             SET state = 'STOPPED', updated_at = ?
             WHERE user_id = ? AND state = 'ACTIVE'",
        )
        .bind(now)
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
        Ok(Some(hls_session_ids))
    }

    pub(crate) async fn restore_deleted_user(&self, user_id: &str) -> Result<bool, StorageError> {
        let result = self
            .query(
                "UPDATE users
                 SET is_disabled = is_disabled_before_delete,
                     is_disabled_before_delete = 0,
                     deleted_at = NULL, deletion_purge_at = NULL,
                     updated_at = unixepoch()
                 WHERE id = ? AND deleted_at IS NOT NULL",
            )
            .bind(user_id)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn purge_deleted_user(&self, user_id: &str) -> Result<bool, StorageError> {
        let result = self
            .query("DELETE FROM users WHERE id = ? AND deleted_at IS NOT NULL")
            .bind(user_id)
            .execute(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn purge_expired_deleted_users(
        &self,
        now: i64,
        batch_limit: i64,
    ) -> Result<Vec<String>, StorageError> {
        let rows = self
            .query(
                "DELETE FROM users
                 WHERE id IN (
                     SELECT id FROM users
                     WHERE deleted_at IS NOT NULL AND deletion_purge_at <= ?
                     ORDER BY deletion_purge_at ASC, id ASC
                     LIMIT ?
                 )
                   AND deleted_at IS NOT NULL AND deletion_purge_at <= ?
                 RETURNING id",
            )
            .bind(now)
            .bind(batch_limit.clamp(1, 100))
            .bind(now)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(rows
            .into_iter()
            .map(|row| row.get::<String, _>("id"))
            .collect())
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
            "UPDATE access_tokens
                SET last_seen_at = unixepoch(),
                    updated_at = unixepoch(),
                    expires_at = unixepoch() + ?
              WHERE token_hash = ? AND revoked_at IS NULL",
        )
        .bind(EMBY_ACCESS_TOKEN_TTL_SECONDS)
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

    // LUX-721 wires this persistence method into the activity event recorder.
    #[allow(dead_code)]
    pub(crate) async fn insert_user_activity_event(
        &self,
        event: NewUserActivityEvent<'_>,
    ) -> Result<String, StorageError> {
        let metadata = serde_json::from_str::<serde_json::Value>(event.metadata_json)
            .map(crate::observability::logs::redact_sensitive_log_values)
            .unwrap_or_else(|_| serde_json::json!({ "invalid": true }));
        let metadata_json = serde_json::to_string(&metadata)
            .map_err(|error| StorageError::Serialization(error.to_string()))?;
        let event_id = Uuid::now_v7().to_string();

        self.query(
            "INSERT INTO user_activity_events (
                 id, user_id, actor_user_id, event_type, target_type, target_id, metadata_json
             ) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&event_id)
        .bind(event.user_id)
        .bind(event.actor_user_id)
        .bind(event.event_type)
        .bind(event.target_type)
        .bind(event.target_id)
        .bind(metadata_json)
        .execute(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;

        Ok(event_id)
    }

    // LUX-741 wires this monotonic inventory upsert into authentication and activity recording.
    #[allow(dead_code)]
    pub(crate) async fn upsert_user_device(
        &self,
        observation: NewUserDeviceObservation<'_>,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO user_devices (
                 user_id, device_id, device_name, device_type, client_name,
                 client_version, first_seen_at, last_seen_at, last_remote_ip
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(user_id, device_id) DO UPDATE SET
                 first_seen_at = CASE
                     WHEN excluded.first_seen_at < user_devices.first_seen_at
                     THEN excluded.first_seen_at ELSE user_devices.first_seen_at END,
                 device_name = CASE
                     WHEN excluded.last_seen_at >= user_devices.last_seen_at
                     THEN COALESCE(NULLIF(excluded.device_name, ''), user_devices.device_name)
                     ELSE user_devices.device_name END,
                 device_type = CASE
                     WHEN excluded.last_seen_at >= user_devices.last_seen_at
                     THEN COALESCE(NULLIF(excluded.device_type, ''), user_devices.device_type)
                     ELSE user_devices.device_type END,
                 client_name = CASE
                     WHEN excluded.last_seen_at >= user_devices.last_seen_at
                     THEN excluded.client_name ELSE user_devices.client_name END,
                 client_version = CASE
                     WHEN excluded.last_seen_at >= user_devices.last_seen_at
                     THEN COALESCE(NULLIF(excluded.client_version, ''), user_devices.client_version)
                     ELSE user_devices.client_version END,
                 last_seen_at = CASE
                     WHEN excluded.last_seen_at > user_devices.last_seen_at
                     THEN excluded.last_seen_at ELSE user_devices.last_seen_at END,
                 last_remote_ip = CASE
                     WHEN excluded.last_seen_at >= user_devices.last_seen_at
                     THEN COALESCE(NULLIF(excluded.last_remote_ip, ''), user_devices.last_remote_ip)
                     ELSE user_devices.last_remote_ip END",
        )
        .bind(observation.user_id)
        .bind(observation.device_id)
        .bind(observation.device_name)
        .bind(observation.device_type)
        .bind(observation.client_name)
        .bind(observation.client_version)
        .bind(observation.seen_at)
        .bind(observation.seen_at)
        .bind(observation.last_remote_ip)
        .execute(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        Ok(())
    }

    // LUX-745 consumes this bounded inventory page in the administrator user API.
    #[allow(dead_code)]
    pub(crate) async fn list_user_devices_page(
        &self,
        query: UserDevicePageQuery<'_>,
    ) -> Result<(Vec<StoredUserDevice>, i64), StorageError> {
        let total_count = self
            .query("SELECT COUNT(*) AS total_count FROM user_devices WHERE user_id = ?")
            .bind(query.user_id)
            .fetch_one(&self.pool)
            .await
            .map(|row| row.get::<i64, _>("total_count"))
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;

        let rows = self
            .query(
                "SELECT ud.device_id, ud.device_name, ud.device_type, ud.client_name,
                        ud.client_version, ud.first_seen_at, ud.last_seen_at,
                        ud.last_remote_ip,
                        CASE WHEN EXISTS (
                            SELECT 1 FROM web_sessions ws
                            WHERE ws.user_id = ud.user_id
                              AND ws.device_id = ud.device_id
                              AND ws.revoked_at IS NULL
                              AND ws.expires_at > ?
                        ) OR EXISTS (
                            SELECT 1 FROM access_tokens at
                            WHERE at.user_id = ud.user_id
                              AND at.device_id = ud.device_id
                              AND at.revoked_at IS NULL
                        ) THEN CAST(1 AS BIGINT) ELSE CAST(0 AS BIGINT) END AS has_active_login
                 FROM user_devices ud
                 WHERE ud.user_id = ?
                 ORDER BY ud.last_seen_at DESC, ud.device_id ASC
                 LIMIT ? OFFSET ?",
            )
            .bind(current_unix_timestamp())
            .bind(query.user_id)
            .bind(query.limit.clamp(1, 100))
            .bind(query.offset.max(0))
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let devices = rows
            .into_iter()
            .map(|row| StoredUserDevice {
                device_id: row.get("device_id"),
                has_active_login: row.get::<i64, _>("has_active_login") != 0,
                device_name: row.get("device_name"),
                device_type: row.get("device_type"),
                client_name: row.get("client_name"),
                client_version: row.get("client_version"),
                first_seen_at: row.get("first_seen_at"),
                last_seen_at: row.get("last_seen_at"),
                last_remote_ip: row.get("last_remote_ip"),
            })
            .collect();

        Ok((devices, total_count))
    }

    // LUX-755 supplies scope-safe CRUD for future administrator blacklist APIs.
    #[allow(dead_code)]
    pub(crate) async fn insert_user_access_blacklist_rule(
        &self,
        rule: NewUserAccessBlacklistRule<'_>,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO user_access_blacklist_rules (
                 id, user_id, rule_type, match_value, is_enabled, created_by,
                 created_at, updated_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(rule.id)
        .bind(rule.user_id)
        .bind(rule.rule_type)
        .bind(rule.match_value)
        .bind(database_flag(rule.is_enabled))
        .bind(rule.created_by)
        .bind(rule.created_at)
        .bind(rule.created_at)
        .execute(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) async fn list_user_access_blacklist_rules_page(
        &self,
        query: UserAccessBlacklistRulePageQuery<'_>,
    ) -> Result<(Vec<StoredUserAccessBlacklistRule>, i64), StorageError> {
        let (where_clause, user_id) = match query.scope {
            UserAccessBlacklistRuleScope::Global => ("user_id IS NULL", None),
            UserAccessBlacklistRuleScope::User(user_id) => ("user_id = ?", Some(user_id)),
        };
        let count_sql = format!(
            "SELECT COUNT(*) AS total_count FROM user_access_blacklist_rules WHERE {where_clause}"
        );
        let mut count_query = self.query(sqlx::AssertSqlSafe(count_sql));
        if let Some(user_id) = user_id {
            count_query = count_query.bind(user_id);
        }
        let total_count = count_query
            .fetch_one(&self.pool)
            .await
            .map(|row| row.get::<i64, _>("total_count"))
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;

        let page_sql = format!(
            "SELECT id, user_id, rule_type, match_value, is_enabled, created_by,
                    created_at, updated_at
             FROM user_access_blacklist_rules WHERE {where_clause}
             ORDER BY created_at DESC, id ASC
             LIMIT ? OFFSET ?"
        );
        let mut page_query = self.query(sqlx::AssertSqlSafe(page_sql));
        if let Some(user_id) = user_id {
            page_query = page_query.bind(user_id);
        }
        let rows = page_query
            .bind(query.limit.clamp(1, 100))
            .bind(query.offset.max(0))
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let rules = rows
            .into_iter()
            .map(|row| StoredUserAccessBlacklistRule {
                id: row.get("id"),
                user_id: row.get("user_id"),
                rule_type: row.get("rule_type"),
                match_value: row.get("match_value"),
                is_enabled: row.get::<i64, _>("is_enabled") != 0,
                created_by: row.get("created_by"),
                created_at: row.get("created_at"),
                updated_at: row.get("updated_at"),
            })
            .collect();
        Ok((rules, total_count))
    }

    pub(crate) async fn find_user_access_blacklist_rule(
        &self,
        scope: UserAccessBlacklistRuleScope<'_>,
        rule_id: &str,
    ) -> Result<Option<StoredUserAccessBlacklistRule>, StorageError> {
        let (where_clause, user_id) = match scope {
            UserAccessBlacklistRuleScope::Global => ("user_id IS NULL", None),
            UserAccessBlacklistRuleScope::User(user_id) => ("user_id = ?", Some(user_id)),
        };
        let sql = format!(
            "SELECT id, user_id, rule_type, match_value, is_enabled, created_by,
                    created_at, updated_at
             FROM user_access_blacklist_rules WHERE id = ? AND {where_clause}"
        );
        let mut query = self.query(sqlx::AssertSqlSafe(sql)).bind(rule_id);
        if let Some(user_id) = user_id {
            query = query.bind(user_id);
        }
        query
            .fetch_optional(&self.pool)
            .await
            .map(|row| {
                row.map(|row| StoredUserAccessBlacklistRule {
                    id: row.get("id"),
                    user_id: row.get("user_id"),
                    rule_type: row.get("rule_type"),
                    match_value: row.get("match_value"),
                    is_enabled: row.get::<i64, _>("is_enabled") != 0,
                    created_by: row.get("created_by"),
                    created_at: row.get("created_at"),
                    updated_at: row.get("updated_at"),
                })
            })
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    // LUX-758 checks active device/client rules before auth enforcement is wired.
    #[allow(dead_code)]
    pub(crate) async fn user_access_blacklist_matches(
        &self,
        query: UserAccessBlacklistRuleMatchQuery<'_>,
    ) -> Result<bool, StorageError> {
        if query.device_id.is_none() && query.client_name.is_none() {
            return Ok(false);
        }
        self.query(
            // A bare EXISTS returns BOOLEAN on PostgreSQL, but INTEGER on SQLite.
            "SELECT CAST(CASE WHEN EXISTS (
                 SELECT 1
                 FROM user_access_blacklist_rules
                 WHERE is_enabled = 1
                   AND (user_id IS NULL OR user_id = ?)
                   AND (
                       (rule_type = 'DEVICE' AND ? IS NOT NULL AND match_value = ?)
                       OR (rule_type = 'CLIENT' AND ? IS NOT NULL AND match_value = ?)
                   )
             ) THEN 1 ELSE 0 END AS BIGINT) AS matched",
        )
        .bind(query.user_id)
        .bind(query.device_id)
        .bind(query.device_id)
        .bind(query.client_name)
        .bind(query.client_name)
        .fetch_one(&self.pool)
        .await
        .and_then(|row| row.try_get::<i64, _>("matched").map(|value| value != 0))
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn active_user_access_device_client_blacklist_rules(
        &self,
        user_id: &str,
    ) -> Result<(bool, bool), StorageError> {
        self.query(
            "SELECT
                 CAST(CASE WHEN EXISTS (
                     SELECT 1 FROM user_access_blacklist_rules
                     WHERE is_enabled = 1
                       AND rule_type = 'DEVICE'
                       AND (user_id IS NULL OR user_id = ?)
                 ) THEN 1 ELSE 0 END AS BIGINT) AS has_device_rules,
                 CAST(CASE WHEN EXISTS (
                     SELECT 1 FROM user_access_blacklist_rules
                     WHERE is_enabled = 1
                       AND rule_type = 'CLIENT'
                       AND (user_id IS NULL OR user_id = ?)
                 ) THEN 1 ELSE 0 END AS BIGINT) AS has_client_rules",
        )
        .bind(user_id)
        .bind(user_id)
        .fetch_one(&self.pool)
        .await
        .and_then(|row| {
            Ok((
                row.try_get::<i64, _>("has_device_rules")? != 0,
                row.try_get::<i64, _>("has_client_rules")? != 0,
            ))
        })
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    // LUX-765 loads at most 201 active global and user IP rules so callers can
    // fail closed if the supported per-request evaluation bound is exceeded.
    #[allow(dead_code)]
    pub(crate) async fn list_user_access_ip_blacklist_rule_values(
        &self,
        query: UserAccessBlacklistIpRuleQuery<'_>,
    ) -> Result<Vec<String>, StorageError> {
        let rows = self
            .query(
                "SELECT match_value
                 FROM user_access_blacklist_rules
                 WHERE is_enabled = 1
                   AND rule_type = 'IP_CIDR'
                   AND (user_id IS NULL OR user_id = ?)
                 ORDER BY CASE WHEN user_id IS NULL THEN 0 ELSE 1 END,
                          created_at ASC, id ASC
                 LIMIT ?",
            )
            .bind(query.user_id)
            .bind(query.limit.clamp(1, 201))
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(rows.into_iter().map(|row| row.get("match_value")).collect())
    }

    #[allow(dead_code)]
    pub(crate) async fn set_user_access_blacklist_rule_enabled(
        &self,
        scope: UserAccessBlacklistRuleScope<'_>,
        rule_id: &str,
        is_enabled: bool,
    ) -> Result<bool, StorageError> {
        let (where_clause, user_id) = match scope {
            UserAccessBlacklistRuleScope::Global => ("user_id IS NULL", None),
            UserAccessBlacklistRuleScope::User(user_id) => ("user_id = ?", Some(user_id)),
        };
        let sql = format!(
            "UPDATE user_access_blacklist_rules
             SET is_enabled = ?, updated_at = ?
             WHERE id = ? AND {where_clause}"
        );
        let mut update = self
            .query(sqlx::AssertSqlSafe(sql))
            .bind(database_flag(is_enabled))
            .bind(current_unix_timestamp())
            .bind(rule_id);
        if let Some(user_id) = user_id {
            update = update.bind(user_id);
        }
        update
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected() > 0)
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    #[allow(dead_code)]
    pub(crate) async fn delete_user_access_blacklist_rule(
        &self,
        scope: UserAccessBlacklistRuleScope<'_>,
        rule_id: &str,
    ) -> Result<bool, StorageError> {
        let (where_clause, user_id) = match scope {
            UserAccessBlacklistRuleScope::Global => ("user_id IS NULL", None),
            UserAccessBlacklistRuleScope::User(user_id) => ("user_id = ?", Some(user_id)),
        };
        let sql =
            format!("DELETE FROM user_access_blacklist_rules WHERE id = ? AND {where_clause}");
        let mut delete = self.query(sqlx::AssertSqlSafe(sql)).bind(rule_id);
        if let Some(user_id) = user_id {
            delete = delete.bind(user_id);
        }
        delete
            .execute(&self.pool)
            .await
            .map(|result| result.rows_affected() > 0)
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    // LUX-723 wires this storage contract into the administrator activity API.
    #[allow(dead_code)]
    pub(crate) async fn list_user_activity_events(
        &self,
        query: UserActivityEventQuery<'_>,
    ) -> Result<(Vec<StoredUserActivityEvent>, i64), StorageError> {
        let event_type = query
            .event_type
            .map(str::trim)
            .filter(|event_type| !event_type.is_empty());
        let mut conditions = vec!["user_id = ?"];
        if event_type.is_some() {
            conditions.push("event_type = ?");
        }
        if query.from_at.is_some() {
            conditions.push("occurred_at >= ?");
        }
        if query.to_at.is_some() {
            conditions.push("occurred_at <= ?");
        }
        let where_clause = format!(" WHERE {}", conditions.join(" AND "));

        let count_sql =
            format!("SELECT COUNT(*) AS total_count FROM user_activity_events{where_clause}");
        let mut count_query = self
            .query(sqlx::AssertSqlSafe(count_sql))
            .bind(query.user_id);
        if let Some(event_type) = event_type {
            count_query = count_query.bind(event_type);
        }
        if let Some(from_at) = query.from_at {
            count_query = count_query.bind(from_at);
        }
        if let Some(to_at) = query.to_at {
            count_query = count_query.bind(to_at);
        }
        let total_count = count_query
            .fetch_one(&self.pool)
            .await
            .map(|row| row.get::<i64, _>("total_count"))
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;

        let page_sql = format!(
            "SELECT id, user_id, actor_user_id, event_type, target_type, target_id,
                    metadata_json, occurred_at
             FROM user_activity_events{where_clause}
             ORDER BY occurred_at DESC, id DESC
             LIMIT ? OFFSET ?"
        );
        let mut page_query = self
            .query(sqlx::AssertSqlSafe(page_sql))
            .bind(query.user_id);
        if let Some(event_type) = event_type {
            page_query = page_query.bind(event_type);
        }
        if let Some(from_at) = query.from_at {
            page_query = page_query.bind(from_at);
        }
        if let Some(to_at) = query.to_at {
            page_query = page_query.bind(to_at);
        }
        let rows = page_query
            .bind(query.limit.clamp(1, 100))
            .bind(query.offset.max(0))
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let events = rows
            .into_iter()
            .map(|row| StoredUserActivityEvent {
                id: row.get("id"),
                user_id: row.get("user_id"),
                actor_user_id: row.get("actor_user_id"),
                event_type: row.get("event_type"),
                target_type: row.get("target_type"),
                target_id: row.get("target_id"),
                metadata_json: row.get("metadata_json"),
                occurred_at: row.get("occurred_at"),
            })
            .collect();
        Ok((events, total_count))
    }

    #[allow(dead_code)] // LUX-732 wires the bounded activity-retention worker.
    pub(crate) async fn delete_user_activity_events_before(
        &self,
        occurred_before: i64,
        limit: i64,
    ) -> Result<u64, StorageError> {
        if limit <= 0 {
            return Ok(0);
        }
        let limit = limit.min(USER_ACTIVITY_RETENTION_MAX_BATCH_SIZE);
        self.query(
            "DELETE FROM user_activity_events
             WHERE id IN (
                 SELECT id FROM user_activity_events
                 WHERE occurred_at < ?
                 ORDER BY occurred_at, id
                 LIMIT ?
             )",
        )
        .bind(occurred_before)
        .bind(limit)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected())
        .map_err(|source| StorageError::Sqlx {
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
                    u.can_remote_access, u.can_download, u.email, u.email_verified_at,
                    u.deleted_at, u.deletion_purge_at, u.last_login_at,
                    COALESCE(
                        (SELECT MAX(COALESCE(at2.last_seen_at, at2.created_at))
                         FROM access_tokens at2 WHERE at2.user_id = u.id),
                        u.last_login_at
                    ) AS last_activity_at
             FROM access_tokens at
             JOIN users u ON u.id = at.user_id
             WHERE at.token_hash = ? AND at.revoked_at IS NULL
               AND (at.expires_at IS NULL OR at.expires_at > unixepoch())",
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await
        .map(|row| row.map(stored_user))
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
             WHERE token_hash = ? AND revoked_at IS NULL
               AND (expires_at IS NULL OR expires_at > unixepoch())",
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

    #[allow(dead_code)]
    pub(crate) async fn list_user_emby_login_sessions_for_admin(
        &self,
        user_id: &str,
        page_size: i64,
        offset: i64,
    ) -> Result<(Vec<StoredEmbyLoginSessionSummary>, i64), StorageError> {
        let page_size = page_size.clamp(1, 100);
        let offset = offset.max(0);
        let total_count = self
            .query_scalar(
                "SELECT COUNT(*) FROM access_tokens
                 WHERE user_id = ? AND revoked_at IS NULL",
            )
            .bind(user_id)
            .fetch_one(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let sessions = self
            .query(
                "SELECT id, device_id, device_name, device_type, client_name, client_version,
                        created_at, updated_at, last_seen_at
                 FROM access_tokens
                 WHERE user_id = ? AND revoked_at IS NULL
                 ORDER BY COALESCE(last_seen_at, updated_at) DESC, id DESC
                 LIMIT ? OFFSET ?",
            )
            .bind(user_id)
            .bind(page_size)
            .bind(offset)
            .fetch_all(&self.pool)
            .await
            .map(|rows| {
                rows.into_iter()
                    .map(|row| StoredEmbyLoginSessionSummary {
                        id: row.get("id"),
                        device_id: row.get("device_id"),
                        device_name: row.get("device_name"),
                        device_type: row.get("device_type"),
                        client_name: row.get("client_name"),
                        client_version: row.get("client_version"),
                        created_at: row.get("created_at"),
                        updated_at: row.get("updated_at"),
                        last_seen_at: row.get("last_seen_at"),
                    })
                    .collect()
            })
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok((sessions, total_count))
    }

    pub(crate) async fn revoke_user_emby_login_session_for_admin(
        &self,
        user_id: &str,
        session_id: &str,
    ) -> Result<bool, StorageError> {
        self.query(
            "UPDATE access_tokens
             SET revoked_at = unixepoch(), updated_at = unixepoch()
             WHERE user_id = ? AND id = ? AND revoked_at IS NULL",
        )
        .bind(user_id)
        .bind(session_id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected() > 0)
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn revoke_all_user_emby_login_sessions_for_admin(
        &self,
        user_id: &str,
    ) -> Result<u64, StorageError> {
        self.revoke_all_access_tokens_for_user(user_id).await
    }

    /// Revokes every active Emby access token of a user.
    pub(crate) async fn revoke_all_access_tokens_for_user(
        &self,
        user_id: &str,
    ) -> Result<u64, StorageError> {
        self.query(
            "UPDATE access_tokens
             SET revoked_at = unixepoch(), updated_at = unixepoch()
             WHERE user_id = ? AND revoked_at IS NULL",
        )
        .bind(user_id)
        .execute(&self.pool)
        .await
        .map(|result| result.rows_affected())
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
        let mut transaction = self.begin_metadata_write_transaction().await?;
        self.query(
            "INSERT INTO user_library_access (user_id, library_id, can_view)
             VALUES (?, ?, ?)
             ON CONFLICT(user_id, library_id) DO UPDATE SET
                 can_view = excluded.can_view, updated_at = unixepoch()",
        )
        .bind(user_id)
        .bind(library_id)
        .bind(database_flag(can_view))
        .execute(&mut *transaction)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        self.query(
            "UPDATE users
             SET library_access_mode = CASE WHEN EXISTS (
                 SELECT 1 FROM user_library_access
                 WHERE user_id = users.id AND can_view = 1
             ) THEN 'SELECTED' ELSE 'ALL' END
             WHERE id = ?",
        )
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
        self.query(
            "UPDATE users
             SET library_access_mode = CASE WHEN EXISTS (
                 SELECT 1 FROM user_library_access
                 WHERE user_id = users.id AND can_view = 1
             ) THEN 'SELECTED' ELSE 'ALL' END
             WHERE id = ?",
        )
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
            })
    }

    pub(crate) async fn user_library_access_mode(
        &self,
        user_id: &str,
    ) -> Result<Option<String>, StorageError> {
        self.query_scalar("SELECT library_access_mode FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    pub(crate) async fn replace_user_library_access_mode(
        &self,
        user_id: &str,
        mode: &str,
        library_ids: &[String],
    ) -> Result<bool, StorageError> {
        let mut transaction = self.begin_metadata_write_transaction().await?;
        let updated = self
            .query(
                "UPDATE users SET library_access_mode = ?, updated_at = unixepoch()
                 WHERE id = ? AND deleted_at IS NULL",
            )
            .bind(mode)
            .bind(user_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if updated.rows_affected() == 0 {
            transaction
                .rollback()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(false);
        }
        self.query("DELETE FROM user_library_access WHERE user_id = ?")
            .bind(user_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if mode == "SELECTED" {
            for library_id in library_ids {
                self.query(
                    "INSERT INTO user_library_access (user_id, library_id, can_view)
                     VALUES (?, ?, 1)",
                )
                .bind(user_id)
                .bind(library_id)
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
        Ok(true)
    }

    pub(crate) async fn has_user_library_access(
        &self,
        user_id: &str,
        library_id: &str,
    ) -> Result<bool, StorageError> {
        self.query_scalar(
            "SELECT CASE WHEN EXISTS(
                SELECT 1 FROM libraries
                WHERE id = ? AND is_enabled = 1 AND deletion_state = 'ACTIVE'
            ) AND EXISTS(
                SELECT 1 FROM users access_user
                WHERE access_user.id = ?
                  AND (
                      access_user.library_access_mode = 'ALL'
                      OR (access_user.library_access_mode = 'SELECTED' AND EXISTS (
                          SELECT 1 FROM user_library_access allowed_library
                          WHERE allowed_library.user_id = access_user.id
                            AND allowed_library.library_id = ?
                            AND allowed_library.can_view = 1
                      ))
                  )
            ) THEN 1 ELSE 0 END",
        )
        .bind(library_id)
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
        // The explicit mode decides whether enabled libraries or only positive selections are visible.
        self.query_scalar(
            "SELECT l.id
             FROM libraries l
             WHERE l.is_enabled = 1
               AND l.system_kind IS NULL
               AND l.deletion_state = 'ACTIVE'
               AND EXISTS (
                   SELECT 1 FROM users access_user
                   WHERE access_user.id = ?
                     AND (
                         access_user.library_access_mode = 'ALL'
                         OR (access_user.library_access_mode = 'SELECTED' AND EXISTS (
                             SELECT 1 FROM user_library_access allowed_library
                             WHERE allowed_library.user_id = access_user.id
                               AND allowed_library.library_id = l.id
                               AND allowed_library.can_view = 1
                         ))
                     )
               )
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

    pub(crate) async fn list_selected_library_ids(
        &self,
        user_id: &str,
    ) -> Result<Vec<String>, StorageError> {
        self.query_scalar(
            "SELECT ula.library_id
             FROM user_library_access ula
             JOIN libraries l ON l.id = ula.library_id
             WHERE ula.user_id = ? AND ula.can_view = 1
               AND l.deletion_state = 'ACTIVE'
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
        self.query_scalar(
            "SELECT id FROM libraries
             WHERE is_enabled = 1 AND system_kind IS NULL
               AND deletion_state = 'ACTIVE'
             ORDER BY name, id",
        )
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
            "SELECT position_ticks, is_played, is_favorite, hide_from_resume, play_count,
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
                hide_from_resume: row.get::<i64, _>("hide_from_resume") != 0,
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

    pub(crate) async fn list_enabled_media_library_ids(&self) -> Result<Vec<String>, StorageError> {
        self.query_scalar(
            "SELECT id FROM libraries
             WHERE is_enabled = 1 AND system_kind IS NULL
             ORDER BY name, id",
        )
        .fetch_all(&self.pool)
        .await
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
                "SELECT item_id, position_ticks, is_played, is_favorite, hide_from_resume, play_count,
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
                        hide_from_resume: row.get::<i64, _>("hide_from_resume") != 0,
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

    pub(crate) async fn user_deletion_grace_days(&self) -> Result<i64, StorageError> {
        let value: Option<String> = self
            .query_scalar(
                "SELECT value FROM server_settings
                 WHERE key = 'user_deletion_grace_days'",
            )
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(normalize_user_deletion_grace_days(value))
    }

    pub(crate) async fn set_user_deletion_grace_days(&self, days: i64) -> Result<(), StorageError> {
        if !(1..=MAX_USER_DELETION_GRACE_DAYS).contains(&days) {
            return Err(StorageError::Serialization(
                "user deletion grace period is outside the allowed range".to_owned(),
            ));
        }
        self.query(
            "INSERT INTO server_settings (key, value)
             VALUES ('user_deletion_grace_days', ?)
             ON CONFLICT(key) DO UPDATE SET
                 value = excluded.value, updated_at = unixepoch()",
        )
        .bind(days.to_string())
        .execute(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        Ok(())
    }

    #[allow(dead_code)] // LUX-732 exposes the stored value through the admin service.
    pub(crate) async fn user_activity_retention_days(&self) -> Result<i64, StorageError> {
        let value: Option<String> = self
            .query_scalar(
                "SELECT value FROM server_settings
                 WHERE key = 'user_activity_retention_days'",
            )
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(normalize_user_activity_retention_days(value))
    }

    #[allow(dead_code)] // LUX-732 exposes the stored value through the admin service.
    pub(crate) async fn set_user_activity_retention_days(
        &self,
        days: i64,
    ) -> Result<(), StorageError> {
        if !(1..=MAX_USER_ACTIVITY_RETENTION_DAYS).contains(&days) {
            return Err(StorageError::Serialization(
                "user activity retention period is outside the allowed range".to_owned(),
            ));
        }
        self.query(
            "INSERT INTO server_settings (key, value)
             VALUES ('user_activity_retention_days', ?)
             ON CONFLICT(key) DO UPDATE SET
                 value = excluded.value, updated_at = unixepoch()",
        )
        .bind(days.to_string())
        .execute(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        Ok(())
    }

    #[allow(dead_code)] // LUX-763 exposes and validates this setting through the admin API.
    pub(crate) async fn trusted_proxy_cidrs_json(&self) -> Result<Option<String>, StorageError> {
        self.query_scalar(
            "SELECT value FROM server_settings
             WHERE key = 'trusted_proxy_cidrs'",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    #[allow(dead_code)] // LUX-763 exposes and validates this setting through the admin API.
    pub(crate) async fn set_trusted_proxy_cidrs_json(
        &self,
        value: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO server_settings (key, value)
             VALUES ('trusted_proxy_cidrs', ?)
             ON CONFLICT(key) DO UPDATE SET
                 value = excluded.value, updated_at = unixepoch()",
        )
        .bind(value)
        .execute(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        Ok(())
    }

    pub(crate) async fn new_user_permissions_json(&self) -> Result<Option<String>, StorageError> {
        self.query_scalar(
            "SELECT value FROM server_settings
             WHERE key = 'new_user_permissions'",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    pub(crate) async fn set_new_user_permissions_json(
        &self,
        value: &str,
    ) -> Result<(), StorageError> {
        self.query(
            "INSERT INTO server_settings (key, value)
             VALUES ('new_user_permissions', ?)
             ON CONFLICT(key) DO UPDATE SET
                 value = excluded.value, updated_at = unixepoch()",
        )
        .bind(value)
        .execute(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) async fn user_permission_presets_json(
        &self,
    ) -> Result<Option<String>, StorageError> {
        self.query_scalar(
            "SELECT value FROM server_settings
             WHERE key = 'user_permission_presets'",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })
    }

    #[allow(dead_code)]
    pub(crate) async fn compare_and_set_user_permission_presets_json(
        &self,
        expected: Option<&str>,
        value: &str,
    ) -> Result<bool, StorageError> {
        let result = if let Some(expected) = expected {
            self.query(
                "UPDATE server_settings
                 SET value = ?, updated_at = unixepoch()
                 WHERE key = 'user_permission_presets' AND value = ?",
            )
            .bind(value)
            .bind(expected)
            .execute(&self.pool)
            .await
        } else {
            self.query(
                "INSERT INTO server_settings (key, value)
                 VALUES ('user_permission_presets', ?)
                 ON CONFLICT(key) DO NOTHING",
            )
            .bind(value)
            .execute(&self.pool)
            .await
        }
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;

        Ok(result.rows_affected() == 1)
    }

    pub(crate) async fn are_enabled_regular_libraries(
        &self,
        library_ids: &[String],
    ) -> Result<bool, StorageError> {
        if library_ids.len() > 100
            || library_ids
                .iter()
                .enumerate()
                .any(|(index, id)| library_ids[..index].contains(id))
        {
            return Ok(false);
        }
        for library_id in library_ids {
            let enabled: i64 = self
                .query_scalar(
                    "SELECT CASE WHEN EXISTS (
                         SELECT 1 FROM libraries
                         WHERE id = ? AND is_enabled = 1 AND system_kind IS NULL
                     ) THEN 1 ELSE 0 END",
                )
                .bind(library_id)
                .fetch_one(&self.pool)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if enabled == 0 {
                return Ok(false);
            }
        }
        Ok(true)
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

    // LUX-677 creates the persistent content restriction contract.
    #[allow(dead_code)]
    pub(crate) async fn user_content_restrictions(
        &self,
        user_id: &str,
    ) -> Result<Option<UserContentRestrictions>, StorageError> {
        let rows = self
            .query(
                "SELECT restrictions.rating_system, restrictions.maximum_rating,
                        restrictions.allow_unrated, restrictions.version, restrictions.updated_at,
                        tags.tag_kind, tags.tag
                 FROM user_content_restrictions restrictions
                 LEFT JOIN user_content_restriction_tags tags
                   ON tags.user_id = restrictions.user_id
                 WHERE restrictions.user_id = ?
                 ORDER BY tags.tag_kind, tags.tag",
            )
            .bind(user_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };

        let mut allowed_tags = Vec::new();
        let mut blocked_tags = Vec::new();
        for tag_row in &rows {
            let tag_kind: Option<String> = tag_row.get("tag_kind");
            let tag: Option<String> = tag_row.get("tag");
            match (tag_kind.as_deref(), tag) {
                (None, None) => {}
                (Some("ALLOW"), Some(tag)) => allowed_tags.push(tag),
                (Some("BLOCK"), Some(tag)) => blocked_tags.push(tag),
                _ => {
                    return Err(StorageError::Serialization(
                        "invalid user content restriction tag row".to_owned(),
                    ));
                }
            }
        }
        allowed_tags.sort_unstable();
        blocked_tags.sort_unstable();

        Ok(Some(UserContentRestrictions {
            rating_system: row.get("rating_system"),
            maximum_rating: row.get("maximum_rating"),
            allow_unrated: row.get::<i64, _>("allow_unrated") != 0,
            allowed_tags,
            blocked_tags,
            version: row.get("version"),
            updated_at: row.get("updated_at"),
        }))
    }

    // LUX-708 reads the source and all selected targets from one bounded database snapshot.
    #[allow(dead_code)]
    pub(crate) async fn user_permission_copy_snapshots(
        &self,
        user_ids: &[String],
    ) -> Result<HashMap<String, UserPermissionCopySnapshot>, StorageError> {
        if user_ids.is_empty() {
            return Ok(HashMap::new());
        }
        if user_ids.len() > MAX_PERMISSION_COPY_USER_IDS
            || user_ids.iter().collect::<HashSet<_>>().len() != user_ids.len()
        {
            return Err(StorageError::Conflict(
                "user permission copy snapshot request is invalid".to_owned(),
            ));
        }

        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if self.backend() == crate::config::DatabaseBackend::Postgres {
            sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }
        let snapshots = self
            .user_permission_copy_snapshots_in_transaction(user_ids, &mut transaction)
            .await?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(snapshots)
    }

    async fn user_permission_copy_snapshots_in_transaction(
        &self,
        user_ids: &[String],
        transaction: &mut sqlx::Transaction<'_, Any>,
    ) -> Result<HashMap<String, UserPermissionCopySnapshot>, StorageError> {
        let placeholders = vec!["?"; user_ids.len()].join(", ");
        let user_sql = format!(
            "SELECT id, library_access_mode, can_download, can_play,
                    max_simultaneous_playbacks, allow_remux,
                    allow_audio_transcode, allow_video_transcode
             FROM users WHERE deleted_at IS NULL AND id IN ({placeholders})"
        );
        let mut user_query = self.query(sqlx::AssertSqlSafe(user_sql));
        for user_id in user_ids {
            user_query = user_query.bind(user_id);
        }
        let user_rows = user_query
            .fetch_all(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let mut snapshots = HashMap::with_capacity(user_rows.len());
        for row in user_rows {
            let user_id: String = row.get("id");
            snapshots.insert(
                user_id,
                UserPermissionCopySnapshot {
                    library_access_mode: row.get("library_access_mode"),
                    library_ids: Vec::new(),
                    can_download: row.get::<i64, _>("can_download") != 0,
                    playback_policy: UserPlaybackPolicy {
                        can_play: row.get::<i64, _>("can_play") != 0,
                        max_simultaneous_playbacks: row.get("max_simultaneous_playbacks"),
                        allow_remux: row.get::<i64, _>("allow_remux") != 0,
                        allow_audio_transcode: row.get::<i64, _>("allow_audio_transcode") != 0,
                        allow_video_transcode: row.get::<i64, _>("allow_video_transcode") != 0,
                    },
                    content_restrictions: None,
                },
            );
        }

        let library_sql = format!(
            "SELECT access.user_id, access.library_id
             FROM user_library_access access
             INNER JOIN users ON users.id = access.user_id AND users.deleted_at IS NULL
             WHERE access.can_view = 1 AND users.library_access_mode = 'SELECTED'
               AND access.user_id IN ({placeholders})
             ORDER BY access.user_id, access.library_id"
        );
        let mut library_query = self.query(sqlx::AssertSqlSafe(library_sql));
        for user_id in user_ids {
            library_query = library_query.bind(user_id);
        }
        for row in library_query
            .fetch_all(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
        {
            let user_id: String = row.get("user_id");
            let library_id: String = row.get("library_id");
            if let Some(snapshot) = snapshots.get_mut(&user_id) {
                snapshot.library_ids.push(library_id);
            }
        }

        let restrictions_sql = format!(
            "SELECT restrictions.user_id, restrictions.rating_system,
                    restrictions.maximum_rating, restrictions.allow_unrated,
                    restrictions.version, restrictions.updated_at,
                    tags.tag_kind, tags.tag
             FROM user_content_restrictions restrictions
             INNER JOIN users ON users.id = restrictions.user_id AND users.deleted_at IS NULL
             LEFT JOIN user_content_restriction_tags tags
               ON tags.user_id = restrictions.user_id
             WHERE restrictions.user_id IN ({placeholders})
             ORDER BY restrictions.user_id, tags.tag_kind, tags.tag"
        );
        let mut restrictions_query = self.query(sqlx::AssertSqlSafe(restrictions_sql));
        for user_id in user_ids {
            restrictions_query = restrictions_query.bind(user_id);
        }
        let mut restrictions_by_user = HashMap::<String, UserContentRestrictions>::new();
        for row in restrictions_query
            .fetch_all(&mut **transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?
        {
            let user_id: String = row.get("user_id");
            let restrictions =
                restrictions_by_user
                    .entry(user_id)
                    .or_insert_with(|| UserContentRestrictions {
                        rating_system: row.get("rating_system"),
                        maximum_rating: row.get("maximum_rating"),
                        allow_unrated: row.get::<i64, _>("allow_unrated") != 0,
                        allowed_tags: Vec::new(),
                        blocked_tags: Vec::new(),
                        version: row.get("version"),
                        updated_at: row.get("updated_at"),
                    });
            let tag_kind: Option<String> = row.get("tag_kind");
            let tag: Option<String> = row.get("tag");
            match (tag_kind.as_deref(), tag) {
                (None, None) => {}
                (Some("ALLOW"), Some(tag)) => restrictions.allowed_tags.push(tag),
                (Some("BLOCK"), Some(tag)) => restrictions.blocked_tags.push(tag),
                _ => {
                    return Err(StorageError::Serialization(
                        "invalid user content restriction tag row".to_owned(),
                    ));
                }
            }
        }
        for (user_id, restrictions) in restrictions_by_user {
            if let Some(snapshot) = snapshots.get_mut(&user_id) {
                snapshot.content_restrictions = Some(restrictions);
            }
        }

        Ok(snapshots)
    }

    // LUX-709 writes one target's selected permission groups as a compare-and-swap transaction.
    #[allow(dead_code)]
    pub(crate) async fn copy_user_permission_groups(
        &self,
        source_user_id: &str,
        expected_source: &UserPermissionCopySnapshot,
        target_user_id: &str,
        expected_target: &UserPermissionCopySnapshot,
        groups: UserPermissionCopyGroups,
    ) -> Result<UserPermissionCopyResult, StorageError> {
        if groups.is_empty() || source_user_id == target_user_id {
            return Ok(UserPermissionCopyResult::InvalidSnapshot);
        }

        let mut transaction = self.begin_metadata_write_transaction().await?;
        if self.backend() == crate::config::DatabaseBackend::Postgres {
            let mut lock_ids = [source_user_id.to_owned(), target_user_id.to_owned()];
            lock_ids.sort_unstable();
            let lock_sql =
                "SELECT id FROM users WHERE id IN (?, ?) ORDER BY id FOR UPDATE".to_owned();
            let lock_query = self
                .query(sqlx::AssertSqlSafe(lock_sql))
                .bind(&lock_ids[0])
                .bind(&lock_ids[1]);
            lock_query
                .fetch_all(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }

        let user_ids = vec![source_user_id.to_owned(), target_user_id.to_owned()];
        let mut snapshots = self
            .user_permission_copy_snapshots_in_transaction(&user_ids, &mut transaction)
            .await?;
        let Some(source) = snapshots.remove(source_user_id) else {
            return Ok(UserPermissionCopyResult::UserUnavailable);
        };
        let Some(target) = snapshots.remove(target_user_id) else {
            return Ok(UserPermissionCopyResult::UserUnavailable);
        };
        if !permission_copy_groups_match(&source, expected_source, groups)
            || !permission_copy_groups_match(&target, expected_target, groups)
        {
            return Ok(UserPermissionCopyResult::Conflict);
        }

        if groups.library {
            let library_access_is_valid = match source.library_access_mode.as_str() {
                "ALL" | "NONE" => source.library_ids.is_empty(),
                "SELECTED" => {
                    !source.library_ids.is_empty()
                        && source.library_ids.iter().collect::<HashSet<_>>().len()
                            == source.library_ids.len()
                }
                _ => false,
            };
            if !library_access_is_valid {
                return Ok(UserPermissionCopyResult::InvalidSnapshot);
            }
            if source.library_access_mode == "SELECTED" {
                let mut available = 0_i64;
                for library_chunk in source
                    .library_ids
                    .chunks(PERMISSION_COPY_LIBRARY_QUERY_CHUNK_SIZE)
                {
                    let placeholders = vec!["?"; library_chunk.len()].join(", ");
                    let library_sql = format!(
                        "SELECT COUNT(*) FROM libraries
                         WHERE id IN ({placeholders}) AND is_enabled = 1
                           AND system_kind IS NULL
                           AND kind IN ('MOVIE', 'SERIES', 'MIXED', 'HOMEVIDEOS')"
                    );
                    let mut library_query =
                        self.query_scalar::<i64>(sqlx::AssertSqlSafe(library_sql));
                    for library_id in library_chunk {
                        library_query = library_query.bind(library_id);
                    }
                    available +=
                        library_query
                            .fetch_one(&mut *transaction)
                            .await
                            .map_err(|source| StorageError::Sqlx {
                                path: self.path.clone(),
                                source,
                            })?;
                }
                if available != source.library_ids.len() as i64 {
                    return Ok(UserPermissionCopyResult::InvalidSnapshot);
                }
            }

            let updated = self
                .query(
                    "UPDATE users SET library_access_mode = ?, updated_at = unixepoch()
                     WHERE id = ? AND deleted_at IS NULL",
                )
                .bind(&source.library_access_mode)
                .bind(target_user_id)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if updated.rows_affected() != 1 {
                return Ok(UserPermissionCopyResult::UserUnavailable);
            }
            self.query("DELETE FROM user_library_access WHERE user_id = ?")
                .bind(target_user_id)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if source.library_access_mode == "SELECTED" {
                for library_id in &source.library_ids {
                    self.query(
                        "INSERT INTO user_library_access (user_id, library_id, can_view)
                         VALUES (?, ?, 1)",
                    )
                    .bind(target_user_id)
                    .bind(library_id)
                    .execute(&mut *transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                }
            }
        }

        if groups.playback {
            let updated = self
                .query(
                    "UPDATE users
                     SET can_download = ?, can_play = ?, max_simultaneous_playbacks = ?,
                         allow_remux = ?, allow_audio_transcode = ?, allow_video_transcode = ?,
                         updated_at = unixepoch()
                     WHERE id = ? AND deleted_at IS NULL",
                )
                .bind(database_flag(source.can_download))
                .bind(database_flag(source.playback_policy.can_play))
                .bind(source.playback_policy.max_simultaneous_playbacks)
                .bind(database_flag(source.playback_policy.allow_remux))
                .bind(database_flag(source.playback_policy.allow_audio_transcode))
                .bind(database_flag(source.playback_policy.allow_video_transcode))
                .bind(target_user_id)
                .execute(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if updated.rows_affected() != 1 {
                return Ok(UserPermissionCopyResult::UserUnavailable);
            }
        }

        if groups.content
            && !self
                .copy_user_content_restrictions_in_transaction(
                    target_user_id,
                    target.content_restrictions.as_ref(),
                    source.content_restrictions.as_ref(),
                    &mut transaction,
                )
                .await?
        {
            return Ok(UserPermissionCopyResult::Conflict);
        }

        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        Ok(UserPermissionCopyResult::Updated)
    }

    async fn copy_user_content_restrictions_in_transaction(
        &self,
        target_user_id: &str,
        target: Option<&UserContentRestrictions>,
        source: Option<&UserContentRestrictions>,
        transaction: &mut sqlx::Transaction<'_, Any>,
    ) -> Result<bool, StorageError> {
        match source {
            Some(source) => {
                let next_version = match target {
                    Some(target) => target.version.checked_add(1),
                    None => Some(1),
                }
                .ok_or_else(|| {
                    StorageError::Conflict(
                        "user content restriction version is exhausted".to_owned(),
                    )
                })?;
                let result = if let Some(target) = target {
                    self.query(
                        "UPDATE user_content_restrictions
                         SET rating_system = ?, maximum_rating = ?, allow_unrated = ?,
                             version = ?, updated_at = unixepoch()
                         WHERE user_id = ? AND version = ?",
                    )
                    .bind(&source.rating_system)
                    .bind(&source.maximum_rating)
                    .bind(database_flag(source.allow_unrated))
                    .bind(next_version)
                    .bind(target_user_id)
                    .bind(target.version)
                    .execute(&mut **transaction)
                    .await
                } else {
                    self.query(
                        "INSERT INTO user_content_restrictions (
                            user_id, rating_system, maximum_rating, allow_unrated, version, updated_at
                         )
                         SELECT ?, ?, ?, ?, 1, unixepoch()
                         WHERE EXISTS (
                            SELECT 1 FROM users WHERE id = ? AND deleted_at IS NULL
                         )",
                    )
                    .bind(target_user_id)
                    .bind(&source.rating_system)
                    .bind(&source.maximum_rating)
                    .bind(database_flag(source.allow_unrated))
                    .bind(target_user_id)
                    .execute(&mut **transaction)
                    .await
                }
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
                if result.rows_affected() != 1 {
                    return Ok(false);
                }

                self.query("DELETE FROM user_content_restriction_tags WHERE user_id = ?")
                    .bind(target_user_id)
                    .execute(&mut **transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                for (tag_kind, tags) in [
                    ("ALLOW", source.allowed_tags.as_slice()),
                    ("BLOCK", source.blocked_tags.as_slice()),
                ] {
                    for tag in tags {
                        self.query(
                            "INSERT INTO user_content_restriction_tags
                             (user_id, tag_kind, tag) VALUES (?, ?, ?)",
                        )
                        .bind(target_user_id)
                        .bind(tag_kind)
                        .bind(tag)
                        .execute(&mut **transaction)
                        .await
                        .map_err(|source| StorageError::Sqlx {
                            path: self.path.clone(),
                            source,
                        })?;
                    }
                }
                Ok(true)
            }
            None => {
                let Some(target) = target else {
                    return Ok(true);
                };
                let deleted = self
                    .query(
                        "DELETE FROM user_content_restrictions
                         WHERE user_id = ? AND version = ?",
                    )
                    .bind(target_user_id)
                    .bind(target.version)
                    .execute(&mut **transaction)
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                Ok(deleted.rows_affected() == 1)
            }
        }
    }

    // LUX-677 keeps the policy version and both tag groups in one CAS transaction.
    #[allow(dead_code)]
    pub(crate) async fn compare_and_swap_user_content_restrictions(
        &self,
        user_id: &str,
        expected_version: i64,
        restrictions: NewUserContentRestrictions<'_>,
    ) -> Result<UserContentRestrictionsCasResult, StorageError> {
        if expected_version < 0 {
            return Ok(UserContentRestrictionsCasResult::Conflict);
        }
        let rating_system = restrictions.rating_system.trim();
        if !(1..=64).contains(&rating_system.chars().count()) {
            return Err(StorageError::Conflict(
                "user content restriction rating system is invalid".to_owned(),
            ));
        }
        let maximum_rating = restrictions.maximum_rating.map(str::trim);
        if maximum_rating.is_some_and(|rating| !(1..=64).contains(&rating.chars().count())) {
            return Err(StorageError::Conflict(
                "user content restriction maximum rating is invalid".to_owned(),
            ));
        }
        let allowed_tags = normalize_user_content_restriction_tags(restrictions.allowed_tags)?;
        let blocked_tags = normalize_user_content_restriction_tags(restrictions.blocked_tags)?;

        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        if self.backend() == crate::config::DatabaseBackend::Postgres {
            let active_user: Option<String> = self
                .query_scalar("SELECT id FROM users WHERE id = ? AND deleted_at IS NULL FOR UPDATE")
                .bind(user_id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            if active_user.is_none() {
                transaction
                    .rollback()
                    .await
                    .map_err(|source| StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    })?;
                return Ok(UserContentRestrictionsCasResult::UserUnavailable);
            }
        }
        let result = if expected_version == 0 {
            self.query(
                "INSERT INTO user_content_restrictions (
                    user_id, rating_system, maximum_rating, allow_unrated, version, updated_at
                 )
                 SELECT ?, ?, ?, ?, 1, unixepoch()
                 WHERE EXISTS (
                    SELECT 1 FROM users WHERE id = ? AND deleted_at IS NULL
                 )
                 ON CONFLICT(user_id) DO NOTHING",
            )
            .bind(user_id)
            .bind(rating_system)
            .bind(maximum_rating)
            .bind(database_flag(restrictions.allow_unrated))
            .bind(user_id)
            .execute(&mut *transaction)
            .await
        } else {
            self.query(
                "UPDATE user_content_restrictions
                 SET rating_system = ?, maximum_rating = ?, allow_unrated = ?,
                     version = version + 1, updated_at = unixepoch()
                 WHERE user_id = ? AND version = ?
                   AND EXISTS (
                       SELECT 1 FROM users WHERE id = ? AND deleted_at IS NULL
                   )",
            )
            .bind(rating_system)
            .bind(maximum_rating)
            .bind(database_flag(restrictions.allow_unrated))
            .bind(user_id)
            .bind(expected_version)
            .bind(user_id)
            .execute(&mut *transaction)
            .await
        }
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        if result.rows_affected() == 0 {
            let user_available: i64 = self
                .query_scalar(
                    "SELECT CAST(CASE WHEN EXISTS(
                        SELECT 1 FROM users WHERE id = ? AND deleted_at IS NULL
                    ) THEN 1 ELSE 0 END AS BIGINT)",
                )
                .bind(user_id)
                .fetch_one(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            transaction
                .rollback()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(if user_available != 0 {
                UserContentRestrictionsCasResult::Conflict
            } else {
                UserContentRestrictionsCasResult::UserUnavailable
            });
        }

        self.query("DELETE FROM user_content_restriction_tags WHERE user_id = ?")
            .bind(user_id)
            .execute(&mut *transaction)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        for (tag_kind, tags) in [
            ("ALLOW", allowed_tags.as_slice()),
            ("BLOCK", blocked_tags.as_slice()),
        ] {
            for tag in tags {
                if let Err(source) = self
                    .query(
                        "INSERT INTO user_content_restriction_tags (user_id, tag_kind, tag)
                         VALUES (?, ?, ?)",
                    )
                    .bind(user_id)
                    .bind(tag_kind)
                    .bind(tag)
                    .execute(&mut *transaction)
                    .await
                {
                    Self::rollback_write_transaction(transaction, "write failed").await;
                    return Err(StorageError::Sqlx {
                        path: self.path.clone(),
                        source,
                    });
                }
            }
        }
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let Some(updated) = self.user_content_restrictions(user_id).await? else {
            return Ok(UserContentRestrictionsCasResult::UserUnavailable);
        };
        Ok(UserContentRestrictionsCasResult::Updated(updated))
    }

    // LUX-654 connects this storage contract to the authenticated settings API.
    #[allow(dead_code)]
    pub(crate) async fn user_account_preferences(
        &self,
        user_id: &str,
    ) -> Result<Option<UserAccountPreferences>, StorageError> {
        let row = self
            .query(
                "SELECT theme, accent_color, show_media_libraries,
                        show_continue_watching, audio_language, subtitle_language,
                        auto_play_next_episode, version, updated_at
                 FROM user_account_preferences WHERE user_id = ?",
            )
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let Some(row) = row else {
            return Ok(None);
        };
        let library_order = self.user_library_order(user_id).await?;
        let played_percent = self.user_played_percent(user_id).await?;
        Ok(Some(UserAccountPreferences {
            theme: row.get("theme"),
            accent_color: row.get("accent_color"),
            show_media_libraries: row.get::<i64, _>("show_media_libraries") != 0,
            show_continue_watching: row.get::<i64, _>("show_continue_watching") != 0,
            audio_language: row.get("audio_language"),
            subtitle_language: row.get("subtitle_language"),
            auto_play_next_episode: row.get::<i64, _>("auto_play_next_episode") != 0,
            library_order,
            played_percent,
            version: row.get("version"),
            updated_at: row.get("updated_at"),
        }))
    }

    #[allow(dead_code)]
    pub(crate) async fn compare_and_swap_user_account_preferences(
        &self,
        user_id: &str,
        expected_version: i64,
        preferences: NewUserAccountPreferences<'_>,
    ) -> Result<UserAccountPreferencesCasResult, StorageError> {
        if expected_version < 0 {
            return Ok(UserAccountPreferencesCasResult::Conflict);
        }
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let result = if expected_version == 0 {
            self.query(
                "INSERT INTO user_account_preferences (
                    user_id, theme, accent_color, show_media_libraries,
                    show_continue_watching, audio_language, subtitle_language,
                    auto_play_next_episode, version, updated_at
                 )
                 SELECT ?, ?, ?, ?, ?, ?, ?, ?, 1, unixepoch()
                 WHERE EXISTS (
                    SELECT 1 FROM users WHERE id = ? AND deleted_at IS NULL
                 )
                 ON CONFLICT(user_id) DO NOTHING",
            )
            .bind(user_id)
            .bind(preferences.theme)
            .bind(preferences.accent_color)
            .bind(database_flag(preferences.show_media_libraries))
            .bind(database_flag(preferences.show_continue_watching))
            .bind(preferences.audio_language)
            .bind(preferences.subtitle_language)
            .bind(database_flag(preferences.auto_play_next_episode))
            .bind(user_id)
            .execute(&mut *transaction)
            .await
        } else {
            self.query(
                "UPDATE user_account_preferences
                 SET theme = ?, accent_color = ?, show_media_libraries = ?,
                     show_continue_watching = ?, audio_language = ?, subtitle_language = ?,
                     auto_play_next_episode = ?,
                     version = version + 1, updated_at = unixepoch()
                 WHERE user_id = ? AND version = ?
                   AND EXISTS (
                       SELECT 1 FROM users WHERE id = ? AND deleted_at IS NULL
                   )",
            )
            .bind(preferences.theme)
            .bind(preferences.accent_color)
            .bind(database_flag(preferences.show_media_libraries))
            .bind(database_flag(preferences.show_continue_watching))
            .bind(preferences.audio_language)
            .bind(preferences.subtitle_language)
            .bind(database_flag(preferences.auto_play_next_episode))
            .bind(user_id)
            .bind(expected_version)
            .bind(user_id)
            .execute(&mut *transaction)
            .await
        }
        .map_err(|source| StorageError::Sqlx {
            path: self.path.clone(),
            source,
        })?;
        if result.rows_affected() == 0 {
            let user_available: i64 = self
                .query_scalar(
                    "SELECT CAST(CASE WHEN EXISTS(
                        SELECT 1 FROM users WHERE id = ? AND deleted_at IS NULL
                    ) THEN 1 ELSE 0 END AS BIGINT)",
                )
                .bind(user_id)
                .fetch_one(&mut *transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            transaction
                .rollback()
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
            return Ok(if user_available != 0 {
                UserAccountPreferencesCasResult::Conflict
            } else {
                UserAccountPreferencesCasResult::UserUnavailable
            });
        }

        if let Some(played_percent) = preferences.played_percent {
            if let Err(source) = self
                .query(
                    "INSERT INTO user_playback_settings (user_id, played_percent)
                     VALUES (?, ?)
                     ON CONFLICT(user_id) DO UPDATE SET
                         played_percent = excluded.played_percent,
                         updated_at = unixepoch()",
                )
                .bind(user_id)
                .bind(played_percent)
                .execute(&mut *transaction)
                .await
            {
                Self::rollback_write_transaction(transaction, "write failed").await;
                return Err(StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                });
            }
        }
        if let Some(library_order) = preferences.library_order {
            if let Err(error) = self
                .replace_user_library_order_in_transaction(user_id, library_order, &mut transaction)
                .await
            {
                Self::rollback_write_transaction(transaction, "write failed").await;
                return Err(error);
            }
        }
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })?;
        let Some(updated) = self.user_account_preferences(user_id).await? else {
            return Ok(UserAccountPreferencesCasResult::UserUnavailable);
        };
        Ok(UserAccountPreferencesCasResult::Updated(updated))
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

    #[cfg(test)]
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
        self.replace_user_library_order_in_transaction(user_id, library_ids, &mut transaction)
            .await?;
        transaction
            .commit()
            .await
            .map_err(|source| StorageError::Sqlx {
                path: self.path.clone(),
                source,
            })
    }

    async fn replace_user_library_order_in_transaction(
        &self,
        user_id: &str,
        library_ids: &[String],
        transaction: &mut sqlx::Transaction<'_, sqlx::Any>,
    ) -> Result<(), StorageError> {
        self.query("DELETE FROM user_library_order WHERE user_id = ?")
            .bind(user_id)
            .execute(&mut **transaction)
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
                .execute(&mut **transaction)
                .await
                .map_err(|source| StorageError::Sqlx {
                    path: self.path.clone(),
                    source,
                })?;
        }
        Ok(())
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

fn permission_copy_groups_match(
    current: &UserPermissionCopySnapshot,
    expected: &UserPermissionCopySnapshot,
    groups: UserPermissionCopyGroups,
) -> bool {
    (!groups.library
        || (current.library_access_mode == expected.library_access_mode
            && current.library_ids == expected.library_ids))
        && (!groups.content || current.content_restrictions == expected.content_restrictions)
        && (!groups.playback
            || (current.can_download == expected.can_download
                && current.playback_policy == expected.playback_policy))
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
    async fn set_user_library_access_batch_updates_rows_and_mode_transactionally()
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
        assert_eq!(database.query_count(), 2);
        let mode: String = sqlx::query_scalar("SELECT library_access_mode FROM users WHERE id = ?")
            .bind(&user_id)
            .fetch_one(database.pool())
            .await?;
        assert_eq!(mode, "SELECTED");
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
    async fn replace_user_library_access_mode_rolls_back_partial_selection()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let user_id = Uuid::now_v7().to_string();
        let library_id = Uuid::now_v7().to_string();
        let missing_library_id = Uuid::now_v7().to_string();
        database
            .insert_initial_user(&user_id, "access-mode-rollback", "Access Mode", "hash")
            .await?;
        sqlx::query("INSERT INTO libraries (id, name, kind) VALUES (?, ?, 'MOVIE')")
            .bind(&library_id)
            .bind(&library_id)
            .execute(database.pool())
            .await?;
        database
            .set_user_library_access(&user_id, &library_id, true)
            .await?;

        assert!(
            database
                .replace_user_library_access_mode(
                    &user_id,
                    "SELECTED",
                    &[library_id.clone(), missing_library_id],
                )
                .await
                .is_err()
        );
        assert_eq!(
            database.user_library_access_mode(&user_id).await?,
            Some("SELECTED".to_owned())
        );
        assert_eq!(
            database.list_selected_library_ids(&user_id).await?,
            vec![library_id]
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
