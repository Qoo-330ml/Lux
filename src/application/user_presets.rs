use serde::{Deserialize, Serialize};
use std::fmt;

use crate::{
    application::settings::NewUserPermissions,
    storage::{Database, StorageError},
};

const PRESETS_SETTING_KEY: &str = "user_permission_presets";
const MAX_CUSTOM_PRESETS: usize = 100;
const MAX_PRESET_NAME_CHARS: usize = 80;
const MAX_PRESET_DESCRIPTION_CHARS: usize = 500;

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

#[derive(Clone, Debug)]
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
    use crate::{application::settings::NewUserPermissions, config::Config, storage::Database};

    use super::*;

    async fn test_database() -> Result<(tempfile::TempDir, Database), Box<dyn std::error::Error>> {
        let temp_dir = tempfile::tempdir()?;
        let database = Database::connect(&Config {
            http_addr: "127.0.0.1:8097".parse()?,
            config_dir: temp_dir.path().join("config"),
        })
        .await?;
        Ok((temp_dir, database))
    }

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
