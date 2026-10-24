use serde::{Deserialize, Serialize};
use std::fmt;

use crate::{
    application::settings::NewUserPermissions,
    domain::ids::UserId,
    storage::{ApplyUserPermissionPreset, Database, StorageError},
};

const PRESETS_SETTING_KEY: &str = "user_permission_presets";
const MAX_CUSTOM_PRESETS: usize = 100;
const MAX_PRESET_NAME_CHARS: usize = 80;
const MAX_PRESET_DESCRIPTION_CHARS: usize = 500;
pub(crate) const MAX_BATCH_USER_PERMISSION_PRESET_TARGETS: usize = 100;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct UserPermissionPreset {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) version: u64,
    pub(crate) is_builtin: bool,
    pub(crate) is_admin: bool,
    pub(crate) can_manage_server: bool,
    pub(crate) permissions: NewUserPermissions,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct NewUserPermissionPresetInput {
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) is_admin: bool,
    pub(crate) can_manage_server: bool,
    pub(crate) permissions: NewUserPermissions,
}

#[derive(Debug)]
pub(crate) enum UserPermissionPresetError {
    BuiltInPreset,
    ConcurrentUpdate,
    DuplicateName,
    InvalidName,
    InvalidDescription,
    InvalidRole,
    InvalidSnapshot,
    NotFound,
    PresetLimitReached,
    Storage(StorageError),
    VersionConflict,
}

impl fmt::Display for UserPermissionPresetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BuiltInPreset => formatter.write_str("built-in presets cannot be changed"),
            Self::ConcurrentUpdate => formatter.write_str("preset list changed concurrently"),
            Self::DuplicateName => formatter.write_str("preset name already exists"),
            Self::InvalidName => formatter.write_str("preset name is invalid"),
            Self::InvalidDescription => formatter.write_str("preset description is invalid"),
            Self::InvalidRole => formatter.write_str("preset role is invalid"),
            Self::InvalidSnapshot => formatter.write_str("permission snapshot is invalid"),
            Self::NotFound => formatter.write_str("preset was not found"),
            Self::PresetLimitReached => formatter.write_str("custom preset limit was reached"),
            Self::Storage(error) => error.fmt(formatter),
            Self::VersionConflict => formatter.write_str("preset version does not match"),
        }
    }
}

impl std::error::Error for UserPermissionPresetError {}

impl From<StorageError> for UserPermissionPresetError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

#[derive(Debug)]
pub(crate) enum UserPermissionPresetApplicationError {
    InvalidUserId,
    InvalidSnapshot,
    LastManager,
    NotFound,
    PresetNotFound,
    Storage(StorageError),
}

impl fmt::Display for UserPermissionPresetApplicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUserId => formatter.write_str("user ID is invalid"),
            Self::InvalidSnapshot => formatter.write_str("permission preset snapshot is invalid"),
            Self::LastManager => {
                formatter.write_str("at least one active server manager is required")
            }
            Self::NotFound => formatter.write_str("user was not found"),
            Self::PresetNotFound => formatter.write_str("permission preset was not found"),
            Self::Storage(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for UserPermissionPresetApplicationError {}

impl From<StorageError> for UserPermissionPresetApplicationError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum UserPermissionPresetBatchErrorCode {
    InvalidUserId,
    UserNotFound,
    LastManager,
    InvalidSnapshot,
    StorageUnavailable,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UserPermissionPresetBatchItemResult {
    pub(crate) user_id: String,
    pub(crate) success: bool,
    pub(crate) error_code: Option<UserPermissionPresetBatchErrorCode>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UserPermissionPresetBatchSummary {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) version: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UserPermissionPresetBatchResult {
    pub(crate) preset: UserPermissionPresetBatchSummary,
    pub(crate) results: Vec<UserPermissionPresetBatchItemResult>,
}

pub(crate) async fn list_user_permission_presets(
    database: &Database,
) -> Result<Vec<UserPermissionPreset>, UserPermissionPresetError> {
    let (_, mut custom_presets) = read_custom_presets(database).await?;
    let mut presets = built_in_presets();
    presets.append(&mut custom_presets);
    Ok(presets)
}

pub(crate) async fn create_user_permission_preset(
    database: &Database,
    mut input: NewUserPermissionPresetInput,
) -> Result<UserPermissionPreset, UserPermissionPresetError> {
    normalize_and_validate_input(database, &mut input).await?;
    let (expected, mut presets) = read_custom_presets(database).await?;
    if presets.len() >= MAX_CUSTOM_PRESETS {
        return Err(UserPermissionPresetError::PresetLimitReached);
    }
    ensure_unique_name(&presets, &input.name, None)?;

    let preset = UserPermissionPreset {
        id: uuid::Uuid::now_v7().to_string(),
        name: input.name,
        description: input.description,
        version: 1,
        is_builtin: false,
        is_admin: input.is_admin,
        can_manage_server: input.can_manage_server,
        permissions: input.permissions,
    };
    presets.push(preset.clone());
    write_custom_presets(database, expected.as_deref(), &presets).await?;
    Ok(preset)
}

pub(crate) async fn update_user_permission_preset(
    database: &Database,
    preset_id: &str,
    expected_version: u64,
    mut input: NewUserPermissionPresetInput,
) -> Result<UserPermissionPreset, UserPermissionPresetError> {
    if is_builtin_id(preset_id) {
        return Err(UserPermissionPresetError::BuiltInPreset);
    }
    normalize_and_validate_input(database, &mut input).await?;
    let (expected, mut presets) = read_custom_presets(database).await?;
    let Some(index) = presets.iter().position(|preset| preset.id == preset_id) else {
        return Err(UserPermissionPresetError::NotFound);
    };
    if presets[index].version != expected_version {
        return Err(UserPermissionPresetError::VersionConflict);
    }
    ensure_unique_name(&presets, &input.name, Some(preset_id))?;
    let next_version = expected_version
        .checked_add(1)
        .ok_or(UserPermissionPresetError::VersionConflict)?;
    let preset = UserPermissionPreset {
        id: preset_id.to_owned(),
        name: input.name,
        description: input.description,
        version: next_version,
        is_builtin: false,
        is_admin: input.is_admin,
        can_manage_server: input.can_manage_server,
        permissions: input.permissions,
    };
    presets[index] = preset.clone();
    write_custom_presets(database, expected.as_deref(), &presets).await?;
    Ok(preset)
}

pub(crate) async fn delete_user_permission_preset(
    database: &Database,
    preset_id: &str,
    expected_version: u64,
) -> Result<(), UserPermissionPresetError> {
    if is_builtin_id(preset_id) {
        return Err(UserPermissionPresetError::BuiltInPreset);
    }
    let (expected, mut presets) = read_custom_presets(database).await?;
    let Some(index) = presets.iter().position(|preset| preset.id == preset_id) else {
        return Err(UserPermissionPresetError::NotFound);
    };
    if presets[index].version != expected_version {
        return Err(UserPermissionPresetError::VersionConflict);
    }
    presets.remove(index);
    write_custom_presets(database, expected.as_deref(), &presets).await
}

pub(crate) async fn apply_user_permission_preset(
    database: &Database,
    user_id: &str,
    preset_id: &str,
) -> Result<UserPermissionPreset, UserPermissionPresetApplicationError> {
    if user_id.parse::<UserId>().is_err() {
        return Err(UserPermissionPresetApplicationError::InvalidUserId);
    }
    let mut preset = list_user_permission_presets(database)
        .await
        .map_err(|error| match error {
            UserPermissionPresetError::Storage(error) => {
                UserPermissionPresetApplicationError::Storage(error)
            }
            _ => UserPermissionPresetApplicationError::InvalidSnapshot,
        })?
        .into_iter()
        .find(|preset| preset.id == preset_id)
        .ok_or(UserPermissionPresetApplicationError::PresetNotFound)?;

    preset.permissions.normalize_empty_selection();
    if !preset.permissions.has_valid_shape()
        || (preset.permissions.library_access_mode == "SELECTED"
            && !database
                .are_enabled_regular_libraries(&preset.permissions.library_ids)
                .await?)
    {
        return Err(UserPermissionPresetApplicationError::InvalidSnapshot);
    }
    let preset_version = i64::try_from(preset.version)
        .map_err(|_| UserPermissionPresetApplicationError::InvalidSnapshot)?;
    let permissions = preset.permissions.stored_snapshot();
    match database
        .apply_user_permission_preset(
            user_id,
            ApplyUserPermissionPreset {
                preset_id: &preset.id,
                preset_name: &preset.name,
                preset_version,
                is_admin: preset.is_admin,
                can_manage_server: preset.can_manage_server,
                permissions,
            },
        )
        .await
    {
        Ok(true) => Ok(preset),
        Ok(false) => Err(UserPermissionPresetApplicationError::NotFound),
        Err(StorageError::LastManager) => Err(UserPermissionPresetApplicationError::LastManager),
        Err(StorageError::Conflict(_)) => {
            Err(UserPermissionPresetApplicationError::InvalidSnapshot)
        }
        Err(error) => Err(UserPermissionPresetApplicationError::Storage(error)),
    }
}

pub(crate) async fn apply_user_permission_preset_batch(
    database: &Database,
    user_ids: &[String],
    preset_id: &str,
) -> Result<UserPermissionPresetBatchResult, UserPermissionPresetApplicationError> {
    let mut preset = list_user_permission_presets(database)
        .await
        .map_err(|error| match error {
            UserPermissionPresetError::Storage(error) => {
                UserPermissionPresetApplicationError::Storage(error)
            }
            _ => UserPermissionPresetApplicationError::InvalidSnapshot,
        })?
        .into_iter()
        .find(|preset| preset.id == preset_id)
        .ok_or(UserPermissionPresetApplicationError::PresetNotFound)?;

    preset.permissions.normalize_empty_selection();
    if !preset.permissions.has_valid_shape()
        || (preset.permissions.library_access_mode == "SELECTED"
            && !database
                .are_enabled_regular_libraries(&preset.permissions.library_ids)
                .await?)
    {
        return Err(UserPermissionPresetApplicationError::InvalidSnapshot);
    }
    let preset_version = i64::try_from(preset.version)
        .map_err(|_| UserPermissionPresetApplicationError::InvalidSnapshot)?;
    let permissions = preset.permissions.stored_snapshot();
    let mut results = Vec::with_capacity(user_ids.len());

    for user_id in user_ids {
        let Ok(user_id) = user_id.parse::<UserId>() else {
            results.push(UserPermissionPresetBatchItemResult {
                user_id: user_id.clone(),
                success: false,
                error_code: Some(UserPermissionPresetBatchErrorCode::InvalidUserId),
            });
            continue;
        };
        let user_id = user_id.to_string();
        let apply = ApplyUserPermissionPreset {
            preset_id: &preset.id,
            preset_name: &preset.name,
            preset_version,
            is_admin: preset.is_admin,
            can_manage_server: preset.can_manage_server,
            permissions,
        };
        let outcome = match database.apply_user_permission_preset(&user_id, apply).await {
            Ok(true) => Ok(()),
            Ok(false) => Err(UserPermissionPresetBatchErrorCode::UserNotFound),
            Err(StorageError::LastManager) => Err(UserPermissionPresetBatchErrorCode::LastManager),
            Err(StorageError::Conflict(_)) => {
                Err(UserPermissionPresetBatchErrorCode::InvalidSnapshot)
            }
            Err(_) => Err(UserPermissionPresetBatchErrorCode::StorageUnavailable),
        };
        results.push(UserPermissionPresetBatchItemResult {
            user_id,
            success: outcome.is_ok(),
            error_code: outcome.err(),
        });
    }

    Ok(UserPermissionPresetBatchResult {
        preset: UserPermissionPresetBatchSummary {
            id: preset.id,
            name: preset.name,
            version: preset.version,
        },
        results,
    })
}

fn built_in_presets() -> Vec<UserPermissionPreset> {
    let admin_permissions = NewUserPermissions {
        library_access_mode: "ALL".to_owned(),
        library_ids: Vec::new(),
        can_download: true,
        can_play: true,
        max_simultaneous_playbacks: 0,
        allow_remux: true,
        allow_audio_transcode: true,
        allow_video_transcode: true,
    };
    vec![
        UserPermissionPreset {
            id: "builtin-admin".to_owned(),
            name: "管理员".to_owned(),
            description: Some("拥有服务器管理权限".to_owned()),
            version: 1,
            is_builtin: true,
            is_admin: true,
            can_manage_server: true,
            permissions: admin_permissions,
        },
        UserPermissionPreset {
            id: "builtin-user".to_owned(),
            name: "普通用户".to_owned(),
            description: Some("默认普通用户权限".to_owned()),
            version: 1,
            is_builtin: true,
            is_admin: false,
            can_manage_server: false,
            permissions: NewUserPermissions::default(),
        },
    ]
}

fn is_builtin_id(preset_id: &str) -> bool {
    matches!(preset_id, "builtin-admin" | "builtin-user")
}

async fn normalize_and_validate_input(
    database: &Database,
    input: &mut NewUserPermissionPresetInput,
) -> Result<(), UserPermissionPresetError> {
    input.name = input.name.trim().to_owned();
    if input.name.is_empty()
        || input.name.chars().count() > MAX_PRESET_NAME_CHARS
        || input.name.chars().any(char::is_control)
    {
        return Err(UserPermissionPresetError::InvalidName);
    }
    input.description = input
        .description
        .take()
        .map(|description| description.trim().to_owned())
        .filter(|description| !description.is_empty());
    if input.description.as_ref().is_some_and(|description| {
        description.chars().count() > MAX_PRESET_DESCRIPTION_CHARS
            || description.chars().any(char::is_control)
    }) {
        return Err(UserPermissionPresetError::InvalidDescription);
    }
    if input.can_manage_server && !input.is_admin {
        return Err(UserPermissionPresetError::InvalidRole);
    }

    input.permissions.normalize_empty_selection();
    if !input.permissions.has_valid_shape()
        || (input.permissions.library_access_mode == "SELECTED"
            && !database
                .are_enabled_regular_libraries(&input.permissions.library_ids)
                .await?)
    {
        return Err(UserPermissionPresetError::InvalidSnapshot);
    }
    Ok(())
}

fn ensure_unique_name(
    presets: &[UserPermissionPreset],
    name: &str,
    except_id: Option<&str>,
) -> Result<(), UserPermissionPresetError> {
    let normalized_name = name.to_lowercase();
    let duplicate = built_in_presets()
        .into_iter()
        .chain(presets.iter().cloned())
        .any(|preset| {
            Some(preset.id.as_str()) != except_id && preset.name.to_lowercase() == normalized_name
        });
    if duplicate {
        Err(UserPermissionPresetError::DuplicateName)
    } else {
        Ok(())
    }
}

async fn read_custom_presets(
    database: &Database,
) -> Result<(Option<String>, Vec<UserPermissionPreset>), UserPermissionPresetError> {
    let stored = database.user_permission_presets_json().await?;
    let presets = match stored.as_deref() {
        Some(json) => serde_json::from_str(json).map_err(|error| {
            UserPermissionPresetError::Storage(StorageError::Serialization(format!(
                "stored {PRESETS_SETTING_KEY} is invalid: {error}"
            )))
        })?,
        None => Vec::new(),
    };
    Ok((stored, presets))
}

async fn write_custom_presets(
    database: &Database,
    expected: Option<&str>,
    presets: &[UserPermissionPreset],
) -> Result<(), UserPermissionPresetError> {
    let json = serde_json::to_string(presets).map_err(|error| {
        UserPermissionPresetError::Storage(StorageError::Serialization(error.to_string()))
    })?;
    if !database
        .compare_and_set_user_permission_presets_json(expected, &json)
        .await?
    {
        return Err(UserPermissionPresetError::ConcurrentUpdate);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::storage::test_database;
    use crate::{
        application::{libraries::LibraryService, settings::NewUserPermissions},
        domain::ids::UserId,
        library::LibraryKind,
    };

    use super::*;

    fn preset_input(name: &str) -> NewUserPermissionPresetInput {
        NewUserPermissionPresetInput {
            name: name.to_owned(),
            description: Some("A reusable permission snapshot".to_owned()),
            is_admin: false,
            can_manage_server: false,
            permissions: NewUserPermissions::default(),
        }
    }

    #[tokio::test]
    async fn lists_admin_and_regular_user_built_in_presets()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;

        let presets = list_user_permission_presets(&database).await?;

        assert_eq!(presets.len(), 2);
        assert!(presets.iter().any(|preset| {
            preset.id == "builtin-admin" && preset.is_builtin && preset.can_manage_server
        }));
        assert!(presets.iter().any(|preset| {
            preset.id == "builtin-user" && preset.is_builtin && !preset.can_manage_server
        }));
        Ok(())
    }

    #[tokio::test]
    async fn custom_presets_create_update_delete_and_check_versions()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;

        let created = create_user_permission_preset(&database, preset_input("Family")).await?;
        assert_eq!(created.version, 1);
        assert!(!created.is_builtin);

        let updated =
            update_user_permission_preset(&database, &created.id, 1, preset_input("Family Media"))
                .await?;
        assert_eq!(updated.version, 2);
        assert_eq!(updated.name, "Family Media");
        assert!(matches!(
            update_user_permission_preset(&database, &created.id, 1, preset_input("Stale")).await,
            Err(UserPermissionPresetError::VersionConflict)
        ));

        delete_user_permission_preset(&database, &created.id, 2).await?;
        assert_eq!(list_user_permission_presets(&database).await?.len(), 2);
        assert!(matches!(
            delete_user_permission_preset(&database, "builtin-admin", 1).await,
            Err(UserPermissionPresetError::BuiltInPreset)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn rejects_duplicate_names_invalid_snapshots_and_stale_storage_writes()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;

        create_user_permission_preset(&database, preset_input("Friends")).await?;
        assert!(matches!(
            create_user_permission_preset(&database, preset_input(" friends ")).await,
            Err(UserPermissionPresetError::DuplicateName)
        ));
        let mut invalid = preset_input("Invalid");
        invalid.permissions.max_simultaneous_playbacks = -1;
        assert!(matches!(
            create_user_permission_preset(&database, invalid).await,
            Err(UserPermissionPresetError::InvalidSnapshot)
        ));
        assert!(matches!(
            create_user_permission_preset(&database, preset_input("管理员")).await,
            Err(UserPermissionPresetError::DuplicateName)
        ));

        let (_cas_temp_dir, cas_database) = test_database().await?;
        assert!(
            cas_database
                .compare_and_set_user_permission_presets_json(None, "[]")
                .await?
        );
        assert!(
            !cas_database
                .compare_and_set_user_permission_presets_json(None, "[]")
                .await?
        );
        assert!(
            !cas_database
                .compare_and_set_user_permission_presets_json(Some("stale"), "[]")
                .await?
        );
        Ok(())
    }

    #[tokio::test]
    async fn normalizes_empty_selected_library_mode_to_none()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let mut input = preset_input("Empty selection");
        input.permissions.library_access_mode = "SELECTED".to_owned();
        input.permissions.library_ids.clear();

        let created = create_user_permission_preset(&database, input).await?;

        assert_eq!(created.permissions.library_access_mode, "NONE");
        assert!(created.permissions.library_ids.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn applies_permission_snapshot_and_keeps_source_after_preset_deletion()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let library = LibraryService::new(database.clone())
            .create_library("Applied preset library", LibraryKind::Movie, false)
            .await?;
        let mut input = preset_input("Family");
        input.permissions.library_access_mode = "SELECTED".to_owned();
        input.permissions.library_ids = vec![library.id.to_string()];
        input.permissions.can_download = true;
        input.permissions.can_play = false;
        input.permissions.max_simultaneous_playbacks = 5;
        input.permissions.allow_video_transcode = false;
        let preset = create_user_permission_preset(&database, input).await?;
        let user_id = UserId::new().to_string();
        database
            .insert_user(
                &user_id,
                &user_id,
                "Preset Target",
                "test-hash",
                false,
                true,
            )
            .await?;

        let applied = apply_user_permission_preset(&database, &user_id, &preset.id).await?;

        assert_eq!(applied.id, preset.id);
        assert_eq!(applied.version, 1);
        assert_eq!(
            database.list_accessible_library_ids(&user_id).await?,
            vec![library.id.to_string()]
        );
        let playback_policy = database
            .get_user_playback_policy(&user_id)
            .await?
            .expect("user exists");
        assert!(!playback_policy.can_play);
        assert_eq!(playback_policy.max_simultaneous_playbacks, 5);
        assert!(!playback_policy.allow_video_transcode);

        delete_user_permission_preset(&database, &preset.id, 1).await?;
        let source: (String, String, i64) = sqlx::query_as(
            "SELECT preset_id, preset_name, preset_version
             FROM user_permission_preset_sources WHERE user_id = ?",
        )
        .bind(&user_id)
        .fetch_one(database.pool())
        .await?;
        assert_eq!(source, (preset.id, "Family".to_owned(), 1));
        assert_eq!(
            database.list_accessible_library_ids(&user_id).await?,
            vec![library.id.to_string()]
        );
        let builtin = apply_user_permission_preset(&database, &user_id, "builtin-user").await?;
        assert!(builtin.is_builtin);
        assert_eq!(builtin.id, "builtin-user");
        assert!(
            database
                .list_accessible_library_ids(&user_id)
                .await?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn rejects_invalid_targets_expired_library_snapshots_and_last_manager_demotion()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        assert!(matches!(
            apply_user_permission_preset(&database, "not-a-uuid", "builtin-user").await,
            Err(UserPermissionPresetApplicationError::InvalidUserId)
        ));
        let missing_user_id = UserId::new().to_string();
        assert!(matches!(
            apply_user_permission_preset(&database, &missing_user_id, "builtin-user").await,
            Err(UserPermissionPresetApplicationError::NotFound)
        ));
        let missing_preset_user_id = UserId::new().to_string();
        database
            .insert_user(
                &missing_preset_user_id,
                &missing_preset_user_id,
                "Missing Preset Target",
                "test-hash",
                false,
                true,
            )
            .await?;
        assert!(matches!(
            apply_user_permission_preset(&database, &missing_preset_user_id, "missing-preset")
                .await,
            Err(UserPermissionPresetApplicationError::PresetNotFound)
        ));

        let library = LibraryService::new(database.clone())
            .create_library("Disabled preset library", LibraryKind::Movie, false)
            .await?;
        let mut input = preset_input("Enabled library only");
        input.permissions.library_access_mode = "SELECTED".to_owned();
        input.permissions.library_ids = vec![library.id.to_string()];
        let preset = create_user_permission_preset(&database, input).await?;
        sqlx::query("UPDATE libraries SET is_enabled = 0 WHERE id = ?")
            .bind(library.id.to_string())
            .execute(database.pool())
            .await?;
        assert!(matches!(
            apply_user_permission_preset(&database, &missing_preset_user_id, &preset.id).await,
            Err(UserPermissionPresetApplicationError::InvalidSnapshot)
        ));
        let mode: String = sqlx::query_scalar("SELECT library_access_mode FROM users WHERE id = ?")
            .bind(&missing_preset_user_id)
            .fetch_one(database.pool())
            .await?;
        assert_eq!(mode, "ALL");

        let manager_id = UserId::new().to_string();
        database
            .insert_user(
                &manager_id,
                &manager_id,
                "Only Manager",
                "test-hash",
                true,
                true,
            )
            .await?;
        assert!(matches!(
            apply_user_permission_preset(&database, &manager_id, "builtin-user").await,
            Err(UserPermissionPresetApplicationError::LastManager)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn enforces_custom_preset_limit() -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        let presets = (0..MAX_CUSTOM_PRESETS)
            .map(|index| UserPermissionPreset {
                id: format!("custom-{index}"),
                name: format!("Preset {index}"),
                description: None,
                version: 1,
                is_builtin: false,
                is_admin: false,
                can_manage_server: false,
                permissions: NewUserPermissions::default(),
            })
            .collect::<Vec<_>>();
        let json = serde_json::to_string(&presets)?;
        assert!(
            database
                .compare_and_set_user_permission_presets_json(None, &json)
                .await?
        );

        assert!(matches!(
            create_user_permission_preset(&database, preset_input("One too many")).await,
            Err(UserPermissionPresetError::PresetLimitReached)
        ));
        Ok(())
    }
}
