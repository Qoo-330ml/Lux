use serde::{Deserialize, Serialize};
use std::fmt;

use crate::storage::{
    Database, NewUserAccountPreferences, StorageError, UserAccountPreferences as StoredPreferences,
    UserAccountPreferencesCasResult,
};

const MAX_LANGUAGE_PREFERENCE_CHARS: usize = 64;

#[allow(dead_code)]
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
            version: 0,
            updated_at: None,
        }
    }
}

#[allow(dead_code)]
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

impl From<StorageError> for AccountPreferencesError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

#[allow(dead_code)]
pub(crate) async fn get_account_preferences(
    database: &Database,
    user_id: &str,
) -> Result<AccountPreferences, AccountPreferencesError> {
    Ok(database
        .user_account_preferences(user_id)
        .await?
        .map(AccountPreferences::from)
        .unwrap_or_default())
}

#[allow(dead_code)]
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

fn validate_preferences(input: &UpdateAccountPreferences) -> Result<(), AccountPreferencesError> {
    if input.expected_version < 0
        || !matches!(input.theme.as_str(), "light" | "dark")
        || !matches!(
            input.accent_color.as_str(),
            "silver" | "berry" | "ocean" | "amber" | "mint"
        )
        || !valid_language(&input.audio_language)
        || !valid_language(&input.subtitle_language)
    {
        return Err(AccountPreferencesError::InvalidPreferences);
    }
    Ok(())
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
            version: preferences.version,
            updated_at: Some(preferences.updated_at),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{config::Config, storage::Database};

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
}
