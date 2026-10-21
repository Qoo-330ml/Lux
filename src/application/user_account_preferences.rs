use serde::{Deserialize, Serialize};
use std::fmt;

use crate::storage::{
    Database, NewUserAccountPreferences, StorageError, UserAccountPreferences as StoredPreferences,
    UserAccountPreferencesCasResult,
};

const MAX_LANGUAGE_PREFERENCE_CHARS: usize = 64;
pub(crate) const MAX_BATCH_ACCOUNT_PREFERENCE_TARGETS: usize = 100;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountPreferences {
    pub(crate) theme: String,
    pub(crate) accent_color: String,
    pub(crate) show_media_libraries: bool,
    pub(crate) show_continue_watching: bool,
    pub(crate) audio_language: String,
    pub(crate) subtitle_language: String,
    pub(crate) auto_play_next_episode: bool,
    pub(crate) library_order: Vec<String>,
    pub(crate) played_percent: i64,
    pub(crate) version: i64,
    pub(crate) updated_at: Option<i64>,
}

impl Default for AccountPreferences {
    fn default() -> Self {
        Self {
            theme: "dark".to_owned(),
            accent_color: "silver".to_owned(),
            show_media_libraries: true,
            show_continue_watching: true,
            audio_language: "原始音轨".to_owned(),
            subtitle_language: "简体中文".to_owned(),
            auto_play_next_episode: true,
            library_order: Vec::new(),
            played_percent: 95,
            version: 0,
            updated_at: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct UpdateAccountPreferences {
    pub(crate) expected_version: i64,
    pub(crate) theme: String,
    pub(crate) accent_color: String,
    pub(crate) show_media_libraries: bool,
    pub(crate) show_continue_watching: bool,
    pub(crate) audio_language: String,
    pub(crate) subtitle_language: String,
    pub(crate) auto_play_next_episode: bool,
    pub(crate) library_order: Option<Vec<String>>,
    pub(crate) played_percent: Option<i64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CopyAccountPreferencesInput {
    pub(crate) source_user_id: String,
    pub(crate) source_expected_version: i64,
    pub(crate) targets: Vec<CopyAccountPreferencesTarget>,
    pub(crate) groups: CopyAccountPreferenceGroups,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CopyAccountPreferencesTarget {
    pub(crate) user_id: String,
    pub(crate) expected_version: i64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PreviewAccountPreferencesCopyInput {
    pub(crate) source_user_id: String,
    pub(crate) target_user_ids: Vec<String>,
    pub(crate) groups: CopyAccountPreferenceGroups,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct CopyAccountPreferenceGroups {
    pub(crate) appearance: bool,
    pub(crate) home: bool,
    pub(crate) playback: bool,
    #[serde(default)]
    pub(crate) library_order: bool,
}

impl CopyAccountPreferenceGroups {
    fn is_empty(self) -> bool {
        !self.appearance && !self.home && !self.playback && !self.library_order
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum AccountPreferencesCopyErrorCode {
    UserUnavailable,
    VersionConflict,
    InvalidPreferences,
    StorageUnavailable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountPreferencesCopyTargetResult {
    pub(crate) user_id: String,
    pub(crate) success: bool,
    pub(crate) error_code: Option<AccountPreferencesCopyErrorCode>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountPreferencesCopyBatchResult {
    pub(crate) source_user_id: String,
    pub(crate) source_version: i64,
    pub(crate) results: Vec<AccountPreferencesCopyTargetResult>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountPreferencesCopyPreview {
    pub(crate) source_user_id: String,
    pub(crate) source_version: i64,
    pub(crate) source_snapshot: AccountPreferencesCopySnapshot,
    pub(crate) targets: Vec<AccountPreferencesCopyPreviewTarget>,
    pub(crate) groups: CopyAccountPreferenceGroups,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountPreferencesCopyPreviewTarget {
    pub(crate) user_id: String,
    pub(crate) expected_version: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) library_order: Option<Vec<String>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountPreferencesCopySnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) appearance: Option<AccountPreferencesAppearanceSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) home: Option<AccountPreferencesHomeSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) playback: Option<AccountPreferencesPlaybackSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) library_order: Option<Vec<String>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountPreferencesAppearanceSnapshot {
    pub(crate) theme: String,
    pub(crate) accent_color: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountPreferencesHomeSnapshot {
    pub(crate) show_media_libraries: bool,
    pub(crate) show_continue_watching: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountPreferencesPlaybackSnapshot {
    pub(crate) audio_language: String,
    pub(crate) subtitle_language: String,
    pub(crate) auto_play_next_episode: bool,
    pub(crate) played_percent: i64,
}

#[derive(Debug)]
pub(crate) enum AccountPreferencesError {
    InvalidPreferences,
    Storage(StorageError),
    UserUnavailable,
    VersionConflict,
}

impl fmt::Display for AccountPreferencesError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPreferences => formatter.write_str("account preferences are invalid"),
            Self::Storage(error) => error.fmt(formatter),
            Self::UserUnavailable => formatter.write_str("account is unavailable"),
            Self::VersionConflict => formatter.write_str("account preferences version changed"),
        }
    }
}

impl std::error::Error for AccountPreferencesError {}

#[derive(Debug)]
pub(crate) enum AccountPreferencesCopyError {
    InvalidRequest,
    SourceUnavailable,
    SourceVersionConflict,
    UserUnavailable,
    VersionConflict,
    InvalidPreferences,
    Storage(StorageError),
}

impl fmt::Display for AccountPreferencesCopyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest => {
                formatter.write_str("account preference copy request is invalid")
            }
            Self::SourceUnavailable => formatter.write_str("source account is unavailable"),
            Self::SourceVersionConflict => {
                formatter.write_str("source account preferences version changed")
            }
            Self::UserUnavailable => formatter.write_str("target account is unavailable"),
            Self::VersionConflict => {
                formatter.write_str("target account preferences version changed")
            }
            Self::InvalidPreferences => formatter.write_str("account preferences are invalid"),
            Self::Storage(error) => {
                let _ = error;
                formatter.write_str("account preference storage is unavailable")
            }
        }
    }
}

impl std::error::Error for AccountPreferencesCopyError {}

impl From<StorageError> for AccountPreferencesCopyError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<StorageError> for AccountPreferencesError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

pub(crate) async fn get_account_preferences(
    database: &Database,
    user_id: &str,
) -> Result<AccountPreferences, AccountPreferencesError> {
    let mut preferences = database
        .user_account_preferences(user_id)
        .await?
        .map(AccountPreferences::from)
        .unwrap_or_default();
    if preferences.version == 0 {
        preferences.library_order = database.user_library_order(user_id).await?;
        preferences.played_percent = database.user_played_percent(user_id).await?;
    }
    Ok(preferences)
}

pub(crate) async fn update_account_preferences(
    database: &Database,
    user_id: &str,
    input: UpdateAccountPreferences,
) -> Result<AccountPreferences, AccountPreferencesError> {
    validate_preferences(&input)?;
    match database
        .compare_and_swap_user_account_preferences(
            user_id,
            input.expected_version,
            NewUserAccountPreferences {
                theme: &input.theme,
                accent_color: &input.accent_color,
                show_media_libraries: input.show_media_libraries,
                show_continue_watching: input.show_continue_watching,
                audio_language: &input.audio_language,
                subtitle_language: &input.subtitle_language,
                auto_play_next_episode: input.auto_play_next_episode,
                library_order: input.library_order.as_deref(),
                played_percent: input.played_percent,
            },
        )
        .await?
    {
        UserAccountPreferencesCasResult::Updated(preferences) => Ok(preferences.into()),
        UserAccountPreferencesCasResult::Conflict => Err(AccountPreferencesError::VersionConflict),
        UserAccountPreferencesCasResult::UserUnavailable => {
            Err(AccountPreferencesError::UserUnavailable)
        }
    }
}

pub(crate) async fn preview_account_preferences_copy(
    database: &Database,
    input: PreviewAccountPreferencesCopyInput,
) -> Result<AccountPreferencesCopyPreview, AccountPreferencesCopyError> {
    let validation_targets = input
        .target_user_ids
        .iter()
        .map(|user_id| CopyAccountPreferencesTarget {
            user_id: user_id.clone(),
            expected_version: 0,
        })
        .collect::<Vec<_>>();
    validate_copy_input(&CopyAccountPreferencesInput {
        source_user_id: input.source_user_id.clone(),
        source_expected_version: 0,
        targets: validation_targets,
        groups: input.groups,
    })?;

    let source_user = database
        .find_user_by_id(&input.source_user_id)
        .await?
        .filter(|user| user.deleted_at.is_none())
        .ok_or(AccountPreferencesCopyError::SourceUnavailable)?;
    let source_preferences = database
        .user_account_preferences(&source_user.id)
        .await?
        .map(AccountPreferences::from)
        .unwrap_or_default();

    let mut targets = Vec::with_capacity(input.target_user_ids.len());
    for target_user_id in &input.target_user_ids {
        let user = database
            .find_user_by_id(target_user_id)
            .await?
            .filter(|user| user.deleted_at.is_none())
            .ok_or(AccountPreferencesCopyError::UserUnavailable)?;
        let expected_version = database
            .user_account_preferences(&user.id)
            .await?
            .map(|preferences| preferences.version)
            .unwrap_or_default();
        let library_order = if input.groups.library_order {
            let accessible_library_ids = database.list_accessible_library_ids(&user.id).await?;
            let accessible_library_ids = accessible_library_ids
                .into_iter()
                .collect::<std::collections::HashSet<_>>();
            Some(
                source_preferences
                    .library_order
                    .iter()
                    .filter(|library_id| accessible_library_ids.contains(*library_id))
                    .cloned()
                    .collect(),
            )
        } else {
            None
        };
        targets.push(AccountPreferencesCopyPreviewTarget {
            user_id: user.id,
            expected_version,
            library_order,
        });
    }

    Ok(AccountPreferencesCopyPreview {
        source_user_id: source_user.id,
        source_version: source_preferences.version,
        source_snapshot: copy_snapshot(&source_preferences, input.groups),
        targets,
        groups: input.groups,
    })
}

fn copy_snapshot(
    preferences: &AccountPreferences,
    groups: CopyAccountPreferenceGroups,
) -> AccountPreferencesCopySnapshot {
    AccountPreferencesCopySnapshot {
        appearance: groups
            .appearance
            .then(|| AccountPreferencesAppearanceSnapshot {
                theme: preferences.theme.clone(),
                accent_color: preferences.accent_color.clone(),
            }),
        home: groups.home.then_some(AccountPreferencesHomeSnapshot {
            show_media_libraries: preferences.show_media_libraries,
            show_continue_watching: preferences.show_continue_watching,
        }),
        playback: groups.playback.then(|| AccountPreferencesPlaybackSnapshot {
            audio_language: preferences.audio_language.clone(),
            subtitle_language: preferences.subtitle_language.clone(),
            auto_play_next_episode: preferences.auto_play_next_episode,
            played_percent: preferences.played_percent,
        }),
        library_order: groups
            .library_order
            .then(|| preferences.library_order.clone()),
    }
}

pub(crate) async fn copy_account_preferences_batch(
    database: &Database,
    input: CopyAccountPreferencesInput,
) -> Result<AccountPreferencesCopyBatchResult, AccountPreferencesCopyError> {
    validate_copy_input(&input)?;
    let source_user = database
        .find_user_by_id(&input.source_user_id)
        .await?
        .filter(|user| user.deleted_at.is_none())
        .ok_or(AccountPreferencesCopyError::SourceUnavailable)?;
    let source_preferences = database
        .user_account_preferences(&source_user.id)
        .await?
        .map(AccountPreferences::from)
        .unwrap_or_default();
    if source_preferences.version != input.source_expected_version {
        return Err(AccountPreferencesCopyError::SourceVersionConflict);
    }

    let mut results = Vec::with_capacity(input.targets.len());
    for target in &input.targets {
        let error_code =
            match copy_preferences_to_target(database, target, &source_preferences, input.groups)
                .await
            {
                Ok(()) => None,
                Err(error) => Some(match error {
                    AccountPreferencesCopyError::VersionConflict => {
                        AccountPreferencesCopyErrorCode::VersionConflict
                    }
                    AccountPreferencesCopyError::UserUnavailable => {
                        AccountPreferencesCopyErrorCode::UserUnavailable
                    }
                    AccountPreferencesCopyError::InvalidPreferences => {
                        AccountPreferencesCopyErrorCode::InvalidPreferences
                    }
                    AccountPreferencesCopyError::Storage(_) => {
                        AccountPreferencesCopyErrorCode::StorageUnavailable
                    }
                    AccountPreferencesCopyError::InvalidRequest
                    | AccountPreferencesCopyError::SourceUnavailable
                    | AccountPreferencesCopyError::SourceVersionConflict => {
                        AccountPreferencesCopyErrorCode::StorageUnavailable
                    }
                }),
            };
        results.push(AccountPreferencesCopyTargetResult {
            user_id: target.user_id.clone(),
            success: error_code.is_none(),
            error_code,
        });
    }
    Ok(AccountPreferencesCopyBatchResult {
        source_user_id: source_user.id,
        source_version: source_preferences.version,
        results,
    })
}

async fn copy_preferences_to_target(
    database: &Database,
    target: &CopyAccountPreferencesTarget,
    source: &AccountPreferences,
    groups: CopyAccountPreferenceGroups,
) -> Result<(), AccountPreferencesCopyError> {
    let user = database
        .find_user_by_id(&target.user_id)
        .await?
        .filter(|user| user.deleted_at.is_none())
        .ok_or(AccountPreferencesCopyError::UserUnavailable)?;
    let current = database
        .user_account_preferences(&user.id)
        .await?
        .map(AccountPreferences::from)
        .unwrap_or_default();
    if current.version != target.expected_version {
        return Err(AccountPreferencesCopyError::VersionConflict);
    }
    let library_order = if groups.library_order {
        let accessible_library_ids = database.list_accessible_library_ids(&user.id).await?;
        let accessible_library_ids = accessible_library_ids
            .into_iter()
            .collect::<std::collections::HashSet<_>>();
        Some(
            source
                .library_order
                .iter()
                .filter(|library_id| accessible_library_ids.contains(*library_id))
                .cloned()
                .collect(),
        )
    } else {
        None
    };
    let input = UpdateAccountPreferences {
        expected_version: current.version,
        theme: if groups.appearance {
            &source.theme
        } else {
            &current.theme
        }
        .clone(),
        accent_color: if groups.appearance {
            &source.accent_color
        } else {
            &current.accent_color
        }
        .clone(),
        show_media_libraries: if groups.home {
            source.show_media_libraries
        } else {
            current.show_media_libraries
        },
        show_continue_watching: if groups.home {
            source.show_continue_watching
        } else {
            current.show_continue_watching
        },
        audio_language: if groups.playback {
            &source.audio_language
        } else {
            &current.audio_language
        }
        .clone(),
        subtitle_language: if groups.playback {
            &source.subtitle_language
        } else {
            &current.subtitle_language
        }
        .clone(),
        auto_play_next_episode: if groups.playback {
            source.auto_play_next_episode
        } else {
            current.auto_play_next_episode
        },
        library_order,
        played_percent: groups.playback.then_some(source.played_percent),
    };
    update_account_preferences(database, &target.user_id, input)
        .await
        .map_err(|error| match error {
            AccountPreferencesError::InvalidPreferences => {
                AccountPreferencesCopyError::InvalidPreferences
            }
            AccountPreferencesError::Storage(error) => AccountPreferencesCopyError::Storage(error),
            AccountPreferencesError::UserUnavailable => {
                AccountPreferencesCopyError::UserUnavailable
            }
            AccountPreferencesError::VersionConflict => {
                AccountPreferencesCopyError::VersionConflict
            }
        })?;
    Ok(())
}

fn validate_copy_input(
    input: &CopyAccountPreferencesInput,
) -> Result<(), AccountPreferencesCopyError> {
    if input.source_expected_version < 0
        || input.targets.is_empty()
        || input.targets.len() > MAX_BATCH_ACCOUNT_PREFERENCE_TARGETS
        || input.groups.is_empty()
        || input
            .targets
            .iter()
            .any(|target| target.expected_version < 0 || target.user_id == input.source_user_id)
    {
        return Err(AccountPreferencesCopyError::InvalidRequest);
    }
    let mut user_ids = std::collections::HashSet::with_capacity(input.targets.len());
    if input
        .targets
        .iter()
        .any(|target| !user_ids.insert(&target.user_id))
    {
        return Err(AccountPreferencesCopyError::InvalidRequest);
    }
    Ok(())
}

fn validate_preferences(input: &UpdateAccountPreferences) -> Result<(), AccountPreferencesError> {
    if input.expected_version < 0
        || !matches!(input.theme.as_str(), "light" | "dark")
        || !matches!(
            input.accent_color.as_str(),
            "silver" | "berry" | "ocean" | "amber" | "mint"
        )
        || !valid_language(&input.audio_language)
        || !valid_language(&input.subtitle_language)
        || input
            .played_percent
            .is_some_and(|played_percent| !(1..=100).contains(&played_percent))
        || input
            .library_order
            .as_ref()
            .is_some_and(|order| order.len() > 1024 || has_duplicates(order))
    {
        return Err(AccountPreferencesError::InvalidPreferences);
    }
    Ok(())
}

fn has_duplicates(values: &[String]) -> bool {
    let mut seen = std::collections::HashSet::with_capacity(values.len());
    values.iter().any(|value| !seen.insert(value))
}

fn valid_language(value: &str) -> bool {
    value.chars().count() <= MAX_LANGUAGE_PREFERENCE_CHARS && !value.chars().any(char::is_control)
}

impl From<StoredPreferences> for AccountPreferences {
    fn from(preferences: StoredPreferences) -> Self {
        Self {
            theme: preferences.theme,
            accent_color: preferences.accent_color,
            show_media_libraries: preferences.show_media_libraries,
            show_continue_watching: preferences.show_continue_watching,
            audio_language: preferences.audio_language,
            subtitle_language: preferences.subtitle_language,
            auto_play_next_episode: preferences.auto_play_next_episode,
            library_order: preferences.library_order,
            played_percent: preferences.played_percent,
            version: preferences.version,
            updated_at: Some(preferences.updated_at),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        application::libraries::LibraryService, config::Config, library::LibraryKind,
        storage::Database,
    };

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

    fn valid_update(expected_version: i64) -> UpdateAccountPreferences {
        UpdateAccountPreferences {
            expected_version,
            theme: "light".to_owned(),
            accent_color: "berry".to_owned(),
            show_media_libraries: false,
            show_continue_watching: true,
            audio_language: "日本語".to_owned(),
            subtitle_language: "关闭".to_owned(),
            auto_play_next_episode: false,
            library_order: None,
            played_percent: None,
        }
    }

    #[tokio::test]
    async fn returns_account_page_defaults_when_no_server_preferences_exist()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        database
            .insert_user(
                "preferences-default-user",
                "preferences-default-user",
                "Defaults",
                "test",
                false,
                true,
            )
            .await?;

        let preferences = get_account_preferences(&database, "preferences-default-user").await?;

        assert_eq!(preferences.theme, "dark");
        assert_eq!(preferences.accent_color, "silver");
        assert!(preferences.show_media_libraries);
        assert!(preferences.show_continue_watching);
        assert_eq!(preferences.audio_language, "原始音轨");
        assert_eq!(preferences.subtitle_language, "简体中文");
        assert!(preferences.auto_play_next_episode);
        assert_eq!(preferences.version, 0);
        assert_eq!(preferences.updated_at, None);
        Ok(())
    }

    #[tokio::test]
    async fn version_zero_snapshot_includes_existing_server_library_order_and_played_percent()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        database
            .insert_user(
                "preferences-legacy-snapshot-user",
                "preferences-legacy-snapshot-user",
                "Legacy snapshot",
                "test",
                false,
                true,
            )
            .await?;
        let library = LibraryService::new(database.clone())
            .create_library("Legacy preference library", LibraryKind::Movie, false)
            .await?;
        database
            .replace_user_library_order(
                "preferences-legacy-snapshot-user",
                &[library.id.to_string()],
            )
            .await?;
        database
            .set_user_played_percent("preferences-legacy-snapshot-user", 81)
            .await?;

        let preferences =
            get_account_preferences(&database, "preferences-legacy-snapshot-user").await?;

        assert_eq!(preferences.version, 0);
        assert_eq!(preferences.library_order, [library.id.to_string()]);
        assert_eq!(preferences.played_percent, 81);
        Ok(())
    }

    #[tokio::test]
    async fn image_optimization_is_not_an_account_preference()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        database
            .insert_user("images-user", "images-user", "Images", "test", false, true)
            .await?;
        let saved = update_account_preferences(&database, "images-user", valid_update(0)).await?;
        assert!(
            serde_json::to_value(&saved)?
                .get("optimizedImages")
                .is_none()
        );
        assert_eq!(
            get_account_preferences(&database, "images-user").await?,
            saved
        );
        let obsolete_input = serde_json::json!({
            "expectedVersion": 1, "theme": "dark", "accentColor": "silver",
            "showMediaLibraries": true, "showContinueWatching": true,
            "audioLanguage": "原始音轨", "subtitleLanguage": "简体中文",
            "autoPlayNextEpisode": true, "optimizedImages": true,
        });
        assert!(serde_json::from_value::<UpdateAccountPreferences>(obsolete_input).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn updates_preferences_with_compare_and_swap_and_reports_stale_versions()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        database
            .insert_user(
                "preferences-update-user",
                "preferences-update-user",
                "Update",
                "test",
                false,
                true,
            )
            .await?;

        let created =
            update_account_preferences(&database, "preferences-update-user", valid_update(0))
                .await?;
        assert_eq!(created.version, 1);
        assert_eq!(created.theme, "light");
        assert_eq!(created.accent_color, "berry");
        assert!(created.updated_at.is_some());

        let stale =
            update_account_preferences(&database, "preferences-update-user", valid_update(0)).await;
        assert!(matches!(
            stale,
            Err(AccountPreferencesError::VersionConflict)
        ));
        assert_eq!(
            get_account_preferences(&database, "preferences-update-user")
                .await?
                .version,
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn validates_account_preferences_before_storage_write()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        database
            .insert_user(
                "preferences-validation-user",
                "preferences-validation-user",
                "Validation",
                "test",
                false,
                true,
            )
            .await?;

        let mut input = valid_update(0);
        input.theme = "sepia".to_owned();
        assert!(matches!(
            update_account_preferences(&database, "preferences-validation-user", input).await,
            Err(AccountPreferencesError::InvalidPreferences)
        ));

        let mut input = valid_update(0);
        input.accent_color = "ultraviolet".to_owned();
        assert!(matches!(
            update_account_preferences(&database, "preferences-validation-user", input).await,
            Err(AccountPreferencesError::InvalidPreferences)
        ));

        let mut input = valid_update(0);
        input.expected_version = -1;
        assert!(matches!(
            update_account_preferences(&database, "preferences-validation-user", input).await,
            Err(AccountPreferencesError::InvalidPreferences)
        ));

        let mut input = valid_update(0);
        input.audio_language = "音轨".repeat(33);
        assert!(matches!(
            update_account_preferences(&database, "preferences-validation-user", input).await,
            Err(AccountPreferencesError::InvalidPreferences)
        ));

        let mut input = valid_update(0);
        input.subtitle_language = "简体\n中文".to_owned();
        assert!(matches!(
            update_account_preferences(&database, "preferences-validation-user", input).await,
            Err(AccountPreferencesError::InvalidPreferences)
        ));

        let mut input = valid_update(0);
        input.played_percent = Some(101);
        assert!(matches!(
            update_account_preferences(&database, "preferences-validation-user", input).await,
            Err(AccountPreferencesError::InvalidPreferences)
        ));

        let mut input = valid_update(0);
        input.library_order = Some(vec!["duplicate".to_owned(), "duplicate".to_owned()]);
        assert!(matches!(
            update_account_preferences(&database, "preferences-validation-user", input).await,
            Err(AccountPreferencesError::InvalidPreferences)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn rejects_updates_for_deleted_users() -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        database
            .insert_user(
                "preferences-deleted-user",
                "preferences-deleted-user",
                "Deleted",
                "test",
                false,
                true,
            )
            .await?;
        database
            .soft_delete_user("preferences-deleted-user")
            .await?;

        assert!(matches!(
            update_account_preferences(&database, "preferences-deleted-user", valid_update(0))
                .await,
            Err(AccountPreferencesError::UserUnavailable)
        ));
        Ok(())
    }

    #[tokio::test]
    async fn copies_only_selected_preference_groups_with_target_version_checks()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        for user_id in ["copy-source", "copy-target"] {
            database
                .insert_user(user_id, user_id, user_id, "test", false, true)
                .await?;
        }
        sqlx::query(
            "INSERT INTO libraries (id, name, kind)
             VALUES ('copy-source-library', 'Source', 'MOVIE'),
                    ('copy-target-library', 'Target', 'MOVIE')",
        )
        .execute(database.pool())
        .await?;
        update_account_preferences(&database, "copy-source", valid_update(0)).await?;
        update_account_preferences(&database, "copy-target", valid_update(0)).await?;
        database
            .replace_user_library_order("copy-source", &["copy-source-library".to_owned()])
            .await?;
        database
            .replace_user_library_order("copy-target", &["copy-target-library".to_owned()])
            .await?;
        database.set_user_played_percent("copy-source", 67).await?;
        database.set_user_played_percent("copy-target", 42).await?;

        let result = copy_account_preferences_batch(
            &database,
            CopyAccountPreferencesInput {
                source_user_id: "copy-source".to_owned(),
                source_expected_version: 1,
                targets: vec![CopyAccountPreferencesTarget {
                    user_id: "copy-target".to_owned(),
                    expected_version: 1,
                }],
                groups: CopyAccountPreferenceGroups {
                    appearance: true,
                    home: true,
                    playback: false,
                    library_order: false,
                },
            },
        )
        .await?;

        assert_eq!(result.source_version, 1);
        assert_eq!(result.results.len(), 1);
        assert!(result.results[0].success);
        assert_eq!(result.results[0].error_code, None);
        let source = get_account_preferences(&database, "copy-source").await?;
        let target = get_account_preferences(&database, "copy-target").await?;
        assert_eq!(target.theme, source.theme);
        assert_eq!(target.accent_color, source.accent_color);
        assert_eq!(target.show_media_libraries, source.show_media_libraries);
        assert_eq!(target.show_continue_watching, source.show_continue_watching);
        assert_eq!(target.audio_language, "日本語");
        assert_eq!(target.subtitle_language, "关闭");
        assert_eq!(target.auto_play_next_episode, source.auto_play_next_episode);
        assert_eq!(target.library_order, ["copy-target-library"]);
        assert_eq!(target.played_percent, 42);
        assert_eq!(target.version, 2);
        Ok(())
    }

    #[tokio::test]
    async fn previews_only_selected_groups_and_captures_source_and_target_versions()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        for user_id in ["preview-source", "preview-target"] {
            database
                .insert_user(user_id, user_id, user_id, "test", false, true)
                .await?;
        }
        update_account_preferences(&database, "preview-source", valid_update(0)).await?;

        let preview = preview_account_preferences_copy(
            &database,
            PreviewAccountPreferencesCopyInput {
                source_user_id: "preview-source".to_owned(),
                target_user_ids: vec!["preview-target".to_owned()],
                groups: CopyAccountPreferenceGroups {
                    appearance: true,
                    home: false,
                    playback: true,
                    library_order: false,
                },
            },
        )
        .await?;

        assert_eq!(preview.source_user_id, "preview-source");
        assert_eq!(preview.source_version, 1);
        assert_eq!(
            preview
                .source_snapshot
                .appearance
                .as_ref()
                .map(|value| value.theme.as_str()),
            Some("light")
        );
        assert_eq!(
            preview
                .source_snapshot
                .appearance
                .as_ref()
                .map(|value| value.accent_color.as_str()),
            Some("berry")
        );
        assert_eq!(preview.source_snapshot.home, None);
        assert_eq!(
            preview
                .source_snapshot
                .playback
                .as_ref()
                .map(|value| value.audio_language.as_str()),
            Some("日本語")
        );
        assert_eq!(preview.targets.len(), 1);
        assert_eq!(preview.targets[0].user_id, "preview-target");
        assert_eq!(preview.targets[0].expected_version, 0);
        Ok(())
    }

    #[tokio::test]
    async fn reports_batch_preference_copy_results_independently_per_target()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        for user_id in [
            "copy-source",
            "copy-ok",
            "copy-conflict",
            "copy-deleted",
            "copy-storage-error",
        ] {
            database
                .insert_user(user_id, user_id, user_id, "test", false, true)
                .await?;
        }
        update_account_preferences(&database, "copy-source", valid_update(0)).await?;
        update_account_preferences(&database, "copy-conflict", valid_update(0)).await?;
        database.soft_delete_user("copy-deleted").await?;
        sqlx::query(
            "CREATE TRIGGER fail_one_account_preference_copy
             BEFORE INSERT ON user_account_preferences
             WHEN NEW.user_id = 'copy-storage-error'
             BEGIN
                 SELECT RAISE(ABORT, 'injected preference copy failure');
             END",
        )
        .execute(database.pool())
        .await?;

        let result = copy_account_preferences_batch(
            &database,
            CopyAccountPreferencesInput {
                source_user_id: "copy-source".to_owned(),
                source_expected_version: 1,
                targets: vec![
                    CopyAccountPreferencesTarget {
                        user_id: "copy-ok".to_owned(),
                        expected_version: 0,
                    },
                    CopyAccountPreferencesTarget {
                        user_id: "copy-conflict".to_owned(),
                        expected_version: 0,
                    },
                    CopyAccountPreferencesTarget {
                        user_id: "copy-deleted".to_owned(),
                        expected_version: 0,
                    },
                    CopyAccountPreferencesTarget {
                        user_id: "copy-missing".to_owned(),
                        expected_version: 0,
                    },
                    CopyAccountPreferencesTarget {
                        user_id: "copy-storage-error".to_owned(),
                        expected_version: 0,
                    },
                ],
                groups: CopyAccountPreferenceGroups {
                    appearance: false,
                    home: false,
                    playback: true,
                    library_order: false,
                },
            },
        )
        .await?;

        assert_eq!(
            result
                .results
                .iter()
                .map(|result| (result.user_id.as_str(), result.error_code))
                .collect::<Vec<_>>(),
            vec![
                ("copy-ok", None),
                (
                    "copy-conflict",
                    Some(AccountPreferencesCopyErrorCode::VersionConflict)
                ),
                (
                    "copy-deleted",
                    Some(AccountPreferencesCopyErrorCode::UserUnavailable)
                ),
                (
                    "copy-missing",
                    Some(AccountPreferencesCopyErrorCode::UserUnavailable)
                ),
                (
                    "copy-storage-error",
                    Some(AccountPreferencesCopyErrorCode::StorageUnavailable)
                ),
            ]
        );
        assert_eq!(
            get_account_preferences(&database, "copy-ok")
                .await?
                .audio_language,
            "日本語"
        );
        assert_eq!(
            get_account_preferences(&database, "copy-conflict")
                .await?
                .version,
            1
        );
        Ok(())
    }

    #[tokio::test]
    async fn rejects_invalid_preference_copy_requests_before_writing_targets()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_temp_dir, database) = test_database().await?;
        for user_id in ["copy-source", "copy-target"] {
            database
                .insert_user(user_id, user_id, user_id, "test", false, true)
                .await?;
        }
        update_account_preferences(&database, "copy-source", valid_update(0)).await?;

        let mut input = copy_input("copy-source", "copy-target", 0);
        input.groups = CopyAccountPreferenceGroups {
            appearance: false,
            home: false,
            playback: false,
            library_order: false,
        };
        assert!(matches!(
            copy_account_preferences_batch(&database, input).await,
            Err(AccountPreferencesCopyError::InvalidRequest)
        ));

        let mut input = copy_input("copy-source", "copy-target", 0);
        input.source_expected_version = 0;
        assert!(matches!(
            copy_account_preferences_batch(&database, input).await,
            Err(AccountPreferencesCopyError::SourceVersionConflict)
        ));

        let mut input = copy_input("copy-source", "copy-target", 0);
        input.targets.push(input.targets[0].clone());
        assert!(matches!(
            copy_account_preferences_batch(&database, input).await,
            Err(AccountPreferencesCopyError::InvalidRequest)
        ));

        let mut input = copy_input("copy-source", "copy-target", 0);
        input.targets[0].expected_version = -1;
        assert!(matches!(
            copy_account_preferences_batch(&database, input).await,
            Err(AccountPreferencesCopyError::InvalidRequest)
        ));

        let mut input = copy_input("copy-source", "copy-target", 0);
        input.targets[0].user_id = input.source_user_id.clone();
        assert!(matches!(
            copy_account_preferences_batch(&database, input).await,
            Err(AccountPreferencesCopyError::InvalidRequest)
        ));

        let mut input = copy_input("copy-source", "copy-target", 0);
        input.targets.clear();
        assert!(matches!(
            copy_account_preferences_batch(&database, input).await,
            Err(AccountPreferencesCopyError::InvalidRequest)
        ));

        let mut input = copy_input("copy-source", "copy-target", 0);
        input.targets = (0..=MAX_BATCH_ACCOUNT_PREFERENCE_TARGETS)
            .map(|index| CopyAccountPreferencesTarget {
                user_id: format!("bulk-target-{index}"),
                expected_version: 0,
            })
            .collect();
        assert!(matches!(
            copy_account_preferences_batch(&database, input).await,
            Err(AccountPreferencesCopyError::InvalidRequest)
        ));

        let mut input = copy_input("missing-source", "copy-target", 0);
        assert!(matches!(
            copy_account_preferences_batch(&database, input.clone()).await,
            Err(AccountPreferencesCopyError::SourceUnavailable)
        ));

        database.soft_delete_user("copy-source").await?;
        input.source_user_id = "copy-source".to_owned();
        input.source_expected_version = 1;
        assert!(matches!(
            copy_account_preferences_batch(&database, input).await,
            Err(AccountPreferencesCopyError::SourceUnavailable)
        ));
        assert_eq!(
            get_account_preferences(&database, "copy-target")
                .await?
                .version,
            0
        );
        Ok(())
    }

    fn copy_input(
        source_user_id: &str,
        target_user_id: &str,
        expected_version: i64,
    ) -> CopyAccountPreferencesInput {
        CopyAccountPreferencesInput {
            source_user_id: source_user_id.to_owned(),
            source_expected_version: 1,
            targets: vec![CopyAccountPreferencesTarget {
                user_id: target_user_id.to_owned(),
                expected_version,
            }],
            groups: CopyAccountPreferenceGroups {
                appearance: true,
                home: false,
                playback: false,
                library_order: false,
            },
        }
    }
}
